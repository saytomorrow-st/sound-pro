//! Bounce a test tone to WAV using the offline renderer.
//!
//! ```text
//! cargo run -p audio-core --example bounce -- /tmp/tone.wav
//! ```
//!
//! Useful as a smoke test of the whole engine path on a machine without audio
//! hardware, and as a template for future "bounce with a script" workflows.

use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::offline::OfflineRenderer;
use audio_core::render::Renderer;
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport, TransportCommand};
use audio_core::wav::{sine, write_mono_slice};

/// Simple sine instrument with a linear fade-out to avoid a click at the end.
struct Tone {
    freq_hz: f64,
    length: u64,
}

impl NodeProcessor for Tone {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        let block = sine(self.freq_hz, ctx.sample_rate, ctx.position, ctx.frames);
        let dst = output.channel_mut(0);
        for (i, sample) in block.iter().enumerate() {
            let abs = ctx.position + i as u64;
            #[allow(clippy::cast_precision_loss)]
            let gain = if abs >= self.length {
                0.0
            } else {
                let remaining = (self.length - abs) as f32;
                (remaining / 2_400.0).min(1.0)
            };
            dst[i] = sample * 0.5 * gain;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "tone.wav".into());
    let sr = DEFAULT_SAMPLE_RATE;

    let mut graph = Graph::with_capacity(sr, 1, 1024, 4, 0);
    graph.add_node(
        "tone",
        Tone {
            freq_hz: 440.0,
            length: u64::from(sr),
        },
    )?;

    let mut transport = Transport::new(sr);
    transport.apply(TransportCommand::Play);

    let renderer = Renderer::new(graph, transport, 1024);
    let mut offline = OfflineRenderer::new(renderer, 512);
    let mono = offline.bounce_mono(u64::from(sr))?;

    write_mono_slice(&path, sr, &mono)?;
    println!("wrote {} frames to {path}", mono.len());
    Ok(())
}
