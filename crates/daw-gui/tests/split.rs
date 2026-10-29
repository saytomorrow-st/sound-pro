//! Acceptance tests for splitting, copying and duplicating clips (SPEC-027).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use audio_core::{Clip, MAX_CLIPS_PER_TRACK, Note, SessionError};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-split-{tag}-{}", std::process::id()));
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

fn write_wav(path: &Path, samples: &[f32]) {
    let mut buf = AudioBuffer::new(1, samples.len());
    buf.channel_mut(0).copy_from_slice(samples);
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

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i + 1) as f32 / 10_000.0).collect()
}

/// One track with one 1000-sample clip, saved so the dirty flag is clear.
fn one_clip(dir: &Scratch) -> Editor {
    let wav = dir.join("a.wav");
    write_wav(&wav, &ramp(1_000));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();
    assert!(!editor.is_dirty());
    editor
}

fn clip(editor: &Editor, track: usize, index: usize) -> &Clip {
    &editor.session().tracks[track].clips[index]
}

fn window(c: &Clip) -> (u64, u64, u64) {
    (c.start_samples, c.offset_samples, c.length_samples)
}

fn fill_track(editor: &mut Editor, track: usize) {
    let spare = clip(editor, track, 0).clone();
    let target = editor.track_mut(track).unwrap();
    while target.clips.len() < MAX_CLIPS_PER_TRACK {
        target.clips.push(spare.clone());
    }
}

fn render(editor: &Editor, out: &Path) -> AudioBuffer {
    editor.export(out).unwrap();
    read_wav_mono(out).unwrap().1
}

#[test]
fn splitting_makes_two_adjacent_parts() {
    let dir = Scratch::new("parts");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 500, 0).unwrap();
    assert_eq!(editor.split_clip(0, 0, 800).unwrap(), (0, 1));
    assert_eq!(window(clip(&editor, 0, 0)), (500, 0, 300));
    assert_eq!(window(clip(&editor, 0, 1)), (800, 300, 700));
    assert_eq!(editor.selected_clip, Some((0, 1)));
    let (left, right) = (clip(&editor, 0, 0), clip(&editor, 0, 1));
    assert_eq!(left.source, right.source);
    assert_eq!(left.source_length, right.source_length);
    assert_eq!(left.name, right.name);
    assert!(editor.is_dirty());
}

#[test]
fn splitting_a_trimmed_clip_respects_its_offset() {
    let dir = Scratch::new("trimmed");
    let mut editor = one_clip(&dir);
    editor.trim_start(0, 0, 200).unwrap();
    editor.trim_end(0, 0, 900).unwrap();
    assert_eq!(window(clip(&editor, 0, 0)), (200, 200, 700));
    editor.split_clip(0, 0, 500).unwrap();
    assert_eq!(window(clip(&editor, 0, 0)), (200, 200, 300));
    assert_eq!(window(clip(&editor, 0, 1)), (500, 500, 400));
}

#[test]
fn splitting_keeps_the_sound_sample_for_sample() {
    let dir = Scratch::new("sound");
    let mut editor = one_clip(&dir);
    editor.track_mut(0).unwrap().clips[0].gain = 0.5;
    let before = render(&editor, &dir.join("before.wav"));
    editor.split_clip(0, 0, 333).unwrap();
    editor.split_clip(0, 1, 777).unwrap();
    assert_eq!(editor.session().tracks[0].clips.len(), 3);
    let after = render(&editor, &dir.join("after.wav"));
    assert_eq!(before.channel(0), after.channel(0));
    assert_eq!(before.channel(1), after.channel(1));
}

#[test]
fn splitting_at_or_outside_the_edges_is_refused() {
    let dir = Scratch::new("edges");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 100, 0).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();
    let before = editor.session().clone();
    for at in [0, 100, 1_100, 5_000] {
        assert!(matches!(
            editor.split_clip(0, 0, at),
            Err(EditorError::SplitOutside)
        ));
    }
    assert!(matches!(
        editor.split_clip(0, 1, 500),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.split_clip(3, 0, 500),
        Err(EditorError::NoClip)
    ));
    assert_eq!(editor.session(), &before);
    assert!(!editor.is_dirty());
}

#[test]
fn splitting_a_midi_clip_keeps_every_note_in_both_parts() {
    let mut editor = Editor::new();
    editor.add_midi_track(1_000, 4_000).unwrap();
    for start in [0, 3_000] {
        let note = Note {
            pitch: 60,
            start,
            length: 500,
            velocity: 100,
        };
        editor.add_note(0, 0, note).unwrap();
    }
    editor.split_clip(0, 0, 3_000).unwrap();
    let (left, right) = (clip(&editor, 0, 0), clip(&editor, 0, 1));
    assert_eq!(window(left), (1_000, 0, 2_000));
    assert_eq!(window(right), (3_000, 2_000, 2_000));
    assert_eq!(left.notes, right.notes);
    assert_eq!(left.notes.as_ref().unwrap().len(), 2);
}

