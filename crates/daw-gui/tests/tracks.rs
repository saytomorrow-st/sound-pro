//! Acceptance tests for renaming, colouring, moving and duplicating tracks
//! (SPEC-029).

use audio_core::{BusSend, Effect, EffectKind, MAX_TRACKS, SessionError};
use daw_gui::{Editor, EditorError};

fn editor_with(count: usize) -> Editor {
    let mut editor = Editor::new();
    for _ in 0..count {
        editor.add_midi_track(0, 48_000).unwrap();
    }
    for index in 0..count {
        editor.rename_track(index, &format!("T{index}")).unwrap();
    }
    editor
}

fn names(editor: &Editor) -> Vec<String> {
    editor
        .session()
        .tracks
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn renaming_trims_and_can_be_undone() {
    let mut editor = editor_with(1);
    editor.rename_track(0, "  Бас  ").unwrap();
    assert_eq!(names(&editor), ["Бас"]);
    assert!(editor.is_dirty());
    assert!(editor.undo());
    assert_eq!(names(&editor), ["T0"]);
}

#[test]
fn an_empty_name_is_rejected() {
    let mut editor = editor_with(1);
    assert!(matches!(
        editor.rename_track(0, "   ").unwrap_err(),
        EditorError::EmptyName
    ));
    assert_eq!(names(&editor), ["T0"]);
}

#[test]
fn a_too_long_name_is_rejected() {
    let mut editor = editor_with(1);
    let long = "x".repeat(10_000);
    assert!(matches!(
        editor.rename_track(0, &long).unwrap_err(),
        EditorError::Session(SessionError::NameTooLong)
    ));
    assert_eq!(names(&editor), ["T0"]);
}

#[test]
fn the_same_name_adds_no_undo_step() {
    let mut editor = editor_with(1);
    editor.rename_track(0, "A").unwrap();
    editor.rename_track(0, "A").unwrap();
    assert!(editor.undo());
    assert_eq!(names(&editor), ["T0"]);
}

#[test]
fn a_bad_track_index_is_reported() {
    let mut editor = editor_with(1);
    assert!(matches!(
        editor.rename_track(1, "x").unwrap_err(),
        EditorError::NoClip
    ));
    assert!(matches!(
        editor.set_track_color(1, Some(0)).unwrap_err(),
        EditorError::NoClip
    ));
    assert!(matches!(
        editor.move_track(0, 1).unwrap_err(),
        EditorError::NoClip
    ));
    assert!(matches!(
        editor.duplicate_track(1).unwrap_err(),
        EditorError::NoClip
    ));
}

#[test]
fn colour_is_set_cleared_and_validated() {
    let mut editor = editor_with(1);
    editor.set_track_color(0, Some(5)).unwrap();
    assert_eq!(editor.session().tracks[0].color, Some(5));
    editor.set_track_color(0, None).unwrap();
    assert_eq!(editor.session().tracks[0].color, None);
    assert!(matches!(
        editor.set_track_color(0, Some(8)).unwrap_err(),
        EditorError::Session(SessionError::InvalidColor { track: 0 })
    ));
    assert!(editor.undo());
    assert_eq!(editor.session().tracks[0].color, Some(5));
}

#[test]
fn moving_a_track_shifts_the_others_and_keeps_the_selection() {
    let mut editor = editor_with(4);
    editor.selected_track = Some(0);
    editor.move_track(0, 2).unwrap();
    assert_eq!(names(&editor), ["T1", "T2", "T0", "T3"]);
    assert_eq!(editor.selected_track, Some(2));
    assert_eq!(editor.selected_clip, None);
    editor.move_track(3, 0).unwrap();
    assert_eq!(names(&editor), ["T3", "T1", "T2", "T0"]);
    assert!(editor.undo());
    assert_eq!(names(&editor), ["T1", "T2", "T0", "T3"]);
}

#[test]
fn moving_a_track_onto_itself_does_nothing() {
    let mut editor = editor_with(2);
    editor.rename_track(1, "B").unwrap();
    editor.move_track(1, 1).unwrap();
    assert_eq!(names(&editor), ["T0", "B"]);
    assert!(editor.undo());
    assert_eq!(names(&editor), ["T0", "T1"]);
}

#[test]
fn duplicating_copies_everything_below_the_original() {
    let mut editor = editor_with(2);
    editor.add_bus("Verb").unwrap();
    editor.set_send(0, 0, 0.5).unwrap();
    editor.add_effect(0, EffectKind::reverb()).unwrap();
    editor.set_track_color(0, Some(2)).unwrap();
    let copy = editor.duplicate_track(0).unwrap();
    assert_eq!(copy, 1);
    assert_eq!(names(&editor), ["T0", "T0 копия", "T1"]);
    let tracks = &editor.session().tracks;
    assert_eq!(tracks[1].clips, tracks[0].clips);
    assert_eq!(tracks[1].effects, vec![Effect::new(EffectKind::reverb())]);
    assert_eq!(tracks[1].sends, vec![BusSend { bus: 0, level: 0.5 }]);
    assert_eq!(tracks[1].color, Some(2));
    assert_eq!(editor.selected_track, Some(1));
    assert!(editor.undo());
    assert_eq!(names(&editor), ["T0", "T1"]);
}

#[test]
fn duplicating_respects_the_track_limit() {
    let mut editor = editor_with(MAX_TRACKS);
    assert!(matches!(
        editor.duplicate_track(0).unwrap_err(),
        EditorError::Session(SessionError::TooManyTracks { .. })
    ));
    assert_eq!(editor.session().tracks.len(), MAX_TRACKS);
}
