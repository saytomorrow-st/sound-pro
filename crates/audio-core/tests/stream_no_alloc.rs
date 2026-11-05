//! Proves the audio-thread side of sample streaming (`VoiceStream`, RFC-002
//! stage 5) performs no allocation.
//!
//! Own test binary because it installs a process-global counting allocator.
//! Pool, heads and PCM are built before the counting window opens; inside it
//! the test exercises start, the head/tail seam, refill requests, stale
//! generation discard (restart/steal), cancel, sample end and the underrun
//! fade, for mono and stereo, linear and Hermite. The counter must not move.

use std::sync::atomic::{AtomicUsize, Ordering};

use audio_core::sampler::stream::{SampleHead, StreamPool};
use audio_core::sampler::voice::Interpolation;

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

fn pcm(frames: usize, channels: usize) -> Vec<i16> {
    (0..frames * channels)
        .map(|k| ((k as f64 * 0.21).sin() * 10_000.0) as i16)
        .collect()
}

/// Loader stand-in: answers pending requests from preallocated PCM, except
/// for the `starved` voice. Pushing into the preallocated rings does not
/// allocate either.
fn serve(pool: &mut StreamPool, data: &[i16], ch: usize, starved: u16) {
    while let Some(req) = pool.receiver.pop() {
        if req.voice == starved {
            continue;
        }
        let feeder = &mut pool.feeders[usize::from(req.voice)];
        if !feeder.is_current(req.generation) {
            continue;
        }
        let from = req.from_frame as usize * ch;
        let to = from + req.frames as usize * ch;
        let _ = feeder.push_block(&req, &data[from..to]);
    }
}

fn run(channels: u8, interp: Interpolation) -> usize {
    let ch = usize::from(channels);
    let total = 4_000;
    let data = pcm(total, ch);
    let head = SampleHead::from_pcm(&data, channels, 0, 256);
    let mut pool = StreamPool::with_ring_frames(4, channels, 512);
    let mut requests = std::mem::replace(
        &mut pool.requests,
        StreamPool::with_ring_frames(0, 1, 1).requests,
    );
    let mut sink = 0.0_f32;

    let before = ALLOCS.load(Ordering::Relaxed);

    // Voice 0 plays the whole sample through the seam with a live loader.
    assert!(pool.voices[0].start(&head, 0, &mut requests));
    // Voice 1 starts, gets restarted (steal) with stale data in flight.
    assert!(pool.voices[1].start(&head, 0, &mut requests));
    serve(&mut pool, &data, ch, 2);
    assert!(pool.voices[1].start(&head, 0, &mut requests));
    // Voice 2 never gets tail data: underrun fade.
    assert!(pool.voices[2].start(&head, 0, &mut requests));
    // Voice 3 starts then is cancelled.
    assert!(pool.voices[3].start(&head, 0, &mut requests));
    pool.voices[3].cancel();

    for _ in 0..(total * 2) {
        serve(&mut pool, &data, ch, 2);
        for v in 0..2 {
            if let Some((l, r)) = pool.voices[v].next_frame(&head, 1.37, interp, &mut requests) {
                sink += l + r;
            }
        }
        if let Some((l, r)) = pool.voices[2].next_frame(&head, 1.0, interp, &mut requests) {
            sink += l + r;
        }
    }

    let after = ALLOCS.load(Ordering::Relaxed);
    std::hint::black_box(sink);
    assert!(pool.voices.iter().all(|v| !v.is_active()));
    assert_eq!(pool.stats.underruns(), 1);
    assert!(pool.stats.stale_blocks() >= 1);
    after - before
}

#[test]
fn voice_stream_does_not_allocate() {
    for channels in [1, 2] {
        for interp in [Interpolation::Linear, Interpolation::Hermite4] {
            assert_eq!(run(channels, interp), 0, "ch={channels} {interp:?}");
        }
    }
}