#[test]
fn a_full_track_refuses_a_split() {
    let dir = Scratch::new("full-split");
    let mut editor = one_clip(&dir);
    fill_track(&mut editor, 0);
    let before = editor.session().clone();
    assert!(matches!(
        editor.split_clip(0, 0, 500),
        Err(EditorError::Session(SessionError::TooManyClips { .. }))
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn copying_leaves_the_project_untouched() {
    let dir = Scratch::new("copy");
    let mut editor = one_clip(&dir);
    assert!(!editor.has_clipboard());
    let before = editor.session().clone();
    editor.copy_clip(0, 0).unwrap();
    assert!(editor.has_clipboard());
    assert!(!editor.is_dirty());
    assert_eq!(editor.session(), &before);
    // Copying adds no undo step: one undo goes past the import.
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty());
    assert!(matches!(editor.copy_clip(0, 5), Err(EditorError::NoClip)));
    assert!(editor.has_clipboard());
}

#[test]
fn pasting_adds_the_copy_at_the_position_again_and_again() {
    let dir = Scratch::new("paste");
    let mut editor = one_clip(&dir);
    editor.trim_end(0, 0, 600).unwrap();
    editor.copy_clip(0, 0).unwrap();
    assert_eq!(editor.paste_clip(0, 2_000).unwrap(), (0, 1));
    assert_eq!(editor.paste_clip(0, 3_000).unwrap(), (0, 2));
    assert_eq!(window(clip(&editor, 0, 1)), (2_000, 0, 600));
    assert_eq!(window(clip(&editor, 0, 2)), (3_000, 0, 600));
    assert_eq!(clip(&editor, 0, 1).source, clip(&editor, 0, 0).source);
    assert_eq!(editor.selected_clip, Some((0, 2)));
    assert!(editor.is_dirty());
}

#[test]
fn pasting_onto_another_track_works() {
    let dir = Scratch::new("paste-other");
    let mut editor = one_clip(&dir);
    editor.add_midi_track(0, 100).unwrap();
    editor.copy_clip(0, 0).unwrap();
    assert_eq!(editor.paste_clip(1, 50).unwrap(), (1, 1));
    assert_eq!(window(clip(&editor, 1, 1)), (50, 0, 1_000));
    assert!(!clip(&editor, 1, 1).is_midi());
}

#[test]
fn pasting_errors_leave_the_project_unchanged() {
    let dir = Scratch::new("paste-errors");
    let mut editor = one_clip(&dir);
    let before = editor.session().clone();
    assert!(matches!(
        editor.paste_clip(0, 0),
        Err(EditorError::EmptyClipboard)
    ));
    editor.copy_clip(0, 0).unwrap();
    assert!(matches!(editor.paste_clip(4, 0), Err(EditorError::NoClip)));
    assert!(matches!(
        editor.paste_clip(0, u64::MAX - 10),
        Err(EditorError::Session(_))
    ));
    fill_track(&mut editor, 0);
    let full = editor.session().clone();
    assert!(matches!(
        editor.paste_clip(0, 0),
        Err(EditorError::Session(SessionError::TooManyClips { .. }))
    ));
    assert_eq!(editor.session(), &full);
    assert_eq!(full.tracks[0].clips[0], before.tracks[0].clips[0]);
}

#[test]
fn duplicating_places_the_copy_right_after_the_original() {
    let dir = Scratch::new("dup");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 250, 0).unwrap();
    editor.trim_start(0, 0, 300).unwrap();
    assert_eq!(editor.duplicate_clip(0, 0).unwrap(), (0, 1));
    assert_eq!(window(clip(&editor, 0, 0)), (300, 50, 950));
    assert_eq!(window(clip(&editor, 0, 1)), (1_250, 50, 950));
    assert_eq!(editor.duplicate_clip(0, 1).unwrap(), (0, 2));
    assert_eq!(clip(&editor, 0, 2).start_samples, 2_200);
    assert_eq!(editor.selected_clip, Some((0, 2)));
    assert!(!editor.has_clipboard());
}

#[test]
fn duplicating_errors_leave_the_project_unchanged() {
    let dir = Scratch::new("dup-errors");
    let mut editor = one_clip(&dir);
    assert!(matches!(
        editor.duplicate_clip(0, 1),
        Err(EditorError::NoClip)
    ));
    fill_track(&mut editor, 0);
    let before = editor.session().clone();
    assert!(matches!(
        editor.duplicate_clip(0, 0),
        Err(EditorError::Session(SessionError::TooManyClips { .. }))
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn each_edit_is_one_undo_step() {
    let dir = Scratch::new("undo");
    let mut editor = one_clip(&dir);
    let original = editor.session().clone();
    editor.split_clip(0, 0, 400).unwrap();
    let split = editor.session().clone();
    editor.copy_clip(0, 1).unwrap();
    editor.paste_clip(0, 2_000).unwrap();
    let pasted = editor.session().clone();
    editor.duplicate_clip(0, 2).unwrap();

    assert!(editor.undo());
    assert_eq!(editor.session(), &pasted);
    assert!(editor.undo());
    assert_eq!(editor.session(), &split);
    assert!(editor.undo());
    assert_eq!(editor.session(), &original);
    assert!(editor.redo() && editor.redo() && editor.redo());
    assert_eq!(editor.session().tracks[0].clips.len(), 4);
}

#[test]
fn the_clipboard_is_cleared_by_opening_another_project() {
    let dir = Scratch::new("open");
    let mut editor = one_clip(&dir);
    editor.copy_clip(0, 0).unwrap();
    editor.open(&dir.join("song.json")).unwrap();
    assert!(!editor.has_clipboard());
}
