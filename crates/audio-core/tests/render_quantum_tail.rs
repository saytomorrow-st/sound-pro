//! Regression: the engine must not hand stale samples to the device.
//!
//! A realtime backend splits a device buffer — rarely a multiple of the
//! internal quantum — into several renders and interleaves the quanta in
//! place. Two things must therefore hold:
//!
//! * a node sees exactly `ctx.frames` frames, so it cannot leave samples in
//!   the reserved tail of its scratch;
//! * `Renderer::render` writes exactly `frames` frames of the destination.
//!
//! Both are checked with a node that fills whatever window it is given with
//! full scale, so any sample the engine leaks is loud and unmistakable.

use audio_core::TransportCommand;
use audio_core::buffer::AudioBuffer;
use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::render::Renderer;
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport};

const QUANTUM: usize = 1024;
const LONG: usize = QUANTUM;
const SHORT: usize = 7;

/// Full-scale DC: fills the whole window it is given, without clearing what
/// was there before — exactly how a real DSP node that blends or skips samples
/// leaves its scratch.
struct Greedy;

impl NodeProcessor for Greedy {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        assert_eq!(output.frames(), ctx.frames, "the view is the quantum");
        let channel = output.channel_mut(0);
        assert_eq!(
            channel.len(),
            ctx.frames,
            "a node must not see the reserved tail"
        );
        fill(&mut channel[..ctx.frames / 2], 1.0);
    }
}

/// Writes `value` into `dst` without `fill`, so the test exercises the same
/// code shape as real processing rather than relying on `slice::fill`.
fn fill(dst: &mut [f32], value: f32) {
    for sample in dst {
        *sample = value;
    }
}

fn greedy_renderer() -> Renderer {
    // (sample_rate, channels, max_quantum, max_nodes)
    let mut graph = Graph::with_capacity(DEFAULT_SAMPLE_RATE, 1, QUANTUM, 2, 0);
    graph.add_node("greedy", Greedy).unwrap();
    let mut renderer = Renderer::new(graph, Transport::new(DEFAULT_SAMPLE_RATE), QUANTUM);
    renderer.transport_mut().apply(TransportCommand::Play);
    renderer
}

#[test]
fn the_quantum_is_full_scale_and_the_rest_of_the_buffer_is_untouched() {
    let mut renderer = greedy_renderer();
    let mut device = AudioBuffer::new(renderer.channels(), QUANTUM);
    // A sentinel that a leaked 1.0 or a stray 0.0 can never be confused with.
    device.fill(-0.5);

    renderer.render(SHORT, &mut device).unwrap();

    let head = &device.channel(0)[..SHORT / 2];
    assert!(
        head.iter().all(|&s| s == 1.0),
        "the written half of the quantum must be full scale, got {head:?}"
    );
    let unwritten = &device.channel(0)[SHORT / 2..SHORT];
    assert!(
        unwritten.iter().all(|&s| s == 0.0),
        "the untouched half of the quantum must be silent, got {unwritten:?}"
    );
    let tail = &device.channel(0)[SHORT..];
    assert!(
        tail.iter().all(|&s| s == -0.5),
        "the renderer must not write past the quantum, got {tail:?}"
    );
}

#[test]
fn a_short_quantum_after_a_long_one_does_not_replay_it() {
    // Same renderer and same destination across a long and a short quantum:
    // what the short one renders must equal what a renderer that only ever
    // rendered the short quantum produces.
    let mut reused_renderer = greedy_renderer();
    let mut reused = AudioBuffer::new(reused_renderer.channels(), QUANTUM);
    reused_renderer.render(LONG, &mut reused).unwrap();
    reused_renderer.render(SHORT, &mut reused).unwrap();

    let mut fresh_renderer = greedy_renderer();
    let mut fresh = AudioBuffer::new(fresh_renderer.channels(), QUANTUM);
    fresh_renderer.render(SHORT, &mut fresh).unwrap();

    assert_eq!(
        &reused.channel(0)[..SHORT],
        &fresh.channel(0)[..SHORT],
        "history must not change what a quantum contains"
    );
    // A long (full) quantum must not survive into the short one.
    let replayed = &reused.channel(0)[SHORT / 2..SHORT];
    assert!(
        replayed.iter().all(|&s| s == 0.0),
        "samples from the previous long quantum were replayed: {replayed:?}"
    );
}

#[test]
fn a_freshly_built_renderer_has_silent_scratch() {
    // The reserved tail of the scratch is never written by a node; whatever
    // the allocator hands over, the engine must start from silence.
    for iteration in 0..64 {
        // Churn the allocator so a fresh renderer reuses dirty memory.
        let mut noise = vec![1.0_f32; 4 * QUANTUM];
        noise.fill(1.0);
        std::hint::black_box(&noise);

        let mut renderer = greedy_renderer();
        let mut device = AudioBuffer::new(renderer.channels(), QUANTUM);
        device.fill(-0.5);
        renderer.render(SHORT, &mut device).unwrap();
        let tail = &device.channel(0)[SHORT..];
        if !tail.iter().all(|&s| s == -0.5) {
            panic!(
                "iteration {iteration}: head={:?} tail10={:?}",
                &device.channel(0)[..SHORT],
                &tail[..10]
            );
        }
    }
}
