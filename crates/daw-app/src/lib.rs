//! Application shell.
//!
//! This crate wires a sound backend to [`audio_core::Renderer`]. The split
//! exists so the engine crate stays free of platform APIs: everything testable
//! lives in `audio-core`, and only the thin backend adapter here needs CI
//! hardware flags.
//!
//! Backend selection:
//!
//! * `offline` (default, always available): bounce to a WAV file. Used by CI,
//!   by agents, and by users without audio hardware.
//! * `realtime` (feature-gated `realtime`): CPAL → WASAPI on Windows,
//!   ALSA/PipeWire/JACK on Linux.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use audio_core::buffer::AudioBuffer;
use audio_core::graph::Graph;
use audio_core::offline::OfflineRenderer;
use audio_core::params::{ParamHandle, ParamId, ParamSpec};
use audio_core::render::Renderer;
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport, TransportCommand};
use audio_core::wav::{Recorder, WavSpec, write_mono_slice};

pub mod nodes;

/// Options for an offline smoke bounce.
#[derive(Debug, Clone)]
pub struct OfflineJob {
    /// Destination WAV path.
    pub output: PathBuf,
    /// Duration in seconds.
    pub seconds: f64,
    /// Sample rate.
    pub sample_rate: u32,
    /// Render quantum used by the bounce, in frames.
    pub block_frames: usize,
    /// Split the output into parts of at most this many samples each.
    ///
    /// `None` (default) writes a single file. `Some(n)` writes
    /// `part-01.wav`, `part-02.wav`, ... as described by
    /// [`audio_core::wav::Recorder`].
    pub split_part_samples: Option<u64>,
}

impl Default for OfflineJob {
    fn default() -> Self {
        Self {
            output: PathBuf::from("bounce.wav"),
            seconds: 1.0,
            sample_rate: DEFAULT_SAMPLE_RATE,
            block_frames: 256,
            split_part_samples: None,
        }
    }
}

/// Result of an offline bounce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BounceReport {
    /// Total frames rendered, across all parts.
    pub frames: u64,
    /// Paths of the WAV files written, in order. A single file when
    /// [`OfflineJob::split_part_samples`] is `None`.
    pub parts: Vec<PathBuf>,
}

/// Renders the demo project offline and writes it as mono WAV.
///
/// Missing parent directories of `job.output` are created. When
/// `job.split_part_samples` is set the audio is streamed into a
/// [`Recorder`] instead of being held in memory, and one file per part is
/// written.
///
/// # Errors
///
/// Returns a boxed error if the graph cannot be built, rendering fails, or the
/// file cannot be written.
pub fn run_offline(job: &OfflineJob) -> Result<BounceReport, Box<dyn std::error::Error>> {
    if let Some(parent) = job.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut graph = Graph::with_capacity(job.sample_rate, 1, 1024, 4, 1);
    let level = ParamHandle::new(demo_level_spec()?);
    graph.register_param(level.clone())?;
    graph.add_node("tone", nodes::Tone::new(440.0, level))?;

    let mut transport = Transport::new(job.sample_rate);
    transport.apply(TransportCommand::Play);

    let renderer = Renderer::new(graph, transport, 1024);
    let mut offline = OfflineRenderer::new(renderer, job.block_frames);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (job.seconds * f64::from(job.sample_rate)).round() as u64;

    let spec = WavSpec {
        sample_rate: job.sample_rate,
        channels: 1,
    };
    match job.split_part_samples {
        None => {
            let mono = offline.bounce_mono(frames)?;
            write_mono_slice(&job.output, job.sample_rate, &mono)?;
            Ok(BounceReport {
                frames,
                parts: vec![job.output.clone()],
            })
        }
        Some(max_part_samples) => {
            let mut recorder = Recorder::new(&job.output, spec, max_part_samples)?;
            // Stream in render quanta so a long bounce never holds the whole
            // take in memory.
            let mut done = 0u64;
            while done < frames {
                let n = (frames - done).min(job.block_frames as u64);
                #[allow(clippy::cast_possible_truncation)]
                let n = n as usize;
                let block = offline.bounce_mono(n as u64)?;
                recorder.write(&block)?;
                done += n as u64;
            }
            let parts = recorder.finish()?;
            Ok(BounceReport { frames, parts })
        }
    }
}

/// Converts a planar buffer into an interleaved output slice.
///
/// This is the last step of every realtime callback and is trivial enough to
/// unit-test without any audio device.
pub fn interleave(src: &AudioBuffer, frames: usize, out: &mut [f32]) {
    let channels = src.channels();
    let n = frames.min(src.frames());
    for frame in 0..n {
        for ch in 0..channels {
            out[frame * channels + ch] = src.channel(ch)[frame];
        }
    }
    for s in &mut out[n * channels..] {
        *s = 0.0;
    }
}

/// Realtime CPAL backend (SPEC-005). Always compiled for its pure helpers;
/// the device glue requires the `realtime` feature.
pub mod realtime;

/// Builds the demo graph used by `daw bounce`, `daw play`, and `selftest`: a
/// single 440 Hz sine at half amplitude on a mono graph.
///
/// # Errors
///
/// Returns a boxed error if the graph cannot be built.
pub fn build_demo_graph(sample_rate: u32) -> Result<Graph, Box<dyn std::error::Error>> {
    let mut graph = Graph::with_capacity(sample_rate, 1, 1024, 4, 1);
    let level = ParamHandle::new(demo_level_spec()?);
    graph.register_param(level.clone())?;
    graph.add_node("tone", nodes::Tone::new(440.0, level))?;
    Ok(graph)
}

/// Description of the demo tone's amplitude: 0..1, half by default, 10 ms ramp.
///
/// # Errors
///
/// Returns a boxed [`audio_core::params::ParamError`] if the description is
/// invalid; it is a constant, so this only trips if the constant is edited
/// wrongly.
pub fn demo_level_spec() -> Result<ParamSpec, Box<dyn std::error::Error>> {
    Ok(ParamSpec::new(ParamId(0), "tone.level", 0.0, 1.0, 0.5, 10)?)
}
