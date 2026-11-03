//! Acceptance tests for undo and redo (SPEC-018).

use std::path::{Path, PathBuf};

use audio_core::MAX_CLIPS_PER_TRACK;
use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use audio_core::{TrackInstrument, TrackKind};
use daw_gui::{Editor, UNDO_LIMIT};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-undo-{tag}-{}", std::process::id()));
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

/// An editor with one track holding one 1000-sample clip, saved and
/// reopened so the dirty flag and the history start clear.
fn one_clip(dir: &Scratch) -> Editor {
    let wav = dir.join("a.wav");
    write_wav(&wav, &ramp(1_000));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();
    editor.open(&dir.join("song.json")).unwrap();
    assert!(!editor.is_dirty());
    assert!(!editor.can_undo());
    editor
}

/// Two tracks, one clip each.
fn two_tracks(dir: &Scratch) -> Editor {
    let mut editor = one_clip(dir);
    let wav = dir.join("b.wav");
    write_wav(&wav, &ramp(400));
    editor.selected_track = None;
    editor.import_wav(&wav).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();
    editor.open(&dir.join("song.json")).unwrap();
    editor
}

fn start(editor: &Editor) -> u64 {
    editor.session().tracks[0].clips[0].start_samples
}

#[test]
fn nothing_to_undo_or_redo_at_first() {
    let mut editor = Editor::new();
    assert!(!editor.can_undo());
    assert!(!editor.can_redo());
    assert!(!editor.undo());
    assert!(!editor.redo());
    assert!(!editor.is_dirty());
}

#[test]
fn undo_and_redo_a_move() {
    let dir = Scratch::new("move");
    let mut editor = one_clip(&dir);
    let before = editor.session().clone();
    editor.move_clip(0, 0, 500, 0).unwrap();
    let after = editor.session().clone();
    assert!(editor.can_undo());

    assert!(editor.undo());
    assert_eq!(editor.session(), &before);
    assert!(editor.can_redo());

    assert!(editor.redo());
    assert_eq!(editor.session(), &after);
    assert!(!editor.can_redo());
}

type Edit<'a> = Box<dyn Fn(&mut Editor) + 'a>;

#[test]
fn every_kind_of_edit_can_be_undone() {
    let dir = Scratch::new("kinds");
    let wav = dir.join("c.wav");
    write_wav(&wav, &ramp(300));
    let edits: Vec<(&str, Edit)> = vec![
        (
            "import",
            Box::new(|e: &mut Editor| {
                e.import_wav(&wav).unwrap();
            }),
        ),
        (
            "delete_clip",
            Box::new(|e: &mut Editor| e.delete_clip(0, 0)),
        ),
        ("delete_track", Box::new(|e: &mut Editor| e.delete_track(1))),
        (
            "move",
            Box::new(|e: &mut Editor| {
                e.move_clip(0, 0, 70, 1).unwrap();
            }),
        ),
        (
            "trim_start",
            Box::new(|e: &mut Editor| e.trim_start(0, 0, 100).unwrap()),
        ),
        (
            "trim_end",
            Box::new(|e: &mut Editor| e.trim_end(0, 0, 100).unwrap()),
        ),
        (
            "track_mut",
            Box::new(|e: &mut Editor| e.track_mut(0).unwrap().mute = true),
        ),
        ("master", Box::new(|e: &mut Editor| e.set_master_gain(0.25))),
    ];
    for (name, edit) in edits {
        let mut editor = two_tracks(&dir);
        let before = editor.session().clone();
        edit(&mut editor);
        assert_ne!(editor.session(), &before, "{name} changed nothing");
        assert!(editor.undo(), "{name} left no step");
        assert_eq!(editor.session(), &before, "{name} was not undone");
        assert!(!editor.can_undo(), "{name} left extra steps");
    }
}

#[test]
fn failed_edits_add_no_step() {
    let dir = Scratch::new("failed");
    let mut editor = two_tracks(&dir);
    while editor.session().tracks[1].clips.len() < MAX_CLIPS_PER_TRACK {
        let extra = editor.session().tracks[1].clips[0].clone();
        editor.track_mut(1).unwrap().clips.push(extra);
    }
    // Start from a clean history holding the full track.
    editor.save_as(&dir.join("full.json")).unwrap();
    editor.open(&dir.join("full.json")).unwrap();
    assert!(!editor.can_undo());

    assert!(editor.move_clip(0, 0, 0, 1).is_err());
    assert!(editor.move_clip(9, 0, 0, 0).is_err());
    assert!(editor.trim_start(0, 9, 0).is_err());
    assert!(editor.trim_end(5, 0, 0).is_err());
    editor.delete_clip(0, 9);
    editor.delete_track(9);
    assert!(editor.track_mut(9).is_none());
    assert!(!editor.can_undo());
}

#[test]
fn a_new_edit_clears_redo() {
    let dir = Scratch::new("redo");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 500, 0).unwrap();
    editor.undo();
    assert!(editor.can_redo());
    editor.move_clip(0, 0, 200, 0).unwrap();
    assert!(!editor.can_redo());
    assert!(!editor.redo());
    assert_eq!(start(&editor), 200);
}

#[test]
fn history_keeps_the_last_hundred_steps() {
    let dir = Scratch::new("limit");
    let mut editor = one_clip(&dir);
    for i in 1..=UNDO_LIMIT as u64 + 1 {
        editor.move_clip(0, 0, i, 0).unwrap();
    }
    let mut undone = 0;
    while editor.undo() {
        undone += 1;
    }
    assert_eq!(undone, UNDO_LIMIT);
    assert_eq!(start(&editor), 1);
}

