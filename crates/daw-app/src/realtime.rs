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
    /// No default input device (microphone) is present (SPEC-020).
    #[error("no microphone found")]
    NoInputDevice,
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
    /// The device cannot run at the project's sample rate (SPEC-016).
    #[error("output device cannot play {project} Hz (nearest is {nearest} Hz)")]
    SampleRate {
        /// Rate the project needs.
        project: u32,
        /// Closest rate the device offers.
        nearest: u32,
    },
}

/// Why an ALSA output device failed to open on Linux, for user-facing
/// diagnostics (SPEC-016). Pure data with no filesystem or device access, so
/// it is always compiled and unit-testable without the `realtime` feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlsaFailure {
    /// `libasound.so.2` (the ALSA library) is missing from the host.
    MissingLibasound,
    /// ALSA is present but no output device can be enumerated.
    NoDevice,
    /// A device exists but the PipeWire ALSA bridge plugin
    /// (`libasound_module_pcm_pipewire.so`) is missing, so the `default` PCM
    /// cannot be opened.
    MissingPipewirePlugin,
    /// A device exists and the bridge is present, yet opening still failed.
    Other,
}

impl AlsaFailure {
    /// A plain-language Russian explanation and fix, for the status bar and
    /// `[play-debug]` log.
    #[must_use]
    pub fn russian_message(self) -> &'static str {
        match self {
            AlsaFailure::MissingLibasound => {
                "В системе нет ALSA (libasound.so.2). Установите: Fedora: sudo dnf install \
                 alsa-lib; Ubuntu/Debian: sudo apt install libasound2; Arch: sudo pacman -S alsa-lib"
            }
            AlsaFailure::NoDevice => {
                "Не найдено ни одного устройства вывода звука. Подключите устройство или \
                 включите звуковой сервер (PipeWire/PulseAudio) и попробуйте снова."
            }
            AlsaFailure::MissingPipewirePlugin => {
                "В системе нет pipewire-alsa (мост ALSA→PipeWire). Установите: \
                 Fedora: sudo dnf install pipewire-alsa; Ubuntu/Debian: sudo apt install \
                 pipewire-alsa; Arch: sudo pacman -S pipewire-alsa"
            }
            AlsaFailure::Other => {
                "Не удалось открыть устройство вывода. Проверьте настройки звука и \
                 попробуйте снова."
            }
        }
    }
}

