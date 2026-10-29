//! Live playback of a graph, split between the window and the audio thread
//! (SPEC-016).
//!
//! [`playback`] builds a pair: [`PlaybackControl`] for the window and
//! [`PlaybackEngine`] for the audio callback. They talk through a
//! pre-allocated [`CommandQueue`] (window → audio) and two atomics
//! (audio → window). Nothing here touches a device, so the engine is tested
//! offline; [`crate::realtime::start_output`] hands it to CPAL.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use audio_core::AudioBuffer;
use audio_core::Graph;
use audio_core::{CommandConsumer, CommandQueue};
use audio_core::{Renderer, Transport, TransportCommand};

use crate::realtime::interleave_into;

/// Largest quantum the engine renders at once; bigger device buffers are
/// rendered in pieces of this size.
pub const PLAYBACK_BLOCK: usize = 1024;
/// Commands that can wait for the audio thread before new ones are dropped.
pub const COMMAND_CAPACITY: usize = 64;

#[derive(Debug, Default)]
struct Shared {
    position: AtomicU64,
    playing: AtomicBool,
}

/// The window's side: sends transport commands and reads the position.
pub struct PlaybackControl {
    commands: CommandQueue,
    shared: Arc<Shared>,
    sample_rate: u32,
    end: u64,
}

impl std::fmt::Debug for PlaybackControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackControl")
            .field("position", &self.position())
            .field("playing", &self.is_playing())
            .field("sample_rate", &self.sample_rate)
            .field("end", &self.end)
            .finish_non_exhaustive()
    }
}

impl PlaybackControl {
    /// Starts rolling. `false` when the command queue is full.
    pub fn play(&mut self) -> bool {
        self.send(TransportCommand::Play)
    }

    /// Stops and keeps the position. `false` when the queue is full.
    pub fn stop(&mut self) -> bool {
        self.send(TransportCommand::Stop)
    }

    /// Moves to `frames`, clamped to the end. `false` when the queue is full.
    pub fn seek(&mut self, frames: u64) -> bool {
        self.send(TransportCommand::SeekSamples(frames.min(self.end)))
    }

    /// Last position published by the audio thread.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.shared.position.load(Ordering::Relaxed)
    }

    /// Whether the audio thread was rolling at its last quantum.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Sample rate of the graph.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frame where playback stops by itself.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    fn send(&mut self, command: TransportCommand) -> bool {
        self.commands.try_push(command).is_ok()
    }
}

/// The audio thread's side: renders into device buffers.
pub struct PlaybackEngine {
    renderer: Renderer,
    commands: CommandConsumer,
    block: usize,
    scratch: AudioBuffer,
    shared: Arc<Shared>,
    end: u64,
}

impl std::fmt::Debug for PlaybackEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackEngine")
            .field("end", &self.end)
            .finish_non_exhaustive()
    }
}

/// Builds a stopped playback pair positioned at `start` (clamped to `end`).
///
/// All buffers and the command queue are allocated here. Quanta never exceed
/// [`PLAYBACK_BLOCK`] or the graph's own maximum quantum.
#[must_use]
pub fn playback(
    graph: Graph,
    sample_rate: u32,
    start: u64,
    end: u64,
) -> (PlaybackControl, PlaybackEngine) {
    let start = start.min(end);
    let block = PLAYBACK_BLOCK.min(graph.max_quantum()).max(1);
    let mut transport = Transport::new(sample_rate);
    transport.apply(TransportCommand::SeekSamples(start));
    let renderer = Renderer::new(graph, transport, block);
    let (commands, consumer) = CommandQueue::new(COMMAND_CAPACITY);
    let scratch = AudioBuffer::new(renderer.channels().max(1), block);
    let shared = Arc::new(Shared::default());
    shared.position.store(start, Ordering::Relaxed);
    (
        PlaybackControl {
            commands,
            shared: Arc::clone(&shared),
            sample_rate,
            end,
        },
        PlaybackEngine {
            renderer,
            commands: consumer,
            block,
            scratch,
            shared,
            end,
        },
    )
}

impl PlaybackEngine {
    /// Fills an interleaved device buffer of `channels` channels.
    ///
    /// Realtime-safe: no allocation, locking, I/O or panics. Reaching the end
    /// stops the transport; the rest of `out` is silence.
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let frames = out.len() / channels;
        let mut offset = 0usize;
        while offset < frames {
            let mut n = self.block.min(frames - offset);
            // Commands are applied here, not inside the renderer, so the end
            // check below always sees the transport state of this quantum.
            while let Some(command) = self.commands.pop_next() {
                self.renderer.transport_mut().apply(command);
            }
            let transport = self.renderer.transport();
            if transport.is_playing() {
                let left = self.end.saturating_sub(transport.position());
                n = n.min(usize::try_from(left).unwrap_or(usize::MAX));
                if n == 0 {
                    self.renderer.transport_mut().apply(TransportCommand::Stop);
                    continue;
                }
            }
            if self.renderer.render(n, &mut self.scratch).is_err() {
                break;
            }
            let dst = &mut out[offset * channels..(offset + n) * channels];
            interleave_into(&self.scratch, n, channels, dst);
            offset += n;
        }
        out[offset * channels..].fill(0.0);
        if self.renderer.transport().position() >= self.end {
            self.renderer.transport_mut().apply(TransportCommand::Stop);
        }
        let transport = self.renderer.transport();
        self.shared
            .position
            .store(transport.position(), Ordering::Relaxed);
        self.shared
            .playing
            .store(transport.is_playing(), Ordering::Relaxed);
    }
}
