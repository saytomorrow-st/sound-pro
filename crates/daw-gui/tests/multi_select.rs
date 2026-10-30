//! Acceptance tests for selecting several tracks and regions (SPEC-047).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::Editor;

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-msel-{tag}-{}", std::process::id()));
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

fn starts(editor: &Editor, track: usize) -> Vec<u64> {
    editor.session().tracks[track]
        .clips
        .iter()
        .map(|c| c.start_samples)
        .collect()
}

/// Track 0 holds clips at 0 and 2000; tracks 1 and 2 hold one MIDI clip at 0.
fn arrange(dir: &Scratch) -> Editor {
    let mut editor = tracks(dir, 3);
    editor.select_clip(Some((0, 0)), false);
    editor.duplicate_clip(0, 0).unwrap();
    editor.move_clip(0, 1, 2_000, 0).unwrap();
    editor
}

/// Like [`arrange`], but every track is an audio track, so clips may move
/// between rows (SPEC-049 §4).
fn audio_arrange(dir: &Scratch) -> Editor {
    let mut editor = tracks(dir, 1);
    editor
        .add_tracks(audio_core::TrackKind::Audio, 2, "")
        .unwrap();
    editor.copy_clip(0, 0).unwrap();
    editor.paste_clip(1, 0).unwrap();
    editor.paste_clip(2, 0).unwrap();
    editor.select_clip(Some((0, 0)), false);
    editor.duplicate_clip(0, 0).unwrap();
    editor.move_clip(0, 1, 2_000, 0).unwrap();
    editor
}

#[test]
fn toggle_adds_and_removes_clips() {
    let dir = Scratch::new("toggle");
    let mut editor = arrange(&dir);
    editor.select_clip(Some((0, 0)), false);
    editor.select_clip(Some((1, 0)), true);
    editor.select_clip(Some((2, 0)), true);
    assert_eq!(editor.selected_clips(), vec![(0, 0), (1, 0), (2, 0)]);
    assert_eq!(editor.selected_clip, Some((2, 0)));
    editor.select_clip(Some((1, 0)), true);
    assert_eq!(editor.selected_clips(), vec![(0, 0), (2, 0)]);
    assert!(!editor.is_clip_selected((1, 0)));
    // Removing the primary clip promotes another one.
    editor.select_clip(Some((2, 0)), true);
    assert_eq!(editor.selected_clips(), vec![(0, 0)]);
    assert_eq!(editor.selected_clip, Some((0, 0)));
    // A plain click replaces the selection.
    editor.select_clip(Some((1, 0)), false);
    assert_eq!(editor.selected_clips(), vec![(1, 0)]);
    editor.select_clip(None, false);
    assert!(editor.selected_clips().is_empty());
    // Bad indices do nothing.
    editor.select_clip(Some((9, 9)), true);
    assert!(editor.selected_clips().is_empty());
}

#[test]
fn rubber_band_selects_touched_clips() {
    let dir = Scratch::new("band");
    let mut editor = arrange(&dir);
    // Tracks 0..=1, samples 500..2500: both clips of track 0 and track 1.
    assert_eq!(editor.select_clips_in(0..=1, 2_500, 500, false), 3);
    assert_eq!(editor.selected_clips(), vec![(0, 0), (0, 1), (1, 0)]);
    // A band right of everything selects nothing and clears the selection.
    assert_eq!(editor.select_clips_in(0..=2, 10_000, 20_000, false), 0);
    assert!(editor.selected_clips().is_empty());
    // Additive bands keep what was selected.
    editor.select_clip(Some((2, 0)), false);
    editor.select_clips_in(0..=0, 2_100, 2_200, true);
    assert_eq!(editor.selected_clips(), vec![(0, 1), (2, 0)]);
    editor.select_all_clips();
    assert_eq!(editor.selected_clips().len(), 4);
}

