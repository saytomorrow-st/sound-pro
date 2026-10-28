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
