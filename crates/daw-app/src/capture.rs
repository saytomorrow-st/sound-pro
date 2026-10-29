//! Microphone capture, split between the input callback and the window
//! (SPEC-020).
//!
//! [`capture`] builds a pair: [`CaptureEngine`] for the input callback and
//! [`CaptureControl`] for the window. Mono frames travel through an `rtrb`
//! ring allocated up front. Nothing here touches a device, so it is tested
//! offline; [`crate::realtime::start_input`] hands the engine to CPAL.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

/// A finished recording, in mono at the device rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// Mono samples.
    pub samples: Vec<f32>,
    /// Rate the samples were captured at.
    pub sample_rate: u32,
    /// Frames lost because the ring was full.
    pub dropped: u64,
}

/// The input callback's side: pushes frames into the ring.
pub struct CaptureEngine {
    ring: Producer<f32>,
    dropped: Arc<AtomicU64>,
}

impl std::fmt::Debug for CaptureEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureEngine").finish_non_exhaustive()
    }
}

/// The window's side: collects frames from the ring.
pub struct CaptureControl {
    ring: Consumer<f32>,
    dropped: Arc<AtomicU64>,
    samples: Vec<f32>,
    sample_rate: u32,
}

impl std::fmt::Debug for CaptureControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureControl")
            .field("frames", &self.samples.len())
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

/// Builds a capture pair whose ring holds `capacity` mono frames.
#[must_use]
pub fn capture(capacity: usize, sample_rate: u32) -> (CaptureControl, CaptureEngine) {
    let (producer, consumer) = RingBuffer::new(capacity.max(1));
    let dropped = Arc::new(AtomicU64::new(0));
    (
        CaptureControl {
            ring: consumer,
            dropped: Arc::clone(&dropped),
            samples: Vec::new(),
            sample_rate,
        },
        CaptureEngine {
            ring: producer,
            dropped,
        },
    )
}

impl CaptureEngine {
    /// Mixes interleaved `input` of `channels` channels to mono and queues it.
    ///
    /// Realtime-safe: no allocation, locking, I/O or panics. Frames that do
    /// not fit are counted as dropped.
    pub fn push(&mut self, input: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / channels as f32;
        let mut lost = 0u64;
        for frame in input.chunks_exact(channels) {
            let mono = if channels == 1 {
                frame[0]
            } else {
                frame.iter().sum::<f32>() * scale
            };
            if self.ring.push(mono).is_err() {
                lost += 1;
            }
        }
        if lost > 0 {
            self.dropped.fetch_add(lost, Ordering::Relaxed);
        }
    }
}

impl CaptureControl {
    /// Moves everything queued so far into the recording.
    pub fn drain(&mut self) {
        while let Ok(sample) = self.ring.pop() {
            self.samples.push(sample);
        }
    }

    /// Frames collected by [`Self::drain`] so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64
    }

    /// Frames collected by [`Self::drain`] so far, for drawing the take
    /// while it records (SPEC-035).
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Rate the frames are captured at.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames lost so far because the ring was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Drains the rest and returns the recording.
    #[must_use]
    pub fn finish(mut self) -> Recording {
        self.drain();
        Recording {
            dropped: self.dropped(),
            samples: self.samples,
            sample_rate: self.sample_rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_input_passes_through() {
        let (control, mut engine) = capture(16, 48_000);
        engine.push(&[0.1, 0.2, 0.3], 1);
        let rec = control.finish();
        assert_eq!(rec.samples, vec![0.1, 0.2, 0.3]);
        assert_eq!(rec.sample_rate, 48_000);
        assert_eq!(rec.dropped, 0);
    }

    #[test]
    fn stereo_input_is_averaged() {
        let (control, mut engine) = capture(16, 44_100);
        engine.push(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2);
        assert_eq!(control.finish().samples, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn zero_channels_does_nothing() {
        let (control, mut engine) = capture(16, 48_000);
        engine.push(&[1.0, 2.0], 0);
        assert_eq!(control.finish().samples.len(), 0);
    }

    #[test]
    fn a_full_ring_drops_and_counts() {
        let (control, mut engine) = capture(4, 48_000);
        engine.push(&[1.0; 10], 1);
        assert_eq!(control.dropped(), 6);
        let rec = control.finish();
        assert_eq!(rec.samples.len(), 4);
        assert_eq!(rec.dropped, 6);
    }

    #[test]
    fn drain_frees_room_and_keeps_order() {
        let (mut control, mut engine) = capture(4, 48_000);
        engine.push(&[1.0, 2.0, 3.0], 1);
        control.drain();
        assert_eq!(control.frames(), 3);
        engine.push(&[4.0, 5.0, 6.0], 1);
        let rec = control.finish();
        assert_eq!(rec.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(rec.dropped, 0);
    }
}
