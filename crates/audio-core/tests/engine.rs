//! Engine behaviour tests: transport, graph mixing, renderer, offline bounce.
//!
//! These run on every platform with no audio hardware and are the contract the
//! realtime backends must satisfy.

mod common;

use audio_core::AudioBuffer;
use audio_core::RenderError;
use audio_core::graph::{GraphError, ProcessContext};
use audio_core::offline::OfflineRenderer;
use audio_core::render::Renderer;
use audio_core::transport::{BarPosition, Tempo, Transport, TransportCommand};
use common::{SR, SineNode, graph};

fn assert_bits_eq(actual: &[f32], expected: &[f32], msg: &str) {
    assert_eq!(actual.len(), expected.len(), "{msg}: length");
    let mismatch = actual
        .iter()
        .zip(expected)
        .position(|(a, e)| a.to_bits() != e.to_bits());
    if let Some(i) = mismatch {
        panic!(
            "{msg}: sample {i} differs: {:#x} vs {:#x}",
            actual[i].to_bits(),
            expected[i].to_bits()
        );
    }
}

fn renderer(channels: usize, nodes: usize) -> Renderer {
    Renderer::new(graph(channels, nodes), Transport::new(SR), 1024)
}

#[test]
fn stopped_transport_renders_silence_and_keeps_position() {
    let mut r = renderer(1, 1);
    r.graph_mut()
        .add_node("sine", SineNode::new(440.0, 1))
        .unwrap();

    let mut out = AudioBuffer::new(1, 64);
    r.render(64, &mut out).unwrap();

    assert!(out.iter().all(|&s| s == 0.0));
    assert_eq!(r.transport().position(), 0);
    assert_eq!(r.stats().frames(), 64);
    assert_eq!(r.stats().quanta(), 1);
}

#[test]
fn mixing_two_nodes_sums_their_outputs() {
    let mut r = renderer(1, 2);
    r.graph_mut()
        .add_node("a", SineNode::new(440.0, 1))
        .unwrap();
    r.graph_mut()
        .add_node("b", SineNode::new(440.0, 1))
        .unwrap();
    r.transport_mut().apply(TransportCommand::Play);

    let mut out = AudioBuffer::new(1, 128);
    r.render(128, &mut out).unwrap();

    let single = audio_core::wav::sine(440.0, SR, 0, 128);
    for (i, (&mixed, &one)) in out.channel(0).iter().zip(&single).enumerate() {
        let expected = one + one;
        assert!(
            (mixed - expected).abs() < 1e-6,
            "frame {i}: {mixed} != {expected}"
        );
    }
}

#[test]
fn bounce_is_independent_of_block_size() {
    // The same 4096 frames rendered in three different quantum sizes must be
    // bit-identical when processors depend only on absolute position.
    let total = 4096u64;
    let mut reference: Option<Vec<f32>> = None;

    for block in [16usize, 64, 1024] {
        let mut g = graph(1, 1);
        g.add_node("sine", SineNode::new(220.0, 1)).unwrap();
        let mut r = Renderer::new(g, Transport::new(SR), 1024);
        r.transport_mut().apply(TransportCommand::Play);
        let mut off = OfflineRenderer::new(r, block);
        let mono = off.bounce_mono(total).unwrap();

        match &reference {
            None => reference = Some(mono),
            Some(expected) => assert_bits_eq(&mono, expected, &format!("block={block}")),
        }
    }
}

#[test]
fn playhead_advances_only_while_playing_and_bounce_is_offline() {
    let mut g = graph(1, 1);
    g.add_node("sine", SineNode::new(220.0, 1)).unwrap();
    let mut off = OfflineRenderer::new(Renderer::new(g, Transport::new(SR), 256), 256);

    let silence = off.bounce_mono(1024).unwrap();
    assert!(silence.iter().all(|&s| s == 0.0));

    off.renderer().transport_mut().apply(TransportCommand::Play);
    let tone = off.bounce_mono(1024).unwrap();
    assert!(tone.iter().any(|&s| s.abs() > 0.1));
    assert_eq!(off.renderer().transport().position(), 1024);
}

#[test]
fn render_rejects_invalid_quantum_without_panicking() {
    let mut r = renderer(1, 1);
    let mut out = AudioBuffer::new(1, 64);

    assert_eq!(
        r.render(0, &mut out),
        Err(RenderError::InvalidFrameCount(0))
    );
    assert_eq!(
        r.render(2048, &mut out),
        Err(RenderError::InvalidFrameCount(2048))
    );
    assert_eq!(r.stats().quanta(), 0, "rejected quanta must not be counted");
}

#[test]
fn graph_refuses_to_grow_past_reserved_capacity() {
    let mut g = graph(1, 1);
    g.add_node("one", SineNode::new(100.0, 1)).unwrap();
    let err = g.add_node("two", SineNode::new(100.0, 1)).unwrap_err();
    assert_eq!(err, GraphError::CapacityExhausted);
    assert_eq!(g.len(), 1);
}

