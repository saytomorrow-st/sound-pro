//! Demo nodes shipped with the shell.

use audio_core::graph::{NodeProcessor, NodeView, ProcessContext};
use audio_core::params::ParamHandle;
use audio_core::schedule::{Schedule, ScheduleCursor};
use audio_core::synth::{Envelope, Synth, SynthError};
use audio_core::wav::sine;

/// Steady sine whose amplitude is driven by a live parameter.
///
/// The node owns a [`ParamHandle`]; the UI moves the same parameter through
/// the graph without touching the audio thread (SPEC-006).
pub struct Tone {
    /// Frequency in Hz.
    pub freq_hz: f64,
    /// Linear amplitude, 0.0..=1.0, smoothed by the parameter spec.
    pub level: ParamHandle,
}

impl Tone {
    /// Builds a sine at `freq_hz` whose amplitude follows `level`.
    #[must_use]
    pub fn new(freq_hz: f64, level: ParamHandle) -> Self {
        Self { freq_hz, level }
    }
}

impl std::fmt::Debug for Tone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tone")
            .field("freq_hz", &self.freq_hz)
            .field("level", &self.level.spec().name)
            .finish()
    }
}

impl NodeProcessor for Tone {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let level = self.level.next(ctx.frames, ctx.sample_rate);
        let block = sine(self.freq_hz, ctx.sample_rate, ctx.position, ctx.frames);
        let dst = output.channel_mut(0);
        for (i, sample) in block.iter().enumerate() {
            dst[i] = sample * level;
        }
    }
}

/// Event source that walks a [`Schedule`] quantum by quantum (SPEC-007).
///
/// The node makes no sound: it only proves the scheduler works by counting the
/// events it encounters into `events_seen`. A synthesizer will later replace
/// the counter with voice allocation.
pub struct MidiPlayer {
    schedule: Schedule,
    cursor: ScheduleCursor,
    last_position: u64,
    events_seen: ParamHandle,
}

impl MidiPlayer {
    /// Builds a player over `schedule` that reports into `events_seen`.
    ///
    /// `events_seen` should be an unsmoothed parameter: the counter is a
    /// diagnostic, not something to ramp.
    #[must_use]
    pub fn new(schedule: Schedule, events_seen: ParamHandle) -> Self {
        Self {
            cursor: schedule.cursor(),
            schedule,
            last_position: 0,
            events_seen,
        }
    }

    /// The schedule being played.
    #[must_use]
    pub const fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Number of events the node has consumed so far.
    #[must_use]
    pub fn events_seen(&self) -> f32 {
        self.events_seen.value()
    }
}

impl std::fmt::Debug for MidiPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiPlayer")
            .field("events", &self.schedule.len())
            .field("length_samples", &self.schedule.length_samples())
            .finish()
    }
}

impl NodeProcessor for MidiPlayer {
    fn process(&mut self, ctx: &ProcessContext<'_>, _output: &mut NodeView<'_>) {
        if ctx.position < self.last_position {
            // The transport jumped backwards: the cursor is already past the
            // events before the new position and cannot rewind (SPEC-007 §6).
            self.cursor = self.schedule.cursor();
        }
        self.last_position = ctx.position;
        let mut seen = self.events_seen.value();
        while let Some(_event) =
            self.schedule
                .next_event(&mut self.cursor, ctx.position, ctx.frames)
        {
            seen += 1.0;
        }
        self.events_seen.set(seen);
    }
}

/// Polyphonic instrument: turns a [`Schedule`] into summed voices (SPEC-009).
///
/// `Synth` renders dry; the node owns the live `synth.gain` parameter and
/// applies it to the block, so the UI can mute or ride the instrument without
/// touching the audio thread.
pub struct PolySynth {
    synth: Synth,
    schedule: Schedule,
    cursor: ScheduleCursor,
    last_position: u64,
    gain: ParamHandle,
    scratch: Vec<f32>,
}