#[test]
fn a_group_is_one_step() {
    let dir = Scratch::new("group");
    let mut editor = one_clip(&dir);
    editor.begin_group();
    for x in [10, 20, 30, 40] {
        editor.move_clip(0, 0, x, 0).unwrap();
    }
    editor.begin_group();
    editor.trim_end(0, 0, 500).unwrap();
    editor.end_group();
    assert!(editor.undo());
    assert_eq!(start(&editor), 0);
    assert_eq!(editor.session().tracks[0].clips[0].length_samples, 1_000);
    assert!(!editor.can_undo());
    assert!(editor.redo());
    assert_eq!(start(&editor), 40);
}

#[test]
fn an_empty_group_adds_no_step() {
    let mut editor = Editor::new();
    editor.begin_group();
    editor.end_group();
    editor.end_group();
    assert!(!editor.can_undo());
}

#[test]
fn undo_clears_a_selection_that_no_longer_exists() {
    let dir = Scratch::new("selection");
    let mut editor = one_clip(&dir);
    let wav = dir.join("b.wav");
    write_wav(&wav, &ramp(400));
    editor.selected_track = None;
    let at = editor.import_wav(&wav).unwrap();
    assert_eq!(editor.selected_clip, Some(at));
    editor.undo();
    assert_eq!(editor.selected_clip, None);
    assert_eq!(editor.selected_track, None);

    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 10, 0).unwrap();
    editor.undo();
    assert_eq!(editor.selected_clip, Some((0, 0)));
}

#[test]
fn dirty_follows_the_saved_version() {
    let dir = Scratch::new("dirty");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 10, 0).unwrap();
    assert!(editor.is_dirty());
    editor.undo();
    assert!(!editor.is_dirty(), "back at the saved state");
    editor.redo();
    assert!(editor.is_dirty());

    editor.save_as(&dir.join("other.json")).unwrap();
    assert!(editor.can_undo(), "saving keeps history");
    let name = editor.session().name.clone();
    editor.undo();
    assert!(editor.is_dirty());
    assert_eq!(editor.session().name, name);
    editor.redo();
    assert!(!editor.is_dirty());
}

#[test]
fn new_and_open_clear_history() {
    let dir = Scratch::new("clear");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 10, 0).unwrap();
    editor.open(&dir.join("song.json")).unwrap();
    assert!(!editor.can_undo());
    editor.move_clip(0, 0, 10, 0).unwrap();
    editor = Editor::new();
    assert!(!editor.can_undo());
}

#[test]
fn redo_of_an_import_keeps_the_waveform() {
    let dir = Scratch::new("wave");
    let mut editor = one_clip(&dir);
    editor.delete_clip(0, 0);
    editor.undo();
    let clip = &editor.session().tracks[0].clips[0];
    assert!(editor.peaks(clip).is_some());
}

#[test]
fn undone_project_sounds_like_the_original() {
    let dir = Scratch::new("sound");
    let mut editor = two_tracks(&dir);
    let before = dir.join("before.wav");
    editor.export(&before).unwrap();
    editor.move_clip(1, 0, 900, 0).unwrap();
    editor.trim_end(0, 0, 300).unwrap();
    editor.set_master_gain(0.1);
    editor.undo();
    editor.undo();
    editor.undo();
    let after = dir.join("after.wav");
    editor.export(&after).unwrap();
    assert_eq!(
        std::fs::read(&before).unwrap(),
        std::fs::read(&after).unwrap()
    );
}

#[test]
fn instrument_change_is_one_undoable_step() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "lead").unwrap();
    let before = editor.session().clone();

    // Setting a sampler instrument is one undo step.
    editor
        .set_track_instrument(
            0,
            TrackInstrument::Sampler {
                bank_path: "/tmp/example-bank".to_owned(),
            },
        )
        .unwrap();
    assert_ne!(editor.session(), &before);
    assert!(editor.can_undo());
    assert!(editor.is_dirty());

    assert!(editor.undo());
    assert_eq!(editor.session(), &before);
    assert!(editor.can_redo());
    assert!(editor.redo());
    assert!(matches!(
        &editor.session().tracks[0].instrument,
        TrackInstrument::Sampler { .. }
    ));

    // Switching back to the built-in piano is its own undo step.
    editor
        .set_track_instrument(0, TrackInstrument::BuiltinPiano)
        .unwrap();
    assert_eq!(editor.session(), &before);
    assert!(editor.can_undo());
    assert!(editor.undo());
    assert!(matches!(
        &editor.session().tracks[0].instrument,
        TrackInstrument::Sampler { .. }
    ));
}

#[test]
fn setting_the_same_instrument_adds_no_step() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "lead").unwrap();
    let path = std::env::temp_dir().join(format!("daw-instr-{}.json", std::process::id()));
    editor.save_as(&path).unwrap();
    editor.open(&path).unwrap();
    assert!(!editor.can_undo());

    editor
        .set_track_instrument(
            0,
            TrackInstrument::Sampler {
                bank_path: "/tmp/bank".to_owned(),
            },
        )
        .unwrap();
    assert!(editor.can_undo());
    editor
        .set_track_instrument(
            0,
            TrackInstrument::Sampler {
                bank_path: "/tmp/bank".to_owned(),
            },
        )
        .unwrap();
    // No new step: the second set was a no-op.
    assert!(editor.undo());
    assert!(!editor.can_undo());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn instrument_change_is_rejected_on_audio_tracks() {
    let dir = Scratch::new("audio");
    let mut editor = one_clip(&dir);
    assert!(
        editor
            .set_track_instrument(0, TrackInstrument::BuiltinPiano)
            .is_err()
    );
}
