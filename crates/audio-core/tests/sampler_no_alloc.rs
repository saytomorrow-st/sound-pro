//! Proves the sampler render path performs no allocation (RFC-001 §3.5).
//!
//! This lives in its own test binary because it installs a process-global
//! counting allocator; running alongside other parallel tests would corrupt the
//! count. The test builds the engine and schedule first (allocation is fine
//! there), then snapshots the allocator and asserts the counter does not move
//! across many render calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{DEFAULT_VOICES, SamplerEngine};
use audio_core::sampler::region::SampleRegion;
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

const SR: u32 = 48_000;
const QUANTUM: usize = 256;

/// Counts every allocation; only the render loop is measured.
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

#[test]
fn render_allocates_nothing_after_construction() {
    let reg = SampleRegion {
        sample_index: 0,
        root: 60,
        key_lo: 60,
        key_hi: 60,
        ..SampleRegion::default()
    };

    let samples = vec![Arc::from(vec![10_000_i16; 4096])];
    let mut engine = SamplerEngine::new(SR, vec![reg], samples, DEFAULT_VOICES).unwrap();

    let events = vec![ScheduledEvent {
        sample: 0,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        },
    }];
    let sched = Schedule::new(ScheduleSpec {
        events,
        length_samples: 0,
    });

    let mut left = vec![0.0_f32; 4096];
    let mut right = vec![0.0_f32; 4096];
    let mut cursor = sched.cursor();

    // Warm up once (first call may touch lazily-initialised state), then
    // snapshot the allocation count.
    engine.render(&sched, &mut cursor, 0, QUANTUM, &mut left, &mut right);
    let before = ALLOCS.load(Ordering::Relaxed);

    // Render many blocks; the counter must not move.
    let mut cursor = sched.cursor();
    let mut position = 0_u64;
    while position < 4096 {
        let n = ((4096 - position) as usize).min(QUANTUM);
        engine.render(
            &sched,
            &mut cursor,
            position,
            n,
            &mut left[position as usize..],
            &mut right[position as usize..],
        );
        position += n as u64;
    }

    let after = ALLOCS.load(Ordering::Relaxed);
    assert_eq!(before, after, "render path must not allocate");
}