impl PolySynth {
    /// Builds an instrument for `sample_rate` playing `schedule`.
    ///
    /// # Errors
    ///
    /// Returns [`SynthError`] for a zero sample rate or a bad envelope.
    pub fn new(
        schedule: Schedule,
        sample_rate: u32,
        envelope: Envelope,
        gain: ParamHandle,
    ) -> Result<Self, SynthError> {
        Ok(Self {
            synth: Synth::new(sample_rate, envelope)?,
            cursor: schedule.cursor(),
            schedule,
            last_position: 0,
            gain,
            // Allocated once: the render path only slices it.
            scratch: Vec::new(),
        })
    }

    /// The schedule being played.
    #[must_use]
    pub const fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// The instrument, for diagnostics.
    #[must_use]
    pub const fn synth(&self) -> &Synth {
        &self.synth
    }

    /// Voices started since the last backward seek.
    #[must_use]
    pub const fn voices_started(&self) -> u32 {
        self.synth.voices_started()
    }
}

impl std::fmt::Debug for PolySynth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolySynth")
            .field("events", &self.schedule.len())
            .field("length_samples", &self.schedule.length_samples())
            .field("voices_started", &self.synth.voices_started())
            .finish()
    }
}

impl NodeProcessor for PolySynth {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        if ctx.position < self.last_position {
            // Backward seek: the notes that started before the new position
            // would hang forever, and the cursor cannot rewind (SPEC-009 §10).
            self.cursor = self.schedule.cursor();
            self.synth.reset();
        }
        self.last_position = ctx.position;

        if self.scratch.len() < ctx.frames {
            // Off the hot path in practice: the graph is built for the
            // largest quantum it will see, so this grows once at most.
            self.scratch.resize(ctx.frames, 0.0);
        }
        self.synth.render(
            &self.schedule,
            &mut self.cursor,
            ctx.position,
            ctx.frames,
            &mut self.scratch[..ctx.frames],
        );

        let gain = self.gain.next(ctx.frames, ctx.sample_rate);
        let dst = output.channel_mut(0);
        for (i, sample) in self.scratch[..ctx.frames].iter().enumerate() {
            dst[i] = sample * gain;
        }
    }
}

/// PCM content a clip plays from (SPEC-010).
///
/// Read-only by interface, so a node can never damage the material it visits,
/// and cheap to ask about: both methods are called on the audio thread.
pub trait ClipSource {
    /// Frames of content available, i.e. the clip's `source_length`.
    fn frames(&self) -> u64;
    /// Mono content for channel `ch`; empty if the channel does not exist.
    fn channel(&self, ch: usize) -> &[f32];
}

/// Mono material for a clip, already at the session's sample rate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonoClip {
    samples: Vec<f32>,
}

impl MonoClip {
    /// Takes ownership of `samples` as the clip's content.
    #[must_use]
    pub fn new(samples: Vec<f32>) -> Self {
        Self { samples }
    }

    /// A `freq_hz` sine of `frames` frames, generated off the audio thread.
    #[must_use]
    pub fn sine(freq_hz: f64, sample_rate: u32, frames: u64) -> Self {
        let frames = usize::try_from(frames).unwrap_or(usize::MAX);
        Self {
            samples: sine(freq_hz, sample_rate, 0, frames),
        }
    }

    /// The raw samples.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

impl ClipSource for MonoClip {
    fn frames(&self) -> u64 {
        self.samples.len() as u64
    }

    fn channel(&self, ch: usize) -> &[f32] {
        if ch == 0 { &self.samples } else { &[] }
    }
}

impl ClipSource for Vec<f32> {
    fn frames(&self) -> u64 {
        self.len() as u64
    }

    fn channel(&self, ch: usize) -> &[f32] {
        if ch == 0 { self } else { &[] }
    }
}

/// One clip as the mixer sees it: timeline window, content window, gain.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlacedClip {
    start: u64,
    length: u64,
    offset: u64,
    gain: f32,
}

impl PlacedClip {
    fn new(clip: &audio_core::session::Clip) -> Self {
        Self {
            start: clip.start_samples,
            length: clip.length_samples,
            offset: clip.offset_samples,
            gain: clip.gain,
        }
    }

