//! Sampler engine: voice pool, stealing, groups, and rendering (RFC-001 §3, §5).
//!
//! [`SamplerEngine`] owns a fixed pool of [`SamplerVoice`]s, a [`RegionTable`]
//! for key→region lookup, and the resolved regions plus their `i16` sample
//! data. It turns a [`Schedule`] into stereo output with the same
//! block-independent, allocation-free contract as [`crate::synth::Synth`].
//!
//! The same engine drives the offline renderer (`render(schedule, ...)`) and,
//! in a later PR, the realtime `SamplerNode`. Nothing here allocates, locks,
//! blocks, touches I/O, or uses `unsafe` after construction.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::midi::MidiMessage;
use crate::sampler::region::{RegionTable, SampleRegion};
use crate::sampler::voice::{
    Interpolation, NO_REGION, SamplerVoice, read_hermite_i16, read_hermite_stereo, read_linear_i16,
    read_linear_stereo,
};
use crate::schedule::{Schedule, ScheduleCursor};
use crate::session::pan_law;

/// Upper bound on the voice pool size (RFC-001 §3.1).
pub const MAX_SAMPLER_VOICES: usize = 128;
/// Default voice pool size.
pub const DEFAULT_VOICES: usize = 64;

/// Why the sampler engine could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerError {
    /// `sample_rate` was zero.
    InvalidSampleRate,
    /// The voice count was zero or above [`MAX_SAMPLER_VOICES`].
    InvalidVoices,
}

impl std::fmt::Display for SamplerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSampleRate => f.write_str("sample rate must be positive"),
            Self::InvalidVoices => f.write_str("voice count must be in 1..=128"),
        }
    }
}

impl std::error::Error for SamplerError {}

/// The sampler engine: fixed voice pool plus immutable bank data.
///
/// All buffers (the voice pool) are allocated in [`SamplerEngine::new`]; the
/// render path performs no allocation, locking, I/O, or `unsafe`. Sample data
/// is supplied as already-decoded `Arc<[i16]>` slices (RFC-001 §3.3); file
/// loading, decoding and bank management are out of scope for this module.
pub struct SamplerEngine {
    voices: Vec<SamplerVoice>,
    /// Fade-out tails of stolen voices (RFC-001 §3.1): a stolen voice keeps
    /// ringing its 5 ms tail here while the new note takes its former slot.
    /// Sized the same as the main pool and preallocated at construction, so
    /// the render path stays allocation-free.
    tails: Vec<SamplerVoice>,
    table: RegionTable,
    regions: Vec<SampleRegion>,
    samples: Vec<Arc<[i16]>>,
    sample_rate: u32,
    interpolation: Interpolation,
    started: AtomicU32,
    stolen: AtomicU32,
    dropped_starts: AtomicU32,
}

impl SamplerEngine {
    /// Builds a silent engine with `num_voices` voices.
    ///
    /// `regions` must already be resolved (via [`crate::sampler::region::resolve`]);
    /// `samples` are the decoded `i16` PCM buffers they reference by
    /// `sample_index`. Control thread only: this allocates the voice pool.
    ///
    /// # Errors
    ///
    /// [`SamplerError::InvalidSampleRate`] for a zero rate,
    /// [`SamplerError::InvalidVoices`] for `num_voices` outside `1..=128`.
    pub fn new(
        sample_rate: u32,
        regions: Vec<SampleRegion>,
        samples: Vec<Arc<[i16]>>,
        num_voices: usize,
    ) -> Result<Self, SamplerError> {
        Self::new_with(
            sample_rate,
            regions,
            samples,
            num_voices,
            Interpolation::default(),
        )
    }

    /// Builds an engine like [`Self::new`], but with an explicit
    /// [`Interpolation`] mode (SPEC-068 step A).
    ///
    /// `interpolation` is fixed for the lifetime of the engine unless changed
    /// via [`Self::set_interpolation`]; both are control-thread only. The
    /// default ([`Interpolation::Linear`]) reproduces the pre-SPEC-068 output
    /// bit-for-bit.
    ///
    /// # Errors
    ///
    /// [`SamplerError::InvalidSampleRate`] for a zero rate,
    /// [`SamplerError::InvalidVoices`] for `num_voices` outside `1..=128`.
    pub fn new_with(
        sample_rate: u32,
        regions: Vec<SampleRegion>,
        samples: Vec<Arc<[i16]>>,
        num_voices: usize,
        interpolation: Interpolation,
    ) -> Result<Self, SamplerError> {
        if sample_rate == 0 {
            return Err(SamplerError::InvalidSampleRate);
        }
        if num_voices == 0 || num_voices > MAX_SAMPLER_VOICES {
            return Err(SamplerError::InvalidVoices);
        }
        let table = RegionTable::build(&regions, samples.len());
        Ok(Self {
            voices: vec![SamplerVoice::idle(); num_voices],
            tails: vec![SamplerVoice::idle(); num_voices],
            table,
            regions,
            samples,
            sample_rate,
            interpolation,
            started: AtomicU32::new(0),
            stolen: AtomicU32::new(0),
            dropped_starts: AtomicU32::new(0),
        })
    }

