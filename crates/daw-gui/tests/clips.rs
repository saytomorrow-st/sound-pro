//! Acceptance tests for editing clips with the mouse (SPEC-017).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use audio_core::{MAX_CLIPS_PER_TRACK, SessionError};
use daw_gui::{Editor, EditorError, snap_to_beat};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-clips-{tag}-{}", std::process::id()));
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

/// An editor with one track holding one 1000-sample clip, saved so the
/// dirty flag starts clear.
fn one_clip(dir: &Scratch) -> Editor {
    let wav = dir.join("a.wav");
    write_wav(&wav, &ramp(1_000));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();
    assert!(!editor.is_dirty());
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
    editor
}

fn clip(editor: &Editor, track: usize, clip: usize) -> (u64, u64, u64) {
    let c = &editor.session().tracks[track].clips[clip];
    (c.start_samples, c.offset_samples, c.length_samples)
}

#[test]
fn moving_along_the_same_track_keeps_the_index() {
    let dir = Scratch::new("move");
    let mut editor = one_clip(&dir);
    assert_eq!(editor.move_clip(0, 0, 5_000, 0).unwrap(), (0, 0));
    assert_eq!(clip(&editor, 0, 0), (5_000, 0, 1_000));
    assert!(editor.is_dirty());
    assert_eq!(editor.selected_clip, Some((0, 0)));
    assert_eq!(editor.move_clip(0, 0, 0, 0).unwrap(), (0, 0));
    assert_eq!(clip(&editor, 0, 0), (0, 0, 1_000));
}

#[test]
fn moving_to_another_track_appends_it_there() {
    let dir = Scratch::new("cross");
    let mut editor = two_tracks(&dir);
    assert_eq!(editor.move_clip(0, 0, 700, 1).unwrap(), (1, 1));
    assert!(editor.session().tracks[0].clips.is_empty());
    assert_eq!(clip(&editor, 1, 1), (700, 0, 1_000));
    assert_eq!(editor.selected_clip, Some((1, 1)));
    assert_eq!(editor.selected_track, Some(1));
}

#[test]
fn bad_indices_leave_the_project_unchanged() {
    let dir = Scratch::new("bad");
    let mut editor = one_clip(&dir);
    let before = editor.session().clone();
    assert!(matches!(
        editor.move_clip(0, 0, 10, 3),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.move_clip(0, 1, 10, 0),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.move_clip(2, 0, 10, 0),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.trim_start(0, 4, 10),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.trim_end(5, 0, 10),
        Err(EditorError::NoClip)
    ));
    assert_eq!(editor.session(), &before);
    assert!(!editor.is_dirty());
}

#[test]
fn a_full_track_refuses_another_clip() {
    let dir = Scratch::new("full");
    let mut editor = two_tracks(&dir);
    let spare = editor.session().tracks[1].clips[0].clone();
    let target = editor.track_mut(1).unwrap();
    while target.clips.len() < MAX_CLIPS_PER_TRACK {
        target.clips.push(spare.clone());
    }
    let before = editor.session().clone();
    assert!(matches!(
        editor.move_clip(0, 0, 0, 1),
        Err(EditorError::Session(SessionError::TooManyClips { .. }))
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn a_move_past_the_end_of_time_is_refused() {
    let dir = Scratch::new("overflow");
    let mut editor = one_clip(&dir);
    let before = editor.session().clone();
    assert!(matches!(
        editor.move_clip(0, 0, u64::MAX - 10, 0),
        Err(EditorError::Session(SessionError::TimelineOverflow { .. }))
    ));
    assert_eq!(editor.session(), &before);
    assert!(!editor.is_dirty());
}

#[test]
fn trimming_the_start_keeps_the_right_edge() {
    let dir = Scratch::new("trim-start");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 2_000, 0).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();

    editor.trim_start(0, 0, 2_300).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_300, 300, 700));
    assert!(editor.is_dirty());

    editor.trim_start(0, 0, 2_100).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_100, 100, 900));

    // Cannot reveal audio before the start of the file.
    editor.trim_start(0, 0, 0).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_000, 0, 1_000));

    // Cannot pass the right edge.
    editor.trim_start(0, 0, 9_000).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_999, 999, 1));
}

#[test]
fn trimming_the_start_near_zero_stops_at_the_file_start() {
    let dir = Scratch::new("trim-zero");
    let mut editor = one_clip(&dir);
    editor.trim_start(0, 0, 600).unwrap();
    assert_eq!(clip(&editor, 0, 0), (600, 600, 400));
    editor.move_clip(0, 0, 100, 0).unwrap();
    editor.trim_start(0, 0, 0).unwrap();
    assert_eq!(clip(&editor, 0, 0), (0, 500, 500));
}

#[test]
fn trimming_the_end_keeps_the_left_edge() {
    let dir = Scratch::new("trim-end");
    let mut editor = one_clip(&dir);
    editor.move_clip(0, 0, 2_000, 0).unwrap();
    editor.save_as(&dir.join("song.json")).unwrap();

    editor.trim_end(0, 0, 2_500).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_000, 0, 500));
    assert!(editor.is_dirty());

    editor.trim_end(0, 0, 99_999).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_000, 0, 1_000));

    editor.trim_end(0, 0, 0).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_000, 0, 1));

    editor.trim_end(0, 0, 99_999).unwrap();
    editor.trim_start(0, 0, 2_300).unwrap();
    editor.trim_end(0, 0, 99_999).unwrap();
    assert_eq!(clip(&editor, 0, 0), (2_300, 300, 700));
}

#[test]
fn snapping_picks_the_nearest_beat() {
    // 120 BPM at 48 kHz: one beat is 24000 samples.
    assert_eq!(snap_to_beat(0, RATE, 120.0), 0);
    assert_eq!(snap_to_beat(11_999, RATE, 120.0), 0);
    assert_eq!(snap_to_beat(12_000, RATE, 120.0), 24_000);
    assert_eq!(snap_to_beat(30_000, RATE, 120.0), 24_000);
    assert_eq!(snap_to_beat(100_000, RATE, 120.0), 96_000);
    assert_eq!(snap_to_beat(30_000, RATE, 60.0), 48_000);
    assert_eq!(snap_to_beat(30_000, RATE, f64::NAN), 24_000);
    assert_eq!(snap_to_beat(30_000, RATE, -5.0), 24_000);
}

#[test]
fn an_edited_project_sounds_like_its_saved_file() {
    let dir = Scratch::new("sound");
    let mut editor = two_tracks(&dir);
    editor.move_clip(1, 0, 1_500, 0).unwrap();
    editor.trim_start(0, 0, 100).unwrap();
    editor.trim_end(0, 0, 800).unwrap();
    let project = dir.join("song.json");
    editor.save_as(&project).unwrap();

    let ours = dir.join("ours.wav");
    let report = editor.export(&ours).unwrap();
    assert_eq!(report.frames, 1_900);

    let theirs = dir.join("theirs.wav");
    let job = daw_app::load_session_project(&project, theirs.clone()).unwrap();
    daw_app::render_session(job).unwrap();
    let (_, a) = read_wav_mono(&ours).unwrap();
    let (_, b) = read_wav_mono(&theirs).unwrap();
    assert_eq!(a.channel(0), b.channel(0));
    assert_eq!(a.channel(1), b.channel(1));

    // Silence before the trimmed clip and in the gap before the moved one.
    assert!(a.channel(0)[..100].iter().all(|&s| s == 0.0));
    assert!(a.channel(0)[800..1_500].iter().all(|&s| s == 0.0));
    assert!(a.channel(0)[100] != 0.0 && a.channel(0)[1_500] != 0.0);
}
