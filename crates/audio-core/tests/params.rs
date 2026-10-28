//! Live parameter channel integration tests (SPEC-006).
//!
//! These drive a real [`Renderer`]: a node reads its [`ParamHandle`] once per
//! quantum, the control side calls `set` between quanta, and the rendered audio
//! must follow without clicks and without breaking determinism.

mod common;

use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::params::{ParamError, ParamHandle, ParamId, ParamSpec};
use audio_core::render::Renderer;
use audio_core::transport::{Transport, TransportCommand};
use common::SR;

/// Emits its parameter value as a DC level for the whole quantum.
struct LevelNode {
    level: ParamHandle,
}

impl NodeProcessor for LevelNode {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let value = self.level.next(ctx.frames, ctx.sample_rate);
        for ch in 0..output.channels() {
            output.channel_mut(ch)[..ctx.frames].fill(value);
        }
    }
}

fn renderer(quantum: usize) -> (Renderer, ParamHandle) {
    let mut graph = Graph::with_capacity(SR, 1, quantum, 1, 1);
    let spec = ParamSpec::new(ParamId(7), "level", 0.0, 1.0, 0.0, 10).unwrap();
    let handle = ParamHandle::new(spec);
    graph.register_param(handle.clone()).unwrap();
    graph
        .add_node(
            "level",
            LevelNode {
                level: handle.clone(),
            },
        )
        .unwrap();
    let mut renderer = Renderer::new(graph, Transport::new(SR), quantum);
    renderer.transport_mut().apply(TransportCommand::Play);
    (renderer, handle)
}

/// Renders one quantum into a fresh buffer and returns the channel.
fn quantum(renderer: &mut Renderer, quantum: usize) -> Vec<f32> {
    let mut buf = audio_core::AudioBuffer::new(1, quantum);
    renderer.render(quantum, &mut buf).unwrap();
    buf.channel(0).to_vec()
}

#[test]
fn parameter_change_between_quanta_is_audible_in_the_next_one() {
    let (mut r, level) = renderer(256);

    let first = quantum(&mut r, 256);
    assert!(first.iter().all(|&s| s == 0.0), "starts at the default");

    level.set(1.0);
    // The quantum is read at its start, so the first quantum after `set` still
    // plays the previous value, and the following one is a ramp.
    let still_old = quantum(&mut r, 256);
    assert!(still_old.iter().all(|&s| s == 0.0));
    let second = quantum(&mut r, 256);
    assert!(
        second.iter().all(|&s| (s - 256.0 / 480.0).abs() < 1e-5),
        "second quantum after set is 256 of 480 ramp samples in, got {}",
        second[0]
    );

    // Give the ramp time to finish, then the level is flat at the target.
    for _ in 0..4 {
        let _ = quantum(&mut r, 256);
    }
    let settled = quantum(&mut r, 256);
    assert!(settled.iter().all(|&s| (s - 1.0).abs() < 1e-6));
}

#[test]
fn ramp_is_continuous_in_time() {
    let (mut r, level) = renderer(64);
    level.set(1.0);

    // The parameter is read once per quantum, so a quantum is a step of the
    // ramp: 64 frames out of 480 is 1/7.5 of the travel. The guard is against
    // the quantum-sized jump a raw `set` without smoothing would produce.
    let mut peak_step = 0.0_f32;
    let mut previous: Option<f32> = None;
    for _ in 0..40 {
        let q = quantum(&mut r, 64);
        let level = q[0];
        assert!(q.iter().all(|&s| s == level), "a quantum is one value");
        if let Some(p) = previous {
            peak_step = peak_step.max((level - p).abs());
        }
        previous = Some(level);
    }
    assert!(
        peak_step < 0.15,
        "a 10 ms ramp must not jump by more than 0.15 per quantum, got {peak_step}"
    );
    assert!(
        (previous.unwrap() - 1.0).abs() < 1e-6,
        "ramp reached target"
    );
}

#[test]
fn identical_control_sequences_render_bit_identical_audio() {
    fn run() -> Vec<Vec<f32>> {
        let (mut r, level) = renderer(128);
        let mut out = Vec::new();
        for step in 0..12 {
            if step == 2 {
                level.set(1.0);
            }
            if step == 7 {
                level.set(0.25);
            }
            out.push(quantum(&mut r, 128));
        }
        out
    }

    let a = run();
    let b = run();
    for (q, (qa, qb)) in a.iter().zip(&b).enumerate() {
        for (i, (x, y)) in qa.iter().zip(qb).enumerate() {
            assert_eq!(
                x.to_bits(),
                y.to_bits(),
                "quantum {q} sample {i} is not deterministic"
            );
        }
    }
}

#[test]
fn value_freezes_when_nothing_sets_it() {
    let (mut r, level) = renderer(128);
    level.set(0.5);
    let mut last = 0.0;
    for _ in 0..10 {
        let q = quantum(&mut r, 128);
        last = *q.last().unwrap();
    }
    let frozen = quantum(&mut r, 128);
    assert!(frozen.iter().all(|&s| (s - last).abs() < 1e-6));
}

#[test]
fn registered_handle_is_reachable_from_the_graph() {
    let (mut r, level) = renderer(64);
    let found = r.graph_mut().param(ParamId(7)).unwrap();
    assert_eq!(found.spec().name, "level");
    found.set(0.75);
    assert!((level.value() - 0.75).abs() < 1e-6);
    assert_eq!(r.graph_mut().params().len(), 1);
}

#[test]
fn duplicate_id_and_unknown_id_are_reported() {
    let mut graph = Graph::with_capacity(SR, 1, 64, 1, 1);
    let spec = ParamSpec::unsmoothed(ParamId(1), "gain", 0.0, 1.0, 0.0).unwrap();
    let handle = ParamHandle::new(spec);
    graph.register_param(handle.clone()).unwrap();
    assert_eq!(
        graph.register_param(handle).unwrap_err(),
        ParamError::DuplicateId { id: ParamId(1) }
    );
    assert_eq!(
        graph.param(ParamId(2)).unwrap_err(),
        ParamError::UnknownParam { id: ParamId(2) }
    );
}

#[test]
fn registry_capacity_is_enforced_without_growing() {
    let mut graph = Graph::with_capacity(SR, 1, 64, 1, 1);
    let first = ParamHandle::new(ParamSpec::unsmoothed(ParamId(1), "a", 0.0, 1.0, 0.0).unwrap());
    graph.register_param(first).unwrap();
    let second = ParamHandle::new(ParamSpec::unsmoothed(ParamId(2), "b", 0.0, 1.0, 0.0).unwrap());
    assert_eq!(
        graph.register_param(second).unwrap_err(),
        ParamError::RegistryFull { max: 1 }
    );
    assert_eq!(graph.params().len(), 1, "registry must not grow");
}

#[test]
fn unsmoothed_parameter_reaches_the_target_within_the_quantum() {
    let mut graph = Graph::with_capacity(SR, 1, 64, 1, 1);
    let spec = ParamSpec::unsmoothed(ParamId(1), "gain", 0.0, 1.0, 0.0).unwrap();
    let handle = ParamHandle::new(spec);
    graph.register_param(handle.clone()).unwrap();
    graph
        .add_node(
            "level",
            LevelNode {
                level: handle.clone(),
            },
        )
        .unwrap();
    let mut r = Renderer::new(graph, Transport::new(SR), 64);
    r.transport_mut().apply(TransportCommand::Play);

    handle.set(1.0);
    let q = quantum(&mut r, 64);
    assert!(q.iter().all(|&s| (s - 1.0).abs() < 1e-6));
}
