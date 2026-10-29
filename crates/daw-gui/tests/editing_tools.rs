//! Acceptance tests for fades, joining and record arming (SPEC-038).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use audio_core::{Clip, Note};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-tools-{tag}-{}", std::process::id()));
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

fn one_clip(dir: &Scratch, name: &str) -> Editor {
    let wav = dir.0.join(name);
    write_wav(&wav, &vec![0.5; 1_000]);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor
}

fn clip(editor: &Editor, track: usize, index: usize) -> &Clip {
    &editor.session().tracks[track].clips[index]
}

fn note(pitch: u8, start: u64, length: u64) -> Note {
    Note {
        pitch,
        start,
        length,
        velocity: 100,
    }
}

#[test]
fn set_fades_clamps_to_the_clip_and_is_one_undo_step() {
    let dir = Scratch::new("fades");
    let mut editor = one_clip(&dir, "a.wav");
    editor.set_fades(0, 0, 100, 200).unwrap();
    let c = clip(&editor, 0, 0);
    assert_eq!((c.fade_in_samples, c.fade_out_samples), (100, 200));
    editor.set_fades(0, 0, 700, 700).unwrap();
    let c = clip(&editor, 0, 0);
    assert_eq!((c.fade_in_samples, c.fade_out_samples), (700, 300));
    editor.undo();
    let c = clip(&editor, 0, 0);
    assert_eq!((c.fade_in_samples, c.fade_out_samples), (100, 200));
    assert!(matches!(
        editor.set_fades(0, 5, 1, 1),
        Err(EditorError::NoClip)
    ));
}

#[test]
fn split_parts_of_one_file_join_back_into_one_clip() {
    let dir = Scratch::new("join");
    let mut editor = one_clip(&dir, "a.wav");
    editor.set_fades(0, 0, 10, 20).unwrap();
    editor.split_clip(0, 0, 400).unwrap();
    assert_eq!(clip(&editor, 0, 0).fade_out_samples, 0);
    let original = editor.session().clone();
    assert_eq!(editor.join_clips(0, 0).unwrap(), (0, 0));
    let tracks = &editor.session().tracks;
    assert_eq!(tracks[0].clips.len(), 1);
    let c = clip(&editor, 0, 0);
    assert_eq!(
        (c.start_samples, c.offset_samples, c.length_samples),
        (0, 0, 1_000)
    );
    assert_eq!((c.fade_in_samples, c.fade_out_samples), (10, 20));
    editor.undo();
    assert_eq!(editor.session(), &original);
}

#[test]
fn clips_that_do_not_continue_each_other_cannot_join() {
    let dir = Scratch::new("nojoin");
    let mut editor = one_clip(&dir, "a.wav");
    assert!(matches!(
        editor.join_clips(0, 0),
        Err(EditorError::CannotJoin)
    ));
    editor.split_clip(0, 0, 400).unwrap();
    editor.move_clip(0, 1, 450, 0).unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.join_clips(0, 0),
        Err(EditorError::CannotJoin)
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn midi_clips_join_with_their_notes_and_the_gap() {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 1_000).unwrap();
    editor.add_note(0, 0, note(60, 100, 50)).unwrap();
    let mut second = Clip::midi(2_000, 1_000);
    second.notes = Some(vec![note(64, 10, 20)]);
    editor.track_mut(0).unwrap().clips.push(second);
    assert_eq!(editor.join_clips(0, 0).unwrap(), (0, 0));
    let c = clip(&editor, 0, 0);
    assert_eq!((c.start_samples, c.length_samples), (0, 3_000));
    assert_eq!(
        c.notes.as_deref(),
        Some(&[note(60, 100, 50), note(64, 2_010, 20)][..])
    );
}

#[test]
fn recording_goes_to_the_armed_track_first() {
    let dir = Scratch::new("armed");
    let mut editor = one_clip(&dir, "a.wav");
    editor.add_midi_track(0, 1_000).unwrap();
    editor.arm_track(Some(0));
    editor.selected_track = Some(1);
    let (track, _) = editor
        .add_recording(&[0.1; 64], RATE, 0, &dir.0.join("rec"))
        .unwrap();
    assert_eq!(track, 0);
    editor.arm_track(None);
    editor.selected_track = Some(1);
    let (track, _) = editor
        .add_recording(&[0.1; 64], RATE, 5_000, &dir.0.join("rec"))
        .unwrap();
    assert_eq!(track, 1);
}

#[test]
fn deleting_tracks_keeps_the_arm_on_the_same_track() {
    let dir = Scratch::new("armdel");
    let mut editor = one_clip(&dir, "a.wav");
    editor.add_midi_track(0, 1_000).unwrap();
    editor.add_midi_track(0, 1_000).unwrap();
    editor.arm_track(Some(2));
    editor.delete_track(0);
    assert_eq!(editor.armed_track(), Some(1));
    editor.delete_track(1);
    assert_eq!(editor.armed_track(), None);
    editor.arm_track(Some(9));
    assert_eq!(editor.armed_track(), None);
}

#[test]
fn trimming_shortens_fades_that_no_longer_fit() {
    let dir = Scratch::new("trim");
    let mut editor = one_clip(&dir, "a.wav");
    editor.set_fades(0, 0, 300, 300).unwrap();
    editor.trim_end(0, 0, 400).unwrap();
    let c = clip(&editor, 0, 0);
    assert_eq!(
        (c.length_samples, c.fade_in_samples, c.fade_out_samples),
        (400, 300, 100)
    );
    editor.trim_start(0, 0, 350).unwrap();
    let c = clip(&editor, 0, 0);
    assert_eq!(
        (c.length_samples, c.fade_in_samples, c.fade_out_samples),
        (50, 50, 0)
    );
}
