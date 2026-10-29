//! MIDI event scheduler: ticks become samples once, then stay cheap (SPEC-007).
//!
//! The parser in [`crate::midi`] produces events in ticks; the audio thread
//! cannot convert ticks (it has no wall clock and no place to allocate).
//! [`ScheduleSpec::from_midi`] does the conversion on the control thread, and
//! [`Schedule`] hands the result to nodes as a forward-only
//! [`ScheduleCursor`]:
//!
//! ```text
//! control thread                        audio thread (node.process)
//! ──────────────                        ──────────────────────────
//! from_midi(&file, sr, tempo, tail)     while let Some(ev) =
//!   -> ScheduleSpec { events, len }       schedule.next_event(&mut cur, pos, frames)
//! Schedule::new(spec)   ──move──►         { /*  ev.sample is absolute */ }
//! ```
//!
//! [`Schedule::next_event`] allocates nothing, locks nothing and cannot
//! panic: it is an index walk over a `Vec` that never changes after `new`.

use std::fmt;

use crate::midi::{MidiEvent, MidiFile, MidiMessage};
use crate::transport::Tempo;

/// Why a MIDI file could not be turned into a sample-accurate schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// The file's PPQN division is zero, so a tick has no length.
    InvalidPpqn,
    /// A `Tempo` event carries `micros_per_quarter == 0`.
    InvalidTempo {
        /// Tick position of the offending event.
        tick: u64,
    },
    /// The sample rate is zero, so seconds cannot become samples.
    InvalidSampleRate,
    /// The file has no notes, so there is nothing to play.
    Empty,
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPpqn => write!(f, "PPQN division is zero"),
            Self::InvalidTempo { tick } => {
                write!(
                    f,
                    "tempo event at tick {tick} has zero microseconds per quarter"
                )
            }
            Self::InvalidSampleRate => write!(f, "sample rate is zero"),
            Self::Empty => write!(f, "MIDI file contains no notes"),
        }
    }
}

impl std::error::Error for ScheduleError {}

/// One parsed event placed at an absolute sample position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Absolute position from the start of the sequence.
    pub sample: u64,
    /// The event as the parser produced it, ticks included.
    pub source: MidiEvent,
}

/// Sample-accurate, sorted event list plus its length.
///
/// Plain data: build it with [`ScheduleSpec::from_midi`] on the control
/// thread and hand it to [`Schedule::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSpec {
    /// Events sorted by `(sample, track, order_in_track)`.
    pub events: Vec<ScheduledEvent>,
    /// Position of the last event plus the release tail, in frames.
    pub length_samples: u64,
}

impl ScheduleSpec {
    /// Converts a parsed MIDI file into sample positions.
    ///
    /// `start_tempo` is the tempo at tick 0 (the transport's tempo); a
    /// [`MidiMessage::Tempo`] event overrides it from its own tick onwards.
    /// `tail_samples` is added to the last event's sample to get
    /// [`Self::length_samples`] (release tail).
    ///
    /// # Errors
    ///
    /// [`ScheduleError::InvalidPpqn`], [`ScheduleError::InvalidSampleRate`],
    /// [`ScheduleError::InvalidTempo`] for bad inputs, and
    /// [`ScheduleError::Empty`] when the file has no `NoteOn`.
    pub fn from_midi(
        file: &MidiFile,
        sample_rate: u32,
        start_tempo: Tempo,
        tail_samples: u64,
    ) -> Result<Self, ScheduleError> {
        if file.ppqn == 0 {
            return Err(ScheduleError::InvalidPpqn);
        }
        if sample_rate == 0 {
            return Err(ScheduleError::InvalidSampleRate);
        }
        if !file
            .events
            .iter()
            .any(|event| matches!(event.message, MidiMessage::NoteOn { .. }))
        {
            return Err(ScheduleError::Empty);
        }
        let ppqn = f64::from(file.ppqn);
        let rate = f64::from(sample_rate);
        let mut seconds = 0.0_f64;
        let mut prev_tick = 0_u64;
        let mut bpm = start_tempo.0;
        let mut events = Vec::with_capacity(file.events.len());
        for event in &file.events {
            // The tempo in force *before* this event governs the gap up to it;
            // a `Tempo` event therefore takes effect from its own tick on.
            seconds += tick_delta_seconds(prev_tick, event.tick, bpm, ppqn);
            prev_tick = event.tick;
            events.push(ScheduledEvent {
                sample: seconds_to_sample(seconds, rate),
                source: *event,
            });
            if let MidiMessage::Tempo { micros_per_quarter } = event.message {
                if micros_per_quarter == 0 {
                    return Err(ScheduleError::InvalidTempo { tick: event.tick });
                }
                bpm = 60_000_000.0 / f64::from(micros_per_quarter);
            }
        }
        events.sort_by_key(|event| {
            (
                event.sample,
                event.source.track,
                event.source.order_in_track,
            )
        });
        let last = events.last().map_or(0, |event| event.sample);
        Ok(Self {
            events,
            length_samples: last.saturating_add(tail_samples),
        })
    }
}