    /// The content index for timeline position `position`, if it sounds.
    fn source_index(&self, position: u64) -> Option<u64> {
        let within = position.checked_sub(self.start)?;
        (within < self.length).then(|| self.offset + within)
    }
}

/// A session track: its clips placed on the timeline, summed, gain and mute
/// applied (SPEC-010 §3).
///
/// All the arithmetic is done on prepared slices: no allocation, no locking,
/// no panics. A clip whose `source_length` does not match its source simply
/// stops early instead of reading out of bounds.
pub struct Track {
    clips: Vec<PlacedClip>,
    sources: Vec<Box<dyn ClipSource + Send>>,
    gain: f32,
    mute: bool,
}

impl Track {
    /// Pairs each clip with the source holding its content, in the same order.
    ///
    /// # Errors
    ///
    /// Returns [`daw_app::SessionError::ClipCountMismatch`] when the lists
    /// differ in length.
    pub fn new(
        track: &audio_core::session::Track,
        sources: Vec<Box<dyn ClipSource + Send>>,
    ) -> Result<Self, crate::SessionError> {
        if track.clips.len() != sources.len() {
            return Err(crate::SessionError::ClipCountMismatch {
                track: track.name.clone(),
                clips: track.clips.len(),
                sources: sources.len(),
            });
        }
        Ok(Self {
            clips: track.clips.iter().map(PlacedClip::new).collect(),
            sources,
            gain: track.gain,
            mute: track.mute,
        })
    }

    /// Timeline length of the track in frames: the end of its last clip.
    #[must_use]
    pub fn length_samples(&self) -> u64 {
        self.clips
            .iter()
            .map(|clip| clip.start.saturating_add(clip.length))
            .max()
            .unwrap_or(0)
    }
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Track")
            .field("clips", &self.clips.len())
            .field("gain", &self.gain)
            .field("mute", &self.mute)
            .field("length_samples", &self.length_samples())
            .finish()
    }
}

impl NodeProcessor for Track {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let dst = output.channel_mut(0);
        dst[..ctx.frames].fill(0.0);
        if self.mute || self.gain == 0.0 {
            return;
        }
        let track_gain = self.gain;
        for (clip, source) in self.clips.iter().zip(&self.sources) {
            if clip.gain == 0.0 || clip.length == 0 {
                continue;
            }
            let content = source.channel(0);
            for (index, slot) in dst[..ctx.frames].iter_mut().enumerate() {
                let position = ctx.position + index as u64;
                let Some(source_index) = clip.source_index(position) else {
                    continue;
                };
                let Ok(source_index) = usize::try_from(source_index) else {
                    continue;
                };
                if let Some(sample) = content.get(source_index) {
                    *slot += sample * clip.gain;
                }
            }
        }
        for slot in &mut dst[..ctx.frames] {
            *slot *= track_gain;
        }
    }
}

/// One loaded source with a known timeline offset (SPEC-010 §3).
///
/// The minimal vertical slice used to check summation in a graph: no clip
/// windows, no gain staging, just "this material starts here".
pub struct Strip {
    source: Box<dyn ClipSource + Send>,
    offset: u64,
    gain: f32,
}

impl Strip {
    /// Places `source` at `offset` frames on the timeline, scaled by `gain`.
    #[must_use]
    pub fn new(source: Box<dyn ClipSource + Send>, offset: u64, gain: f32) -> Self {
        Self {
            source,
            offset,
            gain,
        }
    }
}

impl std::fmt::Debug for Strip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Strip")
            .field("frames", &self.source.frames())
            .field("offset", &self.offset)
            .field("gain", &self.gain)
            .finish()
    }
}

impl NodeProcessor for Strip {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let content = self.source.channel(0);
        let dst = output.channel_mut(0);
        for (index, slot) in dst[..ctx.frames].iter_mut().enumerate() {
            let position = ctx.position + index as u64;
            let sample = position
                .checked_sub(self.offset)
                .and_then(|within| usize::try_from(within).ok())
                .and_then(|within| content.get(within).copied());
            *slot = sample.map_or(0.0, |sample| sample * self.gain);
        }
    }
}
