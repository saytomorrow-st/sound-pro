//! Realtime CPAL backend (SPEC-005).
//!
//! This module has two layers that deliberately live side by side:
//!
//! * **Pure decisions** — [`choose_config`], [`QuantumSplitter`], [`RealtimeStats`]:
//!   plain data in, plain data out. They carry no device handle and are always
//!   compiled, so the unit tests here run on CI machines with no sound card and
//!   without enabling the `realtime` feature.
//! * **Device glue** — [`play_default`] and [`RealtimeHandle`]: the CPAL adapter.
//!   Compiled only with `--features realtime`, because CPAL pulls in ALSA on
//!   Linux.
//!
//! The audio callback obeys the realtime contract (see `docs/REALTIME.md`): it
//! never allocates, locks, formats, logs, `unwrap`s, or panics. All expensive
//! work — enumerating configurations, creating buffers, logging the choice —
//! happens before the stream starts. Only `Relaxed` atomics are touched on the
//! audio path.

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "realtime")]
use cpal::traits::StreamTrait;
#[cfg(feature = "realtime")]
use std::sync::Arc;

/// Errors from starting or driving the realtime backend.
///
/// The `NoDevice` / `NoF32Config` variants are always available (they are pure
/// selection outcomes); the wrapped-CPAL variants only exist with the
/// `realtime` feature.
#[derive(Debug, thiserror::Error)]
pub enum RealtimeError {
    /// No default output device is present.
    #[error("no default output device")]
    NoDevice,
    /// The device exposes no 32-bit float output configuration.
    #[error("default output device has no f32 configuration")]
    NoF32Config,
    /// Enumerating device configurations failed.
    #[cfg(feature = "realtime")]
    #[error("audio backend: {0}")]
    Configs(#[from] cpal::SupportedStreamConfigsError),
    /// Opening or configuring the output stream failed.
    #[cfg(feature = "realtime")]
    #[error("audio stream: {0}")]
    Stream(#[from] cpal::BuildStreamError),
    /// Starting playback failed.
    #[cfg(feature = "realtime")]
    #[error("audio play: {0}")]
    Play(#[from] cpal::PlayStreamError),
    /// The caller's graph factory failed.
    #[error("graph: {0}")]
    Graph(String),
}

/// One F32 output configuration advertised by a device.
///
/// Mirrors the fields of `cpal::SupportedStreamConfigRange` that selection needs,
/// but as plain data so [`choose_config`] is testable without CPAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceConfig {
    /// Minimum sample rate the device supports, in Hz.
    pub min_sample_rate: u32,
    /// Maximum sample rate the device supports, in Hz.
    pub max_sample_rate: u32,
    /// Channel count of this configuration.
    pub channels: u16,
    /// Whether this configuration is 32-bit float (the only format we accept).
    pub is_f32: bool,
}

/// The configuration chosen by [`choose_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChosenConfig {
    /// Sample rate to open the stream at.
    pub sample_rate: u32,
    /// Channel count to open the stream at.
    pub channels: u16,
}

/// Picks the F32 output configuration closest to `preferred_rate`.
///
/// Preference order: a config whose `[min, max]` range contains
/// `preferred_rate` wins outright (clamped exactly to `preferred_rate`).
/// Otherwise the F32 config whose range is closest to `preferred_rate` is
/// chosen and clamped to the nearest allowed rate. Non-F32 configs are ignored.
///
/// The scan is O(n) with no allocation; it is meant to run once, before the
/// stream starts, never in the callback.
///
/// # Errors
///
/// [`RealtimeError::NoDevice`] if `configs` is empty, [`RealtimeError::NoF32Config`]
/// if none are F32.
pub fn choose_config(
    configs: &[DeviceConfig],
    preferred_rate: u32,
) -> Result<ChosenConfig, RealtimeError> {
    if configs.is_empty() {
        return Err(RealtimeError::NoDevice);
    }
    let mut best: Option<(u32, ChosenConfig)> = None;
    let mut any_f32 = false;
    for cfg in configs {
        if !cfg.is_f32 {
            continue;
        }
        any_f32 = true;
        // Distance from the preferred rate to this config's range (0 if inside).
        let (rate, distance) = if preferred_rate < cfg.min_sample_rate {
            (cfg.min_sample_rate, cfg.min_sample_rate - preferred_rate)
        } else if preferred_rate > cfg.max_sample_rate {
            (cfg.max_sample_rate, preferred_rate - cfg.max_sample_rate)
        } else {
            (preferred_rate, 0)
        };
        let chosen = ChosenConfig {
            sample_rate: rate,
            channels: cfg.channels,
        };
        match best {
            // An exact in-range match (distance 0) short-circuits immediately.
            Some((0, _)) => {}
            Some((d, _)) if d <= distance => {}
            _ => best = Some((distance, chosen)),
        }
        if distance == 0 {
            break;
        }
    }
    if !any_f32 {
        return Err(RealtimeError::NoF32Config);
    }
    match best {
        Some((_, cfg)) => Ok(cfg),
        None => Err(RealtimeError::NoF32Config),
    }
}

/// Splits a device callback of `frames` into render quanta of at most `block`.
///
/// The audio callback must render in pieces the renderer was built for; a
/// device buffer may be larger than `block`, and never exactly equal. This pure
/// splitter yields successive chunk lengths, allocation-free, so the callback
/// keeps its realtime guarantees.
///
/// ```
/// use daw_app::realtime::QuantumSplitter;
/// let mut s = QuantumSplitter::new(3000, 1024);
/// assert_eq!(s.next_len(), 1024);
/// assert_eq!(s.next_len(), 1024);
/// assert_eq!(s.next_len(), 952);
/// assert_eq!(s.next_len(), 0);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct QuantumSplitter {
    remaining: usize,
    block: usize,
}

