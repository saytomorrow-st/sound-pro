//! Proves the streaming render of [`SamplerEngine`] (RFC-002 stage 6,
//! SPEC-073) performs no allocation.
//!
//! Own test binary because it installs a process-global counting allocator.
//! Engine, stream slots, schedule and PCM are built before the counting
//! window opens; inside it the engine streams several notes through the
//! head/tail seam, handles note-off, voice stealing (more notes than voices),
//! an underrun fade and `reset` (bank change). The counter must not move.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{SamplerEngine, StreamLoaderEnds};
use audio_core::sampler::region::SampleRegion;
use audio_core::sampler::sfz::LoopMode;
use audio_core::sampler::source::SampleSource;
use audio_core::sampler::stream::SampleHead;
use audio_core::sampler::voice::Interpolation;
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: this wrapper only forwards to the system allocator and bumps a
// counter; it upholds the `GlobalAlloc` contract.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const TOTAL: usize = 12_000;
const HEAD: usize = 1_024;
const QUANTUM: usize = 128;

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

/// Loader stand-in answering from preallocated PCM; starves when asked.
/// Returns how many requests started at the seam (one per note-on or wrap).
fn serve(ends: &mut StreamLoaderEnds, data: &[i16], starve: bool) -> usize {
    let mut seams = 0;
    while let Some(req) = ends.receiver.pop() {
        if req.from_frame == HEAD as u64 {
            seams += 1;
        }
        if starve {
            continue;
        }
        let feeder = &mut ends.feeders[usize::from(req.voice)];
        if !feeder.is_current(req.generation) {
            continue;
        }
        let from = req.from_frame as usize * 2;
        let to = from + req.frames as usize * 2;
        let _ = feeder.push_block(&req, &data[from..to]);
    }
    seams
}

fn run(interp: Interpolation, loop_mode: LoopMode) -> usize {
    let data: Vec<i16> = (0..TOTAL * 2)
        .map(|k| ((k as f64 * 0.13).sin() * 10_000.0) as i16)
        .collect();
    let head = SampleHead::new(Arc::from(data[..HEAD * 2].to_vec()), TOTAL as u64, 2, 0);
    let region = SampleRegion {
        sample_index: 0,
        channels: 2,
        root: 60,
        key_lo: 0,
        key_hi: 127,
        loop_mode,
        ..SampleRegion::default()
    };
    let mut engine = SamplerEngine::new_with(
        48_000,
        vec![region],
        vec![SampleSource::streaming(head)],
        3,
        interp,
    )
    .unwrap();
    let mut ends = engine.enable_streaming(512);
    let length = 24_000_u64;
    let sched = Schedule::new(ScheduleSpec {
        events: vec![
            on(0, 60),
            on(100, 64),
            on(200, 67),
            off(1_500, 64),
            on(2_000, 72), // steals a voice
            on(9_000, 55), // starved from 9 000 on: underrun fade
        ],
        length_samples: length,
    });
    let mut cursor = sched.cursor();
    let mut left = vec![0.0_f32; QUANTUM];
    let mut right = vec![0.0_f32; QUANTUM];
    let mut sink = 0.0_f32;

    // A loop must wrap (~12 000 for note 60) before the loader starves.
    let starve = if loop_mode == LoopMode::LoopContinuous {
        14_000..16_000
    } else {
        9_000..16_000
    };
    let mut seams = 0;
    let before = ALLOCS.load(Ordering::Relaxed);
    let mut position = 0_u64;
    while position < length {
        if position == 16_000 {
            engine.reset();
            cursor = sched.cursor();
        }
        seams += serve(&mut ends, &data, starve.contains(&position));
        engine.render(
            &sched,
            &mut cursor,
            position,
            QUANTUM,
            &mut left,
            &mut right,
        );
        sink += left[0] + right[QUANTUM - 1];
        position += QUANTUM as u64;
    }
    let after = ALLOCS.load(Ordering::Relaxed);

    assert!(sink.is_finite());
    assert!(engine.stream_stats().unwrap().underruns() > 0);
    // Five streamed note-ons (one stolen into); a loop adds wrap rewinds.
    if loop_mode == LoopMode::LoopContinuous {
        assert!(seams > 6, "loop wrap exercised: {seams} seam requests");
    }
    after - before
}

#[test]
fn streaming_engine_render_does_not_allocate() {
    for interp in [Interpolation::Linear, Interpolation::Hermite4] {
        // LoopContinuous covers the loop-wrap rewind of the stream.
        for mode in [LoopMode::OneShot, LoopMode::LoopContinuous] {
            assert_eq!(
                run(interp, mode),
                0,
                "{interp:?} {mode:?}: allocations on the audio path"
            );
        }
    }
}
