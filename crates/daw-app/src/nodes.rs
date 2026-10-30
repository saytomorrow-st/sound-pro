//! Demo nodes shipped with the shell.

use audio_core::graph::{NodeProcessor, NodeView, ProcessContext};
use audio_core::params::ParamHandle;
use audio_core::schedule::{Schedule, ScheduleCursor};
use audio_core::synth::{Envelope, Synth, SynthError};
use audio_core::wav::{sine, sine_into};

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
        let dst = output.channel_mut(0);
        // Generate straight into the output buffer: no temporary `Vec`, so
        // nothing allocates here (SPEC-011 §1).
        sine_into(
            self.freq_hz,
            ctx.sample_rate,
            ctx.position,
            &mut dst[..ctx.frames],
        );
        for slot in &mut dst[..ctx.frames] {
            *slot *= level;
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

/// Mono material shared with the window's cache, without a copy (SPEC-035).
///
/// Plays the first `frames` samples of `samples`.
#[derive(Debug, Clone)]
pub struct SharedClip {
    samples: std::sync::Arc<Vec<f32>>,
    frames: usize,
}

impl SharedClip {
    /// The first `frames` samples of `samples`; `None` when it is shorter.
    #[must_use]
    pub fn new(samples: std::sync::Arc<Vec<f32>>, frames: u64) -> Option<Self> {
        let frames = usize::try_from(frames).ok()?;
        (frames <= samples.len()).then_some(Self { samples, frames })
    }
}

impl SharedClip {
    /// All of `samples`.
    #[must_use]
    pub fn whole(samples: std::sync::Arc<Vec<f32>>) -> Self {
        let frames = samples.len();
        Self { samples, frames }
    }
}

impl ClipSource for SharedClip {
    fn frames(&self) -> u64 {
        self.frames as u64
    }

    fn channel(&self, ch: usize) -> &[f32] {
        if ch == 0 {
            &self.samples[..self.frames]
        } else {
            &[]
        }
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

/// Frames between two updates of automated effect parameters (SPEC-041).
pub const EFFECT_AUTOMATION_STEP: usize = 32;

/// A session track: its clips placed on the timeline, summed, gain and mute
/// applied (SPEC-010 §3), then panned and scaled by the master into two
/// channels (SPEC-013).
///
/// All the arithmetic is done on prepared slices: no allocation, no locking,
/// no panics. A clip whose `source_length` does not match its source simply
/// stops early instead of reading out of bounds.
pub struct Track {
    // The model's clips, kept as they are: a second copy of the fields would
    // let the node and the model drift apart silently (SPEC-011 §2).
    clips: Vec<audio_core::session::Clip>,
    sources: Vec<Box<dyn ClipSource + Send>>,
    gain: f32,
    mute: bool,
    left: f32,
    right: f32,
    // SPEC-022: the insert chain, built once; processing never allocates.
    effects: Vec<audio_core::EffectProcessor>,
    // SPEC-041: the chain's base parameters and a scratch copy that the
    // effect lanes write into, both sized at build time.
    kinds: Vec<audio_core::EffectKind>,
    scratch: Vec<audio_core::EffectKind>,
    // SPEC-023: lanes copied at build time; `master` is kept apart from
    // `left`/`right` so an automated pan can be re-scaled per sample.
    automation: audio_core::Automation,
    master: f32,
    // SPEC-042: pan gains without the master, for the post-fader meter.
    pan: (f32, f32),
    meter: Option<(crate::meters::Meters, usize)>,
}

impl Track {
    /// The highest clip covering `position` that lies below the clip at
    /// `above` (or below none), by [`Clip::is_above`]. Allocation-free.
    fn next_below(&self, position: u64, above: Option<usize>) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (index, clip) in self.clips.iter().enumerate() {
            if !clip.covers(position) {
                continue;
            }
            if let Some(top) = above {
                if !self.clips[top].is_above(top, clip, index) {
                    continue;
                }
            }
            if best.is_none_or(|b| clip.is_above(index, &self.clips[b], b)) {
                best = Some(index);
            }
        }
        best
    }

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
        let (left, right) = track.pan_gains();
        Ok(Self {
            clips: track.clips.clone(),
            sources,
            gain: track.gain,
            mute: track.mute,
            left,
            right,
            effects: Vec::new(),
            kinds: Vec::new(),
            scratch: Vec::new(),
            automation: audio_core::Automation::default(),
            master: 1.0,
            pan: (left, right),
            meter: None,
        })
    }

    /// Builds the track's insert chain (SPEC-022) for `sample_rate`. Bypassed
    /// effects become pass-throughs, an empty list leaves the sound untouched.
    #[must_use]
    pub fn with_effects(mut self, effects: &[audio_core::Effect], sample_rate: u32) -> Self {
        self.effects = effects
            .iter()
            .map(|effect| audio_core::EffectProcessor::new(effect, sample_rate))
            .collect();
        self.kinds = effects.iter().map(|effect| effect.kind).collect();
        self.scratch.clone_from(&self.kinds);
        self
    }

    /// Applies session-level mixing decided at build time: solo/mute
    /// audibility from [`audio_core::Session::is_audible`] and the master gain.
    #[must_use]
    pub fn with_mix(mut self, audible: bool, master_gain: f32) -> Self {
        self.mute = self.mute || !audible;
        self.left *= master_gain;
        self.right *= master_gain;
        self.master *= master_gain;
        self
    }

    /// Copies the track's lanes (SPEC-023/041). A non-empty lane replaces
    /// the matching slider or effect parameter; empty lanes leave the sound
    /// untouched.
    #[must_use]
    pub fn with_automation(mut self, automation: &audio_core::Automation) -> Self {
        self.automation = automation.clone();
        self.automation.prune();
        self
    }

    /// Reports this track's post-fader level, without the master, into
    /// `meters` as track `index` (SPEC-042). The sound is unchanged.
    #[must_use]
    pub fn with_meter(mut self, meters: &crate::meters::Meters, index: usize) -> Self {
        self.meter = Some((meters.clone(), index));
        self
    }

    /// Timeline length of the track in frames: the end of its last clip.
    #[must_use]
    pub fn length_samples(&self) -> u64 {
        self.clips
            .iter()
            .map(audio_core::session::Clip::end_samples)
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
            .field("left", &self.left)
            .field("right", &self.right)
            .field("effects", &self.effects.len())
            .field("gain_points", &self.automation.gain.len())
            .field("pan_points", &self.automation.pan.len())
            .field("length_samples", &self.length_samples())
            .finish()
    }
}

impl NodeProcessor for Track {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let frames = ctx.frames;
        if output.channels() > 1 {
            // A view addresses its channels with a stride of `frames`.
            let (left, right) = output.split_at_mut(frames);
            self.render(
                ctx.position,
                &mut left[..frames],
                Some(&mut right[..frames]),
            );
        } else {
            self.render(ctx.position, &mut output.channel_mut(0)[..frames], None);
        }
    }
}

