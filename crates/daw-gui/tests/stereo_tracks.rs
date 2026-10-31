//! Acceptance tests for the GUI side of SPEC-050: stereo/mono tracks (§5),
//! the drop preview position (§2) and the decode cache and header probe
//! of the fast import (§6).

use std::path::{Path, PathBuf};

use audio_core::{TrackFormat, TrackKind};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-stereo-{tag}-{}", std::process::id()));
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

/// A 32-bit float WAV with `channels` interleaved channels.
fn write_float_wav(path: &Path, channels: u16, frames: &[Vec<f32>]) {
    let data: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.iter().flat_map(|s| s.to_le_bytes()))
        .collect();
    let block = u32::from(channels) * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&3_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * block).to_le_bytes());
    bytes.extend_from_slice(&(block as u16).to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    std::fs::write(path, bytes).unwrap();
}

/// Left channel loud and positive, right channel quiet and negative.
fn write_stereo(path: &Path, len: usize) {
    let frames: Vec<Vec<f32>> = (0..len).map(|_| vec![0.8, -0.2]).collect();
    write_float_wav(path, 2, &frames);
}

fn write_mono(path: &Path, len: usize) {
    let frames: Vec<Vec<f32>> = (0..len).map(|_| vec![0.5]).collect();
    write_float_wav(path, 1, &frames);
}

fn sine_mp3() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../daw-app/tests/data/sine.mp3")
}

fn formats(editor: &Editor) -> Vec<TrackFormat> {
    editor.session().tracks.iter().map(|t| t.format).collect()
}

#[test]
fn set_track_format_is_one_undo_step() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    assert_eq!(formats(&editor), vec![TrackFormat::Mono]);
    editor.set_track_format(0, TrackFormat::Stereo).unwrap();
    assert_eq!(formats(&editor), vec![TrackFormat::Stereo]);
    assert!(editor.undo());
    assert_eq!(formats(&editor), vec![TrackFormat::Mono]);
    assert_eq!(editor.session().tracks.len(), 1, "undo keeps the track");
}

#[test]
fn set_track_format_rejects_instrument_tracks_and_bad_indices() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "").unwrap();
    assert!(matches!(
        editor.set_track_format(0, TrackFormat::Stereo),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.set_track_format(5, TrackFormat::Stereo),
        Err(EditorError::NoClip)
    ));
    assert_eq!(formats(&editor), vec![TrackFormat::Mono]);
}

#[test]
fn add_tracks_with_format_makes_stereo_audio_tracks() {
    let mut editor = Editor::new();
    editor
        .add_tracks_with_format(TrackKind::Audio, 2, "Пэд", TrackFormat::Stereo)
        .unwrap();
    assert_eq!(formats(&editor), vec![TrackFormat::Stereo; 2]);
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    assert_eq!(formats(&editor)[2], TrackFormat::Mono, "default stays mono");
    assert!(editor.undo());
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty());
}

#[test]
fn a_stereo_file_makes_a_stereo_track_with_two_waves() {
    let dir = Scratch::new("import");
    let path = dir.0.join("pad.wav");
    write_stereo(&path, 4_800);
    let mut editor = Editor::new();
    let (t, c) = editor.import_audio_at(&path, None, Some(0)).unwrap();
    assert_eq!(editor.session().tracks[t].format, TrackFormat::Stereo);
    let clip = editor.session().tracks[t].clips[c].clone();
    assert_eq!(clip.length_samples, 4_800);
    let (left, right) = editor.stereo_peaks(&clip).expect("two waves");
    let (_, left_hi) = left.range(0, 4_800).unwrap();
    let (right_lo, _) = right.range(0, 4_800).unwrap();
    assert!(
        (left_hi - 0.8).abs() < 1e-6,
        "left wave is the left channel"
    );
    assert!(
        (right_lo + 0.2).abs() < 1e-6,
        "right wave is the right channel"
    );
}

#[test]
fn a_mono_file_keeps_a_mono_track_and_one_wave() {
    let dir = Scratch::new("mono");
    let path = dir.0.join("vox.wav");
    write_mono(&path, 4_800);
    let mut editor = Editor::new();
    let (t, c) = editor.import_audio_at(&path, None, Some(0)).unwrap();
    assert_eq!(editor.session().tracks[t].format, TrackFormat::Mono);
    let clip = &editor.session().tracks[t].clips[c];
    assert!(editor.stereo_peaks(clip).is_none());
    assert!(editor.peaks(clip).is_some());
}

#[test]
fn a_stereo_file_on_an_existing_track_keeps_the_track_format() {
    let dir = Scratch::new("existing");
    let path = dir.0.join("pad.wav");
    write_stereo(&path, 4_800);
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    editor.import_audio_at(&path, Some(0), Some(0)).unwrap();
    assert_eq!(formats(&editor), vec![TrackFormat::Mono]);
}

#[test]
fn a_file_already_in_the_project_is_not_decoded_again() {
    let dir = Scratch::new("cache");
    let path = dir.0.join("pad.wav");
    write_stereo(&path, 4_800);
    let mut editor = Editor::new();
    assert!(editor.cached_source(&path).is_none());
    editor.import_audio_at(&path, None, Some(0)).unwrap();
    std::fs::remove_file(&path).unwrap();
    let cached = editor.cached_source(&path).expect("decoded once");
    assert_eq!(cached.frames(), 4_800);
    assert!(cached.right.is_some(), "the cache keeps both channels");
    editor
        .import_audio_at(&path, Some(0), Some(9_600))
        .expect("second import uses the cache, not the deleted file");
    assert_eq!(editor.session().tracks[0].clips.len(), 2);
}

#[test]
fn the_probed_length_matches_the_imported_clip() {
    let dir = Scratch::new("probe");
    let wav = dir.0.join("pad.wav");
    write_stereo(&wav, 7_000);
    assert_eq!(daw_gui::probe_len(&wav, RATE).unwrap(), 7_000);
    let mut editor = Editor::new();
    let mp3 = sine_mp3();
    let probed = daw_gui::probe_len(&mp3, RATE).unwrap();
    let (t, c) = editor.import_audio_at(&mp3, None, Some(0)).unwrap();
    let length = editor.session().tracks[t].clips[c].length_samples;
    let diff = probed.abs_diff(length);
    assert!(
        diff <= u64::from(RATE / 20),
        "probe {probed} vs decoded {length}"
    );
}

#[test]
fn a_broken_file_has_no_probed_length() {
    let dir = Scratch::new("broken");
    let path = dir.0.join("broken.mp3");
    std::fs::write(&path, b"not audio at all").unwrap();
    assert!(daw_gui::probe_len(&path, RATE).is_err());
}

#[test]
fn the_drop_start_follows_the_pointer_and_snaps_to_the_beat() {
    let zoom = 100.0;
    let free = daw_gui::drop_start(130.0, RATE, zoom, 120.0, false);
    assert_eq!(free, daw_gui::x_to_samples(130.0, RATE, zoom));
    let snapped = daw_gui::drop_start(130.0, RATE, zoom, 120.0, true);
    assert_eq!(snapped, daw_gui::snap_to_beat(free, RATE, 120.0));
    assert_eq!(snapped % u64::from(RATE / 2), 0, "a beat at 120 BPM");
    assert_eq!(daw_gui::drop_start(-5.0, RATE, zoom, 120.0, true), 0);
}
