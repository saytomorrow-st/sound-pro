//! Acceptance tests for the cycle region in the editor (SPEC-025).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-cycle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &Path, len: usize) {
    let mut buf = AudioBuffer::new(1, len);
    for (i, s) in buf.channel_mut(0).iter_mut().enumerate() {
        *s = (i + 1) as f32 / 10_000.0;
    }
    write_wav_mono(
        path,
        WavSpec {
            sample_rate: RATE,
            channels: 1,
        },
        &buf,
    )
    .unwrap();
}

fn editor(dir: &Scratch) -> Editor {
    let a = dir.join("a.wav");
    write_wav(&a, 2_000);
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    editor
}

#[test]
fn a_new_editor_has_no_cycle() {
    let editor = Editor::new();
    assert_eq!(editor.cycle(), None);
    assert!(!editor.cycle_enabled);
    assert_eq!(editor.active_cycle(), None);
}

#[test]
fn setting_a_cycle_orders_its_ends_and_turns_it_on() {
    let mut editor = Editor::new();
    editor.set_cycle(900, 300).unwrap();
    assert_eq!(editor.cycle(), Some((300, 900)));
    assert!(editor.cycle_enabled);
    assert_eq!(editor.active_cycle(), Some((300, 900)));
    editor.cycle_enabled = false;
    assert_eq!(editor.cycle(), Some((300, 900)));
    assert_eq!(editor.active_cycle(), None);
}

#[test]
fn an_empty_cycle_is_rejected_and_keeps_the_old_one() {
    let mut editor = Editor::new();
    editor.set_cycle(100, 200).unwrap();
    assert!(matches!(
        editor.set_cycle(500, 500),
        Err(EditorError::EmptyCycle)
    ));
    assert_eq!(editor.cycle(), Some((100, 200)));
}

#[test]
fn clearing_removes_the_cycle() {
    let mut editor = Editor::new();
    editor.set_cycle(100, 200).unwrap();
    editor.clear_cycle();
    assert_eq!(editor.cycle(), None);
    assert!(!editor.cycle_enabled);
}

#[test]
fn the_cycle_is_not_part_of_undo_or_dirty_state() {
    let dir = Scratch::new("undo");
    let mut editor = editor(&dir);
    let dirty = editor.is_dirty();
    editor.set_cycle(100, 200).unwrap();
    assert_eq!(editor.is_dirty(), dirty);
    editor.undo();
    assert_eq!(editor.cycle(), Some((100, 200)));
}

#[test]
fn playback_outside_an_active_cycle_starts_at_its_start() {
    let dir = Scratch::new("play");
    let mut editor = editor(&dir);
    editor.set_cycle(500, 1_000).unwrap();
    let (control, _) = editor.playback(1_500).unwrap();
    assert_eq!(control.position(), 500);
    assert_eq!(control.cycle(), Some((500, 1_000)));
    let (control, _) = editor.playback(100).unwrap();
    assert_eq!(control.position(), 500);
    let (control, _) = editor.playback(700).unwrap();
    assert_eq!(control.position(), 700);
}

#[test]
fn playback_with_the_cycle_off_ignores_it() {
    let dir = Scratch::new("off");
    let mut editor = editor(&dir);
    editor.set_cycle(500, 1_000).unwrap();
    editor.cycle_enabled = false;
    let (control, _) = editor.playback(1_500).unwrap();
    assert_eq!(control.position(), 1_500);
    assert_eq!(control.cycle(), None);
}

#[test]
fn the_cycle_loops_the_editor_playback() {
    let dir = Scratch::new("loop");
    let mut editor = editor(&dir);
    editor.set_cycle(200, 300).unwrap();
    let (mut control, mut engine) = editor.playback(0).unwrap();
    control.play();
    let mut buffer = vec![0.0f32; 256 * 2];
    for _ in 0..40 {
        engine.fill(&mut buffer, 2);
    }
    assert!(control.is_playing());
    assert!((200..300).contains(&control.position()));
}

#[test]
fn opening_a_project_clears_the_cycle_and_saving_does_not_store_it() {
    let dir = Scratch::new("open");
    let mut editor = editor(&dir);
    editor.set_cycle(100, 200).unwrap();
    let path = dir.join("p.json");
    editor.save_as(&path).unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("cycle"));
    editor.open(&path).unwrap();
    assert_eq!(editor.cycle(), None);
    assert!(!editor.cycle_enabled);
}
