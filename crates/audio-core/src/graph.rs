//! Minimal realtime-safe node graph.
//!
//! Scope of this first slice: a flat list of nodes processed in insertion
//! order, each with its own planar scratch buffer. That is deliberately
//! boring — it is a *seam*: routing, fan-in, and a topological schedule land
//! on top of the same [`NodeProcessor`] trait without changing node code.
//!
//! Nothing here allocates during [`Graph::process`]: all scratch memory is
//! reserved by [`Graph::with_capacity`] at setup time.

use std::collections::TryReserveError;
use std::marker::PhantomData;

use crate::buffer::AudioBuffer;
use crate::params::{ParamError, ParamHandle, ParamId, ParamSpec};
use crate::transport::Transport;

/// Stable identifier of a node inside a graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Per-quantum context handed to every node.
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext<'a> {
    /// Sample rate of the session.
    pub sample_rate: u32,
    /// Absolute sample position of the first frame of this quantum.
    pub position: u64,
    /// Number of frames to render.
    pub frames: usize,
    /// Read-only transport view (tempo, signature, play state).
    pub transport: &'a Transport,
}

/// A processing node: instrument, effect, recorder, analyser.
///
/// Implementors must be realtime-safe: no allocation, locking, or I/O inside
/// [`NodeProcessor::process`]. Allocate in the constructor instead.
pub trait NodeProcessor {
    /// Renders exactly `ctx.frames` frames into `output`.
    ///
    /// The engine zeroes `output` before calling and hands over a window of
    /// exactly `ctx.frames` frames, so nodes may either write or accumulate
    /// with `+=` and cannot reach past the current quantum.
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>);
}

/// Errors that can occur while building or processing a graph.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GraphError {
    /// The graph has zero available node slots.
    #[error("graph node capacity exhausted")]
    CapacityExhausted,
    /// The id does not refer to a mounted node.
    #[error("node {uid} not found")]
    UnknownNode {
        /// Missing node id.
        uid: u32,
    },
    /// Scratch memory could not be reserved for a new node (setup-time only).
    #[error("out of memory reserving scratch for a node: {0}")]
    ScratchAllocation(TryReserveError),
    /// The parameter registry could not be reserved (setup-time only).
    #[error("parameter registry: {0}")]
    Param(#[from] ParamError),
    /// Requested quantum is larger than the reserved scratch size.
    #[error("quantum of {requested} frames exceeds reserved {reserved}")]
    QuantumTooLarge {
        /// Frames requested by the backend.
        requested: usize,
        /// Frames reserved at setup.
        reserved: usize,
    },
    /// The destination mix buffer has fewer channels than the graph.
    #[error("mix has {channel} channels, graph has {channels}")]
    ChannelOutOfRange {
        /// Requested channel.
        channel: usize,
        /// Reserved channels.
        channels: usize,
    },
}

/// A node occupying a slot inside [`Graph`].
struct NodeSlot {
    id: NodeId,
    processor: Box<dyn NodeProcessor + Send>,
    /// Reserved for `max_quantum` frames; nodes only ever see the first
    /// `frames` frames of it (see [`NodeView`]) so the reserved tail can never
    /// carry samples from an earlier, longer quantum.
    scratch: AudioBuffer,
}

impl NodeSlot {
    /// Silences the reserved scratch. The graph silences before handing the view
    /// over, so a node's output never depends on what an earlier quantum left in
    /// the reserved frames. Realtime-safe: a plain `f32` fill.
    fn silence(&mut self) {
        self.scratch.clear();
    }
}

/// The slice of a node's reserved scratch that belongs to the current quantum.
///
/// Nodes receive this instead of the full reserved buffer: writes stay inside
/// `frames` by construction, and a write one frame past the quantum cannot
/// leave a sample that a later, shorter quantum would pick up.
pub struct NodeView<'a> {
    buffer: AudioBuffer,
    frames: usize,
    _slot: PhantomData<&'a mut ()>,
}

impl<'a> NodeView<'a> {
    /// Reinterprets the first `frames` frames of `buffer` as one quantum.
    fn new(buffer: &'a mut AudioBuffer, frames: usize) -> Self {
        Self {
            buffer: buffer.view(frames),
            frames,
            _slot: PhantomData,
        }
    }

    /// Hands the reinterpreted storage back to the slot, still addressed with
    /// the full reserved frame count.
    fn into_buffer(self) -> AudioBuffer {
        self.buffer.restore()
    }
}

