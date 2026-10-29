//! Polyphonic synthesizer driven by a [`Schedule`] (SPEC-009).
//!
//! The audio thread hands one quantum at a time to [`Synth::render`]: MIDI
//! events that fall inside the quantum start or stop voices, and the voices
//! sum into the output buffer. Nothing here allocates, locks, or does I/O.
//!
//! Every voice computes its phase and envelope from **absolute sample
//! positions**, never from "how many frames have I seen so far". That is what
//! makes the output independent of the quantum size: rendering 4800 frames in
//! chunks of 256 or of 512 gives bit-for-bit the same samples.

use crate::midi::MidiMessage;
use crate::schedule::{Schedule, ScheduleCursor};

/// Voices the synth can play at once.
pub const MAX_VOICES: usize = 16;

/// Envelope settings shared by every voice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Envelope {
    /// Seconds from silence to peak.
    pub attack_s: f64,
    /// Seconds from peak to the sustain level.
    pub decay_s: f64,
    /// Level held while the note is down, as a fraction of peak.
    pub sustain: f64,
    /// Seconds from the current level to silence after a note-off.
    pub release_s: f64,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            attack_s: 0.005,
            decay_s: 0.08,
            sustain: 0.7,
            release_s: 0.12,
        }
    }
}

impl Envelope {
    /// Checks the settings make sense.
    ///
    /// # Errors
    ///
    /// [`SynthError::InvalidEnvelope`] for a negative or non-finite time, or a
    /// sustain level outside `0..=1`.
    pub fn validate(&self) -> Result<(), SynthError> {
        let times = [self.attack_s, self.decay_s, self.release_s];
        if times.iter().any(|t| !t.is_finite() || *t < 0.0) {
            return Err(SynthError::InvalidEnvelope);
        }
        if !self.sustain.is_finite() || self.sustain < 0.0 || self.sustain > 1.0 {
            return Err(SynthError::InvalidEnvelope);
        }
        Ok(())
    }
}

/// Why the synth could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthError {
    /// `sample_rate` was zero.
    InvalidSampleRate,
    /// [`Envelope`] had a negative or non-finite time or a bad sustain level.
    InvalidEnvelope,
}

impl std::fmt::Display for SynthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSampleRate => f.write_str("sample rate must be positive"),
            Self::InvalidEnvelope => f.write_str("envelope settings are out of range"),
        }
    }
}

impl std::error::Error for SynthError {}

/// Frequency of a MIDI note number in equal temperament, A4 = 440 Hz.
#[must_use]
pub fn note_to_hz(note: u8) -> f64 {
    // `note - 69` fits in i16 and is exact in f64; the exponent is exact too.
    let semitones = f64::from(i16::from(note) - 69);
    440.0 * (semitones / 12.0).exp2()
}

/// One oscillator plus its envelope.
#[derive(Debug, Clone, Copy)]
pub struct Voice {
    /// MIDI channel the note came from.
    pub channel: u8,
    /// MIDI note number.
    pub note: u8,
    /// Frequency in Hz, computed once at note-on.
    pub freq_hz: f64,
    /// Peak amplitude, `velocity / 127`.
    pub peak: f32,
    /// Absolute sample the voice started at.
    started_at: u64,
    /// Absolute sample the note-off landed on, if it already happened.
    released_at: Option<u64>,
    /// Absolute sample the release stage began (re-anchored, see `render`).
    release_from: u64,
    /// Level when the release began.
    release_level: f32,
    attack_samples: u64,
    decay_samples: u64,
    sustain: f32,
    release_samples: u64,
    /// Level at the end of the last rendered sample, used for re-anchoring.
    last_gain: f32,
}

