//! Acceptance tests for arming several tracks at once (SPEC-046).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::Editor;

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-arm-{tag}-{}", std::process::id()));
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

/// An editor with `count` audio tracks; the first holds one clip.
fn audio_tracks(dir: &Scratch, count: usize) -> Editor {
    let mut editor = tracks(dir, 1);
    editor
        .add_tracks(audio_core::TrackKind::Audio, count - 1, "")
        .unwrap();
    editor
}

fn armed(editor: &Editor) -> Vec<usize> {
    editor.armed_tracks().collect()
}

#[test]
fn several_tracks_arm_independently() {
    let dir = Scratch::new("many");
    let mut editor = tracks(&dir, 4);
    editor.set_armed(2, true);
    editor.set_armed(0, true);
    editor.set_armed(3, true);
    assert_eq!(armed(&editor), vec![0, 2, 3]);
    assert!(editor.is_armed(2));
    assert!(!editor.is_armed(1));
    assert_eq!(editor.armed_track(), Some(0));
    editor.set_armed(2, false);
    assert_eq!(armed(&editor), vec![0, 3]);
    editor.set_armed(0, true);
    assert_eq!(armed(&editor), vec![0, 3], "arming twice keeps one entry");
    editor.set_armed(9, true);
    assert_eq!(armed(&editor), vec![0, 3], "bad index does nothing");
}

#[test]
fn arm_track_still_arms_exactly_one() {
    let dir = Scratch::new("one");
    let mut editor = tracks(&dir, 3);
    editor.set_armed(0, true);
    editor.set_armed(1, true);
    editor.arm_track(Some(2));
    assert_eq!(armed(&editor), vec![2]);
    editor.arm_track(None);
    assert!(armed(&editor).is_empty());
}

#[test]
fn arms_follow_tracks_on_delete_move_and_duplicate() {
    let dir = Scratch::new("remap");
    let mut editor = tracks(&dir, 4);
    editor.set_armed(1, true);
    editor.set_armed(3, true);
    editor.delete_track(1);
    assert_eq!(armed(&editor), vec![2]);
    editor.set_armed(0, true);
    editor.duplicate_track(0).unwrap();
    assert_eq!(armed(&editor), vec![0, 3], "the copy is not armed");
    editor.move_track(3, 1).unwrap();
    assert_eq!(armed(&editor), vec![0, 1]);
    editor.move_track(0, 3).unwrap();
    assert_eq!(armed(&editor), vec![0, 3]);
}

#[test]
fn a_take_lands_on_every_armed_track_as_one_undo_step() {
    let dir = Scratch::new("rec");
    let mut editor = audio_tracks(&dir, 3);
    editor.set_armed(0, true);
    editor.set_armed(2, true);
    editor.selected_track = Some(1);
    let before: Vec<usize> = editor
        .session()
        .tracks
        .iter()
        .map(|t| t.clips.len())
        .collect();
    let (track, clip) = editor
        .add_recording(&[0.1; 64], RATE, 2_000, &dir.0.join("rec"))
        .unwrap();
    assert_eq!(track, 0);
    let session = editor.session();
    assert_eq!(session.tracks[0].clips.len(), before[0] + 1);
    assert_eq!(
        session.tracks[1].clips.len(),
        before[1],
        "unarmed untouched"
    );
    assert_eq!(session.tracks[2].clips.len(), before[2] + 1);
    let first = &session.tracks[0].clips[clip];
    let second = session.tracks[2].clips.last().unwrap();
    assert_eq!(first.source, second.source, "both share one file");
    assert_eq!(first.start_samples, 2_000);
    assert_eq!(second.start_samples, 2_000);
    let files = std::fs::read_dir(dir.0.join("rec")).unwrap().count();
    assert_eq!(files, 1, "one take, one file");
    assert!(editor.undo());
    let after: Vec<usize> = editor
        .session()
        .tracks
        .iter()
        .map(|t| t.clips.len())
        .collect();
    assert_eq!(after, before);
}
