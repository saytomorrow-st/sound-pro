//! Pure transport and ruler geometry (SPEC-040).
//!
//! Bar stepping for the ⏪/⏩ buttons and hit zones of the cycle bar, kept
//! free of drawing so they are tested without a display.

/// Width in pixels of the grab zone at each end of the cycle bar.
pub const CYCLE_EDGE_PX: f32 = 6.0;

/// What a press on the cycle bar grabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleHit {
    /// The left edge: dragging changes the start.
    Start,
    /// The right edge: dragging changes the end.
    End,
    /// The body: dragging moves the whole region.
    Body,
    /// Outside the region: dragging draws a new one.
    Empty,
}

/// Which part of the cycle `[start_x, end_x]` the pointer at `x` grabs.
/// Edges win over the body; a region narrower than two edge zones splits
/// in the middle so both edges stay reachable.
#[must_use]
pub fn cycle_hit(x: f32, cycle: Option<(f32, f32)>) -> CycleHit {
    let Some((start, end)) = cycle else {
        return CycleHit::Empty;
    };
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let edge = CYCLE_EDGE_PX.min((end - start) / 2.0).max(1.0);
    if x < start - edge || x > end + edge {
        CycleHit::Empty
    } else if x <= start + edge {
        CycleHit::Start
    } else if x >= end - edge {
        CycleHit::End
    } else {
        CycleHit::Body
    }
}

/// Length of one bar in samples; bad inputs fall back to 120 BPM in 4/4.
/// Routes through the tempo map so multi-point maps report the bar length at
/// the start of the map.
#[must_use]
pub fn bar_samples(map: &audio_core::TempoMap, beats_per_bar: u32) -> u64 {
    let beats = if beats_per_bar == 0 { 4 } else { beats_per_bar };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let len = (map.samples_per_beat_at(0) * f64::from(beats)).round() as u64;
    len.max(1)
}

/// The bar line one step from `at`: forward goes to the next bar start,
/// back to the previous one (from inside a bar, to that bar's own start).
#[must_use]
pub fn step_bar(at: u64, bar: u64, forward: bool) -> u64 {
    let bar = bar.max(1);
    if forward {
        (at / bar + 1) * bar
    } else if at % bar == 0 {
        at.saturating_sub(bar)
    } else {
        at / bar * bar
    }
}

/// A cycle region moved by `delta` samples, clamped so it never starts
/// before zero and keeps its length.
#[must_use]
pub fn move_cycle(start: u64, end: u64, delta: i64) -> (u64, u64) {
    let len = end.saturating_sub(start);
    let start = start.saturating_add_signed(delta.max(-i64::try_from(start).unwrap_or(i64::MAX)));
    (start, start + len)
}

/// A cycle with one edge dragged to `at`; the edges swap if they cross and
/// the region never collapses below `min_len` samples.
#[must_use]
pub fn resize_cycle(start: u64, end: u64, hit: CycleHit, at: u64, min_len: u64) -> (u64, u64) {
    let (a, b) = match hit {
        CycleHit::Start => (at, end),
        CycleHit::End => (start, at),
        CycleHit::Body | CycleHit::Empty => return (start, end),
    };
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    (lo, hi.max(lo + min_len.max(1)))
}

/// Smallest piano roll horizontal zoom (SPEC-040 §5).
pub const PIANO_ZOOM_MIN: f32 = 0.25;
/// Largest piano roll horizontal zoom.
pub const PIANO_ZOOM_MAX: f32 = 8.0;

/// Applies a pinch / Ctrl+wheel factor to the piano roll zoom, clamped.
#[must_use]
pub fn piano_zoom(zoom: f32, factor: f32) -> f32 {
    if !(factor.is_finite() && factor > 0.0) {
        return zoom;
    }
    (zoom * factor).clamp(PIANO_ZOOM_MIN, PIANO_ZOOM_MAX)
}

/// Scroll offset that keeps the time under `anchor` (pixels from the left
/// edge of the view) still after a zoom, when the first `fixed` pixels of the
/// content (the keyboard) do not scale.
#[must_use]
pub fn keyed_zoom_offset(old: f32, new: f32, offset: f32, anchor: f32, fixed: f32) -> f32 {
    if !(old.is_finite() && new.is_finite() && old > 0.0 && new > 0.0) {
        return offset.max(0.0);
    }
    let x = offset + anchor;
    if x <= fixed {
        return offset.max(0.0);
    }
    (fixed + (x - fixed) * new / old - anchor).max(0.0)
}