#[test]
fn group_delete_is_one_undo_step() {
    let dir = Scratch::new("delete");
    let mut editor = arrange(&dir);
    editor.select_clip(Some((0, 0)), false);
    editor.select_clip(Some((0, 1)), true);
    editor.select_clip(Some((2, 0)), true);
    assert_eq!(editor.delete_selected_clips(), 3);
    assert!(editor.session().tracks[0].clips.is_empty());
    assert_eq!(editor.session().tracks[1].clips.len(), 1);
    assert!(editor.session().tracks[2].clips.is_empty());
    assert!(editor.selected_clips().is_empty());
    assert_eq!(editor.delete_selected_clips(), 0);
    assert!(editor.undo());
    assert_eq!(starts(&editor, 0), vec![0, 2_000]);
    assert_eq!(editor.session().tracks[2].clips.len(), 1);
}

#[test]
fn group_move_keeps_offsets_and_undoes_at_once() {
    let dir = Scratch::new("move");
    let mut editor = arrange(&dir);
    editor.move_clip(1, 0, 500, 1).unwrap();
    editor.select_clip(Some((0, 1)), false);
    editor.select_clip(Some((1, 0)), true);
    editor.move_selected_clips(1_000, 0).unwrap();
    assert_eq!(starts(&editor, 0), vec![0, 3_000]);
    assert_eq!(starts(&editor, 1), vec![1_500]);
    assert_eq!(editor.selected_clips(), vec![(0, 1), (1, 0)]);
    // The shift stops when the earliest clip reaches zero.
    editor.move_selected_clips(-100_000, 0).unwrap();
    assert_eq!(starts(&editor, 0), vec![0, 1_500]);
    assert_eq!(starts(&editor, 1), vec![0]);
    assert!(editor.undo());
    assert_eq!(starts(&editor, 0), vec![0, 3_000]);
    assert!(editor.undo());
    assert_eq!(starts(&editor, 0), vec![0, 2_000]);
    assert_eq!(starts(&editor, 1), vec![500]);
}

#[test]
fn group_move_across_tracks_follows_selection() {
    let dir = Scratch::new("rows");
    let mut editor = audio_arrange(&dir);
    editor.select_clip(Some((0, 1)), false);
    editor.select_clip(Some((1, 0)), true);
    // Down by one row: track 0's clip goes to 1, track 1's to 2.
    editor.move_selected_clips(0, 1).unwrap();
    assert_eq!(starts(&editor, 0), vec![0]);
    assert_eq!(starts(&editor, 1), vec![2_000]);
    assert_eq!(starts(&editor, 2), vec![0, 0]);
    assert_eq!(editor.selected_clips(), vec![(1, 0), (2, 1)]);
    assert_eq!(editor.selected_clip, Some((2, 1)));
    // The last row stops the group.
    editor.move_selected_clips(0, 5).unwrap();
    assert_eq!(editor.session().tracks[1].clips.len(), 1);
    assert!(editor.undo());
    assert_eq!(starts(&editor, 0), vec![0, 2_000]);
    assert_eq!(editor.session().tracks[2].clips.len(), 1);
    assert!(matches!(
        Editor::new().move_selected_clips(10, 0),
        Err(daw_gui::EditorError::NoClip)
    ));
}

#[test]
fn stale_selection_is_dropped_after_undo() {
    let dir = Scratch::new("stale");
    let mut editor = arrange(&dir);
    editor.select_clip(Some((0, 0)), false);
    editor.duplicate_clip(0, 1).unwrap();
    editor.select_clip(Some((0, 2)), false);
    editor.select_clip(Some((1, 0)), true);
    assert!(editor.undo());
    assert_eq!(editor.selected_clips(), vec![(1, 0)]);
}

#[test]
fn tracks_select_with_ctrl_and_shift() {
    let dir = Scratch::new("tracks");
    let mut editor = tracks(&dir, 5);
    editor.select_track(1, false, false);
    editor.select_track(3, true, false);
    assert_eq!(editor.selected_tracks(), vec![1, 3]);
    editor.select_track(1, true, false);
    assert_eq!(editor.selected_tracks(), vec![3]);
    assert_eq!(editor.selected_track, Some(3));
    // Shift selects the range to the primary track.
    editor.select_track(0, false, true);
    assert_eq!(editor.selected_tracks(), vec![0, 1, 2, 3]);
    assert!(editor.is_track_selected(2));
    editor.select_track(4, false, false);
    assert_eq!(editor.selected_tracks(), vec![4]);
    editor.delete_track(4);
    assert!(editor.selected_tracks().is_empty());
}