/// Seconds between two ticks at `bpm`.
fn tick_delta_seconds(from_tick: u64, to_tick: u64, bpm: f64, ppqn: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let delta = to_tick.saturating_sub(from_tick) as f64;
    delta / (ppqn * bpm / 60.0)
}

/// `2^64` is exactly representable in `f64` and lies just above `u64::MAX`, so
/// comparing against it catches everything the cast would clamp.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// Rounds accumulated seconds into a sample index, saturating instead of
/// wrapping on absurd inputs.
fn seconds_to_sample(seconds: f64, sample_rate: f64) -> u64 {
    let samples = seconds * sample_rate;
    if samples.is_nan() || samples <= 0.0 {
        return 0;
    }
    if samples >= TWO_POW_64 {
        return u64::MAX;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = samples.round() as u64;
    out
}

/// Forward-only read position inside a [`Schedule`].
///
/// `Default` is the beginning, and [`Schedule::cursor`] is the canonical way
/// to get one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScheduleCursor {
    next: usize,
}

/// An immutable, ready-to-play schedule.
///
/// Building it moves the `Vec` out of [`ScheduleSpec`]; nothing allocates
/// afterwards, and [`Self::next_event`] is realtime-safe.
#[derive(Debug, Clone)]
pub struct Schedule {
    events: Vec<ScheduledEvent>,
    length_samples: u64,
}

impl Schedule {
    /// Takes ownership of `spec`. Control thread only.
    #[must_use]
    pub fn new(spec: ScheduleSpec) -> Self {
        Self {
            events: spec.events,
            length_samples: spec.length_samples,
        }
    }

    /// Every event, sorted by `(sample, track, order_in_track)`.
    #[must_use]
    pub fn events(&self) -> &[ScheduledEvent] {
        &self.events
    }

    /// Number of scheduled events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the schedule has no events at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Length of the sequence plus its release tail, in frames.
    #[must_use]
    pub const fn length_samples(&self) -> u64 {
        self.length_samples
    }

    /// A cursor at the beginning of the schedule.
    #[must_use]
    pub const fn cursor(&self) -> ScheduleCursor {
        ScheduleCursor { next: 0 }
    }