    /// The interpolation mode used by the render path.
    #[must_use]
    pub const fn interpolation(&self) -> Interpolation {
        self.interpolation
    }

    /// Sets the interpolation mode (control thread only; never from the audio
    /// callback). Changing the mode does not affect voices already sounding
    /// until the next rendered frame.
    pub fn set_interpolation(&mut self, interpolation: Interpolation) {
        self.interpolation = interpolation;
    }

    /// Sample rate the engine renders at.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of voices in the pool.
    #[must_use]
    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    /// Voices started since construction (stealing included).
    #[must_use]
    pub fn voices_started(&self) -> u32 {
        self.started.load(Ordering::Relaxed)
    }

    /// Voices stolen since construction.
    #[must_use]
    pub fn stolen(&self) -> u32 {
        self.stolen.load(Ordering::Relaxed)
    }

    /// Note-ons dropped because the pool was exhausted and no voice could be
    /// stolen (RFC-001 §3.1: never block).
    #[must_use]
    pub fn dropped_starts(&self) -> u32 {
        self.dropped_starts.load(Ordering::Relaxed)
    }

    /// Silences every voice (and fade tail) without touching the statistics.
    ///
    /// Call after a backward seek so notes started before the new position do
    /// not ring on.
    pub fn reset(&mut self) {
        self.voices
            .iter_mut()
            .for_each(|v| *v = SamplerVoice::idle());
        self.tails
            .iter_mut()
            .for_each(|v| *v = SamplerVoice::idle());
    }

    /// Resets the diagnostic counters (`started`, `stolen`, `dropped_starts`).
    ///
    /// Separate from [`Self::reset`] so a seek can silence voices without
    /// zeroing the lifetime statistics, or vice versa.
    pub fn reset_voice_counters(&mut self) {
        self.started.store(0, Ordering::Relaxed);
        self.stolen.store(0, Ordering::Relaxed);
        self.dropped_starts.store(0, Ordering::Relaxed);
    }

    /// Picks the slot for a new note per RFC-001 §3.1.
    ///
    /// Order: a free voice; else the quietest release-stage voice; else the
    /// oldest held voice (ties broken by lowest velocity). Returns `None` when
    /// the pool is fully held and nothing can be stolen — the caller counts a
    /// dropped start rather than blocking.
    fn pick_voice(&mut self) -> Option<usize> {
        for (i, v) in self.voices.iter().enumerate() {
            if v.is_idle() {
                return Some(i);
            }
        }
        // Steal the quietest voice in release stage.
        let mut best: Option<usize> = None;
        for (i, v) in self.voices.iter().enumerate() {
            if v.released_at.is_some() && !v.fading {
                let quieter = best.is_none_or(|b| v.last_gain < self.voices[b].last_gain);
                if quieter {
                    best = Some(i);
                }
            }
        }
        if let Some(i) = best {
            return Some(i);
        }
        // Steal the oldest held voice; ties go to lowest velocity.
        let mut best: Option<usize> = None;
        for (i, v) in self.voices.iter().enumerate() {
            if v.released_at.is_none() && !v.fading {
                let take = best.is_none_or(|b| {
                    let vb = &self.voices[b];
                    v.started_at < vb.started_at
                        || (v.started_at == vb.started_at && v.velocity < vb.velocity)
                });
                if take {
                    best = Some(i);
                }
            }
        }
        best
    }

    /// Finds a free slot in the fade-tail pool for a stolen voice's tail.
    ///
    /// Falls back to the quietest existing tail (nearly done) when all tail
    /// slots are busy, so a burst of steals cannot allocate and never blocks.
    fn pick_tail(&self) -> usize {
        for (i, v) in self.tails.iter().enumerate() {
            if v.is_idle() {
                return i;
            }
        }
        let mut best = 0;
        for (i, v) in self.tails.iter().enumerate() {
            if v.last_gain < self.tails[best].last_gain {
                best = i;
            }
        }
        best
    }

