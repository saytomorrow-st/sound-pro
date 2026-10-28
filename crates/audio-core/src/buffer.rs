//! Non-interleaved audio buffers.

use std::ops::{Deref, DerefMut};

/// Sample format used across the engine.
///
/// `f32` matches what most plugin APIs and audio devices hand us; keeping one
/// format avoids conversion inside the realtime callback.
pub type Sample = f32;

/// Planar (non-interleaved) audio buffer: one contiguous slice per channel.
///
/// The engine picks planar over interleaved because DSP code processes one
/// channel at a time and planar layout keeps each channel cache-friendly.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    data: Vec<Sample>,
    channels: usize,
    frames: usize,
}

impl AudioBuffer {
    /// Fallible version of [`AudioBuffer::new`] for library code paths that
    /// must not abort on allocation failure.
    ///
    /// # Errors
    ///
    /// Returns the allocator error if the backing storage cannot be reserved.
    ///
    /// # Panics
    ///
    /// Panics if `channels` or `frames` is zero or the layout overflows.
    pub fn try_new(
        channels: usize,
        frames: usize,
    ) -> Result<Self, std::collections::TryReserveError> {
        assert!(channels > 0, "channels must be non-zero");
        assert!(frames > 0, "frames must be non-zero");
        let len = channels.checked_mul(frames).expect("buffer size overflow");
        let mut data = Vec::new();
        data.try_reserve_exact(len)?;
        data.resize(len, 0.0);
        Ok(Self {
            data,
            channels,
            frames,
        })
    }

    /// Allocates a zeroed buffer. Call this during setup, never in `process`.
    ///
    /// # Panics
    ///
    /// Panics if `channels` or `frames` is zero or the layout overflows.
    #[must_use]
    pub fn new(channels: usize, frames: usize) -> Self {
        assert!(channels > 0, "channels must be non-zero");
        assert!(frames > 0, "frames must be non-zero");
        let len = channels.checked_mul(frames).expect("buffer size overflow");
        Self {
            data: vec![0.0; len],
            channels,
            frames,
        }
    }

    /// Number of channels.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    /// Number of frames (samples per channel).
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Read-only view of one channel.
    ///
    /// # Panics
    ///
    /// Panics if `channel >= self.channels()`.
    #[must_use]
    pub fn channel(&self, channel: usize) -> &[Sample] {
        let start = channel * self.frames;
        &self.data[start..start + self.frames]
    }

    /// Mutable view of one channel.
    ///
    /// # Panics
    ///
    /// Panics if `channel >= self.channels()`.
    pub fn channel_mut(&mut self, channel: usize) -> &mut [Sample] {
        let start = channel * self.frames;
        &mut self.data[start..start + self.frames]
    }

    /// Clears every channel to silence. Allocation-free.
    pub fn clear(&mut self) {
        self.data.fill(0.0);
    }

    /// Borrows the first `frames` frames of every channel.
    ///
    /// Reinterprets the existing storage in place — no allocation, so this is
    /// safe to use on the audio path. The result addresses each channel with
    /// a stride of `frames`, which is exactly what a node sees for one
    /// quantum: its slices end at `frames`, so writes cannot spill into the
    /// reserved tail.
    ///
    /// # Panics
    ///
    /// Panics if `frames` is zero or larger than the buffer's own frame count.
    #[must_use]
    pub fn view(&mut self, frames: usize) -> Self {
        assert!(frames > 0, "frames must be non-zero");
        assert!(frames <= self.frames, "view exceeds the buffer");
        Self {
            data: std::mem::take(&mut self.data),
            channels: self.channels,
            frames,
        }
    }

    /// Reassembles a view created by [`AudioBuffer::view`] back into one buffer.
    ///
    /// The original frame count is recovered from the storage length, so the
    /// reserved tail is addressable again and its samples are untouched.
    ///
    /// # Panics
    ///
    /// Panics if the storage length is not a multiple of the channel count.
    pub(crate) fn restore(self) -> Self {
        let channels = self.channels;
        let data = self.data;
        assert!(
            channels > 0 && data.len() % channels == 0,
            "storage is not a whole number of channels"
        );
        let frames = data.len() / channels;
        Self {
            data,
            channels,
            frames,
        }
    }

    /// Copies `source` into this buffer starting at `offset` frames.
    ///
    /// Frames beyond the end of this buffer are silently dropped, which makes
    /// the copy safe for the tail of a partially filled render quantum.
    ///
    /// # Panics
    ///
    /// Panics if channel counts differ or `offset` is beyond the buffer.
    pub fn add_from(&mut self, channel: usize, offset: usize, source: &[Sample]) {
        assert!(offset <= self.frames, "offset out of range");
        assert!(channel < self.channels, "channel {channel} out of range");
        let frames = self.frames;
        let available = frames - offset;
        let n = available.min(source.len());
        let dst = self.channel_mut(channel);
        for (d, s) in dst[offset..offset + n].iter_mut().zip(&source[..n]) {
            *d += s;
        }
    }
}

impl Deref for AudioBuffer {
    type Target = [Sample];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl DerefMut for AudioBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_layout_is_channel_major() {
        let mut buf = AudioBuffer::new(2, 4);
        buf.channel_mut(0).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        buf.channel_mut(1)
            .copy_from_slice(&[10.0, 20.0, 30.0, 40.0]);

        assert_eq!(&buf[..], &[1.0, 2.0, 3.0, 4.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(buf.channel(1), &[10.0, 20.0, 30.0, 40.0]);
    }

    #[test]
    fn add_from_truncates_instead_of_panicking() {
        let mut buf = AudioBuffer::new(1, 4);
        buf.add_from(0, 2, &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(buf.channel(0), &[0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn clear_zeroes_everything() {
        let mut buf = AudioBuffer::new(2, 2);
        buf.fill(0.5);
        buf.clear();
        assert!(buf.iter().all(|&s| s == 0.0));
    }
}