/// Classifies why ALSA playback failed from three host facts, in priority order:
/// the presence of the ALSA library, the presence of any output device, and the
/// presence of the PipeWire ALSA bridge plugin.
///
/// Pure: takes booleans so callers (and tests) supply mocked filesystem/device
/// results instead of touching the host.
#[must_use]
pub fn classify_alsa_failure(
    has_libasound: bool,
    has_device: bool,
    has_pipewire_plugin: bool,
) -> AlsaFailure {
    if !has_libasound {
        AlsaFailure::MissingLibasound
    } else if !has_device {
        AlsaFailure::NoDevice
    } else if !has_pipewire_plugin {
        AlsaFailure::MissingPipewirePlugin
    } else {
        AlsaFailure::Other
    }
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
    let mut best: Option<((u32, u8), ChosenConfig)> = None;
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
        // Rate distance first, then stereo over more channels over mono
        // (SPEC-037): a mono stream would fold the pan back into one
        // channel. Equal keys keep the earlier config.
        let key = (distance, channel_rank(cfg.channels));
        match best {
            Some((k, _)) if k <= key => {}
            _ => best = Some((key, chosen)),
        }
        if key == (0, 0) {
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

/// How well a channel count carries the stereo mix: 0 for stereo, 1 for
/// more channels (extra ones repeat the right), 2 for mono or none.
const fn channel_rank(channels: u16) -> u8 {
    match channels {
        2 => 0,
        3.. => 1,
        _ => 2,
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

/// Picks an F32 configuration that runs at exactly `rate` (SPEC-016).
///
/// Playback never resamples, so a near miss is an error rather than a
/// detuned song.
///
/// # Errors
///
/// Those of [`choose_config`], and [`RealtimeError::SampleRate`] when no F32
/// configuration covers `rate`.
pub fn choose_exact_config(
    configs: &[DeviceConfig],
    rate: u32,
) -> Result<ChosenConfig, RealtimeError> {
    let chosen = choose_config(configs, rate)?;
    if chosen.sample_rate == rate {
        Ok(chosen)
    } else {
        Err(RealtimeError::SampleRate {
            project: rate,
            nearest: chosen.sample_rate,
        })
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
        choose_config, choose_exact_config, interleave_into,
    };
    use crate::audio_settings::{AudioSettings, choose_buffer, find_device};
    use crate::capture::{CaptureControl, capture};
    use crate::playback::PlaybackEngine;

    fn names(devices: impl Iterator<Item = cpal::Device>) -> (Vec<cpal::Device>, Vec<String>) {
        let mut found = Vec::new();
        let mut names = Vec::new();
        for device in devices {
            let name = device.name().unwrap_or_else(|_| "(без имени)".to_owned());
            found.push(device);
            names.push(name);
        }
        (found, names)
    }

    /// Names of the output devices the system offers (SPEC-033).
    #[must_use]
    pub fn output_device_names() -> Vec<String> {
        cpal::default_host()
            .output_devices()
            .map(|d| names(d).1)
            .unwrap_or_default()
    }

    /// Names of the input devices the system offers (SPEC-033).
    #[must_use]
    pub fn input_device_names() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|d| names(d).1)
            .unwrap_or_default()
    }

    /// Output device named in `settings`, or the default when it is absent.
    fn pick_output(settings: &AudioSettings) -> Result<cpal::Device, RealtimeError> {
        let host = cpal::default_host();
        if settings.output.is_some()
            && let Ok(devices) = host.output_devices()
        {
            let (mut found, names) = names(devices);
            if let Some(i) = find_device(&names, settings.output.as_deref()) {
                return Ok(found.swap_remove(i));
            }
        }
        host.default_output_device().ok_or(RealtimeError::NoDevice)
    }

    /// Input device named in `settings`, or the default when it is absent.
    fn pick_input(settings: &AudioSettings) -> Result<cpal::Device, RealtimeError> {
        let host = cpal::default_host();
        if settings.input.is_some()
            && let Ok(devices) = host.input_devices()
        {
            let (mut found, names) = names(devices);
            if let Some(i) = find_device(&names, settings.input.as_deref()) {
                return Ok(found.swap_remove(i));
            }
        }
        host.default_input_device()
            .ok_or(RealtimeError::NoInputDevice)
    }

    fn buffer_size(
        requested: Option<u32>,
        supported: &cpal::SupportedBufferSize,
    ) -> cpal::BufferSize {
        let range = match supported {
            cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            cpal::SupportedBufferSize::Unknown => None,
        };
        choose_buffer(requested, range).map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed)
    }

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

    /// Opens the default output device at `sample_rate` and lets `engine`
    /// fill every buffer (SPEC-016).
    ///
    /// # Errors
    ///
    /// [`RealtimeError::NoDevice`], [`RealtimeError::NoF32Config`],
    /// [`RealtimeError::SampleRate`] when the device cannot run at
    /// `sample_rate`, or a CPAL error while building/starting the stream.
    pub fn start_output(
        engine: PlaybackEngine,
        sample_rate: u32,
    ) -> Result<RealtimeHandle, RealtimeError> {
        start_output_with(engine, sample_rate, &AudioSettings::default()).map_err(|boxed| boxed.0)
    }

    /// [`start_output`] on the device and buffer chosen in `settings`
    /// (SPEC-033). A device that is gone falls back to the default.
    ///
    /// Unlike [`start_output`], the engine is returned on error so the caller
    /// can keep the transport rolling on a silent clock when no usable output
    /// device exists. The engine is `None` only in the pathological case where
    /// a probe stream opened successfully but the real stream then failed; in
    /// that rare case the engine has already been handed to the realtime
    /// callback and cannot be recovered without cloning or locking (both
    /// forbidden on the audio path).
    ///
    /// # Errors
    ///
    /// The [`RealtimeError`] is paired with the still-owned [`PlaybackEngine`]
    /// (when recoverable) so it can be reused, for example by
    /// [`PlaybackEngine::advance_silent`].
    pub fn start_output_with(
        mut engine: PlaybackEngine,
        sample_rate: u32,
        settings: &AudioSettings,
    ) -> Result<RealtimeHandle, Box<(RealtimeError, Option<PlaybackEngine>)>> {
        // Preflight: resolve the device and stream config without touching the
        // engine. Any failure here returns the engine so the caller can fall
        // back to a silent transport.
        let (device, stream_config, channels) = match resolve_output(sample_rate, settings) {
            Ok(v) => v,
            Err(e) => return Err(Box::new((e, Some(engine)))),
        };

        // Probe the device *before* moving the engine into the realtime
        // callback. Opening (and starting) a no-op stream exercises the exact
        // failure that a missing PipeWire ALSA plugin causes on Fedora — the
        // dlopen happens inside `build_output_stream`/`play` — while the engine
        // is still owned here. The probe is dropped immediately.
        let probe = match device.build_output_stream(
            &stream_config,
            move |_data: &mut [f32], _| {},
            |_err| {},
            None,
        ) {
            Ok(s) => s,
            Err(e) => return Err(Box::new((RealtimeError::Stream(e), Some(engine)))),
        };
        if let Err(e) = probe.play() {
            return Err(Box::new((RealtimeError::Play(e), Some(engine))));
        }
        drop(probe);

        // The probe succeeded, so the same device/config will open here. Move
        // the engine into the real callback. A failure at this point is
        // pathological (the identical open just succeeded) and the engine is
        // unrecoverable without cloning or locking, so it is reported as `None`.
        let stats = Arc::new(RealtimeStats::default());
        let cb_stats = Arc::clone(&stats);
        let err_stats = Arc::clone(&stats);
        let stream = match device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| {
                engine.fill(data, channels);
                cb_stats
                    .rendered_frames
                    .fetch_add((data.len() / channels.max(1)) as u64, Ordering::Relaxed);
            },
            move |_err| {
                err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        ) {
            Ok(s) => s,
            Err(e) => return Err(Box::new((RealtimeError::Stream(e), None))),
        };
        if let Err(e) = stream.play() {
            return Err(Box::new((RealtimeError::Play(e), None)));
        }
        Ok(RealtimeHandle {
            stream: Some(stream),
            stats,
        })
    }

    /// Resolves the output device and stream configuration for `sample_rate`
    /// without touching the engine. Pure selection plus CPAL enumeration; runs
    /// once, before the stream starts.
    fn resolve_output(
        sample_rate: u32,
        settings: &AudioSettings,
    ) -> Result<(cpal::Device, StreamConfig, usize), RealtimeError> {
        let device = pick_output(settings)?;
        let configs = device_configs(&device)?;
        let chosen = choose_exact_config(&configs, sample_rate)?;
        let supported = device
            .supported_output_configs()?
            .find(|r| {
                r.sample_format() == SampleFormat::F32
                    && r.channels() == chosen.channels
                    && r.min_sample_rate().0 <= sample_rate
                    && sample_rate <= r.max_sample_rate().0
            })
            .ok_or(RealtimeError::NoF32Config)?;
        let buffer = buffer_size(settings.buffer_frames, supported.buffer_size());
        let mut stream_config: StreamConfig = supported
            .with_sample_rate(cpal::SampleRate(sample_rate))
            .into();
        stream_config.buffer_size = buffer;
        let channels = usize::from(stream_config.channels);
        Ok((device, stream_config, channels))
    }

    /// Candidate locations of `libasound.so.2` on common Linux layouts.
    #[cfg(target_os = "linux")]
    const LIBASOUND_PATHS: &[&str] = &[
        "/usr/lib64/libasound.so.2",
        "/usr/lib/x86_64-linux-gnu/libasound.so.2",
        "/usr/lib/aarch64-linux-gnu/libasound.so.2",
        "/usr/lib/libasound.so.2",
    ];

    /// Candidate locations of the PipeWire ALSA bridge plugin.
    #[cfg(target_os = "linux")]
    const PIPEWIRE_PLUGIN_PATHS: &[&str] = &[
        "/usr/lib64/alsa-lib/libasound_module_pcm_pipewire.so",
        "/usr/lib/x86_64-linux-gnu/alsa-lib/libasound_module_pcm_pipewire.so",
        "/usr/lib/aarch64-linux-gnu/alsa-lib/libasound_module_pcm_pipewire.so",
        "/usr/lib/alsa-lib/libasound_module_pcm_pipewire.so",
    ];

    /// Determines why ALSA playback failed on this Linux host and returns a
    /// plain-language Russian explanation (mirrored to `[play-debug]` by the
    /// caller). Returns `None` on non-Linux, where ALSA does not apply.
    ///
    /// Distinguishes three cases: no `libasound.so.2`, no output device, and a
    /// missing PipeWire ALSA bridge plugin. The transport keeps rolling on a
    /// silent clock regardless, so this only shapes the user-facing message.
    pub fn diagnose_alsa_failure() -> Option<String> {
        diagnose_alsa_failure_impl()
    }

    #[cfg(target_os = "linux")]
    fn diagnose_alsa_failure_impl() -> Option<String> {
        let has_libasound = any_path_exists(LIBASOUND_PATHS);
        let has_pipewire_plugin = any_path_exists(PIPEWIRE_PLUGIN_PATHS);
        let has_device = cpal::default_host().default_output_device().is_some()
            || cpal::default_host()
                .output_devices()
                .map(|mut devices| devices.next().is_some())
                .unwrap_or(false);
        Some(
            super::classify_alsa_failure(has_libasound, has_device, has_pipewire_plugin)
                .russian_message()
                .to_owned(),
        )
    }

    #[cfg(not(target_os = "linux"))]
    fn diagnose_alsa_failure_impl() -> Option<String> {
        None
    }

    /// Whether any of `paths` exists on the host filesystem.
    #[cfg(target_os = "linux")]
    fn any_path_exists(paths: &[&str]) -> bool {
        paths.iter().any(|path| std::path::Path::new(path).exists())
    }

    /// Opens the default microphone and feeds a capture pair (SPEC-020).
    ///
    /// Runs at `preferred_rate` when the device offers it, otherwise at the
    /// closest F32 rate; the returned control reports the rate in use. The
    /// ring holds `seconds` of audio between window drains.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::NoInputDevice`], [`RealtimeError::NoF32Config`], or a
    /// CPAL error while building/starting the stream.
    pub fn start_input(
        preferred_rate: u32,
        seconds: u32,
    ) -> Result<(CaptureControl, RealtimeHandle), RealtimeError> {
        start_input_with(preferred_rate, seconds, &AudioSettings::default())
    }

    /// [`start_input`] on the microphone and buffer chosen in `settings`
    /// (SPEC-033). A device that is gone falls back to the default.
    ///
    /// # Errors
    ///
    /// As [`start_input`].
    pub fn start_input_with(
        preferred_rate: u32,
        seconds: u32,
        settings: &AudioSettings,
    ) -> Result<(CaptureControl, RealtimeHandle), RealtimeError> {
        let device = pick_input(settings)?;
        let mut configs = Vec::new();
        for range in device.supported_input_configs()? {
            configs.push(DeviceConfig {
                min_sample_rate: range.min_sample_rate().0,
                max_sample_rate: range.max_sample_rate().0,
                channels: range.channels(),
                is_f32: range.sample_format() == SampleFormat::F32,
            });
        }
        let chosen = choose_config(&configs, preferred_rate)?;
        let rate = chosen.sample_rate;
        let supported = device
            .supported_input_configs()?
            .find(|r| {
                r.sample_format() == SampleFormat::F32
                    && r.channels() == chosen.channels
                    && r.min_sample_rate().0 <= rate
                    && rate <= r.max_sample_rate().0
            })
            .ok_or(RealtimeError::NoF32Config)?;
        let buffer = buffer_size(settings.buffer_frames, supported.buffer_size());
        let mut stream_config: StreamConfig =
            supported.with_sample_rate(cpal::SampleRate(rate)).into();
        stream_config.buffer_size = buffer;
        let channels = usize::from(stream_config.channels);
        let capacity =
            usize::try_from(u64::from(rate) * u64::from(seconds.max(1))).unwrap_or(usize::MAX);
        let (control, mut engine) = capture(capacity, rate);
        let stats = Arc::new(RealtimeStats::default());
        let cb_stats = Arc::clone(&stats);
        let err_stats = Arc::clone(&stats);
        let stream = device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                engine.push(data, channels);
                cb_stats
                    .rendered_frames
                    .fetch_add((data.len() / channels.max(1)) as u64, Ordering::Relaxed);
            },
            move |_err| {
                err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )?;
        stream.play()?;
        Ok((
            control,
            RealtimeHandle {
                stream: Some(stream),
                stats,
            },
        ))
    }
}

