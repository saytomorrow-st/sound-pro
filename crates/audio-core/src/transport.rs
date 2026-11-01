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

/// Most tempo points a tempo map may hold (SPEC-054).
pub const MAX_TEMPO_POINTS: usize = 256;

/// One tempo change on the timeline: from `position_ticks` the tempo is
/// `bpm` until the next point (stepwise, no ramps — SPEC-054).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoPoint {
    /// Absolute position in ticks (TPQN subdivision) where this tempo begins.
    pub position_ticks: u64,
    /// Tempo in beats per minute, valid `30.0..=300.0`.
    pub bpm: f64,
}

/// A stepwise tempo map: an ordered list of [`TempoPoint`] whose first entry
/// is always at tick 0, plus a precomputed cumulative sample position for
/// each segment so the audio thread can convert ticks ↔ samples with a binary
/// search and no allocation (SPEC-054).
///
/// The map is built on the control thread and never mutated afterwards; it is
/// `Clone`-free on the audio path because it is passed by reference into
/// [`Transport`]. A single point reproduces the old single-`tempo_bpm`
/// behaviour bit-for-bit.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    /// Ordered tempo points; `points[0].position_ticks == 0`.
    points: Vec<TempoPoint>,
    /// Cumulative sample position of each point at a fixed `sample_rate`.
    /// `segment_samples[i]` is where `points[i]` lands in samples; segment `i`
    /// runs from `points[i]` to `points[i + 1]` at `points[i].bpm`.
    segment_samples: Vec<u64>,
    /// Samples per tick of each segment (constant inside a segment).
    samples_per_tick: Vec<f64>,
    /// Samples per beat of each segment (`rate * 60 / bpm`). Kept separately
    /// from `samples_per_tick` so beat placement stays bit-identical to the
    /// pre-map metronome, which computed `ceil(k · rate · 60 / tempo)`.
    samples_per_beat: Vec<f64>,
    /// Beat position of each point (`position_ticks / TPQN`; may be fractional
    /// if a point sits between beats).
    segment_beats: Vec<f64>,
    /// Sample rate the cumulative positions were computed for.
    sample_rate: u32,
}

impl TempoMap {
    /// Builds a map from an ordered list of points, precomputing cumulative
    /// sample positions for `sample_rate`. The first point must sit at tick 0.
    ///
    /// This is the control-side constructor: it allocates the segment arrays,
    /// so it must not run on the audio thread. Use [`Transport`] to read a
    /// pre-built map inside `process`.
    ///
    /// # Errors
    ///
    /// Returns [`TempoMapError`] if the list is empty, the first point is not
    /// at tick 0, a point is out of order, a BPM is out of `30.0..=300.0`, or
    /// there are more than [`MAX_TEMPO_POINTS`] points.
    pub fn new(points: Vec<TempoPoint>, sample_rate: u32) -> Result<Self, TempoMapError> {
        if sample_rate == 0 {
            return Err(TempoMapError::InvalidSampleRate);
        }
        if points.is_empty() {
            return Err(TempoMapError::Empty);
        }
        if points.len() > MAX_TEMPO_POINTS {
            return Err(TempoMapError::TooManyPoints {
                max: MAX_TEMPO_POINTS,
            });
        }
        let mut prev_tick = 0_u64;
        for (index, point) in points.iter().enumerate() {
            if index == 0 {
                if point.position_ticks != 0 {
                    return Err(TempoMapError::FirstPointNotAtZero);
                }
            } else if point.position_ticks <= prev_tick {
                return Err(TempoMapError::Unordered { index });
            }
            if !point.bpm.is_finite() || !(30.0..=300.0).contains(&point.bpm) {
                return Err(TempoMapError::InvalidBpm { index });
            }
            prev_tick = point.position_ticks;
        }

        // Precompute cumulative sample positions and per-segment samples/tick.
        let mut segment_samples = Vec::with_capacity(points.len());
        let mut samples_per_tick = Vec::with_capacity(points.len());
        let mut samples_per_beat = Vec::with_capacity(points.len());
        let mut segment_beats = Vec::with_capacity(points.len());
        segment_samples.push(0_u64);
        for index in 0..points.len() {
            let bpm = points[index].bpm;
            let spb = Tempo(bpm).samples_per_beat(sample_rate);
            let spt = spb / f64::from(TPQN);
            samples_per_tick.push(spt);
            samples_per_beat.push(spb);
            #[allow(clippy::cast_precision_loss)]
            segment_beats.push(points[index].position_ticks as f64 / f64::from(TPQN));
            if index + 1 < points.len() {
                let tick_span = points[index + 1].position_ticks - points[index].position_ticks;
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let span_samples = (tick_span as f64 * spt).round() as u64;
                let next = segment_samples[index].saturating_add(span_samples);
                segment_samples.push(next);
            }
        }

        Ok(Self {
            points,
            segment_samples,
            samples_per_tick,
            samples_per_beat,
            segment_beats,
            sample_rate,
        })
    }

