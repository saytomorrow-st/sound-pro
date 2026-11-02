//! Acceptance tests for editing notes in MIDI clips (SPEC-021).

use audio_core::Note;
use daw_gui::quantize::{Division, QuantizeMode, QuantizeParams};
use daw_gui::{Editor, EditorError};

const fn note(pitch: u8, start: u64) -> Note {
    Note {
        pitch,
        start,
        length: 100,
        velocity: 100,
    }
}

fn editor_with_midi() -> Editor {
    let mut editor = Editor::new();
    assert_eq!(editor.add_midi_track(1_000, 48_000).unwrap(), (0, 0));
    editor
}

fn notes(editor: &Editor) -> Vec<Note> {
    editor.session().tracks[0].clips[0].notes.clone().unwrap()
}

#[test]
fn a_new_midi_track_holds_one_empty_selected_clip() {
    let editor = editor_with_midi();
    let session = editor.session();
    assert_eq!(session.tracks.len(), 1);
    assert_eq!(session.tracks[0].name, "MIDI");
    let clip = &session.tracks[0].clips[0];
    assert!(clip.is_midi());
    assert_eq!((clip.start_samples, clip.length_samples), (1_000, 48_000));
    assert_eq!(clip.source_length, 48_000);
    assert_eq!(editor.selected_clip, Some((0, 0)));
    assert!(editor.is_dirty());
    assert!(editor.can_undo());
}

#[test]
fn an_empty_midi_clip_is_refused() {
    let mut editor = Editor::new();
    assert!(matches!(
        editor.add_midi_track(0, 0),
        Err(EditorError::EmptyClip)
    ));
    assert!(editor.session().tracks.is_empty());
    assert!(!editor.can_undo());
}

#[test]
fn notes_are_kept_sorted_by_start_then_pitch() {
    let mut editor = editor_with_midi();
    assert_eq!(editor.add_note(0, 0, note(64, 500)).unwrap(), 0);
    assert_eq!(editor.add_note(0, 0, note(60, 0)).unwrap(), 0);
    assert_eq!(editor.add_note(0, 0, note(62, 500)).unwrap(), 1);
    let pitches: Vec<u8> = notes(&editor).iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, [60, 62, 64]);
}

#[test]
fn a_bad_note_leaves_the_project_unchanged() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    let before = editor.session().clone();
    let past_end = note(60, 47_950);
    let mut silent = note(60, 0);
    silent.velocity = 0;
    for bad in [past_end, silent, note(128, 0)] {
        assert!(matches!(
            editor.add_note(0, 0, bad),
            Err(EditorError::InvalidNote)
        ));
    }
    assert!(matches!(
        editor.move_note(0, 0, 0, 48_000, 60),
        Err(EditorError::InvalidNote)
    ));
    assert!(matches!(
        editor.delete_note(0, 0, 5),
        Err(EditorError::InvalidNote)
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn notes_cannot_go_into_an_audio_clip_or_a_missing_one() {
    let dir = std::env::temp_dir().join(format!("daw-gui-midi-wav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("a.wav");
    audio_core::wav::write_mono_slice(&wav, 48_000, &[0.1; 64]).unwrap();
    let mut editor = editor_with_midi();
    editor.selected_track = None;
    let (track, clip) = editor.import_wav(&wav).unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.add_note(track, clip, note(60, 0)),
        Err(EditorError::InvalidNote)
    ));
    assert!(matches!(
        editor.add_note(9, 0, note(60, 0)),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.add_note(0, 3, note(60, 0)),
        Err(EditorError::NoClip)
    ));
    assert_eq!(editor.session(), &before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn moving_a_note_changes_time_and_pitch_and_resorts() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    editor.add_note(0, 0, note(62, 1_000)).unwrap();
    assert_eq!(editor.move_note(0, 0, 0, 2_000, 67).unwrap(), 1);
    let moved = notes(&editor)[1];
    assert_eq!((moved.start, moved.pitch, moved.length), (2_000, 67, 100));
}

#[test]
fn deleting_a_note_removes_only_that_note() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    editor.add_note(0, 0, note(62, 10)).unwrap();
    editor.delete_note(0, 0, 0).unwrap();
    assert_eq!(notes(&editor), [note(62, 10)]);
}

#[test]
fn every_note_edit_is_one_undo_step() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    editor.move_note(0, 0, 0, 200, 61).unwrap();
    editor.delete_note(0, 0, 0).unwrap();
    assert!(notes(&editor).is_empty());
    assert!(editor.undo());
    assert_eq!(notes(&editor), [note(61, 200)]);
    assert!(editor.undo());
    assert_eq!(notes(&editor), [note(60, 0)]);
    assert!(editor.undo());
    assert!(notes(&editor).is_empty());
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty());
    assert!(editor.redo());
    assert!(editor.redo());
    assert_eq!(notes(&editor), [note(60, 0)]);
}

