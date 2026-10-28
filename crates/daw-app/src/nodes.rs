//! Demo nodes shipped with the shell.

use audio_core::graph::{NodeProcessor, NodeView, ProcessContext};
use audio_core::params::ParamHandle;
use audio_core::wav::sine;

/// Steady sine whose amplitude is driven by a live parameter.
///
/// The node owns a [`ParamHandle`]; the UI moves the same parameter through
/// the graph without touching the audio thread (SPEC-006).
pub struct Tone {
    /// Frequency in Hz.
    pub freq_hz: f64,
    /// Linear amplitude, 0.0..=1.0, smoothed by the parameter spec.
    pub level: ParamHandle,
}

impl Tone {
    /// Builds a sine at `freq_hz` whose amplitude follows `level`.
    #[must_use]
    pub fn new(freq_hz: f64, level: ParamHandle) -> Self {
        Self { freq_hz, level }
    }
}

impl std::fmt::Debug for Tone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tone")
            .field("freq_hz", &self.freq_hz)
            .field("level", &self.level.spec().name)
            .finish()
    }
}

impl NodeProcessor for Tone {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let level = self.level.next(ctx.frames, ctx.sample_rate);
        let block = sine(self.freq_hz, ctx.sample_rate, ctx.position, ctx.frames);
        let dst = output.channel_mut(0);
        for (i, sample) in block.iter().enumerate() {
            dst[i] = sample * level;
        }
    }
}