    /// Builds a single-point map at `bpm`, the old single-tempo behaviour.
    ///
    /// # Errors
    ///
    /// Returns [`TempoMapError`] if `bpm` is out of range or `sample_rate` is
    /// zero.
    pub fn single(bpm: f64, sample_rate: u32) -> Result<Self, TempoMapError> {
        Self::new(
            vec![TempoPoint {
                position_ticks: 0,
                bpm,
            }],
            sample_rate,
        )
    }

    /// Builds a single-point map, falling back to 120 BPM and a sample rate
    /// of 1 for invalid inputs, matching the old scalar helpers' behaviour.
    /// Used by UI display helpers that previously accepted a raw `tempo_bpm`.
    ///
    /// # Panics
    ///
    /// Never: `bpm` is clamped into `30.0..=300.0` and `sample_rate` is at
    /// least 1, both of which [`Self::single`] always accepts.
    #[must_use]
    pub fn single_or_default(bpm: f64, sample_rate: u32) -> Self {
        let bpm = if bpm.is_finite() && (30.0..=300.0).contains(&bpm) {
            bpm
        } else {
            120.0
        };
        let rate = sample_rate.max(1);
        Self::single(bpm, rate).expect("clamped bpm and rate are always valid")
    }

    /// The ordered tempo points.
    #[must_use]
    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Number of tempo points.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the map has no points (always `false` for a valid map).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Sample rate the map was built for.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// BPM of the first point (the project's headline tempo).
    #[must_use]
    pub fn initial_bpm(&self) -> f64 {
        self.points.first().map_or(120.0, |p| p.bpm)
    }

    /// Converts an absolute sample position to an absolute tick position.
    ///
    /// Binary search over the precomputed cumulative sample positions; no
    /// allocation, realtime-safe.
    #[must_use]
    pub fn samples_to_ticks(&self, samples: u64) -> u64 {
        let segment = self.segment_at(samples);
        let start_sample = self.segment_samples[segment];
        let start_tick = self.points[segment].position_ticks;
        let spt = self.samples_per_tick[segment];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let delta_ticks = ((samples - start_sample) as f64 / spt).round() as u64;
        start_tick.saturating_add(delta_ticks)
    }

    /// Converts an absolute tick position to an absolute sample position.
    ///
    /// Binary search over the precomputed cumulative sample positions; no
    /// allocation, realtime-safe.
    #[must_use]
    pub fn ticks_to_samples(&self, ticks: u64) -> u64 {
        let segment = self.tick_segment(ticks);
        let start_sample = self.segment_samples[segment];
        let start_tick = self.points[segment].position_ticks;
        let spt = self.samples_per_tick[segment];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let delta_samples = ((ticks - start_tick) as f64 * spt).round() as u64;
        start_sample.saturating_add(delta_samples)
    }

    /// Converts an absolute beat index to an absolute sample position,
    /// rounding up so a fractional beat length lands on the next frame. The
    /// metronome uses this to keep click placement bit-identical to the
    /// pre-map `ceil(k · rate · 60 / tempo)` behaviour (SPEC-026); a
    /// single-point map reproduces that formula exactly.
    ///
    /// Binary search over the precomputed beat positions; no allocation,
    /// realtime-safe.
    #[must_use]
    pub fn beat_to_samples_ceil(&self, beat: u64) -> u64 {
        #[allow(clippy::cast_precision_loss)]
        let beat_f = beat as f64;
        let segment = self
            .segment_beats
            .partition_point(|&b| b <= beat_f)
            .saturating_sub(1);
        let start_sample = self.segment_samples[segment];
        let start_beat = self.segment_beats[segment];
        let spb = self.samples_per_beat[segment];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let delta_samples = ((beat_f - start_beat) * spb).ceil() as u64;
        start_sample.saturating_add(delta_samples)
    }

    /// Samples per tick at the given sample position (the tempo in force there).
    #[must_use]
    pub fn samples_per_tick_at(&self, samples: u64) -> f64 {
        self.samples_per_tick[self.segment_at(samples)]
    }

