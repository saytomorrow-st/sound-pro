//! Streaming stress test (RFC-002 stage 7, SPEC-074): "pedal, 128 voices".
//!
//! Own test binary because it installs a process-global counting allocator.
//! Dense note-ons without note-offs keep all 128 voices busy and force
//! stealing, while a misbehaving loader is slow (few requests per quantum),
//! blocks completely for a window, starves at random, pushes stale blocks
//! for old generations and finally stops for good. Expected: no panic, no
//! allocation on the audio path, finite bounded output, underruns resolved
//! by fade-out, stale blocks discarded, and exact silence once the loader
//! is gone and every fade has finished.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{MAX_SAMPLER_VOICES, SamplerEngine, StreamLoaderEnds};
use audio_core::sampler::region::SampleRegion;
use audio_core::sampler::sfz::LoopMode;
use audio_core::sampler::source::SampleSource;
use audio_core::sampler::stream::{LoadRequest, SampleHead};
use audio_core::sampler::voice::Interpolation;
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

struct CountingAlloc;

thread_local! {
    /// Per-thread count, so a loader thread's allocations do not pollute the
    /// audio thread's window (const init: no allocation on first access).
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn bump() {
    // `try_with` fails only during thread teardown; nothing to count then.
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

// SAFETY: this wrapper only forwards to the system allocator and bumps a
// counter; it upholds the `GlobalAlloc` contract.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        bump();
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        bump();
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const TOTAL: usize = 30_000;
const HEAD: usize = 512;
const RING: usize = 512;
const QUANTUM: usize = 128;
const LENGTH: u64 = 96_000;
const NOTES_END: u64 = 60_000;
/// Phases of the loader, by render position.
const FULL_UNTIL: u64 = 16_000;
const SLOW_UNTIL: u64 = 28_000;
const BLOCKED: std::ops::Range<u64> = 28_000..36_000;
const RANDOM_UNTIL: u64 = 50_000;
const RESET_AT: u64 = 45_056;
const LOADER_GONE: u64 = 70_016;
/// A full 512-frame ring at ratio 0.5 lasts 1 024 samples, plus a 240-sample
/// fade; everything is silent long before this.
const SILENT_FROM: u64 = 80_000;
/// Generous: 128 voices at full scale would sum to about 47.
const BOUND: f32 = 256.0;

/// Deterministic LCG (Numerical Recipes constants).
struct Lcg(u32);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }
}

fn note_on(sample: u64, note: u8, velocity: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity,
            },
        },
    }
}

/// Answers one request from preallocated PCM, ignoring errors (a full ring
/// or a stale generation are both legal under stress).
fn answer(ends: &mut StreamLoaderEnds, data: &[i16], req: &LoadRequest) {
    let from = req.from_frame as usize * 2;
    let to = from + req.frames as usize * 2;
    if to <= data.len() {
        let _ = ends.feeders[usize::from(req.voice)].push_block(req, &data[from..to]);
    }
}

struct Loader {
    rng: Lcg,
    /// Last answered request; replayed later for a then-stale generation.
    last: Option<LoadRequest>,
}

impl Loader {
    /// One loader turn before the quantum starting at `position`.
    fn turn(&mut self, ends: &mut StreamLoaderEnds, data: &[i16], position: u64) {
        if BLOCKED.contains(&position) || position >= LOADER_GONE {
            return;
        }
        let budget = if !(FULL_UNTIL..RANDOM_UNTIL).contains(&position) {
            usize::MAX
        } else if position < SLOW_UNTIL {
            3
        } else {
            if self.rng.next() % 3 == 0 {
                return;
            }
            8
        };
        if let Some(old) = self.last {
            if self.rng.next() % 4 == 0 {
                // Misbehaving loader: does not check `is_current`.
                answer(ends, data, &old);
            }
        }
        let mut served = 0;
        while served < budget {
            let Some(req) = ends.receiver.pop() else {
                break;
            };
            served += 1;
            if !ends.feeders[usize::from(req.voice)].is_current(req.generation) {
                continue;
            }
            answer(ends, data, &req);
            self.last = Some(req);
        }
    }
}

struct Report {
    allocs: usize,
    underruns: u32,
    stale_blocks: u32,
    dropped_requests: u32,
    stolen: u32,
    active_after_reset: usize,
    active_at_end: usize,
    peak: f32,
    tail_silent: bool,
}

