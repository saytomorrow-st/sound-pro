//! The [`Instrument`] trait: offline pre-render of a MIDI clip's notes to a
//! mono buffer (SPEC-064 / RFC-001 §4).
//!
//! An instrument turns a set of [`Note`]s into PCM on the control thread. Two
//! implementations exist: [`PianoInstrument`], the built-in Salamander piano
//! (the existing fallback, kept bit-identical), and [`SamplerInstrument`], a
//! loaded SFZ bank rendered through [`audio_core::SamplerEngine`].

use std::sync::Arc;

use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};
use audio_core::synth::SynthError;
use audio_core::{Interpolation, MidiEvent, MidiMessage, Note, SampleRegion, SamplerEngine};

use crate::sample_bank::LoadedBank;

/// Renders a set of MIDI notes to a mono buffer, offline.
///
/// Implementations run on the control thread; the audio path never calls
/// [`Instrument::render_notes`].
pub trait Instrument {
    /// A short, stable identifier for the instrument.
    fn id(&self) -> &str;

    /// Renders `notes` into `frames` mono samples at `sample_rate`.
    ///
    /// Deterministic: equal inputs give bit-identical output.
    ///
    /// # Errors
    ///
    /// [`SynthError::InvalidSampleRate`] for a zero `sample_rate`.
    fn render_notes(
        &self,
        notes: &[Note],
        sample_rate: u32,
        frames: u64,
    ) -> Result<Vec<f32>, SynthError>;
}

/// The built-in Salamander piano as an [`Instrument`].
///
/// Delegates to [`crate::piano::render_piano`], so its output is bit-identical
/// to the existing fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct PianoInstrument;

impl Instrument for PianoInstrument {
    fn id(&self) -> &str {
        "piano"
    }

    fn render_notes(
        &self,
        notes: &[Note],
        sample_rate: u32,
        frames: u64,
    ) -> Result<Vec<f32>, SynthError> {
        crate::piano::render_piano(notes, sample_rate, frames)
    }
}

/// An [`Instrument`] that renders through a loaded SFZ bank.
///
/// Holds the bank's resolved regions and decoded samples and builds a
/// [`audio_core::SamplerEngine`] per render, so pre-render runs the same code
/// path as the future realtime node. Control thread only.
pub struct SamplerInstrument {
    regions: Vec<SampleRegion>,
    samples: Vec<Arc<[i16]>>,
    interpolation: Interpolation,
}

impl SamplerInstrument {
    /// The sample rate the bank is rendered at (fixed at 48 kHz).
    pub const SAMPLE_RATE: u32 = 48_000;

    /// Builds an instrument from a [`LoadedBank`].
    ///
    /// The engine is not built here (it is not `Clone`); it is constructed
    /// fresh inside [`Instrument::render_notes`], which is offline and may
    /// allocate the voice pool.
    #[must_use]
    pub fn new(bank: &LoadedBank) -> Self {
        Self {
            regions: bank.regions.clone(),
            samples: bank.samples.clone(),
            interpolation: Interpolation::default(),
        }
    }

    /// The same instrument rendering with `interpolation`.
    #[must_use]
    pub const fn with_interpolation(mut self, interpolation: Interpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// The regions the instrument plays.
    #[must_use]
    pub fn regions(&self) -> &[SampleRegion] {
        &self.regions
    }

    /// The decoded samples the instrument plays.
    #[must_use]
    pub fn samples(&self) -> &[Arc<[i16]>] {
        &self.samples
    }

    fn build_engine(&self) -> Result<SamplerEngine, SynthError> {
        SamplerEngine::new_with(
            Self::SAMPLE_RATE,
            self.regions.clone(),
            self.samples.clone(),
            audio_core::DEFAULT_VOICES,
            self.interpolation,
        )
        .map_err(|_| SynthError::InvalidSampleRate)
    }
}

impl Instrument for SamplerInstrument {
    fn id(&self) -> &str {
        "sampler"
    }

    fn render_notes(
        &self,
        notes: &[Note],
        sample_rate: u32,
        frames: u64,
    ) -> Result<Vec<f32>, SynthError> {
        if sample_rate != Self::SAMPLE_RATE {
            return Err(SynthError::InvalidSampleRate);
        }

        let mut events = Vec::with_capacity(notes.len() * 2);
        for note in notes {
            let event = |sample, order, message| ScheduledEvent {
                sample,
                source: MidiEvent {
                    track: 0,
                    tick: 0,
                    order_in_track: order,
                    message,
                },
            };
            events.push(event(
                note.start,
                1,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: note.pitch,
                    velocity: note.velocity.max(1),
                },
            ));
            events.push(event(
                note.end().unwrap_or(u64::MAX),
                0,
                MidiMessage::NoteOff {
                    channel: 0,
                    note: note.pitch,
                    velocity: 0,
                },
            ));
        }
        events.sort_by_key(|event| {
            let pitch = match event.source.message {
                MidiMessage::NoteOn { note, .. } | MidiMessage::NoteOff { note, .. } => note,
                MidiMessage::Tempo { .. } => 0,
            };
            (event.sample, event.source.order_in_track, pitch)
        });
        let schedule = Schedule::new(ScheduleSpec {
            events,
            length_samples: frames,
        });

        // The engine renders stereo; mix down to mono `(L + R) · 0.5`.
        let mut engine = self.build_engine()?;
        let mut cursor = schedule.cursor();
        let total = usize::try_from(frames).unwrap_or(usize::MAX);
        let mut out = vec![0.0_f32; total];
        let mut l = vec![0.0_f32; 4096];
        let mut r = vec![0.0_f32; 4096];
        let mut position = 0_u64;
        for chunk in out.chunks_mut(4096) {
            let len = chunk.len();
            engine.render(
                &schedule,
                &mut cursor,
                position,
                len,
                &mut l[..len],
                &mut r[..len],
            );
            for (i, out_sample) in chunk.iter_mut().enumerate() {
                *out_sample = (l[i] + r[i]) * 0.5;
            }
            position += len as u64;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piano_instrument_matches_piano() {
        let notes = [
            Note {
                pitch: 60,
                start: 0,
                length: 48_000,
                velocity: 100,
            },
            Note {
                pitch: 64,
                start: 24_000,
                length: 24_000,
                velocity: 80,
            },
        ];
        let rate = 48_000;
        let frames = 96_000;
        let a = PianoInstrument.render_notes(&notes, rate, frames).unwrap();
        let b = crate::piano::render_piano(&notes, rate, frames).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn piano_instrument_rejects_zero_rate() {
        assert!(matches!(
            PianoInstrument.render_notes(&[], 0, 0),
            Err(SynthError::InvalidSampleRate)
        ));
    }
}
