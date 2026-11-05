//! Streaming vs Resident equivalence (RFC-002 stage 7, SPEC-074).
//!
//! With a loader that answers every request before the next quantum the
//! streamed render must be bit-identical to the resident one for every
//! engine path: chords, note-off with release inside the tail, loops over
//! several passes, voice stealing, `reset` mid-render and retriggering a note
//! while it still plays. Each scenario runs with Linear and Hermite4.

use std::sync::Arc;

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{SamplerEngine, StreamLoaderEnds};
use audio_core::sampler::region::{AmpEnvelope, SampleRegion};
use audio_core::sampler::sfz::LoopMode;
use audio_core::sampler::source::SampleSource;
use audio_core::sampler::stream::SampleHead;
use audio_core::sampler::voice::Interpolation;
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

const SR: u32 = 48_000;
const TOTAL: usize = 20_000;
const HEAD: usize = 4_096;
const RING: usize = 2_048;
const QUANTUM: usize = 256;
const INTERPS: [Interpolation; 2] = [Interpolation::Linear, Interpolation::Hermite4];

fn event(sample: u64, message: MidiMessage) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message,
        },
    }
}

fn on(sample: u64, note: u8, velocity: u8) -> ScheduledEvent {
    event(
        sample,
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity,
        },
    )
}

fn off(sample: u64, note: u8) -> ScheduledEvent {
    event(
        sample,
        MidiMessage::NoteOff {
            channel: 0,
            note,
            velocity: 0,
        },
    )
}

/// Interleaved stereo test signal: two incommensurate sines.
fn pcm() -> Vec<i16> {
    (0..TOTAL)
        .flat_map(|f| {
            let t = f as f64;
            [
                ((t * 0.031).sin() * 12_000.0) as i16,
                ((t * 0.017).cos() * 9_000.0) as i16,
            ]
        })
        .collect()
}

fn region(loop_mode: LoopMode, release: u32) -> SampleRegion {
    SampleRegion {
        sample_index: 0,
        channels: 2,
        root: 60,
        key_lo: 0,
        key_hi: 127,
        loop_mode,
        envelope: AmpEnvelope {
            release_samples: release,
            ..AmpEnvelope::default()
        },
        ..SampleRegion::default()
    }
}

fn looped(loop_start: u32, loop_end: u32, release: u32) -> SampleRegion {
    SampleRegion {
        loop_start,
        loop_end,
        ..region(LoopMode::LoopContinuous, release)
    }
}

struct Scenario {
    region: SampleRegion,
    voices: usize,
    events: Vec<ScheduledEvent>,
    frames: usize,
    /// Positions (multiples of [`QUANTUM`]) where both engines are `reset`.
    resets: Vec<u64>,
}

struct Outcome {
    left: Vec<f32>,
    right: Vec<f32>,
    stolen: u32,
    underruns: u32,
    active_streams: usize,
}

/// Answers every pending, current request from `data`.
fn serve(ends: &mut StreamLoaderEnds, data: &[i16]) {
    while let Some(req) = ends.receiver.pop() {
        let feeder = &mut ends.feeders[usize::from(req.voice)];
        if !feeder.is_current(req.generation) {
            continue;
        }
        let from = req.from_frame as usize * 2;
        let to = from + req.frames as usize * 2;
        feeder
            .push_block(&req, &data[from..to])
            .expect("a current request always fits its ring");
    }
}

fn run(s: &Scenario, data: &[i16], interp: Interpolation, streaming: bool) -> Outcome {
    let source = if streaming {
        let head = SampleHead::new(Arc::from(data[..HEAD * 2].to_vec()), TOTAL as u64, 2, 0);
        SampleSource::streaming(head)
    } else {
        SampleSource::resident(Arc::from(data.to_vec()))
    };
    let mut engine =
        SamplerEngine::new_with(SR, vec![s.region.clone()], vec![source], s.voices, interp)
            .unwrap();
    let mut ends = streaming.then(|| engine.enable_streaming(RING));
    let sched = Schedule::new(ScheduleSpec {
        events: s.events.clone(),
        length_samples: s.frames as u64,
    });
    let mut left = vec![0.0_f32; s.frames];
    let mut right = vec![0.0_f32; s.frames];
    let mut cursor = sched.cursor();
    let mut position = 0_u64;
    while (position as usize) < s.frames {
        if s.resets.contains(&position) {
            engine.reset();
            cursor = sched.cursor();
        }
        if let Some(ends) = ends.as_mut() {
            serve(ends, data);
        }
        let n = (s.frames - position as usize).min(QUANTUM);
        let at = position as usize;
        engine.render(
            &sched,
            &mut cursor,
            position,
            n,
            &mut left[at..],
            &mut right[at..],
        );
        position += n as u64;
    }
    Outcome {
        left,
        right,
        stolen: engine.stolen(),
        underruns: engine.stream_stats().map_or(0, |s| s.underruns()),
        active_streams: engine.active_streams(),
    }
}