    /// Samples per beat (quarter note) at the given sample position. This is
    /// the tempo in force there, so a single-point map returns the old
    /// `rate * 60 / bpm` exactly.
    #[must_use]
    pub fn samples_per_beat_at(&self, samples: u64) -> f64 {
        self.samples_per_beat[self.segment_at(samples)]
    }

    /// Converts an absolute sample position to an absolute beat index
    /// (quarter notes), flooring the fractional part. Realtime-safe, no
    /// allocation. A single-point map reproduces the old
    /// `floor(samples * tempo / (rate * 60))` formula exactly.
    #[must_use]
    pub fn samples_to_beats(&self, samples: u64) -> u64 {
        let segment = self.segment_at(samples);
        let start_sample = self.segment_samples[segment];
        let start_beat = self.segment_beats[segment];
        let spb = self.samples_per_beat[segment];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let delta_beats = (((samples - start_sample) as f64) / spb).floor() as u64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let start_beat_u64 = start_beat.round() as u64;
        start_beat_u64.saturating_add(delta_beats)
    }

    /// Converts an absolute beat index to an absolute sample position,
    /// rounding to the nearest frame. Realtime-safe, no allocation. A
    /// single-point map reproduces `round(beat * rate * 60 / bpm)` exactly.
    #[must_use]
    pub fn beat_to_samples(&self, beat: u64) -> u64 {
        #[allow(clippy::cast_precision_loss)]
        let beat_f = beat as f64;
        let segment = self
            .segment_beats
            .partition_point(|&b| b <= beat_f)
            .saturating_sub(1);
        let start_sample = self.segment_samples[segment];
        let start_beat = self.segment_beats[segment];
        let spb = self.samples_per_beat[segment];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let delta_samples = ((beat_f - start_beat) * spb).round() as u64;
        start_sample.saturating_add(delta_samples)
    }

    /// BPM in force at the given sample position.
    #[must_use]
    pub fn bpm_at(&self, samples: u64) -> f64 {
        self.points[self.segment_at(samples)].bpm
    }

    /// Index of the segment (tempo point) governing `samples`. Realtime-safe
    /// binary search, no allocation.
    fn segment_at(&self, samples: u64) -> usize {
        // Last segment whose start sample is <= samples.
        let idx = self
            .segment_samples
            .partition_point(|&start| start <= samples);
        idx.saturating_sub(1)
    }

    /// Index of the segment (tempo point) governing `ticks`.
    fn tick_segment(&self, ticks: u64) -> usize {
        self.points
            .partition_point(|p| p.position_ticks <= ticks)
            .saturating_sub(1)
    }
}

