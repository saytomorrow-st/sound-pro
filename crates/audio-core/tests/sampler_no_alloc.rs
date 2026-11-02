//! Proves the sampler render path performs no allocation across the full
//! voice lifecycle (SPEC-063 acceptance criteria).
//!
//! This lives in its own test binary because it installs a process-global
//! counting allocator; running alongside other parallel tests would corrupt the
//! count. Everything is built before the counting window opens (engine,
//! schedule, output buffers), then the allocator is snapshotted and the whole
//! timeline is rendered. The counter must not move.
//!
//! The schedule deliberately exercises every allocation-prone branch of the
//! render path inside the window — not a single mono note-on — so the test is
//! meaningful:
//!
//! * **Stealing**: 128 note-ons into a 64-voice pool force 64 steals, each
//!   moving a victim into a preallocated fade-tail slot (`fast_fade`).
//! * **`off_by` choke**: one region cuts a group with a fast fade.
//! * **Stereo**: a two-channel sample region.
//! * **Loop**: a `loop_continuous` region wraps its playhead.
//! * **Note-off + voice free**: note-offs release voices, and short non-looping
//!   samples run to their end so `advance()` exhausts and frees them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{DEFAULT_VOICES, SamplerEngine};
use audio_core::sampler::region::SampleRegion;
use audio_core::sampler::sfz::{LoopMode, OffMode};
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

const SR: u32 = 48_000;
const QUANTUM: usize = 256;
const TOTAL: u64 = 8192;

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

fn note_off(sample: u64, note: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: MidiMessage::NoteOff {
                channel: 0,
                note,
                velocity: 0,
            },
        },
    }
}

#[test]
fn render_allocates_nothing_across_the_full_voice_lifecycle() {
    // Sample 0: short mono DC — held notes exhaust and free their voices.
    let mono: Vec<i16> = vec![10_000_i16; 512];
    // Sample 1: stereo DC (interleaved L/R).
    let stereo: Vec<i16> = (0..512)
        .map(|i| if i % 2 == 0 { 9000 } else { 7000 })
        .collect();
    // Sample 2: long looped mono DC so its voice never exhausts.
    let looped: Vec<i16> = vec![8000_i16; 4096];

    let samples = vec![Arc::from(mono), Arc::from(stereo), Arc::from(looped)];

    // One region per note. Notes 69/70 form an off_by choke pair; note 71 is
    // stereo; note 72 loops. The rest are short mono one-shots.
    let mut regions: Vec<SampleRegion> = (0..=127)
        .map(|n| SampleRegion {
            sample_index: 0,
            root: n,
            key_lo: n,
            key_hi: n,
            ..SampleRegion::default()
        })
        .collect();
    // Group 1: note 69 is the choke victim; note 70 cuts group 1 fast.
    regions[69].group = Some(1);
    regions[70].off_by = Some(1);
    regions[70].off_mode = OffMode::Fast;
    // Stereo region.
    regions[71].sample_index = 1;
    regions[71].channels = 2;
    // Loop region.
    regions[72].sample_index = 2;
    regions[72].loop_mode = LoopMode::LoopContinuous;

    let mut engine = SamplerEngine::new(SR, regions, samples, DEFAULT_VOICES).unwrap();
    assert_eq!(engine.voice_count(), DEFAULT_VOICES);

    // Fill and overflow the pool: 128 note-ons force 64 steals. A note-off and
    // a re-trigger of the off_by note exercise release and choke.
    let mut events: Vec<ScheduledEvent> = (0..=127).map(|n| note_on(0, n as u8, 100)).collect();
    events.push(note_off(256, 60));
    events.push(note_on(512, 70, 120)); // re-trigger off_by -> chokes note 69
    events.push(note_on(1024, 60, 100)); // re-trigger a released note
    events.push(note_off(2048, 72)); // release the looping voice
    let sched = Schedule::new(ScheduleSpec {
        events,
        length_samples: TOTAL,
    });

    let mut left = vec![0.0_f32; TOTAL as usize];
    let mut right = vec![0.0_f32; TOTAL as usize];
    let mut cursor = sched.cursor();

    // Warm up once (first call may touch lazily-initialised state), then
    // snapshot the allocation count.
    engine.render(&sched, &mut cursor, 0, QUANTUM, &mut left, &mut right);
    let before = ALLOCS.load(Ordering::Relaxed);

    // Render the whole timeline; the counter must not move.
    let mut cursor = sched.cursor();
    let mut position = 0_u64;
    while position < TOTAL {
        let n = ((TOTAL - position) as usize).min(QUANTUM);
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

    // The test is only meaningful if it actually started voices, stole, and
    // produced non-zero output.
    assert!(engine.voices_started() > 0, "no voices started");
    assert!(engine.stolen() > 0, "no voices stolen");
    let peak = left
        .iter()
        .chain(right.iter())
        .cloned()
        .fold(0.0_f32, f32::max);
    assert!(peak > 0.0, "output was silent");
    assert!(peak.is_finite(), "output was non-finite");
}
