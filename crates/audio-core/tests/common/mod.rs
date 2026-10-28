//! Shared fixtures for integration and golden tests.

#![allow(dead_code)]

use audio_core::buffer::AudioBuffer;
use audio_core::graph::{Graph, NodeProcessor, ProcessContext};
use audio_core::wav::sine;

pub const SR: u32 = 48_000;

/// Pure sine generator. Output depends only on absolute sample position, which
/// makes it the reference for block-size independence tests.
pub struct SineNode {
    freq_hz: f64,
    channels: usize,
}

impl SineNode {
    #[must_use]
    pub fn new(freq_hz: f64, channels: usize) -> Self {
        Self { freq_hz, channels }
    }
}

impl NodeProcessor for SineNode {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut AudioBuffer) {
        let block = sine(self.freq_hz, ctx.sample_rate, ctx.position, ctx.frames);
        for ch in 0..self.channels.min(output.channels()) {
            output.channel_mut(ch)[..ctx.frames].copy_from_slice(&block);
        }
    }
}

/// Plays a pre-loaded planar clip, delayed by `delay` frames.
pub struct ClipNode {
    data: Vec<Vec<f32>>,
    delay: u64,
}

impl ClipNode {
    #[must_use]
    pub fn new(data: Vec<Vec<f32>>, delay: u64) -> Self {
        Self { data, delay }
    }
}

impl NodeProcessor for ClipNode {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut AudioBuffer) {
        for ch in 0..output.channels() {
            let dst = output.channel_mut(ch);
            dst[..ctx.frames].fill(0.0);
            let src = &self.data[ch];
            for (i, d) in dst[..ctx.frames].iter_mut().enumerate() {
                let abs = ctx.position + i as u64;
                let Some(idx) = abs.checked_sub(self.delay) else {
                    continue;
                };
                if let Some(&s) = src.get(idx as usize) {
                    *d = s;
                }
            }
        }
    }
}

/// Builds a graph with room for `nodes` nodes and a 1024-frame quantum.
#[must_use]
pub fn graph(channels: usize, nodes: usize) -> Graph {
    Graph::with_capacity(SR, channels, 1024, nodes)
}
