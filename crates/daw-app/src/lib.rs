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
    let mut graph = Graph::with_capacity(job.sample_rate, 1, 1024, 4);
    graph.add_node(
        "tone",
        nodes::Tone {
            freq_hz: 440.0,
            level: 0.5,
        },
    )?;

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

/// Realtime backend (CPAL). Compiled only with `--features realtime`.
///
/// The adapter is deliberately tiny: CPAL calls [`audio_core::Renderer::render`]
/// directly from the data callback, then [`interleave`]s the planar result.
#[cfg(feature = "realtime")]
pub mod realtime {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, StreamConfig};

    use audio_core::buffer::AudioBuffer;
    use audio_core::graph::Graph;
    use audio_core::render::Renderer;
    use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport};

    /// Error type for stream startup.
    #[derive(Debug, thiserror::Error)]
    pub enum BackendError {
        /// Enumerating device formats failed.
        #[error("audio backend: {0}")]
        Configs(#[from] cpal::SupportedStreamConfigsError),
        /// Opening or starting the stream failed.
        #[error("audio stream: {0}")]
        Stream(#[from] cpal::BuildStreamError),
        /// Starting playback failed.
        #[error("audio play: {0}")]
        Play(#[from] cpal::PlayStreamError),
        /// The default output device has no usable configuration.
        #[error("default output device has no f32 configuration")]
        NoF32Config,
        /// No default output device.
        #[error("no default output device")]
        NoDevice,
    }

    /// Writes `frames` planar samples into an interleaved output slice.
    fn interleave_stereo(src: &AudioBuffer, frames: usize, channels: usize, out: &mut [f32]) {
        for frame in 0..frames {
            for ch in 0..channels {
                // Mono graphs are upmixed by repeating channel 0.
                let s = src.channel(ch.min(src.channels() - 1))[frame];
                out[frame * channels + ch] = s;
            }
        }
    }

    /// Plays a graph through the default output device until `stream` drops.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] if the device cannot be opened or configured,
    /// or [`BackendError::NoF32Config`] if no output config supports 48 kHz f32.
    pub fn play_default(graph: Graph) -> Result<cpal::Stream, BackendError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(BackendError::NoDevice)?;
        let config = device
            .supported_output_configs()?
            .find(|c| {
                c.sample_format() == SampleFormat::F32
                    && c.min_sample_rate().0 <= DEFAULT_SAMPLE_RATE
                    && DEFAULT_SAMPLE_RATE <= c.max_sample_rate().0
            })
            .ok_or(BackendError::NoF32Config)?
            .with_sample_rate(cpal::SampleRate(DEFAULT_SAMPLE_RATE));
        let config: StreamConfig = config.into();
        let channels = usize::from(config.channels);
        // Fixed quantum: the renderer reserves scratch for this size once, so
        // the callback never allocates or resizes. Larger device buffers are
        // rendered in pieces of `BLOCK` frames.
        const BLOCK: usize = 1024;
        let mut renderer = Renderer::new(graph, Transport::new(DEFAULT_SAMPLE_RATE), BLOCK);
        renderer
            .transport_mut()
            .apply(audio_core::TransportCommand::Play);
        let mut scratch = AudioBuffer::new(renderer.channels(), BLOCK);

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let frames = data.len() / channels;
                let mut written = 0;
                // Loop instead of assuming the device honours our block size.
                while written < frames {
                    let n = (frames - written).min(BLOCK);
                    if renderer.render(n, &mut scratch).is_err() {
                        // Never panic in the callback: silence on error.
                        data[written * channels..].fill(0.0);
                        return;
                    }
                    interleave_stereo(&scratch, n, channels, &mut data[written * channels..]);
                    written += n;
                }
            },
            move |err| eprintln!("audio stream error: {err}"),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }
}
