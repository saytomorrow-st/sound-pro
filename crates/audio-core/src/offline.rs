//! Offline renderer: runs the engine as fast as the CPU allows.
//!
//! This is the backbone of AI-assisted development in this project. Agents and
//! CI have no sound card, so every DSP, routing, and timing test goes through
//! [`OfflineRenderer`]. Because it drives the exact same [`Renderer`] as the
//! realtime backend, a passing offline test means the stage code is
//! exercised too.

use crate::buffer::{AudioBuffer, Sample};
use crate::render::Renderer;
use crate::{AudioBackend, RenderError};

/// Errors produced by offline bounces.
#[derive(Debug, thiserror::Error)]
pub enum OfflineError {
    /// Renderer failed while producing audio.
    #[error("render error: {0}")]
    Render(#[from] RenderError),
    /// Total frames would exceed addressable memory or overflow `usize`.
    #[error("bounce of {0} frames is too large")]
    TooLarge(u64),
}

/// Bounces a [`Renderer`] into memory.
pub struct OfflineRenderer {
    inner: Renderer,
    block_frames: usize,
    channels: usize,
}

impl OfflineRenderer {
    /// Wraps a renderer, rendering in `block_frames` quanta.
    ///
    /// `block_frames` is independent of the hardware block size, so tests can
    /// prove block-size independence: the same bounce must produce the same
    /// samples at 16, 64, or 1024 frames per quantum (for processors whose
    /// output depends only on absolute position).
    ///
    /// # Panics
    ///
    /// Panics if `block_frames` is zero or exceeds the renderer's maximum.
    #[must_use]
    pub fn new(inner: Renderer, block_frames: usize) -> Self {
        assert!(block_frames > 0, "block_frames must be non-zero");
        let channels = inner.channels();
        let max_quantum = inner.max_quantum();
        assert!(
            block_frames <= max_quantum,
            "block_frames {block_frames} exceeds renderer max_quantum {max_quantum}"
        );
        Self {
            inner,
            block_frames,
            channels,
        }
    }

    /// Renders `total_frames` and returns a planar buffer.
    ///
    /// # Errors
    ///
    /// [`OfflineError::TooLarge`] if the bounce does not fit in a `Vec`, or any
    /// renderer error.
    pub fn bounce(&mut self, total_frames: u64) -> Result<AudioBuffer, OfflineError> {
        let total =
            usize::try_from(total_frames).map_err(|_| OfflineError::TooLarge(total_frames))?;
        let mut out = AudioBuffer::try_new(self.channels, total.max(1))
            .map_err(|_| OfflineError::TooLarge(total_frames))?;
        let mut block = AudioBuffer::new(self.channels, self.block_frames);
        let mut done = 0usize;
        while done < total {
            let n = (total - done).min(self.block_frames);
            self.inner.render(n, &mut block)?;
            for ch in 0..self.channels {
                out.channel_mut(ch)[done..done + n].copy_from_slice(&block.channel(ch)[..n]);
            }
            done += n;
        }
        Ok(out)
    }

    /// Renders a single mono channel into a `Vec`; the common case for golden
    /// tests and fixture generation.
    ///
    /// # Errors
    ///
    /// Same as [`OfflineRenderer::bounce`].
    pub fn bounce_mono(&mut self, total_frames: u64) -> Result<Vec<Sample>, OfflineError> {
        let buf = self.bounce(total_frames)?;
        Ok(buf.channel(0).to_vec())
    }

    /// Access to the wrapped renderer (transport, graph, stats).
    pub const fn renderer(&mut self) -> &mut Renderer {
        &mut self.inner
    }
}

impl AudioBackend for OfflineRenderer {
    fn process(&mut self, frames: usize, output: &mut AudioBuffer) -> Result<(), RenderError> {
        self.inner.process(frames, output)
    }
}
