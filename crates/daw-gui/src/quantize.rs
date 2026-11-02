//! Quantization of MIDI notes to a musical grid (SPEC-061 §1).
//!
//! Pure, GUI-free math: given a note's absolute sample position, its length,
//! the clip's content length and the tempo map, return the note's new
//! `(start, length)`. All conversion routes through [`audio_core::TempoMap`]
//! (SPEC-054), never a second sample↔tick path.

use audio_core::{TPQN, TempoMap};

/// Which grid line a note snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Division {
    /// Quarter note.
    Quarter,
    /// Eighth note.
    Eighth,
    /// Sixteenth note.
    Sixteenth,
    /// Thirty-second note.
    ThirtySecond,
    /// Quarter-note triplet.
    QuarterTriplet,
    /// Eighth-note triplet.
    EighthTriplet,
    /// Sixteenth-note triplet.
    SixteenthTriplet,
    /// Thirty-second-note triplet.
    ThirtySecondTriplet,
}

impl Division {
    /// All divisions, in display order.
    pub const ALL: [Self; 8] = [
        Self::Quarter,
        Self::Eighth,
        Self::Sixteenth,
        Self::ThirtySecond,
        Self::QuarterTriplet,
        Self::EighthTriplet,
        Self::SixteenthTriplet,
        Self::ThirtySecondTriplet,
    ];

    /// Human label, e.g. `"1/16"` or `"1/8T"`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
            Self::QuarterTriplet => "1/4T",
            Self::EighthTriplet => "1/8T",
            Self::SixteenthTriplet => "1/16T",
            Self::ThirtySecondTriplet => "1/32T",
        }
    }

    /// Beat divisor: how many of this division fit in one beat. A quarter note
    /// is one beat (`1`), an eighth is half a beat (`2`), a sixteenth a quarter
    /// (`4`), a thirty-second an eighth (`8`).
    const fn parts(self) -> u32 {
        match self {
            Self::Quarter | Self::QuarterTriplet => 1,
            Self::Eighth | Self::EighthTriplet => 2,
            Self::Sixteenth | Self::SixteenthTriplet => 4,
            Self::ThirtySecond | Self::ThirtySecondTriplet => 8,
        }
    }

    /// Whether the division is a triplet.
    const fn triplet(self) -> bool {
        matches!(
            self,
            Self::QuarterTriplet
                | Self::EighthTriplet
                | Self::SixteenthTriplet
                | Self::ThirtySecondTriplet
        )
    }
}

/// Which part of a note the grid snaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizeMode {
    /// Snap only the note start; length is unchanged.
    StartOnly,
    /// Snap both the start and the end; length follows.
    StartAndLength,
}

/// Quantization parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantizeParams {
    /// Grid division.
    pub division: Division,
    /// What to snap.
    pub mode: QuantizeMode,
    /// Pull toward the grid, `0..=100`; `0` leaves the note untouched,
    /// `100` snaps fully.
    pub strength: u8,
}

impl Default for QuantizeParams {
    fn default() -> Self {
        Self {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartOnly,
            strength: 100,
        }
    }
}

/// Result of quantizing one note: the new `(start, length)` in samples.
pub type QuantizedNote = (u64, u64);

/// Length of one grid step in ticks, given a division.
///
/// A plain division is `TPQN / beat_divisor` ticks; a triplet is two thirds of
/// that. All divisions are integer multiples of a tick (TPQN is divisible by
/// 3, 4 and 8), so the result is always an exact integer.
#[must_use]
pub fn step_ticks(division: Division) -> u64 {
    let parts = u64::from(division.parts());
    if division.triplet() {
        u64::from(TPQN) * 2 / (3 * parts)
    } else {
        u64::from(TPQN) / parts
    }
}

/// Snap an absolute sample position to the nearest grid line, returning the
/// sample position of that line. Strength `0..=100` pulls the result toward
/// the grid; `100` snaps fully, `0` returns `samples` unchanged.
#[must_use]
pub fn snap_sample(samples: u64, map: &TempoMap, division: Division, strength: u8) -> u64 {
    let strength = u8::min(strength, 100);
    if strength == 0 {
        return samples;
    }
    let step = step_ticks(division);
    let ticks = map.samples_to_ticks(samples);
    let snapped_ticks = (ticks + step / 2) / step * step;
    let snapped = map.ticks_to_samples(snapped_ticks);
    if strength == 100 {
        return snapped;
    }
    // Linear interpolation between the original and the snapped position.
    #[allow(clippy::cast_precision_loss)]
    let t = f64::from(strength) / 100.0;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let blended = (samples as f64 + (snapped as f64 - samples as f64) * t).round() as u64;
    blended
}