#[cfg(feature = "realtime")]
pub use device::{
    diagnose_alsa_failure, input_device_names, output_device_names, play_default, start_input,
    start_input_with, start_output, start_output_with,
};

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
    fn choose_prefers_stereo_over_a_mono_config_listed_first() {
        let cfgs = [f32_cfg(8_000, 192_000, 1), f32_cfg(8_000, 192_000, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!(chosen.channels, 2);
        let cfgs = [f32_cfg(48_000, 48_000, 1), f32_cfg(48_000, 48_000, 6)];
        assert_eq!(
            choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap().channels,
            6
        );
    }

    #[test]
    fn choose_still_puts_the_rate_before_stereo() {
        let cfgs = [f32_cfg(48_000, 48_000, 1), f32_cfg(44_100, 44_100, 2)];
        let chosen = choose_config(&cfgs, DEFAULT_SAMPLE_RATE).unwrap();
        assert_eq!((chosen.sample_rate, chosen.channels), (48_000, 1));
    }

    #[test]
    fn exact_config_accepts_a_rate_inside_a_range() {
        let cfgs = [f32_cfg(44_100, 96_000, 2)];
        let chosen = choose_exact_config(&cfgs, 44_100).unwrap();
        assert_eq!(chosen.sample_rate, 44_100);
    }

    #[test]
    fn exact_config_rejects_a_rate_the_device_cannot_play() {
        let cfgs = [f32_cfg(48_000, 48_000, 2)];
        match choose_exact_config(&cfgs, 44_100) {
            Err(RealtimeError::SampleRate { project, nearest }) => {
                assert_eq!((project, nearest), (44_100, 48_000));
            }
            other => panic!("expected SampleRate, got {other:?}"),
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

    #[test]
    fn classify_missing_libasound_wins_over_everything() {
        // No library: the most fundamental failure, regardless of device/plugin.
        assert_eq!(
            classify_alsa_failure(false, false, false),
            AlsaFailure::MissingLibasound
        );
        assert_eq!(
            classify_alsa_failure(false, true, true),
            AlsaFailure::MissingLibasound
        );
    }

    #[test]
    fn classify_no_device_reported_when_library_present() {
        assert_eq!(
            classify_alsa_failure(true, false, false),
            AlsaFailure::NoDevice
        );
        assert_eq!(
            classify_alsa_failure(true, false, true),
            AlsaFailure::NoDevice
        );
    }

    #[test]
    fn classify_missing_pipewire_plugin_when_device_present() {
        assert_eq!(
            classify_alsa_failure(true, true, false),
            AlsaFailure::MissingPipewirePlugin
        );
    }

    #[test]
    fn classify_other_when_all_present() {
        assert_eq!(classify_alsa_failure(true, true, true), AlsaFailure::Other);
    }

    #[test]
    fn classify_messages_cover_all_cases_and_are_static() {
        for case in [
            AlsaFailure::MissingLibasound,
            AlsaFailure::NoDevice,
            AlsaFailure::MissingPipewirePlugin,
            AlsaFailure::Other,
        ] {
            let msg = case.russian_message();
            assert!(!msg.is_empty());
        }
    }
}
