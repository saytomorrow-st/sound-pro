//! Acceptance tests for adding microphone takes to a project (SPEC-020).

use std::path::PathBuf;

use audio_core::wav::read_wav_mono;
use audio_core::{MAX_CLIPS_PER_TRACK, SessionError};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-rec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn take(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i % 100) as f32 / 200.0).collect()
}

#[test]
fn a_take_becomes_a_clip_on_a_new_track_and_a_wav_file() {
    let dir = Scratch::new("new");
    let mut editor = Editor::new();
    let samples = take(4800);
    let (t, c) = editor
        .add_recording(&samples, RATE, 9600, &dir.0)
        .expect("added");
    let session = editor.session();
    assert_eq!(session.tracks.len(), 1);
    assert_eq!(session.tracks[t].name, "Запись");
    let clip = &session.tracks[t].clips[c];
    assert_eq!(clip.start_samples, 9600);
    assert_eq!(clip.length_samples, 4800);
    let file = dir.0.join("rec-001.wav");
    assert_eq!(clip.source.as_deref(), file.to_str());
    let (spec, buf) = read_wav_mono(&file).expect("written");
    assert_eq!(spec.sample_rate, RATE);
    assert_eq!(buf.channel(0), samples.as_slice());
    assert_eq!(editor.selected_clip, Some((t, c)));
    assert!(editor.is_dirty());
    assert!(editor.peaks(clip).is_some());
}

#[test]
fn a_take_goes_on_the_selected_track_with_the_next_free_number() {
    let dir = Scratch::new("next");
    let mut editor = Editor::new();
    editor
        .add_recording(&take(100), RATE, 0, &dir.0)
        .expect("first");
    editor.selected_track = Some(0);
    let (t, c) = editor
        .add_recording(&take(100), RATE, 500, &dir.0)
        .expect("second");
    assert_eq!((t, c), (0, 1));
    assert_eq!(editor.session().tracks.len(), 1);
    assert!(dir.0.join("rec-002.wav").exists());
    assert_eq!(
        editor.session().tracks[0].clips[1].source.as_deref(),
        dir.0.join("rec-002.wav").to_str()
    );
}

#[test]
fn adding_a_take_is_one_undo_step() {
    let dir = Scratch::new("undo");
    let mut editor = Editor::new();
    editor
        .add_recording(&take(100), RATE, 0, &dir.0)
        .expect("added");
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty());
    assert!(!editor.can_undo());
    assert!(editor.redo());
    assert_eq!(editor.session().tracks.len(), 1);
}

#[test]
fn a_take_at_another_rate_is_converted() {
    let dir = Scratch::new("rate");
    let mut editor = Editor::new();
    let (t, c) = editor
        .add_recording(&take(44_100), 44_100, 0, &dir.0)
        .expect("added");
    let length = editor.session().tracks[t].clips[c].length_samples;
    assert!((47_990..=48_010).contains(&length), "length {length}");
    let (spec, _) = read_wav_mono(dir.0.join("rec-001.wav")).expect("written");
    assert_eq!(spec.sample_rate, RATE);
}

#[test]
fn an_empty_take_changes_nothing() {
    let dir = Scratch::new("empty");
    let mut editor = Editor::new();
    let err = editor.add_recording(&[], RATE, 0, &dir.0).unwrap_err();
    assert!(matches!(err, EditorError::EmptyRecording), "{err}");
    assert!(editor.session().tracks.is_empty());
    assert!(!editor.is_dirty());
    assert!(!dir.0.exists());
}

#[test]
fn the_clip_limit_rejects_a_take_without_writing_a_file() {
    let dir = Scratch::new("limit");
    let mut editor = Editor::new();
    editor
        .add_recording(&take(10), RATE, 0, &dir.0)
        .expect("first");
    editor.selected_track = Some(0);
    for i in 1..MAX_CLIPS_PER_TRACK {
        editor
            .add_recording(&take(10), RATE, i as u64 * 10, &dir.0)
            .expect("fill");
    }
    let files = std::fs::read_dir(&dir.0).unwrap().count();
    let err = editor
        .add_recording(&take(10), RATE, 0, &dir.0)
        .unwrap_err();
    assert!(
        matches!(err, EditorError::Session(SessionError::TooManyClips { .. })),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), files);
}

#[test]
fn recordings_go_next_to_a_saved_project() {
    let dir = Scratch::new("dir");
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut editor = Editor::new();
    assert!(editor.recordings_dir().starts_with(std::env::temp_dir()));
    let project = dir.0.join("song.json");
    editor.save_as(&project).expect("saved");
    assert_eq!(
        editor.recordings_dir(),
        std::path::absolute(&dir.0).unwrap().join("audio")
    );
}