/// Renders the scenario both ways with each interpolation, asserts
/// bit-identity and returns the streamed outcomes (Linear, Hermite4).
fn assert_equivalent(tag: &str, s: &Scenario) -> Vec<Outcome> {
    let data = pcm();
    INTERPS
        .iter()
        .map(|&interp| {
            let resident = run(s, &data, interp, false);
            let streamed = run(s, &data, interp, true);
            for (ch, a, b) in [
                ("L", &resident.left, &streamed.left),
                ("R", &resident.right, &streamed.right),
            ] {
                let diff = a
                    .iter()
                    .zip(b.iter())
                    .position(|(x, y)| x.to_bits() != y.to_bits());
                assert_eq!(diff, None, "{tag} {interp:?} {ch} diverged");
            }
            assert_eq!(resident.stolen, streamed.stolen, "{tag} {interp:?}");
            assert_eq!(streamed.underruns, 0, "{tag} {interp:?}");
            assert!(
                streamed.left.iter().any(|v| v.abs() > 0.01),
                "{tag} {interp:?}: render is audible"
            );
            streamed
        })
        .collect()
}

#[test]
fn chord_with_mixed_velocities_matches_resident() {
    let s = Scenario {
        region: region(LoopMode::OneShot, 0),
        voices: 8,
        events: vec![
            on(0, 60, 30),
            on(0, 64, 100),
            on(37, 67, 64),
            on(5_000, 55, 127),
        ],
        frames: TOTAL * 2,
        resets: vec![],
    };
    for out in assert_equivalent("chord", &s) {
        assert_eq!(out.active_streams, 0, "every voice reached the sample end");
    }
}

#[test]
fn note_off_with_release_inside_tail_matches_resident() {
    // NoLoop honours note-off; the release starts after the head (HEAD = 4096)
    // and fades over 2000 samples while the tail streams.
    let s = Scenario {
        region: region(LoopMode::NoLoop, 2_000),
        voices: 8,
        events: vec![
            on(0, 60, 100),
            on(100, 67, 80),
            off(6_000, 60),
            off(9_000, 67),
            on(12_000, 62, 90),
            off(12_500, 62),
        ],
        frames: 20_000,
        resets: vec![],
    };
    for out in assert_equivalent("note-off", &s) {
        assert_eq!(out.active_streams, 0, "released voices freed their slots");
        assert!(out.left[16_000..].iter().all(|&v| v == 0.0));
    }
}

#[test]
fn loops_over_several_passes_with_note_off_match_resident() {
    for (loop_start, loop_end) in [(0_u32, 0_u32), (1_000, 15_000)] {
        let s = Scenario {
            region: looped(loop_start, loop_end, 1_500),
            voices: 8,
            events: vec![
                on(0, 60, 100),
                on(64, 72, 70),
                on(1_000, 48, 110),
                off(50_000, 72),
                off(70_000, 60),
            ],
            frames: TOTAL * 4,
            resets: vec![],
        };
        let tag = format!("loop {loop_start}..{loop_end}");
        for out in assert_equivalent(&tag, &s) {
            // Note 48 is still looping; 60 and 72 have released.
            assert_eq!(out.active_streams, 1, "{tag}");
        }
    }
}

#[test]
fn voice_stealing_matches_resident() {
    // Two voices, five overlapping notes: the engine must steal (and move the
    // victims onto fading tail slots) identically in both modes.
    for loop_mode in [LoopMode::OneShot, LoopMode::LoopContinuous] {
        let s = Scenario {
            region: region(loop_mode, 0),
            voices: 2,
            events: vec![
                on(0, 60, 100),
                on(500, 64, 90),
                on(5_000, 67, 80),
                on(5_100, 72, 70),
                on(9_000, 55, 120),
            ],
            frames: TOTAL * 2,
            resets: vec![],
        };
        let tag = format!("steal {loop_mode:?}");
        for out in assert_equivalent(&tag, &s) {
            assert!(out.stolen >= 3, "{tag}: stolen {}", out.stolen);
        }
    }
}

#[test]
fn reset_mid_render_matches_resident() {
    // Reset while every voice is in its tail (bank change), then new notes
    // after the reset reuse the freed slots.
    let s = Scenario {
        region: looped(0, 0, 0),
        voices: 4,
        events: vec![
            on(0, 60, 100),
            on(0, 64, 100),
            on(200, 67, 100),
            on(8_200, 62, 100),
            on(8_200, 69, 50),
        ],
        frames: 30_000,
        resets: vec![(6_144 / QUANTUM as u64) * QUANTUM as u64, 20_480],
    };
    for out in assert_equivalent("reset", &s) {
        assert!(out.left[6_144..8_200].iter().all(|&v| v == 0.0));
        assert!(out.left[20_480..].iter().all(|&v| v == 0.0));
        assert_eq!(out.active_streams, 0);
    }
}

#[test]
fn retrigger_same_note_matches_resident() {
    // The same key is struck again while its first voice is deep in the tail
    // (and once more within the head); stream slots are reused.
    for loop_mode in [
        LoopMode::OneShot,
        LoopMode::NoLoop,
        LoopMode::LoopContinuous,
    ] {
        let s = Scenario {
            region: region(loop_mode, 800),
            voices: 3,
            events: vec![
                on(0, 60, 100),
                on(7_000, 60, 60),
                on(7_500, 60, 127),
                on(15_000, 60, 90),
                off(30_000, 60),
            ],
            frames: 40_000,
            resets: vec![],
        };
        assert_equivalent(&format!("retrigger {loop_mode:?}"), &s);
    }
}