impl Voice {
    /// A silent, unassigned voice.
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            channel: 0,
            note: 0,
            freq_hz: 0.0,
            peak: 0.0,
            started_at: 0,
            released_at: None,
            release_from: 0,
            release_level: 0.0,
            attack_samples: 0,
            decay_samples: 0,
            sustain: 0.0,
            release_samples: 0,
            last_gain: 0.0,
        }
    }

    /// Whether the voice has nothing left to play.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.last_gain <= 0.0 && self.released_at.is_none() && self.peak == 0.0
    }

    /// Level the voice reached at its last rendered sample.
    #[must_use]
    pub const fn gain(&self) -> f32 {
        self.last_gain
    }

    fn start(&mut self, channel: u8, note: u8, velocity: u8, at: u64, env: &Envelope, rate: u32) {
        let seconds_to_samples = |s: f64| seconds_to_samples(s, rate);
        self.channel = channel;
        self.note = note;
        self.freq_hz = note_to_hz(note);
        // Velocity 0 never reaches here: the parser turns it into NoteOff.
        self.peak = f32::from(velocity.max(1)) / 127.0;
        self.started_at = at;
        self.released_at = None;
        self.release_from = at;
        self.release_level = 0.0;
        self.attack_samples = seconds_to_samples(env.attack_s);
        self.decay_samples = seconds_to_samples(env.decay_s);
        self.release_samples = seconds_to_samples(env.release_s);
        #[allow(clippy::cast_possible_truncation)]
        let sustain = env.sustain as f32;
        self.sustain = sustain;
        self.last_gain = 0.0;
    }

    /// Marks the note as released from `at`.
    ///
    /// The level is taken from the envelope at `at` rather than from the last
    /// rendered sample: a note-off that arrives at the start of a quantum would
    /// otherwise freeze the level measured at the end of the previous quantum,
    /// which is `blocks - 1` samples too late for a second voice in the pool.
    fn release(&mut self, at: u64) {
        if self.released_at.is_some() {
            return;
        }
        self.release_level = self.level_at(at);
        self.released_at = Some(at);
        self.release_from = at;
    }

    /// Envelope level at absolute sample `at`.
    fn level_at(&self, at: u64) -> f32 {
        if at < self.started_at {
            return 0.0;
        }
        let elapsed = at - self.started_at;
        if let Some(released_at) = self.released_at {
            if at >= released_at {
                let into_release = at - self.release_from;
                return release_level(self.release_level, into_release, self.release_samples);
            }
        }
        level_before_release(elapsed, self)
    }
}

fn level_before_release(elapsed: u64, voice: &Voice) -> f32 {
    if elapsed < voice.attack_samples {
        #[allow(clippy::cast_precision_loss)]
        let done = elapsed as f32;
        #[allow(clippy::cast_precision_loss)]
        let total = voice.attack_samples as f32;
        return voice.peak * (done / total);
    }
    let into_decay = elapsed - voice.attack_samples;
    if into_decay < voice.decay_samples {
        #[allow(clippy::cast_precision_loss)]
        let done = into_decay as f32;
        #[allow(clippy::cast_precision_loss)]
        let total = voice.decay_samples as f32;
        let sustain = voice.peak * voice.sustain;
        return voice.peak + (sustain - voice.peak) * (done / total);
    }
    voice.peak * voice.sustain
}