    /// Handles a note-on at absolute sample `at`.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::too_many_lines
    )]
    fn note_on(&mut self, note: u8, velocity: u8, at: u64) {
        if velocity == 0 {
            return;
        }
        let Some(region_index) = self.table.find(&self.regions, note, velocity) else {
            self.dropped_starts.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let region = &self.regions[region_index];
        let sample = &self.samples[region.sample_index];
        let frames = if region.channels == 1 {
            sample.len()
        } else {
            sample.len() / 2
        };
        if frames == 0 || region.offset as usize >= frames {
            self.dropped_starts.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let end_frame = if region.end == 0 {
            frames as f64
        } else {
            (region.end as usize).min(frames) as f64
        };
        let ratio = playback_ratio(note, region.root, region.tune_cents, self.sample_rate);

        let vel_gain = f32::from(velocity) / 127.0;
        // SFZ v1 velocity curve: `(1 - t) + t · (vel/127)²`, `t = vel_track`
        // (RFC-001 §2). At `t = 1` this is the squared curve; at `t = 0` it is
        // a flat 1.0.
        let v2 = vel_gain * vel_gain;
        let curve = 1.0 - region.vel_track + region.vel_track * v2;
        let (pan_l, pan_r) = pan_law(region.pan);
        let peak = region.gain * curve;
        let pos = f64::from(region.offset);
        let (loop_start, loop_end, loop_enabled) = match region.loop_mode {
            crate::sampler::sfz::LoopMode::LoopContinuous
            | crate::sampler::sfz::LoopMode::LoopSustain => (
                f64::from(region.loop_start),
                if region.loop_end == 0 {
                    end_frame
                } else {
                    (region.loop_end as usize).min(frames) as f64
                },
                true,
            ),
            _ => (0.0, 0.0, false),
        };
        let envelope = region.envelope;
        let off_by = region.off_by;
        let off_mode = region.off_mode;

        let Some(index) = self.pick_voice() else {
            self.dropped_starts.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let victim = self.voices[index];
        if !victim.is_idle() {
            // RFC-001 §3.1: a stolen voice fades out over 5 ms rather than
            // being cut. Move it to the fade-tail pool so its tail keeps
            // ringing while the new note takes its former slot.
            self.stolen.fetch_add(1, Ordering::Relaxed);
            let tail = self.pick_tail();
            self.tails[tail] = victim;
            self.tails[tail].fast_fade(at);
        }
        self.voices[index].start(
            region_index as u16,
            note,
            velocity,
            at,
            pos,
            ratio,
            peak,
            pan_l,
            pan_r,
            loop_enabled,
            loop_start,
            loop_end,
            end_frame,
            envelope,
        );
        self.started.fetch_add(1, Ordering::Relaxed);

        // off_by: cut every voice in the target group (RFC-001 §3.1), except
        // the new voice itself — a region with `group == off_by` (e.g. a
        // closed hi-hat) must not cut its own just-started note.
        if let Some(off_by) = off_by {
            let fast = off_mode == crate::sampler::sfz::OffMode::Fast;
            for (i, v) in self.voices.iter_mut().enumerate() {
                if i == index || v.region == NO_REGION {
                    continue;
                }
                let same = self.regions[v.region as usize].group == Some(off_by);
                if !same {
                    continue;
                }
                if fast {
                    v.fast_fade(at);
                } else {
                    v.release(at);
                }
            }
            for v in &mut self.tails {
                if v.region == NO_REGION {
                    continue;
                }
                let same = self.regions[v.region as usize].group == Some(off_by);
                if !same {
                    continue;
                }
                if fast {
                    v.fast_fade(at);
                } else {
                    v.release(at);
                }
            }
        }
    }

    /// Handles a note-off at absolute sample `at`.
    fn note_off(&mut self, note: u8, at: u64) {
        for v in &mut self.voices {
            if v.region != NO_REGION && v.note == note {
                v.release(at);
            }
        }
    }

    /// Renders `frames` stereo samples of `schedule` from `position` into
    /// `out_l`/`out_r`.
    ///
    /// Only `[..frames]` is written to each buffer; `frames` is clamped to the
    /// shorter buffer length, so a caller that passes a buffer shorter than
    /// `frames` gets a shorter render rather than a panic. Events inside
    /// `[position, position + frames)` take effect at their own sample, so a
    /// note starting on `position + k` is silent before `k`. No allocation, no
    /// locking, no I/O, no panic.
    pub fn render(
        &mut self,
        schedule: &Schedule,
        cursor: &mut ScheduleCursor,
        position: u64,
        frames: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let frames = frames.min(out_l.len()).min(out_r.len());
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);

        // 1. Apply this quantum's events at their exact sample.
        while let Some(event) = schedule.next_event(cursor, position, frames) {
            match event.source.message {
                MidiMessage::NoteOn { note, velocity, .. } => {
                    self.note_on(note, velocity, event.sample);
                }
                MidiMessage::NoteOff { note, .. } => self.note_off(note, event.sample),
                MidiMessage::Tempo { .. } => {}
            }
        }

        // 2. Sum every sounding voice and fade tail.
        let end = position.saturating_add(frames as u64);
        for i in 0..self.voices.len() {
            if self.voices[i].region == NO_REGION {
                continue;
            }
            let finished = render_voice(
                &mut self.voices[i],
                &self.regions,
                &self.samples,
                self.interpolation,
                position,
                end,
                frames,
                out_l,
                out_r,
            );
            if finished {
                self.voices[i] = SamplerVoice::idle();
            }
        }
        for i in 0..self.tails.len() {
            if self.tails[i].region == NO_REGION {
                continue;
            }
            let finished = render_voice(
                &mut self.tails[i],
                &self.regions,
                &self.samples,
                self.interpolation,
                position,
                end,
                frames,
                out_l,
                out_r,
            );
            if finished {
                self.tails[i] = SamplerVoice::idle();
            }
        }
    }
}

/// Renders one voice over `[position, end)` into the stereo buffers.
///
/// Returns `true` when the voice is exhausted and can be returned to the idle
/// pool: its playhead ran past the sample end, or its fade (release or fast
/// fade) reached zero.
#[allow(clippy::too_many_arguments)]
fn render_voice(
    voice: &mut SamplerVoice,
    regions: &[SampleRegion],
    samples: &[Arc<[i16]>],
    interpolation: Interpolation,
    position: u64,
    end: u64,
    frames: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
) -> bool {
    if voice.started_at >= end {
        return false;
    }
    let region = &regions[voice.region as usize];
    let sample = &samples[region.sample_index];
    let stereo = region.channels == 2;
    let mono_frames = sample.len();
    let frames_count = if stereo { mono_frames / 2 } else { mono_frames };

    let mut exhausted = false;
    for (k, (sl, sr)) in out_l[..frames]
        .iter_mut()
        .zip(out_r[..frames].iter_mut())
        .enumerate()
    {
        let at = position + k as u64;
        if at < voice.started_at {
            continue;
        }
        let gain = voice.gain_at(at);
        voice.last_gain = gain;
        if gain > 0.0 {
            let (sample_l, sample_r) = match interpolation {
                Interpolation::Linear => {
                    if stereo {
                        read_linear_stereo(sample, voice.pos, frames_count)
                    } else {
                        let m = read_linear_i16(sample, voice.pos, mono_frames);
                        (m, m)
                    }
                }
                Interpolation::Hermite4 => {
                    if stereo {
                        read_hermite_stereo(sample, voice.pos, frames_count)
                    } else {
                        let m = read_hermite_i16(sample, voice.pos, mono_frames);
                        (m, m)
                    }
                }
            };
            *sl += sample_l * gain * voice.pan_l;
            *sr += sample_r * gain * voice.pan_r;
        }
        // Advance the playhead even when the level is zero, so a looping or
        // decaying voice does not stall at a silent sample.
        if !voice.advance() {
            exhausted = true;
            break;
        }
    }

    // A voice is finished when its playhead ran out, or its level reached zero
    // while it was released or fading (a held sustain that decays to zero but
    // is not released keeps its slot).
    exhausted || (voice.last_gain <= 0.0 && (voice.released_at.is_some() || voice.fading))
}

/// Playback speed that turns a recording of `root` (with `tune_cents` offset)
/// into `note` at `sample_rate`, equal temperament (RFC-001 §3.2):
///
/// ```text
/// ratio = 2^((note − root)/12 + tune/1200)
/// ```
///
/// The `file_rate / out_rate` term from the RFC is omitted: v1 does not
/// resample, so source and output rates are equal (resampling is a later PR).
#[must_use]
pub fn playback_ratio(note: u8, root: u8, tune_cents: i32, _sample_rate: u32) -> f64 {
    let semis = f64::from(note) - f64::from(root) + f64::from(tune_cents) / 100.0;
    2.0_f64.powf(semis / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_ratio_octave_doubles() {
        assert!((playback_ratio(81, 69, 0, 48_000) - 2.0).abs() < 1e-12);
        assert!((playback_ratio(57, 69, 0, 48_000) - 0.5).abs() < 1e-12);
        assert!((playback_ratio(69, 69, 1200, 48_000) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn zero_rate_and_bad_voice_count_are_errors() {
        assert!(matches!(
            SamplerEngine::new(0, vec![], vec![], DEFAULT_VOICES),
            Err(SamplerError::InvalidSampleRate)
        ));
        assert!(matches!(
            SamplerEngine::new(48_000, vec![], vec![], 0),
            Err(SamplerError::InvalidVoices)
        ));
        assert!(matches!(
            SamplerEngine::new(48_000, vec![], vec![], MAX_SAMPLER_VOICES + 1),
            Err(SamplerError::InvalidVoices)
        ));
    }
}
