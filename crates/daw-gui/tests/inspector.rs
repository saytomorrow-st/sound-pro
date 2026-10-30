//! Acceptance tests for the Logic-style inspector (SPEC-048).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::Editor;

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-insp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &Path, samples: &[f32]) {
    let mut buf = AudioBuffer::new(1, samples.len());
    buf.channel_mut(0).copy_from_slice(samples);
    let spec = WavSpec {
        sample_rate: RATE,
        channels: 1,
    };
    write_wav_mono(path, spec, &buf).unwrap();
}

/// An editor with `count` tracks: one audio track and `count - 1` MIDI ones.
fn tracks(dir: &Scratch, count: usize) -> Editor {
    let wav = dir.0.join("a.wav");
    write_wav(&wav, &vec![0.5; 1_000]);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    for _ in 1..count {
        editor.add_midi_track(0, 1_000).unwrap();
    }
    editor
}

#[test]
fn group_gain_is_one_undo_step() {
    let dir = Scratch::new("gain");
    let mut editor = tracks(&dir, 1);
    editor.duplicate_clip(0, 0).unwrap();
    editor.select_all_clips();
    let selected = editor.selected_clips();
    assert_eq!(selected.len(), 2);
    editor.set_clips_gain(&selected, 0.5).unwrap();
    for c in &editor.session().tracks[0].clips {
        assert!((c.gain - 0.5).abs() < 1e-6);
    }
    // The selection survives the edit.
    assert_eq!(editor.selected_clips(), selected);
    editor.undo();
    for c in &editor.session().tracks[0].clips {
        assert!((c.gain - 1.0).abs() < 1e-6);
    }
    // Out-of-range gain is clamped, NaN counts as silence.
    editor.set_clips_gain(&selected, 3.0).unwrap();
    assert!((editor.session().tracks[0].clips[1].gain - 1.0).abs() < 1e-6);
    editor.set_clips_gain(&selected, f32::NAN).unwrap();
    assert!(editor.session().tracks[0].clips[0].gain.abs() < 1e-6);
}

#[test]
fn group_gain_skips_bad_indices() {
    let dir = Scratch::new("gainbad");
    let mut editor = tracks(&dir, 1);
    assert!(editor.set_clips_gain(&[(5, 0), (0, 9)], 0.5).is_err());
    assert!(!editor.can_undo() || editor.session().tracks[0].clips[0].gain > 0.99);
    editor.set_clips_gain(&[(5, 0), (0, 0)], 0.25).unwrap();
    assert!((editor.session().tracks[0].clips[0].gain - 0.25).abs() < 1e-6);
    // Unchanged values add no undo step.
    let before = editor.session().clone();
    editor.set_clips_gain(&[(0, 0)], 0.25).unwrap();
    editor.undo();
    assert_ne!(editor.session(), &before);
}

#[test]
fn rename_clip_trims_and_undoes() {
    let dir = Scratch::new("rename");
    let mut editor = tracks(&dir, 1);
    let old = editor.session().tracks[0].clips[0].name.clone();
    editor.rename_clip(0, 0, "  Вокал дубль 2  ").unwrap();
    assert_eq!(editor.session().tracks[0].clips[0].name, "Вокал дубль 2");
    assert!(editor.rename_clip(0, 0, "   ").is_err());
    assert!(editor.rename_clip(0, 5, "x").is_err());
    assert!(editor.rename_clip(0, 0, &"x".repeat(10_000)).is_err());
    assert_eq!(editor.session().tracks[0].clips[0].name, "Вокал дубль 2");
    editor.undo();
    assert_eq!(editor.session().tracks[0].clips[0].name, old);
}
