//! Streaming playback through [`SamplerEngine`] (RFC-002 stage 6, SPEC-073).
//!
//! A deterministic in-test loader answers the engine's tail requests from the
//! full PCM, standing in for the daw-app disk thread. The no-allocation
//! contract of the streaming render is proven in `sampler_stream_no_alloc.rs`.

use std::sync::Arc;

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{SamplerEngine, StreamLoaderEnds};
use audio_core::sampler::region::SampleRegion;
use audio_core::sampler::sfz::LoopMode;
use audio_core::sampler::source::SampleSource;
use audio_core::sampler::stream::{LoadRequest, SampleHead};
use audio_core::sampler::voice::{Interpolation, STEAL_FADE_SAMPLES};
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

const SR: u32 = 48_000;
const TOTAL: usize = 20_000;
const HEAD: usize = 4_096;
const RING: usize = 2_048;
const QUANTUM: usize = 256;

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

fn on(sample: u64, note: u8) -> ScheduledEvent {
    event(
        sample,
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
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

fn schedule(events: Vec<ScheduledEvent>, length: u64) -> Schedule {
    Schedule::new(ScheduleSpec {
        events,
        length_samples: length,
    })
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

fn region(loop_mode: LoopMode) -> SampleRegion {
    SampleRegion {
        sample_index: 0,
        channels: 2,
        root: 60,
        key_lo: 0,
        key_hi: 127,
        loop_mode,
        ..SampleRegion::default()
    }
}

fn resident_engine(data: &[i16], interp: Interpolation) -> SamplerEngine {
    resident_engine_with(data, interp, region(LoopMode::OneShot))
}

fn resident_engine_with(data: &[i16], interp: Interpolation, r: SampleRegion) -> SamplerEngine {
    SamplerEngine::new_with(
        SR,
        vec![r],
        vec![SampleSource::resident(Arc::from(data.to_vec()))],
        8,
        interp,
    )
    .unwrap()
}

fn streaming_engine(
    data: &[i16],
    interp: Interpolation,
    loop_mode: LoopMode,
) -> (SamplerEngine, StreamLoaderEnds) {
    streaming_engine_with(data, interp, region(loop_mode))
}

fn streaming_engine_with(
    data: &[i16],
    interp: Interpolation,
    r: SampleRegion,
) -> (SamplerEngine, StreamLoaderEnds) {
    let head = SampleHead::new(Arc::from(data[..HEAD * 2].to_vec()), TOTAL as u64, 2, 0);
    let mut engine =
        SamplerEngine::new_with(SR, vec![r], vec![SampleSource::streaming(head)], 8, interp)
            .unwrap();
    let ends = engine.enable_streaming(RING);
    (engine, ends)
}

/// Answers every pending, current request from `data` (unless `starve`) and
/// returns the requests it saw.
fn serve(ends: &mut StreamLoaderEnds, data: &[i16], starve: bool) -> Vec<LoadRequest> {
    let mut seen = Vec::new();
    while let Some(req) = ends.receiver.pop() {
        seen.push(req);
        if starve {
            continue;
        }
        let feeder = &mut ends.feeders[usize::from(req.voice)];
        if !feeder.is_current(req.generation) {
            continue;
        }
        let from = req.from_frame as usize * 2;
        let to = from + req.frames as usize * 2;
        feeder.push_block(&req, &data[from..to]).unwrap();
    }
    seen
}

fn render(
    engine: &mut SamplerEngine,
    mut ends: Option<&mut StreamLoaderEnds>,
    data: &[i16],
    sched: &Schedule,
    frames: usize,
    starve_after: Option<u64>,
) -> (Vec<f32>, Vec<f32>) {
    let mut left = vec![0.0_f32; frames];
    let mut right = vec![0.0_f32; frames];
    let mut cursor = sched.cursor();
    let mut position = 0_u64;
    while (position as usize) < frames {
        if let Some(ends) = ends.as_deref_mut() {
            let starve = starve_after.is_some_and(|s| position >= s);
            serve(ends, data, starve);
        }
        let n = (frames - position as usize).min(QUANTUM);
        let at = position as usize;
        engine.render(
            sched,
            &mut cursor,
            position,
            n,
            &mut left[at..],
            &mut right[at..],
        );
        position += n as u64;
    }
    (left, right)
}

#[test]
fn streamed_render_is_bit_identical_to_resident() {
    let data = pcm();
    for interp in [Interpolation::Linear, Interpolation::Hermite4] {
        for note in [60_u8, 67, 55] {
            // Long enough for the lowest note (ratio < 1) to reach the end.
            let frames = TOTAL * 2;
            let sched = schedule(vec![on(10, note)], frames as u64);
            let mut resident = resident_engine(&data, interp);
            let (rl, rr) = render(&mut resident, None, &data, &sched, frames, None);
            let (mut streaming, mut ends) = streaming_engine(&data, interp, LoopMode::OneShot);
            let (sl, sr) = render(&mut streaming, Some(&mut ends), &data, &sched, frames, None);
            for (i, (a, b)) in rl.iter().zip(&sl).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{interp:?} note {note} L @{i}");
            }
            for (i, (a, b)) in rr.iter().zip(&sr).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{interp:?} note {note} R @{i}");
            }
            assert!(rl[HEAD + 500].abs() > 0.0 || rl[HEAD + 501].abs() > 0.0);
            let stats = streaming.stream_stats().unwrap();
            assert_eq!(stats.underruns(), 0);
            assert_eq!(streaming.active_streams(), 0, "slot freed at sample end");
        }
    }
}

#[test]
fn starved_stream_fades_out_instead_of_pausing() {
    let data = pcm();
    let sched = schedule(vec![on(0, 60)], TOTAL as u64);
    let (mut engine, mut ends) = streaming_engine(&data, Interpolation::Linear, LoopMode::OneShot);
    let (left, _) = render(&mut engine, Some(&mut ends), &data, &sched, TOTAL, Some(0));
    assert_eq!(engine.stream_stats().unwrap().underruns(), 1);
    // The head plays normally, then the fade lasts at most one fade length.
    assert!(left[..HEAD].iter().any(|s| s.abs() > 0.01));
    let silent_from = HEAD + STEAL_FADE_SAMPLES as usize;
    assert!(
        left[silent_from..].iter().all(|&s| s == 0.0),
        "no audio after the underrun fade"
    );
    // The fade is monotonic in magnitude (no jump back up = no pause/resume).
    let fade = &left[HEAD..silent_from];
    let start = left[HEAD - 1].abs();
    assert!(fade.iter().all(|s| s.abs() <= start + 1e-6));
    assert_eq!(engine.active_streams(), 0);
}

#[test]
fn note_off_and_reset_free_stream_slots() {
    let data = pcm();
    let (mut engine, mut ends) = streaming_engine(&data, Interpolation::Linear, LoopMode::NoLoop);
    let sched = schedule(vec![on(0, 60), on(0, 64), off(3_000, 60)], 12_000);
    let mut cursor = sched.cursor();
    let mut l = vec![0.0_f32; QUANTUM];
    let mut r = vec![0.0_f32; QUANTUM];
    let mut position = 0_u64;
    let mut seen = Vec::new();
    while position < 2_048 {
        seen.extend(serve(&mut ends, &data, false));
        engine.render(&sched, &mut cursor, position, QUANTUM, &mut l, &mut r);
        position += QUANTUM as u64;
    }
    assert_eq!(engine.active_streams(), 2);
    while position < 6_000 {
        serve(&mut ends, &data, false);
        engine.render(&sched, &mut cursor, position, QUANTUM, &mut l, &mut r);
        position += QUANTUM as u64;
    }
    assert_eq!(engine.active_streams(), 1, "released voice freed its slot");
    engine.reset();
    assert_eq!(engine.active_streams(), 0, "reset (bank change) frees all");
    // Requests issued before the reset are stale: the loader must drop them.
    assert!(!seen.is_empty());
    for req in &seen {
        assert!(!ends.feeders[usize::from(req.voice)].is_current(req.generation));
    }
}

fn looped(loop_start: u32, loop_end: u32) -> SampleRegion {
    SampleRegion {
        loop_start,
        loop_end,
        ..region(LoopMode::LoopContinuous)
    }
}

#[test]
fn looped_stream_is_bit_identical_to_resident() {
    let data = pcm();
    // Whole-sample loop (loop_end 0, as in Osiris) and a loop starting inside
    // the head; several passes, so the tail is re-streamed after each wrap.
    for (loop_start, loop_end) in [(0_u32, 0_u32), (1_000, 15_000)] {
        for interp in [Interpolation::Linear, Interpolation::Hermite4] {
            for note in [60_u8, 67, 55] {
                let frames = TOTAL * 4;
                let sched = schedule(vec![on(10, note)], frames as u64);
                let mut resident =
                    resident_engine_with(&data, interp, looped(loop_start, loop_end));
                let (rl, rr) = render(&mut resident, None, &data, &sched, frames, None);
                let (mut streaming, mut ends) =
                    streaming_engine_with(&data, interp, looped(loop_start, loop_end));
                let (sl, sr) = render(&mut streaming, Some(&mut ends), &data, &sched, frames, None);
                let tag = format!("{interp:?} note {note} loop {loop_start}..{loop_end}");
                for (i, (a, b)) in rl.iter().zip(&sl).enumerate() {
                    assert_eq!(a.to_bits(), b.to_bits(), "{tag} L @{i}");
                }
                for (i, (a, b)) in rr.iter().zip(&sr).enumerate() {
                    assert_eq!(a.to_bits(), b.to_bits(), "{tag} R @{i}");
                }
                assert!(sl[frames - 1_000..].iter().any(|s| s.abs() > 0.01), "{tag}");
                assert_eq!(streaming.stream_stats().unwrap().underruns(), 0, "{tag}");
                assert_eq!(streaming.active_streams(), 1, "{tag}: still looping");
            }
        }
    }
}

#[test]
fn loop_starting_outside_head_does_not_stream() {
    let data = pcm();
    let (mut engine, mut ends) =
        streaming_engine_with(&data, Interpolation::Linear, looped(HEAD as u32, 0));
    let sched = schedule(vec![on(0, 60)], 1_000);
    let _ = render(&mut engine, Some(&mut ends), &data, &sched, 1_000, None);
    assert_eq!(engine.active_streams(), 0);
    assert!(ends.receiver.is_empty());
}

#[test]
fn engine_without_streaming_plays_streaming_source_head_only() {
    let data = pcm();
    let head = SampleHead::new(Arc::from(data[..HEAD * 2].to_vec()), TOTAL as u64, 2, 0);
    let mut engine = SamplerEngine::new_with(
        SR,
        vec![region(LoopMode::OneShot)],
        vec![SampleSource::streaming(head)],
        8,
        Interpolation::Linear,
    )
    .unwrap();
    assert!(!engine.is_streaming());
    let sched = schedule(vec![on(0, 60)], TOTAL as u64);
    let (left, _) = render(&mut engine, None, &data, &sched, TOTAL, None);
    assert!(left[..HEAD - 1].iter().any(|s| s.abs() > 0.01));
    assert!(left[HEAD + 1..].iter().all(|&s| s == 0.0));
}