/// One clip's sample at timeline `position`, with its gain but without its
/// fades (they are applied as overlap weights); the caller has checked that
/// the clip covers `position`.
fn clip_sample(
    clip: &audio_core::session::Clip,
    source: &(dyn ClipSource + Send),
    position: u64,
) -> f32 {
    if clip.gain == 0.0 {
        return 0.0;
    }
    let Ok(source_index) = usize::try_from(clip.source_index(position)) else {
        return 0.0;
    };
    // `source_length` is only a claim about the content: trust the slice,
    // not the model, so a mismatch is silence, not a panic.
    source
        .channel(0)
        .get(source_index)
        .map_or(0.0, |sample| sample * clip.gain)
}

impl Track {
    /// Renders one quantum into `left` (and `right` when stereo); both slices
    /// hold exactly the quantum. Allocation-free.
    fn render(&mut self, position: u64, left: &mut [f32], mut right: Option<&mut [f32]>) {
        if let Some(right) = right.as_deref_mut() {
            right.fill(0.0);
        }
        let dst = left;
        dst.fill(0.0);
        if self.mute || (self.automation.gain.is_empty() && self.gain == 0.0) {
            return;
        }
        let peaks = self.render_unmetered(position, dst, right);
        if let Some((meters, index)) = &self.meter {
            meters.record_track(*index, &[peaks.0], Some(&[peaks.1]));
        }
    }

