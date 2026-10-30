//! Acceptance tests for crossfades, region note functions and notes
//! (SPEC-043).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use audio_core::{Clip, Note};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-xfade-{tag}-{}", std::process::id()));
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

/// One 1000-sample file split at 500, the right part moved to `right_start`.
fn two_clips(dir: &Scratch, right_start: u64) -> (Editor, usize, usize) {
    let mut editor = one_clip(dir, "a.wav");
    let (_, right) = editor.split_clip(0, 0, 500).unwrap();
    let (_, right) = editor.move_clip(0, right, right_start, 0).unwrap();
    let left = usize::from(right == 0);
    (editor, left, right)
}

#[test]
fn crossfade_of_overlapping_clips_fades_across_the_overlap() {
    let dir = Scratch::new("overlap");
    let (mut editor, left, right) = two_clips(&dir, 400);
    editor.crossfade(0, left).unwrap();
    assert_eq!(clip(&editor, 0, left).fade_out_samples, 100);
    assert_eq!(clip(&editor, 0, right).fade_in_samples, 100);
    editor.undo();
    assert_eq!(clip(&editor, 0, left).fade_out_samples, 0);
    assert_eq!(clip(&editor, 0, right).fade_in_samples, 0);
}

#[test]
fn crossfade_works_from_either_clip_of_the_pair() {
    let dir = Scratch::new("either");
    let (mut editor, left, right) = two_clips(&dir, 400);
    editor.crossfade(0, right).unwrap();
    assert_eq!(clip(&editor, 0, left).fade_out_samples, 100);
    assert_eq!(clip(&editor, 0, right).fade_in_samples, 100);
}

#[test]
fn crossfade_of_adjacent_clips_uses_the_default_length_clamped() {
    let dir = Scratch::new("adjacent");
    let (mut editor, left, right) = two_clips(&dir, 500);
    editor.crossfade(0, left).unwrap();
    // DEFAULT_XFADE = 960 does not fit into 500-sample clips.
    assert_eq!(clip(&editor, 0, left).fade_out_samples, 500);
    assert_eq!(clip(&editor, 0, right).fade_in_samples, 500);
    assert_eq!(daw_gui::DEFAULT_XFADE, 960);
}

#[test]
fn crossfade_shortens_the_other_fade_when_it_does_not_fit() {
    let dir = Scratch::new("shorten");
    let (mut editor, left, right) = two_clips(&dir, 200);
    editor.set_fades(0, left, 400, 0).unwrap();
    editor.crossfade(0, left).unwrap();
    let a = clip(&editor, 0, left);
    assert_eq!((a.fade_in_samples, a.fade_out_samples), (200, 300));
    assert_eq!(clip(&editor, 0, right).fade_in_samples, 300);
}

#[test]
fn crossfade_needs_a_touching_neighbour() {
    let dir = Scratch::new("apart");
    let (mut editor, left, _) = two_clips(&dir, 600);
    let before = editor.session().clone();
    assert!(matches!(
        editor.crossfade(0, left),
        Err(EditorError::CannotCrossfade)
    ));
    assert_eq!(editor.session(), &before);
    let single = Scratch::new("single");
    let mut editor = one_clip(&single, "b.wav");
    assert!(matches!(
        editor.crossfade(0, 0),
        Err(EditorError::CannotCrossfade)
    ));
    assert!(matches!(editor.crossfade(3, 0), Err(EditorError::NoClip)));
}

fn midi(notes: &[Note]) -> Editor {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 96_000).unwrap();
    for &n in notes {
        editor.add_note(0, 0, n).unwrap();
    }
    editor
}

fn notes(editor: &Editor) -> Vec<Note> {
    let mut list = clip(editor, 0, 0).notes.clone().unwrap();
    list.sort_by_key(|n| (n.start, n.pitch));
    list
}

#[test]
fn quantize_moves_note_starts_to_the_nearest_grid_line() {
    // 120 BPM at 48 kHz: a sixteenth is 6000 samples.
    let mut editor = midi(&[
        note(60, 2_000, 100),
        note(62, 4_000, 100),
        note(64, 95_999, 1),
    ]);
    editor.quantize_notes(0, 0, 16).unwrap();
    let starts: Vec<u64> = notes(&editor).iter().map(|n| n.start).collect();
    // The last note would land on 96000, outside the clip; it stays inside.
    assert_eq!(starts, vec![0, 6_000, 95_999]);
    editor.undo();
    assert_eq!(notes(&editor)[0].start, 2_000);
}

