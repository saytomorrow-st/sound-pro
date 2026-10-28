//! Transport clock: tempo, musical position, play state.
//!
//! The transport is the master clock for the whole project. Sample position
//! is the ground truth; musical position (bar/beat/tick) is derived from it
//! so that a tempo change never drifts against the audio stream.

/// Default project sample rate used by the offline renderer and tests.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// Pulses per quarter note. 960 divides both 4/4 and triplets cleanly, which
/// is why the de-facto standard sits in this range.
pub const TPQN: u32 = 960;

/// Tempo in beats per minute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tempo(pub f64);

impl Default for Tempo {
    fn default() -> Self {
        Self(120.0)
    }
}

impl Tempo {
    /// Seconds per quarter note.
    #[must_use]
    pub fn seconds_per_beat(self) -> f64 {
        60.0 / self.0
    }

    /// Samples per quarter note at `sample_rate`.
    #[must_use]
    pub fn samples_per_beat(self, sample_rate: u32) -> f64 {
        self.seconds_per_beat() * f64::from(sample_rate)
    }

    /// Samples per tick (PPQN subdivision) at `sample_rate`.
    #[must_use]
    pub fn samples_per_tick(self, sample_rate: u32) -> f64 {
        self.samples_per_beat(sample_rate) / f64::from(TPQN)
    }
}

/// Time signature. Only the denominator affects bar length in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSignature {
    /// Beats per bar, e.g. 4 in 4/4.
    pub numerator: u8,
    /// Note value of one beat, e.g. 4 in 4/4.
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

impl TimeSignature {
    /// Ticks in one bar.
    #[must_use]
    pub fn ticks_per_bar(self) -> u64 {
        let ticks_per_whole_note = u64::from(TPQN) * 4;
        let beat_ticks = ticks_per_whole_note / u64::from(self.denominator);
        beat_ticks * u64::from(self.numerator)
    }
}

/// Musical position, converted from sample position on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarPosition {
    /// Zero-based bar index.
    pub bar: u64,
    /// Zero-based beat inside the bar.
    pub beat: u32,
    /// Zero-based tick inside the beat.
    pub tick: u32,
}

/// Control-side commands. Sent to the audio thread through a ring buffer so
/// the audio thread never blocks waiting for UI input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportCommand {
    /// Start rolling from the current position.
    Play,
    /// Stop and keep the position.
    Stop,
    /// Stop and rewind to zero.
    Rewind,
    /// Move to an absolute sample position.
    SeekSamples(u64),
    /// Change tempo (control-side only, applied at the next quantum).
    SetTempo(Tempo),
    /// Toggle metronome.
    SetClick(bool),
}

/// Transport state. Lives on the audio thread.
#[derive(Debug, Clone)]
pub struct Transport {
    sample_rate: u32,
    tempo: Tempo,
    signature: TimeSignature,
    position: u64,
    playing: bool,
    click: bool,
}