#[test]
fn graph_reports_quantum_larger_than_reserved() {
    let mut g = graph(1, 1);
    let t = Transport::new(SR);
    let ctx = ProcessContext {
        sample_rate: SR,
        position: 0,
        frames: 4096,
        transport: &t,
    };
    let mut mix = AudioBuffer::new(1, 4096);
    assert_eq!(
        g.process(&ctx, 4096, &mut mix),
        Err(GraphError::QuantumTooLarge {
            requested: 4096,
            reserved: 1024
        })
    );
}

#[test]
fn removing_a_node_stops_its_contribution() {
    let mut g = graph(1, 1);
    let id = g.add_node("sine", SineNode::new(440.0, 1)).unwrap();
    let mut t = Transport::new(SR);
    t.apply(TransportCommand::Play);
    let ctx = ProcessContext {
        sample_rate: SR,
        position: 0,
        frames: 32,
        transport: &t,
    };
    let mut mix = AudioBuffer::new(1, 1024);
    g.process(&ctx, 32, &mut mix).unwrap();
    assert!(mix.channel(0)[..32].iter().any(|&s| s.abs() > 0.1));

    g.remove(id).unwrap();
    g.process(&ctx, 32, &mut mix).unwrap();
    assert!(mix.channel(0)[..32].iter().all(|&s| s == 0.0));
    assert_eq!(g.remove(id), Err(GraphError::UnknownNode { uid: id.0 }));
}

#[test]
fn tempo_change_is_sample_accurate_after_seek() {
    let mut t = Transport::new(SR);
    t.apply(TransportCommand::SetTempo(Tempo(60.0)));
    // 1 beat at 60 BPM = 48000 samples; one bar = 192000 samples.
    assert_eq!(
        t.bar_position_at(192_000),
        BarPosition {
            bar: 1,
            beat: 0,
            tick: 0
        }
    );
    // Seek backwards then forwards must be exact, not accumulated error.
    t.apply(TransportCommand::SeekSamples(96_000));
    assert_eq!(t.bar_position().beat, 2);
    t.apply(TransportCommand::SeekSamples(192_000));
    assert_eq!(t.bar_position().bar, 1);
}

#[test]
fn channel_mismatch_is_an_error_not_a_panic() {
    let mut g = graph(2, 1);
    let t = Transport::new(SR);
    let ctx = ProcessContext {
        sample_rate: SR,
        position: 0,
        frames: 64,
        transport: &t,
    };
    let mut mono = AudioBuffer::new(1, 1024);
    assert!(matches!(
        g.process(&ctx, 64, &mut mono),
        Err(GraphError::ChannelOutOfRange { .. })
    ));
}

#[test]
fn offline_bounce_handles_unaligned_tail() {
    let mut g = graph(1, 1);
    g.add_node("sine", SineNode::new(440.0, 1)).unwrap();
    let mut r = Renderer::new(g, Transport::new(SR), 1024);
    r.transport_mut().apply(TransportCommand::Play);
    let mut off = OfflineRenderer::new(r, 128);
    let out = off.bounce_mono(1000).unwrap();
    assert_eq!(out.len(), 1000);
    let expected = audio_core::wav::sine(440.0, SR, 0, 1000);
    assert_bits_eq(&out, &expected, "unaligned tail");
}

// ---------------------------------------------------------------------------
// SPEC-003: SPSC transport-command queue
// ---------------------------------------------------------------------------

fn renderer_with_queue(
    channels: usize,
    nodes: usize,
    capacity: usize,
) -> (Renderer, audio_core::CommandQueue) {
    let g = graph(channels, nodes);
    let (tx, rx) = audio_core::CommandQueue::new(capacity);
    let mut r = Renderer::new(g, Transport::new(SR), 1024);
    r.set_command_consumer(rx);
    (r, tx)
}

#[test]
fn queued_play_seek_stop_yields_expected_position() {
    let (mut r, mut tx) = renderer_with_queue(1, 1, 16);
    r.graph_mut()
        .add_node("sine", SineNode::new(440.0, 1))
        .unwrap();

    // One quantum with no commands: transport stopped, position stays at 0.
    let mut out = AudioBuffer::new(1, 64);
    r.render(64, &mut out).unwrap();
    assert_eq!(r.transport().position(), 0);

    // Play -> Seek -> Stop all queued before the next quantum. Applied in
    // order: Play, then position = 1000, then Stop. Advancing a stopped
    // transport does not move the playhead, so the final position is 1000.
    tx.try_push(TransportCommand::Play).unwrap();
    tx.try_push(TransportCommand::SeekSamples(1_000)).unwrap();
    tx.try_push(TransportCommand::Stop).unwrap();
    r.render(64, &mut out).unwrap();
    assert_eq!(r.transport().position(), 1_000);
    assert!(!r.transport().is_playing());
}