impl QuantumSplitter {
    /// Creates a splitter for `frames` total rendered in chunks of `block`.
    ///
    /// A `block` of zero is treated as a single chunk covering all frames (a
    /// defensive fallback; the renderer is never built with a zero quantum).
    #[must_use]
    pub const fn new(frames: usize, block: usize) -> Self {
        Self {
            remaining: frames,
            block: if block == 0 { frames } else { block },
        }
    }

    /// Length of the next chunk, or 0 when all frames are consumed.
    #[must_use]
    pub const fn next_len(&mut self) -> usize {
        if self.remaining == 0 {
            return 0;
        }
        let n = if self.remaining < self.block {
            self.remaining
        } else {
            self.block
        };
        self.remaining -= n;
        n
    }

    /// Whether all frames have been handed out.
    #[must_use]
    pub const fn is_done(&self) -> bool {
        self.remaining == 0
    }
}

/// Atomic counters shared between the audio callback and the control side.
///
/// Every field is a `Relaxed` atomic: counters need no ordering, only
/// individual-value consistency. Instantiated once, before the stream starts.
#[derive(Debug, Default)]
pub struct RealtimeStats {
    rendered_frames: AtomicU64,
    dropped_callbacks: AtomicU64,
    render_errors: AtomicU64,
    stream_errors: AtomicU64,
}

impl RealtimeStats {
    /// Total frames successfully rendered into the device.
    #[must_use]
    pub fn rendered_frames(&self) -> u64 {
        self.rendered_frames.load(Ordering::Relaxed)
    }

    /// Callbacks skipped because a previous quantum could not be produced.
    #[must_use]
    pub fn dropped_callbacks(&self) -> u64 {
        self.dropped_callbacks.load(Ordering::Relaxed)
    }

    /// Quanta where the engine returned an error (the callback wrote silence).
    #[must_use]
    pub fn render_errors(&self) -> u64 {
        self.render_errors.load(Ordering::Relaxed)
    }

    /// Errors reported by the audio backend for this stream.
    #[must_use]
    pub fn stream_errors(&self) -> u64 {
        self.stream_errors.load(Ordering::Relaxed)
    }
}

/// Handle to a running realtime stream. Dropping it stops playback.
#[cfg(feature = "realtime")]
pub struct RealtimeHandle {
    stream: Option<cpal::Stream>,
    stats: Arc<RealtimeStats>,
}

#[cfg(feature = "realtime")]
impl RealtimeHandle {
    /// Shared counters for this stream.
    #[must_use]
    pub fn stats(&self) -> Arc<RealtimeStats> {
        Arc::clone(&self.stats)
    }

    /// Stops the stream and blocks until the audio thread has wound down.
    ///
    /// Idempotent: a second call is a no-op. Never panics — errors from stopping
    /// are swallowed (the stream is being discarded anyway). Stream errors
    /// collected by the backend are reported here, on the caller's thread,
    /// because the error callback itself must not log.
    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            // Dropping the `cpal::Stream` joins its audio thread. `pause` is
            // best-effort; either way the drop below is what returns promptly.
            let _ = stream.pause();
            drop(stream);
            let errors = self.stats.stream_errors.load(Ordering::Relaxed);
            if errors > 0 {
                eprintln!("realtime: {errors} audio stream error(s)");
            }
        }
    }
}

