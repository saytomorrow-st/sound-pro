//! Demo nodes shipped with the shell.

use audio_core::graph::{NodeProcessor, NodeView, ProcessContext};
use audio_core::params::ParamHandle;
use audio_core::schedule::{Schedule, ScheduleCursor};
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