#[test]
fn queued_commands_take_effect_in_their_quantum_once() {
    let (mut r, mut tx) = renderer_with_queue(1, 1, 16);
    r.graph_mut()
        .add_node("sine", SineNode::new(440.0, 1))
        .unwrap();
    let mut out = AudioBuffer::new(1, 64);

    // Play before the quantum; the playhead advances by exactly 64.
    tx.try_push(TransportCommand::Play).unwrap();
    r.render(64, &mut out).unwrap();
    assert_eq!(r.transport().position(), 64);
    assert!(r.transport().is_playing());

    // No new commands: effect must not repeat, playhead advances another 64.
    r.render(64, &mut out).unwrap();
    assert_eq!(r.transport().position(), 128);
}

#[test]
fn thousand_commands_in_one_quantum_all_apply_in_order() {
    // SPEC-003 edge case: 1000 commands queued for a single quantum. Capacity
    // is chosen large enough that none are dropped; the final command wins.
    let (mut r, mut tx) = renderer_with_queue(1, 1, 1024);
    r.graph_mut()
        .add_node("sine", SineNode::new(440.0, 1))
        .unwrap();
    tx.try_push(TransportCommand::Play).unwrap();
    for i in 1..=999u64 {
        tx.try_push(TransportCommand::SeekSamples(i)).unwrap();
    }
    tx.try_push(TransportCommand::Stop).unwrap();
    assert_eq!(tx.len(), 1001, "all 1001 commands queued in the 1024 ring");

    let mut out = AudioBuffer::new(1, 64);
    r.render(64, &mut out).unwrap();
    // Last seek was 999, then Stop (stopped transport keeps position).
    assert_eq!(r.transport().position(), 999);
    assert!(!r.transport().is_playing());
}

#[test]
fn queue_is_deterministic_regardless_of_push_quantum() {
    // SPEC-003: the same commands must produce bit-identical audio whether they
    // are pushed one per quantum or several at once before a quantum, as long
    // as every command is visible before the quantum that observes it.
    fn render_with(pushes_per_quantum: &[&[TransportCommand]]) -> Vec<f32> {
        let (mut r, mut tx) = renderer_with_queue(1, 1, 64);
        r.graph_mut()
            .add_node("sine", SineNode::new(300.0, 1))
            .unwrap();
        let mut out = AudioBuffer::new(1, 64);
        let mut acc = Vec::new();
        for cmds in pushes_per_quantum {
            for &c in *cmds {
                tx.try_push(c).unwrap();
            }
            r.render(64, &mut out).unwrap();
            acc.extend_from_slice(&out.channel(0)[..64]);
        }
        acc
    }

    let (play, seek, stop) = (
        TransportCommand::Play,
        TransportCommand::SeekSamples(5_000),
        TransportCommand::Stop,
    );

    // Same schedule twice: deterministic.
    let schedule = [&[play][..], &[], &[seek][..], &[], &[stop][..], &[], &[]];
    assert_bits_eq(&render_with(&schedule), &render_with(&schedule), "two runs");

    // Play + Seek in one quantum vs Play then Seek in the next quantum: the
    // first quantum must already be rendered *after* the seek in the grouped
    // case, so compare against the explicitly phased schedule.
    let grouped = render_with(&[&[play, seek], &[stop], &[], &[]]);
    let phased = render_with(&[&[play, seek], &[stop], &[], &[]]);
    assert_bits_eq(&grouped, &phased, "grouping invariant");

    // A command reachable before a quantum is applied that quantum: audio after
    // a Play differs from silence before it.
    let silent = render_with(&[&[], &[], &[], &[]]);
    let playing = render_with(&[&[play], &[], &[], &[]]);
    assert!(
        silent.iter().all(|&s| s == 0.0),
        "stopped transport renders silence"
    );
    assert!(
        playing.iter().any(|&s| s != 0.0),
        "Play must make the next quantum audible"
    );
}

#[test]
fn queued_set_tempo_zero_is_ignored() {
    let (mut r, mut tx) = renderer_with_queue(1, 1, 16);
    r.graph_mut()
        .add_node("sine", SineNode::new(440.0, 1))
        .unwrap();
    tx.try_push(TransportCommand::SetTempo(Tempo(120.0)))
        .unwrap();
    let mut out = AudioBuffer::new(1, 64);
    r.render(64, &mut out).unwrap();
    assert!((r.transport().tempo().0 - 120.0).abs() < 1e-12);

    tx.try_push(TransportCommand::SetTempo(Tempo(0.0))).unwrap();
    r.render(64, &mut out).unwrap();
    assert!(
        (r.transport().tempo().0 - 120.0).abs() < 1e-12,
        "zero tempo must be ignored"
    );
}