/// Quantize one note.
///
/// `start` is the note's absolute position in samples, `length` its duration
/// and `source_length` the clip's content length. The result never leaves
/// `[0, source_length]`, never has a zero length, and only moves the start
/// when [`QuantizeMode::StartOnly`].
#[must_use]
pub fn quantize_note(
    start: u64,
    length: u64,
    source_length: u64,
    map: &TempoMap,
    params: QuantizeParams,
) -> QuantizedNote {
    let end = start.saturating_add(length).min(source_length);
    let new_start = snap_sample(start, map, params.division, params.strength);
    let new_start = new_start.min(source_length.saturating_sub(1));
    let new_length = match params.mode {
        // Length is untouched; only the start moves.
        QuantizeMode::StartOnly => length,
        QuantizeMode::StartAndLength => {
            let new_end = snap_sample(end, map, params.division, params.strength);
            // The snapped end may land before the snapped start for notes
            // shorter than a step; clamp so the length stays at least 1.
            new_end.max(new_start.saturating_add(1)).min(source_length) - new_start
        }
    }
    .max(1);
    (new_start, new_length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(bpm: f64, rate: u32) -> TempoMap {
        TempoMap::single(bpm, rate).unwrap()
    }

    // 120 BPM at 48 kHz: a beat is 24 000 samples, a sixteenth 6000.
    const RATE: u32 = 48_000;

    #[test]
    fn step_ticks_are_integer_for_plain_and_triplet() {
        assert_eq!(step_ticks(Division::Quarter), 960);
        assert_eq!(step_ticks(Division::Eighth), 480);
        assert_eq!(step_ticks(Division::Sixteenth), 240);
        assert_eq!(step_ticks(Division::ThirtySecond), 120);
        assert_eq!(step_ticks(Division::QuarterTriplet), 640);
        assert_eq!(step_ticks(Division::EighthTriplet), 320);
        assert_eq!(step_ticks(Division::SixteenthTriplet), 160);
        assert_eq!(step_ticks(Division::ThirtySecondTriplet), 80);
    }

    #[test]
    fn start_only_snaps_start_and_keeps_length() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartOnly,
            strength: 100,
        };
        // 2500 → nearest 6000 line is 0.
        assert_eq!(quantize_note(2_500, 1_000, 96_000, &map, p), (0, 1_000));
        // 5000 → nearest 6000 line is 6000.
        assert_eq!(quantize_note(5_000, 1_000, 96_000, &map, p), (6_000, 1_000));
    }

    #[test]
    fn start_and_length_snaps_both_ends() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartAndLength,
            strength: 100,
        };
        // start 2500→0, end 8500→6000 (nearest to 6000 is 6000), length 6000.
        assert_eq!(quantize_note(2_500, 6_000, 96_000, &map, p), (0, 6_000));
        // end 8500 nearest line is 6000, giving length 6000 again.
        assert_eq!(quantize_note(2_500, 5_500, 96_000, &map, p), (0, 6_000));
    }

    #[test]
    fn triplet_uses_two_thirds_of_the_plain_value() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::EighthTriplet,
            mode: QuantizeMode::StartOnly,
            strength: 100,
        };
        // Eighth-triplet step = 12000 * 2/3 = 8000 samples.
        // 3000 is nearer 0 than 8000.
        assert_eq!(quantize_note(3_000, 1_000, 96_000, &map, p), (0, 1_000));
        // 7000 is nearer 8000 than 0.
        assert_eq!(quantize_note(7_000, 1_000, 96_000, &map, p), (8_000, 1_000));
    }

    #[test]
    fn strength_zero_leaves_notes_untouched() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartAndLength,
            strength: 0,
        };
        assert_eq!(quantize_note(2_500, 1_000, 96_000, &map, p), (2_500, 1_000));
    }

    #[test]
    fn strength_fifty_moves_halfway() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartOnly,
            strength: 50,
        };
        // 2500 → snapped 0; halfway is 1250.
        assert_eq!(quantize_note(2_500, 1_000, 96_000, &map, p), (1_250, 1_000));
    }

    #[test]
    fn a_note_shorter_than_a_step_never_collapses() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartAndLength,
            strength: 100,
        };
        // A 10-sample note at 3000: start→6000, end 3010→6000; must stay 1.
        let (start, length) = quantize_note(3_000, 10, 96_000, &map, p);
        assert_eq!(length, 1);
        assert!(start > 0);
    }

    #[test]
    fn notes_crossing_the_clip_bounds_are_clamped() {
        let map = map(120.0, RATE);
        let p = QuantizeParams {
            division: Division::Sixteenth,
            mode: QuantizeMode::StartOnly,
            strength: 100,
        };
        // A note whose start snaps past the end is clamped to the last sample.
        let (start, length) = quantize_note(95_999, 1, 96_000, &map, p);
        assert_eq!(start, 95_999);
        assert_eq!(length, 1);
        // A note starting at 0 with start+length whose end runs past the clip.
        let p2 = QuantizeParams {
            mode: QuantizeMode::StartAndLength,
            ..p
        };
        let (start, length) = quantize_note(0, 96_000, 96_000, &map, p2);
        assert_eq!(start + length, 96_000);
    }
}