    /// Renders an audible quantum and returns its post-fader peaks without
    /// the master (SPEC-042).
    fn render_unmetered(
        &mut self,
        position: u64,
        dst: &mut [f32],
        right: Option<&mut [f32]>,
    ) -> (f32, f32) {
        let frames = dst.len();
        // SPEC-044 §2: where clips overlap the later-starting one crossfades
        // over the earlier one (`Clip::layer_weight`, SPEC-045 §2: explicit
        // fades replace the automatic ramp, never multiply); the result does not
        // depend on the list order. Covering clips are walked from the one
        // above all others down; what a clip leaves uncovered shows the
        // clips below, so a fade over another clip still reveals it.
        for (index, slot) in dst.iter_mut().enumerate() {
            let position = position.saturating_add(index as u64);
            let mut current = self.next_below(position, None);
            let mut above: Option<usize> = None;
            let mut sum = 0.0_f32;
            let mut remaining = 1.0_f32;
            while let Some(at) = current {
                let clip = &self.clips[at];
                let below = self.next_below(position, Some(at));
                let weight = clip.layer_weight(
                    above.map(|a| &self.clips[a]),
                    below.map(|b| &self.clips[b]),
                    position,
                );
                if weight > 0.0 {
                    sum +=
                        remaining * weight * clip_sample(clip, self.sources[at].as_ref(), position);
                }
                remaining *= 1.0 - weight;
                if remaining <= 0.0 {
                    break;
                }
                above = Some(at);
                current = below;
            }
            *slot = sum;
        }
        if self.automation.effects.is_empty() {
            for effect in &mut self.effects {
                effect.process(dst);
            }
        } else {
            self.process_automated_effects(position, dst);
        }
        if !(self.automation.gain.is_empty() && self.automation.pan.is_empty()) {
            return self.apply_automation(position, dst, right);
        }
        let mono = if self.meter.is_some() {
            dst.iter()
                .map(|sample| sample.abs())
                .filter(|level| level.is_finite())
                .fold(0.0, f32::max)
        } else {
            0.0
        };
        if let Some(right) = right {
            let gain = self.gain * self.right;
            for (out, mono) in right.iter_mut().zip(dst.iter()) {
                *out = mono * gain;
            }
        }
        let gain = self.gain * self.left;
        for slot in &mut dst[..frames] {
            *slot *= gain;
        }
        let level = mono * self.gain.abs();
        (level * self.pan.0, level * self.pan.1)
    }

    /// Runs the insert chain in steps of [`EFFECT_AUTOMATION_STEP`] frames,
    /// setting every automated parameter at the start of each step
    /// (SPEC-041); allocation-free.
    fn process_automated_effects(&mut self, position: u64, dst: &mut [f32]) {
        use audio_core::Automation;
        // Steps sit on a grid of absolute frames, so the sound does not
        // depend on the block size.
        let mut start = position;
        let mut rest = dst;
        while !rest.is_empty() {
            let into = usize::try_from(start % EFFECT_AUTOMATION_STEP as u64).unwrap_or(0);
            let len = (EFFECT_AUTOMATION_STEP - into).min(rest.len());
            let (chunk, tail) = rest.split_at_mut(len);
            rest = tail;
            self.scratch.copy_from_slice(&self.kinds);
            for lane in &self.automation.effects {
                let (Some(kind), Some(value)) = (
                    self.scratch.get_mut(usize::from(lane.slot)),
                    Automation::value_at(&lane.points, start - into as u64),
                ) else {
                    continue;
                };
                lane.param.set(kind, value);
            }
            for (effect, kind) in self.effects.iter_mut().zip(&self.scratch) {
                effect.update(kind);
                effect.process(chunk);
            }
            start = start.saturating_add(len as u64);
        }
    }

    /// Per-sample gain and pan from the lanes (SPEC-023); allocation-free.
    /// Returns the post-fader peaks without the master (SPEC-042).
    fn apply_automation(
        &self,
        position: u64,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
    ) -> (f32, f32) {
        use audio_core::Automation;
        let mut peaks = (0.0_f32, 0.0_f32);
        for (index, slot) in left.iter_mut().enumerate() {
            let time = position.saturating_add(index as u64);
            let gain = Automation::value_at(&self.automation.gain, time).unwrap_or(self.gain);
            let ((l, r), (pl, pr)) = match Automation::value_at(&self.automation.pan, time) {
                Some(pan) => {
                    let (l, r) = audio_core::pan_law(pan);
                    ((l * self.master, r * self.master), (l, r))
                }
                None => ((self.left, self.right), self.pan),
            };
            let mono = *slot;
            if let Some(right) = right.as_deref_mut() {
                right[index] = mono * (gain * r);
            }
            *slot = mono * (gain * l);
            let level = (mono * gain).abs();
            if level.is_finite() {
                peaks = (peaks.0.max(level * pl), peaks.1.max(level * pr));
            }
        }
        peaks
    }
}