fn run(interp: Interpolation, loop_mode: LoopMode) -> Report {
    let data: Vec<i16> = (0..TOTAL * 2)
        .map(|k| ((k as f64 * 0.013).sin() * 12_000.0) as i16)
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
        MAX_SAMPLER_VOICES,
        interp,
    )
    .unwrap();
    let mut ends = engine.enable_streaming(RING);

    // Pedal: a new note every 64 samples, never released. Notes 36..=72 play
    // the 30 000-frame sample for at least 15 000 samples, so 128 voices are
    // full after ~8 200 samples and later note-ons steal.
    let mut rng = Lcg(0x5EED);
    let events: Vec<ScheduledEvent> = (0..NOTES_END / 64)
        .map(|k| {
            let note = 36 + (rng.next() % 37) as u8;
            let velocity = 1 + (rng.next() % 127) as u8;
            note_on(k * 64, note, velocity)
        })
        .collect();
    let sched = Schedule::new(ScheduleSpec {
        events,
        length_samples: LENGTH,
    });
    let mut cursor = sched.cursor();
    let mut left = vec![0.0_f32; QUANTUM];
    let mut right = vec![0.0_f32; QUANTUM];
    let mut loader = Loader {
        rng: Lcg(0xD15C),
        last: None,
    };
    let mut peak = 0.0_f32;
    let mut finite = true;
    let mut active_after_reset = usize::MAX;
    let mut tail_silent = true;

    let before = allocs();
    let mut position = 0_u64;
    while position < LENGTH {
        if position == RESET_AT {
            engine.reset();
            active_after_reset = engine.active_streams();
        }
        loader.turn(&mut ends, &data, position);
        engine.render(
            &sched,
            &mut cursor,
            position,
            QUANTUM,
            &mut left,
            &mut right,
        );
        for &v in left.iter().chain(right.iter()) {
            finite &= v.is_finite();
            peak = peak.max(v.abs());
            // The loader is gone and the last note-on lies far behind: every
            // ring has drained and every underrun fade has finished.
            if position >= SILENT_FROM {
                tail_silent &= v == 0.0;
            }
        }
        position += QUANTUM as u64;
    }
    let allocs = allocs() - before;

    assert!(finite, "{interp:?} {loop_mode:?}: non-finite output");
    let stats = engine.stream_stats().unwrap();
    Report {
        allocs,
        underruns: stats.underruns(),
        stale_blocks: stats.stale_blocks(),
        dropped_requests: stats.dropped_requests(),
        stolen: engine.stolen(),
        active_after_reset,
        active_at_end: engine.active_streams(),
        peak,
        tail_silent,
    }
}

#[test]
fn pedal_128_voices_with_misbehaving_loader() {
    for interp in [Interpolation::Linear, Interpolation::Hermite4] {
        for mode in [LoopMode::OneShot, LoopMode::LoopContinuous] {
            let r = run(interp, mode);
            let tag = format!("{interp:?} {mode:?}");
            eprintln!(
                "{tag}: underruns {} stale {} dropped requests {} stolen {} peak {:.2}",
                r.underruns, r.stale_blocks, r.dropped_requests, r.stolen, r.peak
            );
            assert_eq!(r.allocs, 0, "{tag}: allocations on the audio path");
            assert!(r.peak <= BOUND, "{tag}: peak {}", r.peak);
            assert!(r.peak > 0.01, "{tag}: render is audible");
            assert!(r.underruns > 0, "{tag}: starvation must underrun");
            assert!(r.stale_blocks > 0, "{tag}: stale blocks must be discarded");
            assert!(r.stolen > 0, "{tag}: the pedal must steal voices");
            assert_eq!(r.active_after_reset, 0, "{tag}: reset frees every slot");
            assert_eq!(r.active_at_end, 0, "{tag}: underrun fades free their slots");
            assert!(r.tail_silent, "{tag}: silence after the loader is gone");
        }
    }
}