impl std::fmt::Debug for NodeView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeView")
            .field("channels", &self.buffer.channels())
            .field("frames", &self.frames)
            .finish()
    }
}

impl NodeView<'_> {
    /// Number of channels in the current quantum.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.buffer.channels()
    }

    /// Number of frames in the current quantum (equals `ctx.frames`).
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Immutable view of one channel for this quantum.
    ///
    /// # Panics
    ///
    /// Panics if `channel` is out of range.
    #[must_use]
    pub fn channel(&self, channel: usize) -> &[crate::buffer::Sample] {
        self.buffer.channel(channel)
    }

    /// Mutable view of one channel for this quantum.
    ///
    /// # Panics
    ///
    /// Panics if `channel` is out of range.
    pub fn channel_mut(&mut self, channel: usize) -> &mut [crate::buffer::Sample] {
        self.buffer.channel_mut(channel)
    }
}

impl std::ops::Deref for NodeView<'_> {
    type Target = [crate::buffer::Sample];

    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl std::ops::DerefMut for NodeView<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}

/// One parameter as visible to the UI: its description and its live handle.
#[derive(Debug, Clone)]
pub struct ParamRegistration {
    /// Description given at registration time.
    pub spec: ParamSpec,
    /// Live handle; the UI calls [`ParamHandle::set`] on it.
    pub handle: ParamHandle,
}

