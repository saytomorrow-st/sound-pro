//! Acceptance tests for track types and the new-track dialog (SPEC-049).

use std::path::{Path, PathBuf};

use audio_core::TrackKind;
use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-kind-{tag}-{}", std::process::id()));
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

fn kinds(editor: &Editor) -> Vec<TrackKind> {
    editor.session().tracks.iter().map(|t| t.kind).collect()
}

fn names(editor: &Editor) -> Vec<&str> {
    editor
        .session()
        .tracks
        .iter()
        .map(|t| t.name.as_str())
        .collect()
}

#[test]
fn add_tracks_creates_empty_tracks_in_one_undo_step() {
    let mut editor = Editor::new();
    let added = editor.add_tracks(TrackKind::Audio, 3, "Гитара").unwrap();
    assert_eq!(added, vec![0, 1, 2]);
    assert_eq!(names(&editor), vec!["Гитара 1", "Гитара 2", "Гитара 3"]);
    assert_eq!(kinds(&editor), vec![TrackKind::Audio; 3]);
    assert!(editor.session().tracks.iter().all(|t| t.clips.is_empty()));
    assert_eq!(editor.selected_track, Some(0));
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty(), "one undo removes all");
}

#[test]
fn add_tracks_default_names_per_kind() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "  ").unwrap();
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    assert_eq!(names(&editor), vec!["Инструмент", "Аудио"]);
    assert_eq!(
        kinds(&editor),
        vec![TrackKind::Instrument, TrackKind::Audio]
    );
}

#[test]
fn add_tracks_respects_the_track_limit() {
    let mut editor = Editor::new();
    editor
        .add_tracks(TrackKind::Audio, audio_core::MAX_TRACKS - 1, "")
        .unwrap();
    let err = editor.add_tracks(TrackKind::Audio, 2, "").unwrap_err();
    assert!(matches!(err, EditorError::Session(_)), "{err:?}");
    assert_eq!(editor.session().tracks.len(), audio_core::MAX_TRACKS - 1);
    assert!(matches!(
        editor.add_tracks(TrackKind::Audio, 0, ""),
        Err(EditorError::EmptyClip)
    ));
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
}

#[test]
fn midi_track_is_an_instrument_track() {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 1_000).unwrap();
    assert_eq!(kinds(&editor), vec![TrackKind::Instrument]);
}

#[test]
fn kind_survives_save_and_open() {
    let dir = Scratch::new("save");
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "Синт").unwrap();
    editor.add_tracks(TrackKind::Audio, 1, "Вокал").unwrap();
    let path = dir.0.join("p.soundpro");
    editor.save_as(&path).unwrap();
    let mut loaded = Editor::new();
    loaded.open(&path).unwrap();
    assert_eq!(
        kinds(&loaded),
        vec![TrackKind::Instrument, TrackKind::Audio]
    );
}

#[test]
fn audio_region_refuses_instrument_track_and_back() {
    let dir = Scratch::new("move");
    let wav = dir.0.join("a.wav");
    write_wav(&wav, &[0.5; 1_000]);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.add_midi_track(0, 1_000).unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.move_clip(0, 0, 0, 1),
        Err(EditorError::WrongTrackKind)
    ));
    assert!(matches!(
        editor.move_clip(1, 0, 0, 0),
        Err(EditorError::WrongTrackKind)
    ));
    editor.copy_clip(0, 0).unwrap();
    assert!(matches!(
        editor.paste_clip(1, 5_000),
        Err(EditorError::WrongTrackKind)
    ));
    editor.select_clip(Some((0, 0)), false);
    assert!(matches!(
        editor.move_selected_clips(0, 1),
        Err(EditorError::WrongTrackKind)
    ));
    assert_eq!(editor.session(), &before, "nothing changed");
    editor.paste_clip(0, 5_000).unwrap();
}

#[test]
fn import_onto_instrument_track_makes_an_audio_track() {
    let dir = Scratch::new("import");
    let wav = dir.0.join("a.wav");
    write_wav(&wav, &[0.5; 1_000]);
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "").unwrap();
    let (t, _) = editor.import_audio_at(&wav, Some(0), Some(0)).unwrap();
    assert_eq!(t, 1);
    assert_eq!(
        kinds(&editor),
        vec![TrackKind::Instrument, TrackKind::Audio]
    );
    assert!(editor.session().tracks[0].clips.is_empty());
}

#[test]
fn recording_skips_armed_instrument_tracks() {
    let dir = Scratch::new("rec");
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "").unwrap();
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    editor.set_armed(0, true);
    editor.set_armed(1, true);
    editor.add_recording(&[0.25; 500], RATE, 0, &dir.0).unwrap();
    assert!(editor.session().tracks[0].clips.is_empty());
    assert_eq!(editor.session().tracks[1].clips.len(), 1);
}

#[test]
fn recording_with_only_instrument_selected_makes_a_new_track() {
    let dir = Scratch::new("rec-new");
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "").unwrap();
    let (t, _) = editor.add_recording(&[0.25; 500], RATE, 0, &dir.0).unwrap();
    assert_eq!(t, 1);
    assert_eq!(
        kinds(&editor),
        vec![TrackKind::Instrument, TrackKind::Audio]
    );
}

#[test]
fn midi_region_goes_only_on_instrument_tracks() {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Instrument, 1, "Синт").unwrap();
    editor.add_tracks(TrackKind::Audio, 1, "").unwrap();
    let at = editor.add_midi_region(0, 1_000, 2_000).unwrap();
    assert_eq!(at, (0, 0));
    let clip = &editor.session().tracks[0].clips[0];
    assert!(clip.is_midi());
    assert_eq!((clip.start_samples, clip.length_samples), (1_000, 2_000));
    assert_eq!(clip.name, "Синт");
    assert!(matches!(
        editor.add_midi_region(1, 0, 2_000),
        Err(EditorError::WrongTrackKind)
    ));
    assert!(matches!(
        editor.add_midi_region(0, 0, 0),
        Err(EditorError::EmptyClip)
    ));
    assert!(matches!(
        editor.add_midi_region(7, 0, 10),
        Err(EditorError::NoClip)
    ));
}