/// Real loader thread that sleeps at random (and sometimes blocks for
/// milliseconds) while the audio thread renders freely and resets mid-run:
/// generation checks race with `cancel`/`rewind` on the audio thread.
fn run_threaded(interp: Interpolation, loop_mode: LoopMode) -> Report {
    let data: Arc<[i16]> = (0..TOTAL * 2)
        .map(|k| ((k as f64 * 0.013).sin() * 12_000.0) as i16)
        .collect();
    let head = SampleHead::new(Arc::from(&data[..HEAD * 2]), TOTAL as u64, 2, 0);
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
        MAX_SAMPLER_VOICES,
        interp,
    )
    .unwrap();
    let mut ends = engine.enable_streaming(RING);
    let mut rng = Lcg(0xBEEF);
    let events: Vec<ScheduledEvent> = (0..NOTES_END / 64)
        .map(|k| note_on(k * 64, 36 + (rng.next() % 37) as u8, 100))
        .collect();
    let sched = Schedule::new(ScheduleSpec {
        events,
        length_samples: LENGTH,
    });

    let stop = Arc::new(AtomicBool::new(false));
    let loader = {
        let stop = Arc::clone(&stop);
        let data = Arc::clone(&data);
        std::thread::spawn(move || {
            let mut rng = Lcg(0xF00D);
            let mut stale: Option<LoadRequest> = None;
            while !stop.load(Ordering::Acquire) {
                let pause = match rng.next() % 64 {
                    0 => 3_000,
                    n => u64::from(n) * 4,
                };
                std::thread::sleep(Duration::from_micros(pause));
                if let Some(old) = stale.take() {
                    answer(&mut ends, &data, &old);
                }
                for _ in 0..1 + rng.next() % 16 {
                    let Some(req) = ends.receiver.pop() else {
                        break;
                    };
                    // Deliberately unchecked half the time: the voice must
                    // reject the block by generation on its own.
                    if rng.next() % 2 == 0
                        && !ends.feeders[usize::from(req.voice)].is_current(req.generation)
                    {
                        continue;
                    }
                    answer(&mut ends, &data, &req);
                    stale = Some(req);
                }
            }
        })
    };

    let mut cursor = sched.cursor();
    let mut left = vec![0.0_f32; QUANTUM];
    let mut right = vec![0.0_f32; QUANTUM];
    let mut peak = 0.0_f32;
    let mut finite = true;
    let mut active_after_reset = usize::MAX;
    let mut tail_silent = true;
    let before = allocs();
    let mut position = 0_u64;
    while position < LENGTH {
        if position == RESET_AT {
            engine.reset();
            active_after_reset = engine.active_streams();
        }
        if position == LOADER_GONE {
            stop.store(true, Ordering::Release);
        }
        engine.render(
            &sched,
            &mut cursor,
            position,
            QUANTUM,
            &mut left,
            &mut right,
        );
        for &v in left.iter().chain(right.iter()) {
            finite &= v.is_finite();
            peak = peak.max(v.abs());
            if position >= SILENT_FROM {
                tail_silent &= v == 0.0;
            }
        }
        position += QUANTUM as u64;
        // Roughly real time for 128-sample quanta at 48 kHz, scaled down.
        std::thread::sleep(Duration::from_micros(100));
    }
    let allocs = allocs() - before;
    stop.store(true, Ordering::Release);
    loader.join().expect("loader thread panicked");

    assert!(finite, "{interp:?} {loop_mode:?}: non-finite output");
    let stats = engine.stream_stats().unwrap();
    Report {
        allocs,
        underruns: stats.underruns(),
        stale_blocks: stats.stale_blocks(),
        dropped_requests: stats.dropped_requests(),
        stolen: engine.stolen(),
        active_after_reset,
        active_at_end: engine.active_streams(),
        peak,
        tail_silent,
    }
}

#[test]
fn pedal_128_voices_with_sleeping_loader_thread() {
    for interp in [Interpolation::Linear, Interpolation::Hermite4] {
        for mode in [LoopMode::OneShot, LoopMode::LoopContinuous] {
            let r = run_threaded(interp, mode);
            let tag = format!("threaded {interp:?} {mode:?}");
            eprintln!(
                "{tag}: underruns {} stale {} dropped requests {} stolen {} peak {:.2}",
                r.underruns, r.stale_blocks, r.dropped_requests, r.stolen, r.peak
            );
            assert_eq!(r.allocs, 0, "{tag}: allocations on the audio thread");
            assert!(r.peak <= BOUND, "{tag}: peak {}", r.peak);
            assert!(r.peak > 0.01, "{tag}: render is audible");
            assert!(r.underruns > 0, "{tag}: a sleeping loader must underrun");
            assert!(r.stolen > 0, "{tag}: the pedal must steal voices");
            assert_eq!(r.active_after_reset, 0, "{tag}: reset frees every slot");
            assert_eq!(r.active_at_end, 0, "{tag}: underrun fades free their slots");
            assert!(r.tail_silent, "{tag}: silence after the loader is gone");
        }
    }
}
