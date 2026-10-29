//! Realtime audio engine core.
//!
//! # Realtime contract
//!
//! Everything reachable from [`AudioBackend::process`] runs on the audio
//! thread. That code path MUST NOT:
//!
//! * allocate or free (no `Vec::push`, no `Box::new`, no `String`)
//! * lock (`Mutex`, `RwLock`, blocking channels)
//! * touch the filesystem, network, or environment
//! * panic on capacity overflow (use [`rtrb`] try-push and drop instead)
//!
//! The control (UI/agent) side talks to the audio thread only through
//! pre-allocated ring buffers. See `docs/REALTIME.md` and `AGENTS.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::must_use_candidate)]

pub mod buffer;
pub mod commands;
pub mod graph;
pub mod midi;
pub mod offline;
pub mod params;
pub mod render;
pub mod schedule;
pub mod transport;
pub mod wav;

pub use buffer::{AudioBuffer, Sample};
pub use commands::{CommandConsumer, CommandQueue};
pub use graph::{Graph, GraphError, Node, NodeId, NodeProcessor};
pub use midi::{MidiError, MidiEvent, MidiFile, MidiMessage, parse_smf};
pub use offline::{OfflineError, OfflineRenderer};
pub use params::{MAX_SMOOTHING_MS, ParamError, ParamHandle, ParamId, ParamSpec};
pub use render::{RenderStats, Renderer};
pub use schedule::{Schedule, ScheduleCursor, ScheduleError, ScheduleSpec, ScheduledEvent};
pub use transport::{BarPosition, Tempo, TimeSignature, Transport, TransportCommand};
pub use wav::{Recorder, WavError, WavSpec, read_wav_mono, write_wav_mono};

/// Abstraction over "who drives the clock".
///
/// The realtime backend (CPAL/WASAPI) and [`OfflineRenderer`] both drive the
/// same [`Renderer`], so practically all engine logic is testable in CI
/// without a sound card.
pub trait AudioBackend {
    /// Render `frames` into `output` and advance the transport accordingly.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] if the renderer cannot produce audio (for
    /// example, an invalid graph at the moment of rendering).
    fn process(&mut self, frames: usize, output: &mut AudioBuffer) -> Result<(), RenderError>;
}

/// Errors produced by [`AudioBackend::process`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RenderError {
    /// The requested frame count is zero or larger than the buffer allows.
    #[error("invalid frame count: {0}")]
    InvalidFrameCount(usize),
    /// The graph is malformed (cycle, unknown node, capacity overflow).
    #[error("graph error: {0}")]
    Graph(#[from] GraphError),
}