#[cfg(feature = "realtime")]
impl Drop for RealtimeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Writes `frames` planar samples from `src` into an interleaved `out` buffer.
///
/// Mono graphs are upmixed by repeating channel 0 into every device channel;
/// device channels beyond the source channel count also repeat the last
/// available source channel, matching the previous prototype's behaviour.
pub fn interleave_into(
    src: &audio_core::AudioBuffer,
    frames: usize,
    channels: usize,
    out: &mut [f32],
) {
    if channels == 0 {
        return;
    }
    let src_channels = src.channels();
    let n = frames.min(src.frames()).min(out.len() / channels);
    for frame in 0..n {
        for ch in 0..channels {
            let source = ch.min(src_channels - 1);
            out[frame * channels + ch] = src.channel(source)[frame];
        }
    }
    // Silence any remaining device samples (e.g. when frames < out capacity).
    if let Some(tail) = out.get_mut(n * channels..) {
        tail.fill(0.0);
    }
}

#[cfg(feature = "realtime")]
mod device {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use audio_core::buffer::AudioBuffer;
    use audio_core::graph::Graph;
    use audio_core::render::Renderer;
    use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, StreamConfig};

    use super::{
        ChosenConfig, DeviceConfig, QuantumSplitter, RealtimeError, RealtimeHandle, RealtimeStats,
        choose_config, interleave_into,
    };

    /// Fixed render quantum reserved by the engine. Larger device buffers are
    /// rendered in pieces of this many frames; nothing resizes in the callback.
    const BLOCK: usize = 1024;

    /// Snapshot the device's F32 configs as plain data for [`choose_config`].
    fn device_configs(device: &cpal::Device) -> Result<Vec<DeviceConfig>, RealtimeError> {
        let mut out = Vec::new();
        for range in device.supported_output_configs()? {
            out.push(DeviceConfig {
                min_sample_rate: range.min_sample_rate().0,
                max_sample_rate: range.max_sample_rate().0,
                channels: range.channels(),
                is_f32: range.sample_format() == SampleFormat::F32,
            });
        }
        Ok(out)
    }

    /// Starts the default output device playing the graph built by `build`.
    ///
    /// The device may not run at [`DEFAULT_SAMPLE_RATE`], so the graph is built
    /// *after* the configuration is chosen, at the rate the stream actually
    /// uses. Building it up front would detune every node.
    ///
    /// Everything expensive — config enumeration, choosing the rate, creating
    /// buffers, logging to stderr — happens here, before the stream starts. The
    /// returned [`RealtimeHandle`] owns the stream; calling
    /// [`RealtimeHandle::stop`] (or dropping it) stops playback.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::NoDevice`] when there is no output device,
    /// [`RealtimeError::NoF32Config`] when it exposes no F32 config, or a
    /// CPAL error while building/starting the stream.
    pub fn play_default<F>(build: F) -> Result<RealtimeHandle, RealtimeError>
    where
        F: FnOnce(u32) -> Result<Graph, Box<dyn std::error::Error>>,
    {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(RealtimeError::NoDevice)?;
        let configs = device_configs(&device)?;
        let chosen: ChosenConfig = choose_config(&configs, DEFAULT_SAMPLE_RATE)?;

        // Widen the chosen range to a full stream config at its rate.
        let supported = device
            .supported_output_configs()?
            .find(|r| {
                r.sample_format() == SampleFormat::F32
                    && r.channels() == chosen.channels
                    && r.min_sample_rate().0 <= chosen.sample_rate
                    && chosen.sample_rate <= r.max_sample_rate().0
            })
            .ok_or(RealtimeError::NoF32Config)?;
        let stream_config: StreamConfig = supported
            .with_sample_rate(cpal::SampleRate(chosen.sample_rate))
            .into();
        let channels = usize::from(stream_config.channels);

        // Log the decision on the fallback thread, never in the callback.
        eprintln!(
            "realtime: {} Hz, {} ch (preferred {} Hz)",
            stream_config.sample_rate.0, channels, DEFAULT_SAMPLE_RATE
        );

        let graph = build(chosen.sample_rate).map_err(|e| RealtimeError::Graph(e.to_string()))?;
        let mut renderer = Renderer::new(graph, Transport::new(chosen.sample_rate), BLOCK);
        renderer
            .transport_mut()
            .apply(audio_core::TransportCommand::Play);
        let mut scratch = AudioBuffer::new(renderer.channels(), BLOCK);
        let stats = Arc::new(RealtimeStats::default());
        let cb_stats = Arc::clone(&stats);
        let err_stats = Arc::clone(&stats);

        let stream = device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| {
                let frames = data.len() / channels;
                let mut split = QuantumSplitter::new(frames, BLOCK);
                let mut offset = 0usize;
                loop {
                    let n = split.next_len();
                    if n == 0 {
                        break;
                    }
                    if renderer.render(n, &mut scratch).is_err() {
                        // Engine failure: silence the rest of the buffer and
                        // count it. Never panic in the callback.
                        data[offset * channels..].fill(0.0);
                        cb_stats.render_errors.fetch_add(1, Ordering::Relaxed);
                        cb_stats.dropped_callbacks.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    interleave_into(&scratch, n, channels, &mut data[offset * channels..]);
                    offset += n;
                }
                cb_stats
                    .rendered_frames
                    .fetch_add(frames as u64, Ordering::Relaxed);
            },
            move |_err| {
                // CPAL calls this on its own thread with the stream possibly
                // still live. Only bump a counter: no logging, no locks.
                err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )?;
        stream.play()?;

        Ok(RealtimeHandle {
            stream: Some(stream),
            stats,
        })
    }
}