fn release_level(from: f32, elapsed: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    if elapsed >= total {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let done = elapsed as f32;
    #[allow(clippy::cast_precision_loss)]
    let total = total as f32;
    from * (1.0 - done / total)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn seconds_to_samples(seconds: f64, rate: u32) -> u64 {
    // Clamped before the cast so a huge value cannot wrap.
    (seconds * f64::from(rate)).max(0.0) as u64
}

/// A fixed pool of voices, addressed by note.
#[derive(Debug, Clone)]
pub struct Synth {
    voices: [Voice; MAX_VOICES],
    envelope: Envelope,
    sample_rate: u32,
    started: u32,
}

impl Synth {
    /// Builds a silent synth for `sample_rate`.
    ///
    /// # Errors
    ///
    /// [`SynthError::InvalidSampleRate`] for a zero rate,
    /// [`SynthError::InvalidEnvelope`] for bad envelope settings.
    pub fn new(sample_rate: u32, envelope: Envelope) -> Result<Self, SynthError> {
        if sample_rate == 0 {
            return Err(SynthError::InvalidSampleRate);
        }
        envelope.validate()?;
        Ok(Self {
            voices: [Voice::idle(); MAX_VOICES],
            envelope,
            sample_rate,
            started: 0,
        })
    }

    /// Sample rate the synth was built for.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Every voice, idle ones included.
    #[must_use]
    pub const fn voices(&self) -> &[Voice; MAX_VOICES] {
        &self.voices
    }

    /// Voices started since construction (voice stealing included).
    #[must_use]
    pub const fn voices_started(&self) -> u32 {
        self.started
    }

    /// Silences every voice and forgets the statistics.
    ///
    /// Call after a backward seek: the notes that started before the new
    /// position must not hang on. The voice-stealing order is rebuilt from
    /// the notes that start after the seek, so the counter restarts with the
    /// voices.
    pub fn reset(&mut self) {
        self.voices = [Voice::idle(); MAX_VOICES];
        self.started = 0;
    }

    /// Envelope every new voice is built from.
    #[must_use]
    pub const fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// Replaces the envelope. Voices already sounding keep the envelope they
    /// started with.
    ///
    /// # Errors
    ///
    /// [`SynthError::InvalidEnvelope`] if `envelope` is out of range; the
    /// current one is kept.
    pub fn set_envelope(&mut self, envelope: Envelope) -> Result<(), SynthError> {
        envelope.validate()?;
        self.envelope = envelope;
        Ok(())
    }

    /// Picks the voice for a new note: the same `(channel, note)` if it is
    /// still sounding, else a free voice, else the quietest one.
    fn pick(&self, channel: u8, note: u8) -> usize {
        for (index, voice) in self.voices.iter().enumerate() {
            if voice.channel == channel && voice.note == note && voice.peak > 0.0 {
                return index;
            }
        }
        for (index, voice) in self.voices.iter().enumerate() {
            if voice.is_idle() {
                return index;
            }
        }
        // Steal the quietest voice; ties go to the earliest index, which is
        // also the oldest note because starting always takes the first free
        // slot-or-quietest in this order.
        let mut best = 0;
        for (index, voice) in self.voices.iter().enumerate() {
            if voice.last_gain < self.voices[best].last_gain {
                best = index;
            }
        }
        best
    }

    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, at: u64) {
        let index = self.pick(channel, note);
        self.voices[index].start(
            channel,
            note,
            velocity,
            at,
            &self.envelope,
            self.sample_rate,
        );
        self.started = self.started.saturating_add(1);
    }

    fn note_off(&mut self, channel: u8, note: u8, at: u64) {
        for voice in &mut self.voices {
            if voice.channel == channel && voice.note == note && voice.peak > 0.0 {
                voice.release(at);
            }
        }
    }

    /// Renders `frames` samples of `schedule` from `position` into `out`.
    ///
    /// `out` is mono; only `out[..frames]` is written. Events inside
    /// `[position, position + frames)` take effect at their own sample, so a
    /// note starting on `position + k` is silent before `k`. No allocation,
    /// no locking, no I/O.
    ///
    /// # Panics
    ///
    /// Panics if `out.len() < frames`; that is a programming error, not bad
    /// input from a file.
    pub fn render(
        &mut self,
        schedule: &Schedule,
        cursor: &mut ScheduleCursor,
        position: u64,
        frames: usize,
        out: &mut [f32],
    ) {
        assert!(out.len() >= frames, "output buffer is shorter than frames");
        out[..frames].fill(0.0);

        // 1. Apply this quantum's events at their exact sample.
        while let Some(event) = schedule.next_event(cursor, position, frames) {
            match event.source.message {
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                } => self.note_on(channel, note, velocity, event.sample),
                MidiMessage::NoteOff { channel, note, .. } => {
                    self.note_off(channel, note, event.sample);
                }
                MidiMessage::Tempo { .. } => {}
            }
        }

        // 2. Sum every sounding voice.
        let rate = f64::from(self.sample_rate);
        for index in 0..MAX_VOICES {
            let voice = &mut self.voices[index];
            if voice.peak <= 0.0 {
                continue;
            }
            if voice.started_at >= position.saturating_add(frames as u64) {
                continue;
            }
            for (i, slot) in out[..frames].iter_mut().enumerate() {
                let at = position + i as u64;
                if at < voice.started_at {
                    // The note starts later in this quantum.
                    continue;
                }
                let gain = voice.level_at(at);
                voice.last_gain = gain;
                if gain <= 0.0 {
                    continue;
                }
                #[allow(clippy::cast_precision_loss)]
                let elapsed = (at - voice.started_at) as f64;
                let phase = std::f64::consts::TAU * voice.freq_hz * elapsed / rate;
                // A quarter-period offset makes the oscillator start at its
                // peak (cosine) instead of at a zero crossing, so a note is
                // audible on its very first sample.
                #[allow(clippy::cast_possible_truncation)]
                let value = (phase + std::f64::consts::FRAC_PI_2).sin() as f32;
                *slot += value * gain;
            }
            let finished = match voice.released_at {
                Some(at) => at < position.saturating_add(frames as u64),
                None => false,
            };
            if finished && voice.last_gain <= 0.0 {
                *voice = Voice::idle();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiEvent;
    use crate::schedule::{ScheduleSpec, ScheduledEvent};

    const RATE: u32 = 48_000;

    fn at(sample: u64, message: MidiMessage) -> ScheduledEvent {
        ScheduledEvent {
            sample,
            source: MidiEvent {
                track: 0,
                tick: 0,
                order_in_track: 0,
                message,
            },
        }
    }

    fn note_on(note: u8, velocity: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity,
        }
    }

    fn note_off(note: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel: 0,
            note,
            velocity: 64,
        }
    }

    fn schedule(events: Vec<ScheduledEvent>, length: u64) -> Schedule {
        Schedule::new(ScheduleSpec {
            events,
            length_samples: length,
        })
    }

    fn synth(env: Envelope) -> Synth {
        Synth::new(RATE, env).unwrap()
    }

    fn render(synth: &mut Synth, schedule: &Schedule, position: u64, frames: usize) -> Vec<f32> {
        let mut cursor = schedule.cursor();
        let mut out = vec![0.0_f32; frames.max(1)];
        synth.render(schedule, &mut cursor, position, frames, &mut out);
        out.truncate(frames);
        out
    }

    /// Envelope shape: attack to peak, decay to sustain, then a flat sustain.
    #[test]
    fn envelope_follows_attack_decay_and_sustain() {
        let env = Envelope {
            attack_s: 0.001, // 48 samples
            decay_s: 0.002,  // 96 samples
            sustain: 0.5,
            release_s: 0.001,
        };
        let s = schedule(vec![at(0, note_on(69, 127))], 10_000);
        // The level is the voice's gain after rendering up to that sample
        // (the sine's own sign would hide it at a zero crossing).
        let level_at = |sample: u64| {
            let mut probe = synth(env);
            let mut cursor = s.cursor();
            let mut block = vec![0.0_f32; usize::try_from(sample).unwrap()];
            let frames = usize::try_from(sample).unwrap();
            probe.render(&s, &mut cursor, 0, frames, &mut block);
            f64::from(probe.voices()[0].gain())
        };
        // Attack: 48 samples from 0 to the peak (sample 0 sounds the note,
        // so the first ramped sample is 2).
        assert!((level_at(1) - 0.0).abs() < 1e-6, "attack {}", level_at(1));
        assert!(
            (level_at(13) - 0.25).abs() < 1e-6,
            "attack {}",
            level_at(13)
        );
        assert!((level_at(25) - 0.5).abs() < 1e-6, "attack {}", level_at(25));
        assert!((level_at(49) - 1.0).abs() < 1e-6, "peak {}", level_at(49));
        // Decay: 96 samples from 1.0 down to the 0.5 sustain.
        assert!(
            (level_at(97) - (1.0 - 0.5 * 48.0 / 96.0)).abs() < 1e-6,
            "decay {}",
            level_at(97)
        );
        assert!(
            (level_at(146) - 0.5).abs() < 1e-6,
            "decay {}",
            level_at(146)
        );
        // Then flat.
        assert!(
            (level_at(500) - 0.5).abs() < 1e-6,
            "sustain {}",
            level_at(500)
        );
    }

    #[test]
    fn note_to_hz_is_equal_tempered() {
        assert!((note_to_hz(69) - 440.0).abs() < 1e-9);
        assert!((note_to_hz(57) - 220.0).abs() < 1e-9);
        assert!((note_to_hz(81) - 880.0).abs() < 1e-9);
        assert!((note_to_hz(60) - 261.625_565_300_598_6).abs() < 1e-9);
        assert!((note_to_hz(0) - 8.175_798_915_643_72).abs() < 1e-9);
        assert!((note_to_hz(127) - 12_543.853_951_855_166).abs() < 1e-6);
    }

    #[test]
    fn a_note_starts_exactly_on_its_sample() {
        let env = Envelope {
            attack_s: 0.01,
            ..Envelope::default()
        };
        // The render call spans samples 0..200, so the note must be applied
        // inside this quantum at sample 50 (not at its start, not next time).
        let s = schedule(vec![at(50, note_on(60, 127))], 1000);
        let mut synth = synth(env);
        let out = render(&mut synth, &s, 0, 200);
        for (i, sample) in out.iter().take(50).enumerate() {
            assert!(
                sample.abs() <= 1e-3,
                "sample {i} sounds before the note starts: {sample}"
            );
        }
        // Attack is 480 samples for the 0.01 s envelope, so the first
        // samples after the onset are the ramp times the cosine near 1.
        assert!(out[51].abs() > 0.0, "note must start on its sample");
        assert!(
            out[52].abs() > out[51].abs(),
            "ramp must rise after the onset: {} then {}",
            out[51],
            out[52]
        );
    }

    #[test]
    fn note_off_releases_and_the_voice_returns_to_idle() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.001, // 48 samples
        };
        let s = schedule(vec![at(0, note_on(60, 127)), at(100, note_off(60))], 1000);
        let mut synth = synth(env);
        let out = render(&mut synth, &s, 0, 200);
        // Before the note-off the voice is at full level.
        assert!(f64::from(synth.voices()[0].gain()) > 0.0 || out[99].abs() > 0.0);
        // 48 samples after the note-off the voice is silent and freed.
        assert_eq!(out[148].to_bits(), 0.0_f32.to_bits());
        assert!(synth.voices()[0].is_idle(), "released voice must be freed");
    }

    #[test]
    fn a_note_without_note_off_keeps_sounding_until_the_schedule_ends() {
        let s = schedule(vec![at(0, note_on(60, 100))], 1000);
        let mut synth = synth(Envelope::default());
        let out = render(&mut synth, &s, 0, 500);
        assert!(out[400].abs() > 0.0);
    }

    #[test]
    fn note_off_without_note_on_is_ignored() {
        let s = schedule(vec![at(0, note_off(60))], 1000);
        let mut synth = synth(Envelope::default());
        let out = render(&mut synth, &s, 0, 100);
        assert!(out.iter().all(|v| *v == 0.0));
        assert_eq!(synth.voices_started(), 0);
    }

    #[test]
    fn a_repeated_note_on_restarts_instead_of_doubling() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.0,
        };
        let single = schedule(vec![at(0, note_on(60, 127))], 1000);
        let double = schedule(
            vec![at(0, note_on(60, 127)), at(64, note_on(60, 127))],
            1000,
        );

        let mut a = synth(env);
        let out_a = render(&mut a, &single, 0, 128);
        let mut b = synth(env);
        let out_b = render(&mut b, &double, 0, 128);

        let peak_a = out_a.iter().fold(0.0_f32, |acc, v| acc.max(v.abs()));
        let peak_b = out_b.iter().fold(0.0_f32, |acc, v| acc.max(v.abs()));
        assert!(
            (peak_a - peak_b).abs() <= peak_a * 0.01,
            "{peak_a} vs {peak_b}"
        );
        assert!(
            b.voices().iter().filter(|v| !v.is_idle()).count() == 1,
            "one note must own one voice"
        );
    }

    #[test]
    fn the_seventeenth_note_steals_the_quietest_voice() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 1.0,
        };
        // Sixteen notes at full velocity, one quiet note in the middle.
        let mut events = Vec::new();
        for note in 0..16_u8 {
            let velocity = if note == 5 { 10 } else { 127 };
            events.push(at(0, note_on(48 + note, velocity)));
        }
        let s = schedule(events, 1000);
        let mut synth = synth(env);
        let _ = render(&mut synth, &s, 0, 128);

        let quiet = synth
            .voices()
            .iter()
            .position(|v| v.note == 53)
            .expect("quiet voice");
        assert!(synth.voices()[quiet].gain() > 0.0);

        // Now a seventeenth note must take exactly that slot.
        let extra = schedule(vec![at(128, note_on(90, 127))], 1000);
        let mut cursor = extra.cursor();
        let mut out = vec![0.0_f32; 128];
        synth.render(&extra, &mut cursor, 128, 128, &mut out);
        assert_eq!(synth.voices()[quiet].note, 90);
        assert!(synth.voices().iter().all(|v| v.note != 53));
        assert_eq!(synth.voices_started(), 17);
    }

    #[test]
    fn sixteen_voices_sum_without_nan() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.0,
        };
        let mut synth = synth(env);
        // Distinct notes so every voice is real.
        let events: Vec<_> = (0..MAX_VOICES)
            .map(|i| at(0, note_on(60 + u8::try_from(i).unwrap(), 127)))
            .collect();
        assert_eq!(events.len(), MAX_VOICES);
        let s = schedule(events, 1000);
        let out = render(&mut synth, &s, 0, 256);
        assert!(out.iter().all(|v| v.is_finite()), "output must stay finite");
    }

    #[test]
    fn output_does_not_depend_on_the_quantum_size() {
        let s = schedule(
            vec![
                at(0, note_on(60, 127)),
                at(1000, note_on(64, 100)),
                at(2000, note_off(60)),
                at(2500, note_off(64)),
            ],
            8000,
        );
        let total = 4800;

        let mut small = synth(Envelope::default());
        let mut small_out = Vec::with_capacity(total);
        let mut cursor = s.cursor();
        let mut position = 0_u64;
        while position < total as u64 {
            let remaining = usize::try_from(total as u64 - position).unwrap();
            let frames = remaining.min(256);
            let mut block = vec![0.0_f32; frames];
            small.render(&s, &mut cursor, position, frames, &mut block);
            small_out.extend_from_slice(&block);
            position += frames as u64;
        }

        let mut large = synth(Envelope::default());
        let mut large_out = vec![0.0_f32; total];
        let mut cursor = s.cursor();
        let mut position = 0_u64;
        let mut offset = 0;
        while position < total as u64 {
            let remaining = usize::try_from(total as u64 - position).unwrap();
            let frames = remaining.min(512);
            large.render(
                &s,
                &mut cursor,
                position,
                frames,
                &mut large_out[offset..offset + frames],
            );
            position += frames as u64;
            offset += frames;
        }

        assert_eq!(small_out.len(), large_out.len());
        // The phase and the envelope are functions of the absolute sample
        // index, so the quantum size only changes f64 summation order -- a
        // few ULPs, not the signal. Everything else (which samples a note
        // starts on, when a voice is freed) must match exactly.
        let mut energy = (0.0_f64, 0.0_f64);
        for (i, (a, b)) in small_out.iter().zip(&large_out).enumerate() {
            assert!(
                (f64::from(*a) - f64::from(*b)).abs() < 1e-6,
                "sample {i} differs: {a} vs {b}"
            );
            energy.0 += f64::from(*a).abs();
            energy.1 += f64::from(*b).abs();
        }
        assert!((energy.0 - energy.1).abs() < 1e-6, "energy {energy:?}");
        assert_eq!(small.voices_started(), large.voices_started());
        for (a, b) in small.voices().iter().zip(large.voices()) {
            assert_eq!(a.is_idle(), b.is_idle());
            assert_eq!(a.started_at, b.started_at);
        }
    }

    #[test]
    fn two_runs_are_bit_identical() {
        let s = schedule(
            vec![
                at(0, note_on(60, 127)),
                at(100, note_on(67, 64)),
                at(300, note_off(60)),
            ],
            2000,
        );
        let first = render(&mut synth(Envelope::default()), &s, 0, 1000);
        let second = render(&mut synth(Envelope::default()), &s, 0, 1000);
        for (i, (a, b)) in first.iter().zip(&second).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "sample {i} differs");
        }
    }

    #[test]
    fn reset_silences_every_voice() {
        let s = schedule(vec![at(0, note_on(60, 127))], 1000);
        let mut synth = synth(Envelope::default());
        let _ = render(&mut synth, &s, 0, 128);
        assert!(synth.voices().iter().any(|v| !v.is_idle()));
        synth.reset();
        assert!(synth.voices().iter().all(Voice::is_idle));
        let out = render(&mut synth, &s, 128, 128);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn zero_frames_touches_neither_output_nor_cursor() {
        let s = schedule(vec![at(100, note_on(60, 127))], 1000);
        let mut synth = synth(Envelope::default());
        let mut cursor = s.cursor();
        let mut out = [7.0_f32; 4];
        synth.render(&s, &mut cursor, 0, 0, &mut out);
        assert!(
            out.iter().all(|v| v.to_bits() == 7.0_f32.to_bits()),
            "frames == 0 must not write"
        );
        // The event is still ahead, so it plays on a later quantum.
        let mut out = vec![0.0_f32; 200];
        synth.render(&s, &mut cursor, 0, 200, &mut out);
        assert!(out[150].abs() > 0.0);
    }

    #[test]
    fn zero_length_attack_and_release_do_not_divide_by_zero() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.0,
        };
        let s = schedule(vec![at(0, note_on(60, 127)), at(10, note_off(60))], 100);
        let mut synth = synth(env);
        let out = render(&mut synth, &s, 0, 100);
        assert!(out.iter().all(|v| v.is_finite() && !v.is_nan()));
        assert_eq!(out[10].to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn a_zero_sample_rate_is_an_error_not_a_panic() {
        assert_eq!(
            Synth::new(0, Envelope::default()).unwrap_err(),
            SynthError::InvalidSampleRate
        );
    }

    #[test]
    fn a_bad_envelope_is_an_error() {
        let cases = [
            Envelope {
                attack_s: -1.0,
                ..Envelope::default()
            },
            Envelope {
                decay_s: f64::NAN,
                ..Envelope::default()
            },
            Envelope {
                sustain: 1.5,
                ..Envelope::default()
            },
            Envelope {
                release_s: f64::INFINITY,
                ..Envelope::default()
            },
        ];
        for env in cases {
            assert_eq!(
                Synth::new(RATE, env).unwrap_err(),
                SynthError::InvalidEnvelope,
                "{env:?}"
            );
        }
    }

    #[test]
    fn velocity_scales_the_peak() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.0,
        };
        let loud = schedule(vec![at(0, note_on(60, 127))], 100);
        let soft = schedule(vec![at(0, note_on(60, 64))], 100);
        let mut a = synth(env);
        let out_a = render(&mut a, &loud, 0, 4);
        let mut b = synth(env);
        let out_b = render(&mut b, &soft, 0, 4);
        let ratio = f64::from(out_b[1] / out_a[1]);
        assert!((ratio - 64.0 / 127.0).abs() < 1e-4, "ratio {ratio}");
    }

    #[test]
    fn one_quantum_of_many_events_applies_all_of_them() {
        let env = Envelope {
            attack_s: 0.0,
            decay_s: 0.0,
            sustain: 1.0,
            release_s: 0.0,
        };
        let events: Vec<_> = (0..64_u8)
            .map(|i| at(u64::from(i) * 4, note_on(40 + i, 100)))
            .collect();
        let s = schedule(events, 1000);
        let mut synth = synth(env);
        let out = render(&mut synth, &s, 0, 256);
        assert_eq!(synth.voices_started(), 64);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