    /// Returns the next event inside `[position, position + frames)`.
    ///
    /// Events before `position` are skipped (they were played earlier or were
    /// jumped over by a seek) and advance the cursor. The range is
    /// half-open, so an event exactly at `position + frames` belongs to the
    /// next quantum. Realtime-safe: index reads only, no allocation, no
    /// locking, no panic.
    pub fn next_event(
        &self,
        cursor: &mut ScheduleCursor,
        position: u64,
        frames: usize,
    ) -> Option<ScheduledEvent> {
        let end = position.saturating_add(frames as u64);
        while let Some(event) = self.events.get(cursor.next) {
            if event.sample >= end {
                // Not yet: the cursor stays on it for a later quantum.
                return None;
            }
            cursor.next += 1;
            if event.sample >= position {
                return Some(*event);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PPQN: u16 = 480;

    fn midi_event(track: u16, tick: u64, order: u32, message: MidiMessage) -> MidiEvent {
        MidiEvent {
            track,
            tick,
            order_in_track: order,
            message,
        }
    }

    fn note_on(_tick: u64) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        }
    }

    fn file(events: Vec<MidiEvent>) -> MidiFile {
        MidiFile {
            format: 0,
            ppqn: PPQN,
            track_count: 1,
            events,
        }
    }

    fn spec(events: Vec<MidiEvent>, tail: u64) -> ScheduleSpec {
        ScheduleSpec::from_midi(&file(events), 48_000, Tempo(120.0), tail).unwrap()
    }

    #[test]
    fn one_quarter_note_is_half_a_second() {
        // 120 bpm, 480 ppqn, 48 kHz: tick 480 = one beat = 0.5 s = 24 000.
        let s = spec(vec![midi_event(0, 480, 0, note_on(480))], 0);
        assert_eq!(s.events[0].sample, 24_000);
        assert_eq!(s.length_samples, 24_000);
    }

    #[test]
    fn tempo_event_halves_the_rate_after_its_tick() {
        // 120 bpm for the first beat, then 60 bpm: the second beat takes twice
        // as long, so tick 960 lands at 24 000 + 48 000 = 72 000.
        let events = vec![
            midi_event(0, 480, 0, note_on(480)),
            midi_event(
                0,
                480,
                1,
                MidiMessage::Tempo {
                    micros_per_quarter: 1_000_000,
                },
            ),
            midi_event(0, 960, 2, note_on(960)),
        ];
        let s = spec(events, 0);
        assert_eq!(s.events[0].sample, 24_000);
        assert_eq!(s.events[1].sample, 24_000, "tempo lands on the beat");
        assert_eq!(s.events[2].sample, 72_000);
    }

    #[test]
    fn a_tempo_event_on_a_note_tick_does_not_move_that_note() {
        // The note lands at 120 bpm (tick 480 = 24 000); the tempo event that
        // shares its tick still uses the old tempo for its own position. An
        // implementation that applies the tempo *before* converting the tick
        // would place both at 48 000.
        let events = vec![
            midi_event(0, 480, 0, note_on(60)),
            midi_event(
                0,
                480,
                1,
                MidiMessage::Tempo {
                    micros_per_quarter: 1_000_000,
                },
            ),
        ];
        let s = spec(events, 0);
        assert_eq!(s.events[0].sample, 24_000);
        assert_eq!(s.events[1].sample, 24_000);
    }

    #[test]
    fn rounding_does_not_accumulate_across_events() {
        // Every tick here is 1/7 of a beat, so each step is 3428.57… samples;
        // rounding per step would drift by several samples over the run.
        let events: Vec<MidiEvent> = (0..=70_u64)
            .map(|i| {
                let order = u32::try_from(i).unwrap();
                midi_event(0, i * 480 / 7, order, note_on(i))
            })
            .collect();
        let s = spec(events, 0);
        // 70 * (480 / 7) = 4800 ticks = 10 beats = 240 000 samples.
        assert_eq!(s.events.last().unwrap().sample, 240_000);
    }

    #[test]
    fn zero_micros_per_quarter_is_rejected() {
        let events = vec![
            midi_event(0, 0, 0, note_on(0)),
            midi_event(
                0,
                0,
                1,
                MidiMessage::Tempo {
                    micros_per_quarter: 0,
                },
            ),
        ];
        let err = ScheduleSpec::from_midi(&file(events), 48_000, Tempo(120.0), 0).unwrap_err();
        assert_eq!(err, ScheduleError::InvalidTempo { tick: 0 });
    }

    #[test]
    fn zero_ppqn_and_zero_sample_rate_are_rejected() {
        let events = vec![midi_event(0, 0, 0, note_on(0))];
        let mut f = file(events.clone());
        f.ppqn = 0;
        assert_eq!(
            ScheduleSpec::from_midi(&f, 48_000, Tempo(120.0), 0).unwrap_err(),
            ScheduleError::InvalidPpqn
        );
        let f = file(events);
        assert_eq!(
            ScheduleSpec::from_midi(&f, 0, Tempo(120.0), 0).unwrap_err(),
            ScheduleError::InvalidSampleRate
        );
    }

    #[test]
    fn tempo_only_file_is_empty() {
        let events = vec![midi_event(
            0,
            0,
            0,
            MidiMessage::Tempo {
                micros_per_quarter: 500_000,
            },
        )];
        assert_eq!(
            ScheduleSpec::from_midi(&file(events), 48_000, Tempo(120.0), 0).unwrap_err(),
            ScheduleError::Empty
        );
    }

    #[test]
    fn tail_is_added_to_the_last_event() {
        let s = spec(vec![midi_event(0, 480, 0, note_on(480))], 1000);
        assert_eq!(s.length_samples, 25_000);
    }

    #[test]
    fn events_are_sorted_by_sample_then_track() {
        // `from_midi` also sorts the list, so a caller that edited events by
        // hand still gets a playable schedule. `sample` comes first, then
        // `track`, then `order_in_track` (the tie-break that keeps the order
        // total and deterministic).
        let events = vec![
            midi_event(1, 480, 0, note_on(60)),
            midi_event(0, 480, 1, note_on(62)),
            midi_event(0, 0, 0, note_on(64)),
        ];
        let s = spec(events, 0);
        let order: Vec<(u64, u16, u32)> = s
            .events
            .iter()
            .map(|e| (e.sample, e.source.track, e.source.order_in_track))
            .collect();
        assert_eq!(order, vec![(24_000, 0, 0), (24_000, 0, 1), (24_000, 1, 0)]);
    }

    #[test]
    fn range_is_half_open() {
        let s = Schedule::new(spec(
            vec![
                midi_event(0, 0, 0, note_on(0)),
                midi_event(0, 240, 1, note_on(1)),
            ],
            0,
        ));
        // 240 ticks at 120 bpm / 480 ppqn = 12 000 samples.
        let mut cur = s.cursor();
        assert_eq!(s.next_event(&mut cur, 0, 100).unwrap().sample, 0);
        assert!(s.next_event(&mut cur, 0, 100).is_none());
        // Exactly at the boundary: still the next quantum.
        assert!(s.next_event(&mut cur, 0, 12_000).is_none());
        assert_eq!(s.next_event(&mut cur, 12_000, 10).unwrap().sample, 12_000);
    }

    #[test]
    fn zero_frames_returns_nothing_and_keeps_the_cursor() {
        let s = Schedule::new(spec(vec![midi_event(0, 0, 0, note_on(0))], 0));
        let mut cur = s.cursor();
        assert!(s.next_event(&mut cur, 0, 0).is_none());
        assert_eq!(cur, s.cursor());
        assert!(s.next_event(&mut cur, 0, 1).is_some());
    }

    #[test]
    fn skipped_events_are_step_over() {
        let s = Schedule::new(spec(
            vec![
                midi_event(0, 0, 0, note_on(0)),
                midi_event(0, 240, 1, note_on(1)),
            ],
            0,
        ));
        let mut cur = s.cursor();
        // Jump past both: the first event of the range is the only survivor.
        assert!(s.next_event(&mut cur, 20_000, 1000).is_none());
        assert_eq!(cur.next, 2);
    }

    #[test]
    fn every_event_is_yielded_exactly_once() {
        // 100 notes, 24 ticks apart = 1200 samples apart; 4000-frame quanta
        // put 3.33 events per quantum, so most boundaries fall between events.
        let events = (0..100)
            .map(|i| midi_event(0, u64::from(i) * 24, i, note_on(60)))
            .collect();
        let s = Schedule::new(spec(events, 0));
        let quantum = 4000_usize;
        let mut cur = s.cursor();
        let mut seen = Vec::new();
        for q in 0..100_u64 {
            let position = q * quantum as u64;
            while let Some(event) = s.next_event(&mut cur, position, quantum) {
                seen.push(event.sample);
            }
        }
        assert_eq!(seen.len(), 100, "each event exactly once");
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "events come out in order"
        );
    }

    #[test]
    fn huge_tick_saturates_instead_of_overflowing() {
        let s = spec(vec![midi_event(0, u64::MAX / 4, 0, note_on(0))], u64::MAX);
        assert_eq!(s.events[0].sample, u64::MAX);
        assert_eq!(s.length_samples, u64::MAX);
    }
}