/// Why a [`TempoMap`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TempoMapError {
    /// `sample_rate` is zero.
    #[error("sample_rate must be non-zero")]
    InvalidSampleRate,
    /// The point list is empty.
    #[error("tempo map must have at least one point")]
    Empty,
    /// The first point is not at tick 0.
    #[error("first tempo point must be at tick 0")]
    FirstPointNotAtZero,
    /// A point is not strictly after its predecessor.
    #[error("tempo point {index} is not strictly after its predecessor")]
    Unordered {
        /// Index of the offending point.
        index: usize,
    },
    /// A BPM is not finite or outside `30.0..=300.0`.
    #[error("tempo point {index} has a BPM outside 30.0..=300.0")]
    InvalidBpm {
        /// Index of the offending point.
        index: usize,
    },
    /// The list is longer than [`MAX_TEMPO_POINTS`].
    #[error("tempo map has more than {max} points")]
    TooManyPoints {
        /// Accepted maximum.
        max: usize,
    },
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
    /// Optional tempo map; when set it governs tick↔sample conversion instead
    /// of the single `tempo` (SPEC-054).
    tempo_map: Option<TempoMap>,
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
            tempo_map: None,
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

    /// The active tempo map, if one was set.
    #[must_use]
    pub const fn tempo_map(&self) -> Option<&TempoMap> {
        self.tempo_map.as_ref()
    }

    /// Sets the tempo map for tick↔sample conversion (control-side only).
    ///
    /// Called before the transport is handed to the audio thread (or between
    /// quanta on the control side). The map's arrays are already sized, so
    /// moving it in allocates nothing on the audio path.
    pub fn set_tempo_map(&mut self, map: TempoMap) {
        self.tempo_map = Some(map);
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
        let total_ticks = if let Some(map) = &self.tempo_map {
            map.samples_to_ticks(samples)
        } else {
            let samples_per_tick = self.tempo.samples_per_tick(self.sample_rate);
            // Ticks are derived from samples, never accumulated, so dragging the
            // playhead back and forth is exact.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let total_ticks = ((samples as f64) / samples_per_tick).round().max(0.0) as u64;
            total_ticks
        };
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
    /// With a tempo map, beat `n` lands at `map.ticks_to_samples(n * TPQN)`.
    pub fn click_for_quantum(&self, start: u64, frames: usize) -> Option<bool> {
        if !self.click || !self.playing || frames == 0 {
            return None;
        }
        let end = start.saturating_add(frames as u64);
        let first_beat = if let Some(map) = &self.tempo_map {
            let beat = map.samples_to_ticks(start) / u64::from(TPQN);
            // First beat boundary at or after `start`.
            if map.ticks_to_samples(beat * u64::from(TPQN)) < start {
                beat + 1
            } else {
                beat
            }
        } else {
            let spp = self.tempo.samples_per_beat(self.sample_rate);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let first = (start as f64 / spp).ceil() as u64;
            first
        };
        let beat_sample = if let Some(map) = &self.tempo_map {
            map.ticks_to_samples(first_beat * u64::from(TPQN))
        } else {
            let spp = self.tempo.samples_per_beat(self.sample_rate);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let sample = (first_beat as f64 * spp).ceil() as u64;
            sample
        };
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

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn tempo_map_single_point_matches_old_behaviour() {
        // A one-point map at 120 BPM must reproduce the single-tempo math.
        let map = TempoMap::single(120.0, 48_000).unwrap();
        let t = Tempo(120.0);
        for samples in [0_u64, 1, 24_000, 24_001, 96_000, 960_000] {
            assert_eq!(map.samples_to_ticks(samples), {
                let spt = t.samples_per_tick(48_000);
                ((samples as f64) / spt).round() as u64
            });
        }
        // tick 3840 == one bar == 96 000 samples.
        assert_eq!(map.ticks_to_samples(3840), 96_000);
    }

    #[test]
    fn tempo_map_conversion_round_trips() {
        let map = TempoMap::new(
            vec![
                TempoPoint {
                    position_ticks: 0,
                    bpm: 120.0,
                },
                TempoPoint {
                    position_ticks: 3840,
                    bpm: 60.0,
                },
                TempoPoint {
                    position_ticks: 7680,
                    bpm: 180.0,
                },
            ],
            48_000,
        )
        .unwrap();
        for ticks in [0_u64, 1, 1920, 3840, 3841, 7680, 9600, 11520] {
            let samples = map.ticks_to_samples(ticks);
            assert_eq!(map.samples_to_ticks(samples), ticks, "tick {ticks}");
        }
    }

    #[test]
    fn tempo_map_is_stepwise() {
        // 120 BPM for bar 0 (3840 ticks), 60 BPM for bar 1.
        let map = TempoMap::new(
            vec![
                TempoPoint {
                    position_ticks: 0,
                    bpm: 120.0,
                },
                TempoPoint {
                    position_ticks: 3840,
                    bpm: 60.0,
                },
            ],
            48_000,
        )
        .unwrap();
        // Bar 0: 96 000 samples. Bar 1 at 60 BPM: 192 000 samples long.
        assert_eq!(map.ticks_to_samples(3840), 96_000);
        assert_eq!(map.ticks_to_samples(7680), 96_000 + 192_000);
    }

    #[test]
    fn tempo_map_rejects_invalid_inputs() {
        assert!(TempoMap::new(vec![], 48_000).is_err());
        assert!(
            TempoMap::new(
                vec![TempoPoint {
                    position_ticks: 1,
                    bpm: 120.0
                }],
                48_000
            )
            .is_err()
        );
        assert!(
            TempoMap::new(
                vec![
                    TempoPoint {
                        position_ticks: 0,
                        bpm: 120.0
                    },
                    TempoPoint {
                        position_ticks: 0,
                        bpm: 90.0
                    },
                ],
                48_000
            )
            .is_err()
        );
        assert!(
            TempoMap::new(
                vec![TempoPoint {
                    position_ticks: 0,
                    bpm: 20.0
                }],
                48_000
            )
            .is_err()
        );
        assert!(
            TempoMap::new(
                vec![TempoPoint {
                    position_ticks: 0,
                    bpm: 301.0
                }],
                48_000
            )
            .is_err()
        );
        assert!(TempoMap::single(120.0, 0).is_err());
    }
}
