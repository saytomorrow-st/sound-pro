//! Live level meters (SPEC-042): peaks the audio thread writes and the
//! window reads, one atomic per channel, allocated once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use audio_core::session::MAX_TRACKS;

/// Meter slots: two per track, then two for the master.
const SLOTS: usize = 2 * MAX_TRACKS + 2;
const MASTER: usize = 2 * MAX_TRACKS;

/// Peak levels since the last read, shared by the audio thread and the
/// window. Cloning gives another handle to the same bank.
#[derive(Debug, Clone)]
pub struct Meters {
    slots: Arc<[AtomicU32; SLOTS]>,
}

impl Default for Meters {
    fn default() -> Self {
        Self::new()
    }
}

/// Largest absolute value of `samples`; `NaN` and infinities are skipped.
fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .map(|sample| sample.abs())
        .filter(|level| level.is_finite())
        .fold(0.0, f32::max)
}

impl Meters {
    /// A bank for [`MAX_TRACKS`] tracks and the master, all at zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Arc::new(std::array::from_fn(|_| AtomicU32::new(0))),
        }
    }

    // Non-negative floats order like their bits, so `fetch_max` on the bits
    // keeps the larger level without a compare loop.
    fn record(&self, slot: usize, samples: &[f32]) {
        let level = peak(samples);
        if level > 0.0
            && let Some(atomic) = self.slots.get(slot)
        {
            atomic.fetch_max(level.to_bits(), Ordering::Relaxed);
        }
    }

    fn take(&self, slot: usize) -> f32 {
        self.slots.get(slot).map_or(0.0, |atomic| {
            f32::from_bits(atomic.swap(0, Ordering::Relaxed))
        })
    }

    /// Records one quantum of track `track`; `right = None` (mono) counts
    /// the left channel for both. Realtime-safe; tracks past
    /// [`MAX_TRACKS`] are ignored.
    pub fn record_track(&self, track: usize, left: &[f32], right: Option<&[f32]>) {
        if track >= MAX_TRACKS {
            return;
        }
        self.record(2 * track, left);
        self.record(2 * track + 1, right.unwrap_or(left));
    }

    /// Records one quantum of the master. Realtime-safe.
    pub fn record_master(&self, left: &[f32], right: &[f32]) {
        self.record(MASTER, left);
        self.record(MASTER + 1, right);
    }

    /// Track `track`'s `(left, right)` peaks since the last read, then
    /// zero; an unknown track reads `(0, 0)`.
    #[must_use]
    pub fn take_track(&self, track: usize) -> (f32, f32) {
        if track >= MAX_TRACKS {
            return (0.0, 0.0);
        }
        (self.take(2 * track), self.take(2 * track + 1))
    }

    /// The master's `(left, right)` peaks since the last read, then zero.
    #[must_use]
    pub fn take_master(&self) -> (f32, f32) {
        (self.take(MASTER), self.take(MASTER + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_largest_absolute_value_until_read() {
        let meters = Meters::new();
        meters.record_track(3, &[0.1, -0.7, 0.2], Some(&[0.3, 0.0]));
        meters.record_track(3, &[0.5], Some(&[-0.9]));
        assert_eq!(meters.take_track(3), (0.7, 0.9));
        assert_eq!(meters.take_track(3), (0.0, 0.0));
        assert_eq!(meters.take_track(2), (0.0, 0.0));
    }

    #[test]
    fn mono_copies_left_and_bad_input_is_ignored() {
        let meters = Meters::new();
        meters.record_track(0, &[f32::NAN, -0.4, f32::INFINITY], None);
        assert_eq!(meters.take_track(0), (0.4, 0.4));
        meters.record_track(MAX_TRACKS, &[1.0], None);
        assert_eq!(meters.take_track(MAX_TRACKS), (0.0, 0.0));
    }

    #[test]
    fn master_and_clones_share_the_bank() {
        let meters = Meters::new();
        let other = meters.clone();
        other.record_master(&[0.25], &[1.5]);
        assert_eq!(meters.take_master(), (0.25, 1.5));
        assert_eq!(other.take_master(), (0.0, 0.0));
    }
}