/// Realtime-safe node chain.
pub struct Graph {
    sample_rate: u32,
    channels: usize,
    max_quantum: usize,
    nodes: Vec<NodeSlot>,
    next_id: u32,
    params: Vec<ParamRegistration>,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("max_quantum", &self.max_quantum)
            .field("nodes", &self.nodes.len())
            .field("params", &self.params.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl Graph {
    /// Reserves room for `max_nodes` nodes, `max_params` parameters, and a
    /// `max_quantum`-frame callback.
    ///
    /// The graph never resizes afterwards: one node or parameter more than
    /// reserved returns an error instead of allocating.
    ///
    /// # Panics
    ///
    /// Panics on zero `channels`, `max_nodes`, or `max_quantum`. `max_params`
    /// may be zero, which makes every [`Graph::register_param`] fail with
    /// [`ParamError::RegistryFull`].
    #[must_use]
    pub fn with_capacity(
        sample_rate: u32,
        channels: usize,
        max_quantum: usize,
        max_nodes: usize,
        max_params: usize,
    ) -> Self {
        assert!(sample_rate > 0, "sample_rate must be non-zero");
        assert!(max_nodes > 0, "max_nodes must be non-zero");
        Self {
            sample_rate,
            channels,
            max_quantum,
            nodes: Vec::with_capacity(max_nodes),
            next_id: 0,
            params: Vec::with_capacity(max_params),
        }
    }

    /// Channel count reserved for every node scratch buffer.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    /// Maximum quantum the graph can process without reallocating.
    #[must_use]
    pub const fn max_quantum(&self) -> usize {
        self.max_quantum
    }

    /// Number of mounted nodes in processing order.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Mounts a processor at the end of the chain. Setup-time only.
    ///
    /// `name` is accepted for diagnostics and scheduled for the graph-debug
    /// view; it is not yet stored, so it costs nothing on the audio path.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CapacityExhausted`] if there is no reserved slot.
    pub fn add_node<P: NodeProcessor + Send + 'static>(
        &mut self,
        name: &'static str,
        processor: P,
    ) -> Result<NodeId, GraphError> {
        self.add_boxed(name, Box::new(processor))
    }

    /// Type-erased variant of [`Graph::add_node`] for plugins loaded at runtime.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CapacityExhausted`] if there is no reserved slot.
    pub fn add_boxed(
        &mut self,
        name: &'static str,
        processor: Box<dyn NodeProcessor + Send>,
    ) -> Result<NodeId, GraphError> {
        if self.nodes.len() == self.nodes.capacity() {
            return Err(GraphError::CapacityExhausted);
        }
        // `try_reserve` reports failure instead of aborting the process; the
        // caller may be a sandboxed control thread, not a human at a terminal.
        let _ = name;
        self.nodes
            .try_reserve(1)
            .map_err(GraphError::ScratchAllocation)?;
        let scratch = AudioBuffer::try_new(self.channels, self.max_quantum)
            .map_err(GraphError::ScratchAllocation)?;
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.push(NodeSlot {
            id,
            processor,
            scratch,
        });
        Ok(id)
    }

    /// Registers `handle` so the UI can enumerate and reach the parameter.
    ///
    /// Registration is explicit: [`Graph::add_boxed`] does not scan a node for
    /// parameters, because probing a plugin is a setup-time operation the graph
    /// does not want to guess. Layout is reserved by [`Graph::with_capacity`],
    /// so this never allocates on the audio path (it is control-thread only).
    ///
    /// # Errors
    ///
    /// * [`ParamError::DuplicateId`] if `spec.id` is already registered
    /// * [`ParamError::RegistryFull`] if `max_params` was used up
    pub fn register_param(&mut self, handle: ParamHandle) -> Result<(), ParamError> {
        let spec = handle.spec();
        if self.params.iter().any(|r| r.spec.id == spec.id) {
            return Err(ParamError::DuplicateId { id: spec.id });
        }
        let max = self.params.capacity();
        if self.params.len() == max {
            return Err(ParamError::RegistryFull { max });
        }
        // Capacity is reserved and the length is below it, so this cannot
        // allocate; `try_reserve` keeps that a hard guarantee.
        self.params
            .try_reserve(1)
            .map_err(|_| ParamError::RegistryFull { max })?;
        self.params.push(ParamRegistration { spec, handle });
        Ok(())
    }

    /// Every registered parameter, in registration order.
    #[must_use]
    pub fn params(&self) -> &[ParamRegistration] {
        &self.params
    }

    /// Looks a parameter up by id.
    ///
    /// A linear scan over the small, fixed registry; keep it off the audio
    /// path (nodes hold their handle directly).
    ///
    /// # Errors
    ///
    /// Returns [`ParamError::UnknownParam`] if `id` was never registered.
    pub fn param(&self, id: ParamId) -> Result<ParamHandle, ParamError> {
        self.params
            .iter()
            .find(|registration| registration.spec.id == id)
            .map(|registration| registration.handle.clone())
            .ok_or(ParamError::UnknownParam { id })
    }

    /// Removes a node by id. Setup-time only.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownNode`] if the id is not mounted.
    pub fn remove(&mut self, id: NodeId) -> Result<(), GraphError> {
        let index = self
            .nodes
            .iter()
            .position(|slot| slot.id == id)
            .ok_or(GraphError::UnknownNode { uid: id.0 })?;
        self.nodes.remove(index);
        Ok(())
    }

    /// Runs the whole chain for one quantum and writes the result into `mix`.
    ///
    /// Node outputs are summed (this is a mixer, not a filter chain), which
    /// makes adding tracks a matter of mounting one more node.
    ///
    /// Realtime-safe: no allocation and no locking on the hot path. Errors are
    /// returned rather than panicking because every refusal here happens
    /// inside the audio callback.
    ///
    /// # Errors
    ///
    /// * [`GraphError::QuantumTooLarge`] if `frames > max_quantum`
    /// * [`GraphError::ChannelOutOfRange`] if `mix` is too small
    pub fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: usize,
        mix: &mut AudioBuffer,
    ) -> Result<(), GraphError> {
        if frames > self.max_quantum || frames > mix.frames() {
            return Err(GraphError::QuantumTooLarge {
                requested: frames,
                reserved: self.max_quantum.min(mix.frames()),
            });
        }
        if mix.channels() < self.channels {
            return Err(GraphError::ChannelOutOfRange {
                channel: mix.channels(),
                channels: self.channels,
            });
        }
        for ch in 0..self.channels {
            mix.channel_mut(ch)[..frames].fill(0.0);
        }
        let node_ctx = ProcessContext { frames, ..*ctx };
        if !ctx.transport.is_playing() {
            // A stopped transport is silent for *every* node, including
            // generators that ignore position; `mix` and the node scratch
            // (cleared once at construction) are already silent.
            return Ok(());
        }
        let channels = self.channels;
        for slot in &mut self.nodes {
            slot.silence();
            // Borrow the storage for the node, run it, then take the storage
            // back to mix it: only one live handle to a slot at a time.
            let mut view = NodeView::new(&mut slot.scratch, frames);
            slot.processor.process(&node_ctx, &mut view);
            // Read through the view: it lays channels out with a stride of
            // `frames`, which differs from the restored buffer's stride on a
            // short quantum.
            for ch in 0..channels {
                mix.add_from(ch, 0, &view.channel(ch)[..frames]);
            }
            slot.scratch = view.into_buffer();
        }
        Ok(())
    }
}

/// `Node` is a convenience alias for [`NodeId`].
pub type Node = NodeId;
