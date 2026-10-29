//! Writes a small demo project (two audio and three MIDI tracks) for
//! looking at the window: `cargo run -p daw-gui --example demo_project -- DIR`.

use std::path::PathBuf;

use audio_core::session::Note;
use daw_gui::Editor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "demo".into()));
    std::fs::create_dir_all(&dir)?;
    let rate = 48_000u32;
    let seconds = 8u32;
    let samples: Vec<f32> = (0..rate * seconds)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / rate as f32;
            let beat = (t * 2.0).fract();
            (-beat * 6.0).exp() * (t * 110.0 * std::f32::consts::TAU).sin() * 0.8
        })
        .collect();
    let wav = dir.join("drums.wav");
    audio_core::wav::write_mono_slice(&wav, rate, &samples)?;

    let mut editor = Editor::new();
    let bar = u64::from(rate) * 2;
    editor.import_audio_at(&wav, None, Some(0))?;
    editor.import_audio_at(&wav, None, Some(bar * 2))?;
    for (k, base) in [48u8, 60, 67].into_iter().enumerate() {
        let (track, clip) = editor.add_midi_track(bar * k as u64, bar * 3)?;
        for step in 0..12u8 {
            let pitch = base + [0u8, 4, 7, 12][usize::from(step % 4)];
            editor.add_note(
                track,
                clip,
                Note {
                    pitch,
                    start: u64::from(step) * bar / 4,
                    length: bar / 6,
                    velocity: 100,
                },
            )?;
        }
    }
    let names = ["Drummer", "Loop", "Bass", "Keys", "Lead"];
    let colors = [2u8, 3, 5, 6, 1];
    for (i, name) in names.iter().enumerate() {
        editor.rename_track(i, name)?;
        editor.set_track_color(i, Some(colors[i]))?;
    }
    editor.save_as(&dir.join("demo.json"))?;
    println!("{}", dir.join("demo.json").display());
    Ok(())
}
