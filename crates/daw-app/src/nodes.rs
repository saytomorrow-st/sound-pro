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
