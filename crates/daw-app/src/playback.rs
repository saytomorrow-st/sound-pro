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

use std::f64::consts::TAU;

use audio_core::AudioBuffer;
use audio_core::Graph;
use audio_core::TempoMap;
use audio_core::{CommandConsumer, CommandQueue};
use audio_core::{Renderer, TPQN, Transport, TransportCommand};

use rtrb::{Consumer, Producer, RingBuffer};

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
    /// Commands the window has queued so far. The audio thread publishes
    /// `playing` only once it has applied all of them, so a stale quantum
    /// never overwrites a fresh `play`/`stop` (SPEC-035).
    sent: AtomicU64,
    /// Cycle region packed as `start << 32 | end`; `end <= start` is off
    /// (SPEC-025). One atomic, so a region is never read half-updated.
    cycle: AtomicU64,
    /// Metronome switch (SPEC-026).
    metronome: AtomicBool,
    /// Tempo (`f32` bits) and beats per bar packed as `tempo << 32 | beats`.
    meter: AtomicU64,
}

/// Tempo and beats per bar a new pair starts with.
const DEFAULT_METER: (f32, u32) = (120.0, 4);
/// Click length in seconds.
const CLICK_SECONDS: f64 = 0.03;
/// Frequency and amplitude of the first beat of a bar and of the others.
const ACCENT: (f64, f64) = (1_500.0, 0.5);
const NORMAL: (f64, f64) = (1_000.0, 0.3);

fn pack_meter(tempo: f32, beats: u32) -> u64 {
    u64::from(tempo.to_bits()) << 32 | u64::from(beats)
}

fn unpack_meter(packed: u64) -> (f64, u64) {
    let tempo = f32::from_bits((packed >> 32) as u32);
    (f64::from(tempo), packed & 0xFFFF_FFFF)
}

fn pack_cycle(start: u64, end: u64) -> u64 {
    let clamp = |v: u64| v.min(u64::from(u32::MAX));
    clamp(start) << 32 | clamp(end)
}

const fn unpack_cycle(packed: u64) -> Option<(u64, u64)> {
    let (start, end) = (packed >> 32, packed & 0xFFFF_FFFF);
    if start < end {
        Some((start, end))
    } else {
        None
    }
}

/// The window's side: sends transport commands and reads the position.
pub struct PlaybackControl {
    commands: CommandQueue,
    shared: Arc<Shared>,
    sample_rate: u32,
    end: u64,
    /// New graphs for the audio thread and the replaced ones coming back,
    /// so they are freed here, never on the audio thread (SPEC-035).
    graphs: Producer<Graph>,
    retired: Consumer<Graph>,
    /// New tempo maps for the audio thread's metronome (SPEC-054). Swapped
    /// through a single-slot ring, like graphs, so nothing allocates or
    /// locks on the audio path.
    tempo_maps: Producer<TempoMap>,
    /// Replaced tempo maps handed back from the audio thread, so they are
    /// freed here, never on the audio thread (SPEC-035).
    retired_tempo: Consumer<TempoMap>,
    channels: usize,
    block: usize,
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
    ///
    /// [`Self::is_playing`] turns true at once, before the audio thread
    /// picks the command up (SPEC-035).
    pub fn play(&mut self) -> bool {
        let sent = self.send(TransportCommand::Play);
        if sent {
            self.shared.playing.store(true, Ordering::Relaxed);
        }
        sent
    }

    /// Stops and keeps the position. `false` when the queue is full.
    pub fn stop(&mut self) -> bool {
        let sent = self.send(TransportCommand::Stop);
        if sent {
            self.shared.playing.store(false, Ordering::Relaxed);
        }
        sent
    }

    /// Moves to `frames`, clamped to the end. `false` when the queue is full.
    pub fn seek(&mut self, frames: u64) -> bool {
        self.send(TransportCommand::SeekSamples(frames.min(self.end)))
    }

    /// Sets the cycle region `[start, end)` in frames, or turns it off
    /// with `None` (SPEC-025). `end` is clamped to [`Self::end`]; an empty
    /// region turns the cycle off.
    pub fn set_cycle(&mut self, region: Option<(u64, u64)>) {
        let packed = region.map_or(0, |(start, end)| pack_cycle(start, end.min(self.end)));
        let packed = if unpack_cycle(packed).is_some() {
            packed
        } else {
            0
        };
        self.shared.cycle.store(packed, Ordering::Relaxed);
    }

    /// The active cycle region after clamping, if any.
    #[must_use]
    pub fn cycle(&self) -> Option<(u64, u64)> {
        unpack_cycle(self.shared.cycle.load(Ordering::Relaxed))
    }

    /// Turns the metronome click on or off (SPEC-026). It sounds only while
    /// playing and is mixed into every channel.
    pub fn set_metronome(&mut self, on: bool) {
        self.shared.metronome.store(on, Ordering::Relaxed);
    }

