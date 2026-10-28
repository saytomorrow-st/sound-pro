//! The renderer: the single code path shared by every backend.
//!
//! `Renderer::render` is what the realtime callback calls and what the
//! offline test harness calls. Keeping one implementation is the whole point:
//! anything exercised by a test is exactly what runs on stage, in CI, and in
//! a user's DAW.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::buffer::{AudioBuffer, Sample};
use crate::commands::CommandConsumer;
use crate::graph::{Graph, ProcessContext};
use crate::transport::Transport;
use crate::{AudioBackend, RenderError};

/// Statistics reported back to the control side. Plain `u64`s so the audio
/// thread can update them without locking; the control side reads them with
/// `Acquire`/`Relaxed`, which is fine for counters.
#[derive(Debug, Default)]
pub struct RenderStats {
    frames: AtomicU64,
    quanta: AtomicU64,
}

impl RenderStats {
    /// Total frames rendered since the renderer was created.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Total render quanta processed.
    #[must_use]
    pub fn quanta(&self) -> u64 {
        self.quanta.load(Ordering::Relaxed)
    }

    fn record(&self, frames: usize) {
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.quanta.fetch_add(1, Ordering::Relaxed);
    }
}

/// Owns the graph, transport, and the scratch buffer for one quantum.
pub struct Renderer {
    graph: Graph,
    transport: Transport,
    stats: RenderStats,
    /// Destination for the current quantum; pre-allocated for `max_quantum`.
    scratch: AudioBuffer,
    max_quantum: usize,
    /// Optional SPSC command channel drained at the start of every quantum.
    commands: Option<CommandConsumer>,
}

impl Renderer {
    /// Builds a renderer around `graph`.
    ///
    /// # Panics
    ///
    /// Panics if `max_quantum` is zero.
    #[must_use]
    pub fn new(graph: Graph, transport: Transport, max_quantum: usize) -> Self {
        assert!(max_quantum > 0, "max_quantum must be non-zero");
        let channels = graph.channels();
        Self {
            graph,
            transport,
            stats: RenderStats::default(),
            scratch: AudioBuffer::new(channels, max_quantum),
            max_quantum,
            commands: None,
        }
    }

    /// Attaches a command consumer that [`Renderer::render`] drains at the start
    /// of every quantum (SPEC-003).
    ///
    /// The matching [`crate::CommandQueue`] (producer) goes to the control
    /// thread. This is setup-time wiring; call it before the audio thread runs.
    pub fn set_command_consumer(&mut self, consumer: CommandConsumer) {
        self.commands = Some(consumer);
    }

    /// Applies every command already waiting on the queue, in FIFO order.
    ///
    /// Draining *all* pending commands at the top of a quantum (never halfway)
    /// is the documented strategy from SPEC-003 §4: a command is either fully
    /// applied in the quantum it is seen in, or — if it arrives after the drain
    /// — applied at the start of the next quantum. Never split.
    fn drain_commands(&mut self) {
        if let Some(commands) = self.commands.as_mut() {
            while let Some(command) = commands.pop_next() {
                self.transport.apply(command);
            }
        }
    }

    /// Read-only transport access for the control side.
    #[must_use]
    pub const fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Mutable transport access. Control side only; do not call from the
    /// audio thread (use queued [`crate::TransportCommand`]s there).
    pub fn transport_mut(&mut self) -> &mut Transport {
        &mut self.transport
    }

    /// Mutable graph access. Setup-time only.
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Graph channel count reserved for every quantum.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.graph.channels()
    }

    /// Largest quantum this renderer accepts.
    #[must_use]
    pub const fn max_quantum(&self) -> usize {
        self.max_quantum
    }

    /// Render statistics handle, shareable with the control thread.
    #[must_use]
    pub const fn stats(&self) -> &RenderStats {
        &self.stats
    }

    /// Renders one quantum into `out` and advances the transport.
    ///
    /// Only the first `frames` frames of every channel are written; the rest of
    /// `out` is left untouched. A realtime backend that renders a large device
    /// buffer in several quanta therefore never leaks samples from a previous
    /// quantum into the tail of the current one — nodes cannot write past
    /// `frames` in the first place (they get a [`crate::graph::NodeView`] of
    /// exactly `frames` frames) and the renderer copies exactly `frames`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidFrameCount`] for zero/oversized quanta and
    /// [`RenderError::Graph`] if graph processing refuses the quantum.
    pub fn render(&mut self, frames: usize, out: &mut AudioBuffer) -> Result<(), RenderError> {
        if frames == 0 || frames > self.max_quantum {
            return Err(RenderError::InvalidFrameCount(frames));
        }
        if out.frames() < frames {
            return Err(RenderError::InvalidFrameCount(out.frames()));
        }
        self.drain_commands();
        let position = self.transport.advance(frames);
        // Nodes write `..=ctx.frames`; clearing the whole reserved scratch keeps
        // any stale samples from a previous, longer quantum out of this one.
        // `AudioBuffer::clear` is allocation-free.
        self.scratch.clear();
        let ctx = ProcessContext {
            sample_rate: self.transport.sample_rate(),
            position,
            frames,
            transport: &self.transport,
        };
        self.graph.process(&ctx, frames, &mut self.scratch)?;
        for ch in 0..out.channels().min(self.scratch.channels()) {
            let dst = &mut out.channel_mut(ch)[..frames];
            let src = &self.scratch.channel(ch)[..frames];
            dst.copy_from_slice(src);
        }
        for ch in self.scratch.channels()..out.channels() {
            out.channel_mut(ch)[..frames].fill(0.0);
        }
        self.stats.record(frames);
        Ok(())
    }

    /// Convenience: renders `frames` and returns them as a fresh buffer.
    ///
    /// # Errors
    ///
    /// Same as [`Renderer::render`]; `frames` must lie in `1..=max_quantum`.
    pub fn render_to_vec(&mut self, frames: usize) -> Result<Vec<Sample>, RenderError> {
        let channels = self.scratch.channels();
        let mut out = AudioBuffer::new(channels, frames);
        self.render(frames, &mut out)?;
        let mut v = Vec::with_capacity(channels * frames);
        for ch in 0..channels {
            v.extend_from_slice(&out.channel(ch)[..frames]);
        }
        Ok(v)
    }
}

impl AudioBackend for Renderer {
    fn process(&mut self, frames: usize, output: &mut AudioBuffer) -> Result<(), RenderError> {
        self.render(frames, output)
    }
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("max_quantum", &self.max_quantum)
            .field("frames_rendered", &self.stats.frames())
            .finish_non_exhaustive()
    }
}