/// One bus at render time: summed sends, its effects per channel, its gain
/// (SPEC-028).
struct BusChannel {
    gain: f32,
    effects: Vec<(audio_core::EffectProcessor, audio_core::EffectProcessor)>,
    left: Vec<f32>,
    right: Vec<f32>,
}

/// The whole mix of a session that has buses (SPEC-028): every track, its
/// post-fader sends, the buses' effects and the master gain, in one node.
///
/// Tracks are built without the master; it is applied once to the sum of
/// the dry tracks and the bus returns. All buffers are reserved for
/// `max_frames` at build time; `process` never allocates.
pub struct Mixer {
    tracks: Vec<(Track, Vec<audio_core::BusSend>)>,
    buses: Vec<BusChannel>,
    master: f32,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Mixer {
    /// Builds the mixer for `buses`; `tracks` pairs each track node (made
    /// with a master gain of `1.0`) with that track's sends.
    #[must_use]
    pub fn new(
        tracks: Vec<(Track, Vec<audio_core::BusSend>)>,
        buses: &[audio_core::Bus],
        master: f32,
        sample_rate: u32,
        max_frames: usize,
    ) -> Self {
        let buses = buses
            .iter()
            .map(|bus| BusChannel {
                gain: bus.gain,
                effects: bus
                    .effects
                    .iter()
                    .map(|effect| {
                        (
                            audio_core::EffectProcessor::new(effect, sample_rate),
                            audio_core::EffectProcessor::new(effect, sample_rate),
                        )
                    })
                    .collect(),
                left: vec![0.0; max_frames],
                right: vec![0.0; max_frames],
            })
            .collect();
        Self {
            tracks,
            buses,
            master,
            left: vec![0.0; max_frames],
            right: vec![0.0; max_frames],
        }
    }

    /// Timeline length: the end of the longest track.
    #[must_use]
    pub fn length_samples(&self) -> u64 {
        self.tracks
            .iter()
            .map(|(track, _)| track.length_samples())
            .max()
            .unwrap_or(0)
    }
}

impl std::fmt::Debug for Mixer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mixer")
            .field("tracks", &self.tracks.len())
            .field("buses", &self.buses.len())
            .field("master", &self.master)
            .finish_non_exhaustive()
    }
}

impl NodeProcessor for Mixer {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        // The graph never hands over more than its reserved quantum; if it
        // somehow did, the excess stays silent instead of panicking.
        let frames = ctx.frames.min(self.left.len());
        let stereo = output.channels() > 1;
        let (out_left, out_right) = output.split_at_mut(ctx.frames);
        let out_left = &mut out_left[..frames];
        let out_right = &mut out_right[..if stereo { frames } else { 0 }];
        out_left.fill(0.0);
        out_right.fill(0.0);
        for bus in &mut self.buses {
            bus.left[..frames].fill(0.0);
            bus.right[..frames].fill(0.0);
        }
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        for (track, sends) in &mut self.tracks {
            track.render(ctx.position, left, Some(right));
            for (dst, src) in out_left.iter_mut().zip(left.iter()) {
                *dst += src;
            }
            if stereo {
                for (dst, src) in out_right.iter_mut().zip(right.iter()) {
                    *dst += src;
                }
            } else {
                for (dst, src) in out_left.iter_mut().zip(right.iter()) {
                    *dst += src;
                }
            }
            for send in sends.iter() {
                let Some(bus) = self.buses.get_mut(send.bus) else {
                    continue;
                };
                if send.level == 0.0 {
                    continue;
                }
                for (dst, src) in bus.left[..frames].iter_mut().zip(left.iter()) {
                    *dst += src * send.level;
                }
                for (dst, src) in bus.right[..frames].iter_mut().zip(right.iter()) {
                    *dst += src * send.level;
                }
            }
        }
        for bus in &mut self.buses {
            let (bus_left, bus_right) = (&mut bus.left[..frames], &mut bus.right[..frames]);
            for (left_fx, right_fx) in &mut bus.effects {
                left_fx.process(bus_left);
                right_fx.process(bus_right);
            }
            for (dst, src) in out_left.iter_mut().zip(bus_left.iter()) {
                *dst += src * bus.gain;
            }
            if stereo {
                for (dst, src) in out_right.iter_mut().zip(bus_right.iter()) {
                    *dst += src * bus.gain;
                }
            } else {
                for (dst, src) in out_left.iter_mut().zip(bus_right.iter()) {
                    *dst += src * bus.gain;
                }
            }
        }
        for slot in out_left.iter_mut().chain(out_right.iter_mut()) {
            *slot *= self.master;
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
