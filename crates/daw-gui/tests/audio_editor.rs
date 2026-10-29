//! Acceptance tests for the audio clip editor's model side (SPEC-039).

use std::path::PathBuf;

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::{Editor, EditorError};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-aed-{tag}-{}", std::process::id()));
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

fn one_clip(dir: &Scratch) -> Editor {
    let wav = dir.0.join("a.wav");
    let mut buf = AudioBuffer::new(1, 1_000);
    buf.channel_mut(0).fill(0.5);
    let spec = WavSpec {
        sample_rate: 48_000,
        channels: 1,
    };
    write_wav_mono(&wav, spec, &buf).unwrap();
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor
}

fn gain(editor: &Editor) -> f32 {
    editor.session().tracks[0].clips[0].gain
}

#[test]
fn clip_gain_is_set_clamped_and_undoable() {
    let dir = Scratch::new("gain");
    let mut editor = one_clip(&dir);
    editor.set_clip_gain(0, 0, 0.25).unwrap();
    assert!((gain(&editor) - 0.25).abs() < f32::EPSILON);
    editor.set_clip_gain(0, 0, 3.0).unwrap();
    assert!((gain(&editor) - 1.0).abs() < f32::EPSILON);
    editor.set_clip_gain(0, 0, -1.0).unwrap();
    assert!(gain(&editor).abs() < f32::EPSILON);
    editor.set_clip_gain(0, 0, f32::NAN).unwrap();
    assert!(gain(&editor).abs() < f32::EPSILON);
    editor.undo();
    assert!((gain(&editor) - 1.0).abs() < f32::EPSILON);
    editor.undo();
    assert!((gain(&editor) - 0.25).abs() < f32::EPSILON);
}

#[test]
fn the_same_gain_adds_no_undo_step() {
    let dir = Scratch::new("same");
    let mut editor = one_clip(&dir);
    editor.set_clip_gain(0, 0, 0.5).unwrap();
    editor.set_clip_gain(0, 0, 0.5).unwrap();
    editor.undo();
    assert!((gain(&editor) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn gain_works_on_midi_clips_and_rejects_bad_indices() {
    let mut editor = Editor::new();
    let at = editor.add_midi_track(0, 48_000).unwrap();
    editor.set_clip_gain(at.0, at.1, 0.5).unwrap();
    assert!((editor.session().tracks[at.0].clips[at.1].gain - 0.5).abs() < f32::EPSILON);
    assert!(matches!(
        editor.set_clip_gain(9, 0, 0.5),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.set_clip_gain(at.0, 9, 0.5),
        Err(EditorError::NoClip)
    ));
}