    /// Whether the metronome is on.
    #[must_use]
    pub fn metronome(&self) -> bool {
        self.shared.metronome.load(Ordering::Relaxed)
    }

    /// Sets the click tempo in BPM and the beats per bar. A tempo that is
    /// not finite and positive, or zero beats, is ignored.
    ///
    /// This also replaces the metronome's tempo map with a single-point map
    /// at `tempo_bpm`, so the map stays the single source of beat placement
    /// (SPEC-054). A multi-point map can then be pushed with
    /// [`Self::set_tempo_map`] to override it.
    pub fn set_meter(&mut self, tempo_bpm: f64, beats_per_bar: u32) {
        let tempo = tempo_bpm as f32;
        if !tempo.is_finite() || tempo <= 0.0 || beats_per_bar == 0 {
            return;
        }
        self.shared
            .meter
            .store(pack_meter(tempo, beats_per_bar), Ordering::Relaxed);
        if let Ok(map) = TempoMap::single(f64::from(tempo), self.sample_rate) {
            self.set_tempo_map(map);
        }
    }

    /// Replaces the tempo map the metronome uses for beat placement
    /// (SPEC-054). The map is precomputed off the audio path; here it is
    /// moved through a single-slot ring, so the audio thread never allocates
    /// or blocks. When a map is already waiting it is dropped (the caller
    /// may push again on the next frame). A map with a single point is the
    /// same as the single-tempo `set_meter` path, so it is still accepted.
    pub fn set_tempo_map(&mut self, map: TempoMap) {
        let _ = self.tempo_maps.push(map);
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

    /// Replaces the sound being played without stopping, keeping the
    /// position and transport state (SPEC-035): edits are heard at once.
    ///
    /// Returns the graph back when it cannot be used: a different channel
    /// count, a smaller maximum quantum, or a swap already waiting.
    ///
    /// # Errors
    ///
    /// The rejected graph, unchanged.
    pub fn replace_graph(&mut self, graph: Graph) -> Result<(), Graph> {
        self.collect();
        if graph.channels() != self.channels || graph.max_quantum() < self.block {
            return Err(graph);
        }
        self.graphs
            .push(graph)
            .map_err(|rtrb::PushError::Full(g)| g)
    }

    /// Frees graphs the audio thread has swapped out.
    pub fn collect(&mut self) {
        while let Ok(graph) = self.retired.pop() {
            drop(graph);
        }
        while let Ok(map) = self.retired_tempo.pop() {
            drop(map);
        }
    }

    fn send(&mut self, command: TransportCommand) -> bool {
        let sent = self.commands.try_push(command).is_ok();
        if sent {
            self.shared.sent.fetch_add(1, Ordering::AcqRel);
        }
        sent
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
    sample_rate: u32,
    click: Click,
    applied: u64,
    graphs: Consumer<Graph>,
    retired: Producer<Graph>,
    tempo_maps: Consumer<TempoMap>,
    retired_tempo: Producer<TempoMap>,
    /// Current tempo map for metronome beat placement (SPEC-054).
    tempo_map: TempoMap,
    meters: Option<crate::meters::Meters>,
}

/// The click being played: frames since its start and its kind.
#[derive(Debug, Default, Clone, Copy)]
struct Click {
    elapsed: usize,
    left: usize,
    accent: bool,
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
    let channels = renderer.channels();
    let (graphs, incoming) = RingBuffer::new(1);
    let (outgoing, retired) = RingBuffer::new(2);
    let (tempo_maps, incoming_tempo) = RingBuffer::new(1);
    let (outgoing_tempo, retired_tempo) = RingBuffer::new(2);
    let initial_map = TempoMap::single(f64::from(DEFAULT_METER.0), sample_rate)
        .expect("default tempo map is valid");
    shared.position.store(start, Ordering::Relaxed);
    shared.meter.store(
        pack_meter(DEFAULT_METER.0, DEFAULT_METER.1),
        Ordering::Relaxed,
    );
    (
        PlaybackControl {
            commands,
            shared: Arc::clone(&shared),
            sample_rate,
            end,
            graphs,
            retired,
            tempo_maps,
            retired_tempo,
            channels,
            block,
        },
        PlaybackEngine {
            renderer,
            commands: consumer,
            block,
            scratch,
            shared,
            end,
            sample_rate,
            click: Click::default(),
            applied: 0,
            graphs: incoming,
            retired: outgoing,
            tempo_maps: incoming_tempo,
            retired_tempo: outgoing_tempo,
            tempo_map: initial_map,
            meters: None,
        },
    )
}

impl PlaybackEngine {
    /// Reports every rendered quantum's master level, before the metronome
    /// click, into `meters` (SPEC-042).
    #[must_use]
    pub fn with_meters(mut self, meters: crate::meters::Meters) -> Self {
        self.meters = Some(meters);
        self
    }

    /// Fills an interleaved device buffer of `channels` channels.
    ///
    /// Realtime-safe: no allocation, locking, I/O or panics. Reaching the end
    /// stops the transport; the rest of `out` is silence. Inside an active
    /// cycle region the position jumps from its end back to its start
    /// without a gap (SPEC-025).
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let cycle = unpack_cycle(self.shared.cycle.load(Ordering::Relaxed));
        let metronome = self.shared.metronome.load(Ordering::Relaxed);
        let meter = unpack_meter(self.shared.meter.load(Ordering::Relaxed));
        self.swap_graph();
        self.swap_tempo_map();
        let frames = out.len() / channels;
        let mut offset = 0usize;
        while offset < frames {
            let mut n = self.block.min(frames - offset);
            // Commands are applied here, not inside the renderer, so the end
            // check below always sees the transport state of this quantum.
            while let Some(command) = self.commands.pop_next() {
                self.renderer.transport_mut().apply(command);
                self.applied += 1;
            }
            let transport = self.renderer.transport();
            let mut wrap = None;
            let playing = transport.is_playing();
            let from = transport.position();
            if playing {
                let position = transport.position();
                if let Some((start, end)) = cycle
                    && position < end
                {
                    let left = usize::try_from(end - position).unwrap_or(usize::MAX);
                    if left <= n {
                        n = left;
                        wrap = Some(start);
                    }
                }
                let left = self.end.saturating_sub(position);
                n = n.min(usize::try_from(left).unwrap_or(usize::MAX));
                if n == 0 {
                    self.renderer.transport_mut().apply(TransportCommand::Stop);
                    continue;
                }
            }
            if self.renderer.render(n, &mut self.scratch).is_err() {
                break;
            }
            if let Some(start) = wrap {
                self.renderer
                    .transport_mut()
                    .apply(TransportCommand::SeekSamples(start));
            }
            if let Some(meters) = &self.meters {
                let left = &self.scratch.channel(0)[..n];
                let right = if self.scratch.channels() > 1 {
                    &self.scratch.channel(1)[..n]
                } else {
                    left
                };
                meters.record_master(left, right);
            }
            let dst = &mut out[offset * channels..(offset + n) * channels];
            interleave_into(&self.scratch, n, channels, dst);
            if playing && metronome {
                self.add_click(dst, channels, from, meter);
            } else {
                self.click = Click::default();
            }
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
        if self.applied >= self.shared.sent.load(Ordering::Acquire) {
            self.shared
                .playing
                .store(transport.is_playing(), Ordering::Relaxed);
        }
    }

    /// Takes a waiting graph and hands the old one back. Moves only; when
    /// the return ring is full the swap waits for the next buffer.
    fn swap_graph(&mut self) {
        if self.retired.is_full() {
            return;
        }
        if let Ok(mut graph) = self.graphs.pop() {
            std::mem::swap(self.renderer.graph_mut(), &mut graph);
            let _ = self.retired.push(graph);
        }
    }

    /// Takes a waiting tempo map for the metronome (SPEC-054). The replaced
    /// map is handed back through a ring so it is freed on the control
    /// thread, never on the audio thread; when the return ring is full the
    /// swap waits for the next buffer.
    fn swap_tempo_map(&mut self) {
        if self.retired_tempo.is_full() {
            return;
        }
        if let Ok(mut map) = self.tempo_maps.pop() {
            std::mem::swap(&mut self.tempo_map, &mut map);
            let _ = self.retired_tempo.push(map);
        }
    }

    /// Mixes the metronome into `dst`, whose first frame is at `from`.
    /// Beat `k` starts on frame `map.beat_to_samples_ceil(k)` (SPEC-054),
    /// preserving the ceil beat placement of the pre-map metronome so a lone
    /// tempo point stays bit-identical (SPEC-026).
    fn add_click(&mut self, dst: &mut [f32], channels: usize, from: u64, meter: (f64, u64)) {
        let (_tempo, beats) = meter;
        let map = &self.tempo_map;
        let rate = f64::from(self.sample_rate);
        let length = (rate * CLICK_SECONDS).round() as usize;
        let starts = |k: u64| map.beat_to_samples_ceil(k);
        // First beat starting at or after `from`.
        let mut k = map.samples_to_ticks(from) / u64::from(TPQN);
        while k > 0 && starts(k - 1) >= from {
            k -= 1;
        }
        while starts(k) < from {
            k += 1;
        }
        let mut next = starts(k);
        for (i, frame) in dst.chunks_exact_mut(channels).enumerate() {
            if from + i as u64 == next {
                self.click = Click {
                    elapsed: 0,
                    left: length,
                    accent: k % beats == 0,
                };
                k += 1;
                next = starts(k);
            }
            if self.click.left == 0 {
                continue;
            }
            let (freq, amp) = if self.click.accent { ACCENT } else { NORMAL };
            let t = self.click.elapsed as f64;
            let fade = 1.0 - t / length as f64;
            let sample = (amp * fade * (TAU * freq * t / rate).cos()) as f32;
            for s in frame.iter_mut() {
                *s += sample;
            }
            self.click.elapsed += 1;
            self.click.left -= 1;
        }
    }
}