#[cfg(feature = "realtime")]
pub use device::play_default;

#[cfg(test)]
mod tests {
    use super::*;
    use audio_core::transport::DEFAULT_SAMPLE_RATE;

    fn f32_cfg(min: u32, max: u32, channels: u16) -> DeviceConfig {
        DeviceConfig {
            min_sample_rate: min,
            max_sample_rate: max,
            channels,
            is_f32: true,
        }
    }

    #[test]
    fn choose_prefers_exact_48k_when_available() {
        let cfgs = [f32_cfg(44_100, 44_100, 2), f32_cfg(48_000, 48_000, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, 48_000);
        assert_eq!(chosen.channels, 2);
    }

    #[test]
    fn choose_keeps_exact_match_even_when_a_farther_config_follows() {
        // Exact 48 kHz match first; 96 kHz-only config follows. A buggy
        // "last valid config wins" would wrongly pick 96 kHz.
        let cfgs = [f32_cfg(48_000, 48_000, 2), f32_cfg(96_000, 96_000, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, 48_000);
    }

    #[test]
    fn choose_keeps_nearest_even_when_a_farther_config_follows() {
        // Nearest is 44.1 kHz-first; 96 kHz (second) is farther from 48 kHz.
        let cfgs = [f32_cfg(44_100, 44_100, 2), f32_cfg(96_000, 96_000, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, 44_100);
    }

    #[test]
    fn choose_falls_back_to_44100_only_device() {
        // Only a 44.1 kHz device exists: it must be used, transport rides 44.1.
        let cfgs = [f32_cfg(44_100, 44_100, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, 44_100);
    }

    #[test]
    fn choose_picks_nearest_when_preferred_outside_every_range() {
        // Neither range contains 48 kHz; 44.1 is nearer than 96 kHz.
        let cfgs = [f32_cfg(88_200, 96_000, 2), f32_cfg(44_100, 48_000 - 1, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, 48_000 - 1);
    }

    #[test]
    fn choose_clamps_inside_a_range_spanning_preferred() {
        let cfgs = [f32_cfg(44_100, 96_000, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.sample_rate, DEFAULT_SAMPLE_RATE);
    }

    #[test]
    fn choose_errors_without_any_config() {
        assert!(matches!(
            choose_config(&[], DEFAULT_SAMPLE_RATE),
            Err(RealtimeError::NoDevice)
        ));
    }

    #[test]
    fn choose_errors_when_only_non_f32_configs_exist() {
        let cfgs = [DeviceConfig {
            min_sample_rate: 44_100,
            max_sample_rate: 48_000,
            channels: 2,
            is_f32: false,
        }];
        assert!(matches!(
            choose_config(&cfgs, DEFAULT_SAMPLE_RATE),
            Err(RealtimeError::NoF32Config)
        ));
    }

    #[test]
    fn splitter_zero_frames_yields_nothing() {
        let mut s = QuantumSplitter::new(0, 1024);
        assert_eq!(s.next_len(), 0);
        assert!(s.is_done());
    }

    #[test]
    fn splitter_less_than_block_is_single_chunk() {
        let mut s = QuantumSplitter::new(300, 1024);
        assert_eq!(s.next_len(), 300);
        assert_eq!(s.next_len(), 0);
    }

    #[test]
    fn splitter_exact_multiple_of_block() {
        let mut s = QuantumSplitter::new(2048, 1024);
        let chunks: Vec<usize> = std::iter::from_fn(|| {
            let n = s.next_len();
            (n != 0).then_some(n)
        })
        .collect();
        assert_eq!(chunks, vec![1024, 1024]);
    }

    #[test]
    fn splitter_with_remainder() {
        let mut s = QuantumSplitter::new(2500, 1024);
        let chunks: Vec<usize> = std::iter::from_fn(|| {
            let n = s.next_len();
            (n != 0).then_some(n)
        })
        .collect();
        assert_eq!(chunks, vec![1024, 1024, 452]);
        assert_eq!(chunks.iter().sum::<usize>(), 2500);
    }

    #[test]
    fn splitter_equal_to_block_is_one_chunk() {
        let mut s = QuantumSplitter::new(1024, 1024);
        assert_eq!(s.next_len(), 1024);
        assert_eq!(s.next_len(), 0);
    }
}