impl Transport {
    /// Creates a stopped transport at sample position zero.
    ///
    /// # Panics
    ///
    /// Panics if `sample_rate` is zero.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        assert!(sample_rate > 0, "sample_rate must be non-zero");
        Self {
            sample_rate,
            tempo: Tempo::default(),
            signature: TimeSignature::default(),
            position: 0,
            playing: false,
            click: false,
        }
    }

    /// Sample rate this transport was created with.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Current tempo.
    #[must_use]
    pub const fn tempo(&self) -> Tempo {
        self.tempo
    }

    /// Current time signature.
    #[must_use]
    pub const fn signature(&self) -> TimeSignature {
        self.signature
    }

    /// Absolute sample position.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// Whether the transport is rolling.
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether the metronome is enabled.
    #[must_use]
    pub const fn click_enabled(&self) -> bool {
        self.click
    }

    /// Applies a control command. Allocation-free, realtime-safe.
    pub fn apply(&mut self, command: TransportCommand) {
        match command {
            TransportCommand::Play => self.playing = true,
            TransportCommand::Stop => self.playing = false,
            TransportCommand::Rewind => {
                self.playing = false;
                self.position = 0;
            }
            TransportCommand::SeekSamples(pos) => self.position = pos,
            TransportCommand::SetTempo(t) if t.0 > 0.0 => self.tempo = t,
            TransportCommand::SetTempo(_) => {}
            TransportCommand::SetClick(on) => self.click = on,
        }
    }

    /// Advances the transport by one render quantum.
    ///
    /// Returns the starting sample position of the quantum so the caller can
    /// render sources sample-accurately instead of at block granularity.
    pub fn advance(&mut self, frames: usize) -> u64 {
        let start = self.position;
        if self.playing {
            self.position = self.position.saturating_add(frames as u64);
        }
        start
    }

    /// Converts a sample position to a musical position.
    #[must_use]
    pub fn bar_position_at(&self, samples: u64) -> BarPosition {
        let ticks_per_bar = self.signature.ticks_per_bar();
        let samples_per_tick = self.tempo.samples_per_tick(self.sample_rate);
        // Ticks are derived from samples, never accumulated, so dragging the
        // playhead back and forth is exact.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let total_ticks = ((samples as f64) / samples_per_tick).round().max(0.0) as u64;
        let bar = total_ticks / ticks_per_bar;
        let in_bar = total_ticks % ticks_per_bar;
        let ticks_per_beat = u64::from(TPQN) * 4 / u64::from(self.signature.denominator);
        #[allow(clippy::cast_possible_truncation)]
        BarPosition {
            bar,
            beat: (in_bar / ticks_per_beat) as u32,
            tick: (in_bar % ticks_per_beat) as u32,
        }
    }

    /// Current musical position.
    #[must_use]
    pub fn bar_position(&self) -> BarPosition {
        self.bar_position_at(self.position)
    }

    /// Metronome tick for the given quantum if a beat boundary falls inside it.
    ///
    /// Returns `Some(true)` for a downbeat, `Some(false)` for other beats.
    /// Beat `n` (absolute, zero-based) lands at sample `ceil(n * samples_per_beat)`.
    pub fn click_for_quantum(&self, start: u64, frames: usize) -> Option<bool> {
        if !self.click || !self.playing || frames == 0 {
            return None;
        }
        let spp = self.tempo.samples_per_beat(self.sample_rate);
        let end = start.saturating_add(frames as u64);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let first_beat = (start as f64 / spp).ceil() as u64;
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let beat_sample = (first_beat as f64 * spp).ceil() as u64;
        if beat_sample < end {
            let in_bar = first_beat % u64::from(self.signature.numerator);
            Some(in_bar == 0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_note_is_half_a_second_at_120_bpm() {
        let t = Tempo::default();
        assert!((t.seconds_per_beat() - 0.5).abs() < 1e-12);
        assert!((t.samples_per_beat(48_000) - 24_000.0).abs() < 1e-9);
        assert!((t.samples_per_tick(48_000) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn four_four_bar_is_3840_ticks() {
        assert_eq!(TimeSignature::default().ticks_per_bar(), 3840);
        let six_eight = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        assert_eq!(six_eight.ticks_per_bar(), 2880);
    }

    #[test]
    fn bar_position_matches_samples() {
        let t = Transport::new(48_000);
        // One bar at 120 BPM 4/4 = 4 beats * 24000 samples = 96000.
        assert_eq!(
            t.bar_position_at(96_000),
            BarPosition {
                bar: 1,
                beat: 0,
                tick: 0
            }
        );
        assert_eq!(
            t.bar_position_at(96_000 + 24_000),
            BarPosition {
                bar: 1,
                beat: 1,
                tick: 0
            }
        );
    }

    #[test]
    fn playing_advances_position_and_stopped_does_not() {
        let mut t = Transport::new(48_000);
        let start = t.advance(64);
        assert_eq!(start, 0);
        assert_eq!(t.position(), 0, "stopped transport must not move");
        t.apply(TransportCommand::Play);
        t.advance(64);
        assert_eq!(t.position(), 64);
        t.apply(TransportCommand::Rewind);
        assert_eq!(t.position(), 0);
        assert!(!t.is_playing());
    }

    #[test]
    fn seek_and_invalid_tempo_are_safe() {
        let mut t = Transport::new(48_000);
        t.apply(TransportCommand::SeekSamples(1234));
        assert_eq!(t.position(), 1234);
        t.apply(TransportCommand::SetTempo(Tempo(0.0)));
        assert!((t.tempo().0 - 120.0).abs() < f64::EPSILON);
    }
}
