//! Golden tests: compare rendered output against committed reference audio.
//!
//! Reference files are generated once from a *correct* implementation and then
//! guarded by this test. Regeneration requires `UPDATE_GOLDENS=1`:
//!
//! ```text
//! UPDATE_GOLDENS=1 cargo test -p audio-core --test golden
//! ```
//!
//! Regenerating replaces the oracle with whatever the code does today, so the
//! diff must be reviewed like any other code change (a human listens to the
//! WAV). This is exactly the kind of guardrail an AI agent must not be able to
//! flip silently — see `AGENTS.md`.

mod common;

use std::path::PathBuf;

use audio_core::offline::OfflineRenderer;
use audio_core::render::Renderer;
use audio_core::transport::{Transport, TransportCommand};
use audio_core::wav::{WavSpec, read_wav_mono, sine, write_mono_slice};
use common::{SR, SineNode, graph};

const GOLDEN_FRAMES: u64 = 4800; // exactly 0.1 s at 48 kHz
/// Absolute tolerance in linear amplitude (-120 dBFS). Reference is float, so
/// this only absorbs cross-platform libm differences in `sin`.
const TOLERANCE: f32 = 1e-6;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn render_sine(freq_hz: f64) -> Vec<f32> {
    let mut g = graph(1, 1);
    g.add_node("sine", SineNode::new(freq_hz, 1)).unwrap();
    let mut r = Renderer::new(g, Transport::new(SR), 1024);
    r.transport_mut().apply(TransportCommand::Play);
    let mut off = OfflineRenderer::new(r, 256);
    off.bounce_mono(GOLDEN_FRAMES).unwrap()
}

fn compare_with_golden(name: &str, actual: &[f32]) {
    let path = golden_dir().join(format!("{name}.wav"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        write_mono_slice(&path, SR, actual).unwrap();
        eprintln!("updated golden: {}", path.display());
        return;
    }
    let (spec, expected) = read_wav_mono(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); run UPDATE_GOLDENS=1 cargo test -p audio-core --test golden",
            path.display()
        )
    });
    assert_eq!(
        spec,
        WavSpec {
            sample_rate: SR,
            channels: 1
        }
    );
    assert_eq!(expected.frames(), actual.len(), "{name}: frame count");
    let max_err = actual
        .iter()
        .zip(expected.channel(0))
        .map(|(a, e)| (a - e).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_err <= TOLERANCE,
        "{name}: max error {max_err} exceeds {TOLERANCE}"
    );
}

#[test]
fn golden_sine_440() {
    let actual = render_sine(440.0);
    // Sanity: the signal is not silent before comparing to the oracle.
    assert!(actual.iter().any(|s| s.abs() > 0.9));
    compare_with_golden("sine_440_48k", &actual);
}

#[test]
fn golden_sine_100() {
    compare_with_golden("sine_100_48k", &render_sine(100.0));
}

#[test]
fn sine_helper_matches_file_format() {
    // Guards the fixture writer itself: a hand-generated sine must round-trip.
    let samples = sine(1000.0, SR, 0, 16);
    let dir = std::env::temp_dir().join("daw-golden-sanity");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sanity.wav");
    write_mono_slice(&path, SR, &samples).unwrap();
    let (_, back) = read_wav_mono(&path).unwrap();
    assert_eq!(back.channel(0), &samples[..]);
    std::fs::remove_file(path).unwrap();
}