#[test]
fn a_midi_project_saves_reopens_and_exports() {
    let dir = std::env::temp_dir().join(format!("daw-gui-midi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(69, 0)).unwrap();
    let path = dir.join("song.json");
    editor.save_as(&path).unwrap();
    let mut reopened = Editor::new();
    reopened.open(&path).unwrap();
    assert_eq!(reopened.session(), editor.session());
    let report = reopened.export(&dir.join("mix.wav")).unwrap();
    assert_eq!(report.session_frames, 49_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Quantizes all notes when no selection is given (SPEC-061 §1).
#[test]
fn quantize_without_selection_moves_every_note() {
    let mut editor = editor_with_midi();
    // 1/16 at 120 BPM / 48 kHz = 6000 samples.
    editor.add_note(0, 0, note(60, 2_500)).unwrap();
    editor.add_note(0, 0, note(62, 5_000)).unwrap();
    let params = QuantizeParams {
        division: Division::Sixteenth,
        mode: QuantizeMode::StartOnly,
        strength: 100,
    };
    editor.quantize_selection(0, 0, &[], params).unwrap();
    let starts: Vec<u64> = notes(&editor).iter().map(|n| n.start).collect();
    assert_eq!(starts, [0, 6_000]);
}

/// Quantizes only the selected notes (SPEC-061 §1).
#[test]
fn quantize_selection_moves_only_the_selected_notes() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 2_500)).unwrap();
    editor.add_note(0, 0, note(62, 5_000)).unwrap();
    let params = QuantizeParams {
        division: Division::Sixteenth,
        mode: QuantizeMode::StartOnly,
        strength: 100,
    };
    editor.quantize_selection(0, 0, &[0], params).unwrap();
    let starts: Vec<u64> = notes(&editor).iter().map(|n| n.start).collect();
    // Note 0 snapped to 0; note 1 untouched at 5000.
    assert_eq!(starts, [0, 5_000]);
}

/// Quantization is one undo step restoring the exact ticks (SPEC-061 §1).
#[test]
fn quantize_is_one_undo_step() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 2_500)).unwrap();
    editor.add_note(0, 0, note(62, 5_000)).unwrap();
    let before = notes(&editor);
    let params = QuantizeParams {
        division: Division::Sixteenth,
        mode: QuantizeMode::StartAndLength,
        strength: 100,
    };
    editor.quantize_selection(0, 0, &[], params).unwrap();
    assert!(editor.undo());
    assert_eq!(notes(&editor), before);
}

/// Start+length snaps both ends; a short note never collapses to zero
/// (SPEC-061 edge case).
#[test]
fn quantize_start_and_length_never_collapses() {
    let mut editor = editor_with_midi();
    // 10-sample note at 3000; both ends snap to the 6000 grid line.
    let short = Note {
        pitch: 60,
        start: 3_000,
        length: 10,
        velocity: 100,
    };
    editor.add_note(0, 0, short).unwrap();
    let params = QuantizeParams {
        division: Division::Sixteenth,
        mode: QuantizeMode::StartAndLength,
        strength: 100,
    };
    editor.quantize_selection(0, 0, &[], params).unwrap();
    let n = &notes(&editor)[0];
    assert!(n.length > 0);
}

/// Velocity setters clamp to 1..=127 and reject out-of-range (SPEC-061 §2).
#[test]
fn velocity_setters_clamp_and_validate() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    assert!(editor.set_note_velocity(0, 0, 0, 0).is_err());
    assert!(editor.set_note_velocity(0, 0, 0, 128).is_err());
    editor.set_note_velocity(0, 0, 0, 127).unwrap();
    assert_eq!(notes(&editor)[0].velocity, 127);
    editor.set_note_velocity(0, 0, 0, 1).unwrap();
    assert_eq!(notes(&editor)[0].velocity, 1);
}

/// Setting several velocities in one call is one undo step (SPEC-061 §2).
#[test]
fn set_notes_velocity_is_one_undo_step() {
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    editor.add_note(0, 0, note(62, 100)).unwrap();
    editor.add_note(0, 0, note(64, 200)).unwrap();
    let before = notes(&editor);
    editor
        .set_notes_velocity(0, 0, &[(0, 10), (1, 20), (2, 30)])
        .unwrap();
    let v: Vec<u8> = notes(&editor).iter().map(|n| n.velocity).collect();
    assert_eq!(v, [10, 20, 30]);
    assert!(editor.undo());
    assert_eq!(notes(&editor), before);
}

/// A round trip through the project file preserves velocity (SPEC-061 §3).
#[test]
fn save_and_load_preserves_velocity() {
    let dir = std::env::temp_dir().join(format!("daw-gui-vel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut editor = editor_with_midi();
    editor.add_note(0, 0, note(60, 0)).unwrap();
    editor.set_note_velocity(0, 0, 0, 77).unwrap();
    let path = dir.join("vel.json");
    editor.save_as(&path).unwrap();
    let mut reopened = Editor::new();
    reopened.open(&path).unwrap();
    assert_eq!(reopened.session(), editor.session());
    assert_eq!(
        reopened.session().tracks[0].clips[0]
            .notes
            .as_ref()
            .unwrap()[0]
            .velocity,
        77
    );
    let _ = std::fs::remove_dir_all(&dir);
}