#[test]
fn quantize_uses_the_project_grid_not_the_clip_start() {
    let mut editor = Editor::new();
    editor.add_midi_track(1_000, 48_000).unwrap();
    editor.add_note(0, 0, note(60, 1_000, 100)).unwrap();
    editor.quantize_notes(0, 0, 4).unwrap();
    // Absolute 2000 → nearest quarter (24000 samples) is 0, clamped to the
    // clip's first sample.
    assert_eq!(notes(&editor)[0].start, 0);
    editor.add_note(0, 0, note(62, 20_000, 100)).unwrap();
    editor.quantize_notes(0, 0, 4).unwrap();
    // Absolute 21000 → 24000 → relative 23000.
    assert_eq!(notes(&editor)[1].start, 23_000);
    assert!(matches!(
        editor.quantize_notes(0, 0, 3),
        Err(EditorError::InvalidNote)
    ));
}

#[test]
fn transpose_shifts_pitches_and_clamps_to_the_midi_range() {
    let mut editor = midi(&[note(60, 0, 10), note(120, 100, 10)]);
    editor.transpose_notes(0, 0, 12).unwrap();
    let pitches: Vec<u8> = notes(&editor).iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![72, 127]);
    editor.transpose_notes(0, 0, -96).unwrap();
    let pitches: Vec<u8> = notes(&editor).iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![0, 31]);
    assert!(editor.transpose_notes(0, 0, 97).is_err());
}

#[test]
fn velocity_offset_clamps_to_one_through_127() {
    let mut editor = midi(&[note(60, 0, 10), note(62, 100, 10)]);
    editor.offset_velocity(0, 0, 50).unwrap();
    assert!(notes(&editor).iter().all(|n| n.velocity == 127));
    editor.offset_velocity(0, 0, -99).unwrap();
    assert!(notes(&editor).iter().all(|n| n.velocity == 28));
    editor.offset_velocity(0, 0, -99).unwrap();
    assert!(notes(&editor).iter().all(|n| n.velocity == 1));
    assert!(editor.offset_velocity(0, 0, 100).is_err());
}

#[test]
fn gate_time_scales_lengths_within_the_clip() {
    let mut editor = midi(&[note(60, 0, 1_000), note(62, 95_000, 600), note(64, 200, 1)]);
    editor.scale_gate(0, 0, 50).unwrap();
    let lengths: Vec<u64> = notes(&editor).iter().map(|n| n.length).collect();
    assert_eq!(lengths, vec![500, 1, 300]);
    editor.scale_gate(0, 0, 200).unwrap();
    let lengths: Vec<u64> = notes(&editor).iter().map(|n| n.length).collect();
    // The last note stops at the clip end (96000).
    assert_eq!(lengths, vec![1_000, 2, 600]);
    assert!(editor.scale_gate(0, 0, 10).is_err());
    assert!(editor.scale_gate(0, 0, 201).is_err());
}

#[test]
fn note_functions_reject_audio_clips() {
    let dir = Scratch::new("audio");
    let mut editor = one_clip(&dir, "c.wav");
    assert!(matches!(
        editor.transpose_notes(0, 0, 1),
        Err(EditorError::InvalidNote)
    ));
}

#[test]
fn track_and_project_notes_are_undoable_and_dirty_the_project() {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 1_000).unwrap();
    editor.set_track_notes(0, "SM57").unwrap();
    editor.set_project_notes("дедлайн").unwrap();
    assert_eq!(editor.session().tracks[0].notes, "SM57");
    assert_eq!(editor.session().notes, "дедлайн");
    assert!(editor.is_dirty());
    editor.undo();
    assert_eq!(editor.session().notes, "");
    assert!(matches!(
        editor.set_track_notes(4, "x"),
        Err(EditorError::NoClip)
    ));
    let long = "x".repeat(audio_core::MAX_NOTES_BYTES + 1);
    assert!(editor.set_track_notes(0, &long).is_err());
}
