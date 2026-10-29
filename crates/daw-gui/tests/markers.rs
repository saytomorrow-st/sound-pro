//! Acceptance tests for session markers (SPEC-030).

use audio_core::{MAX_MARKERS, SessionError};
use daw_gui::{Editor, EditorError};

fn positions(editor: &Editor) -> Vec<u64> {
    editor
        .session()
        .markers
        .iter()
        .map(|m| m.position)
        .collect()
}

fn names(editor: &Editor) -> Vec<String> {
    editor
        .session()
        .markers
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

#[test]
fn markers_are_added_in_order_with_free_numbers() {
    let mut editor = Editor::new();
    assert_eq!(editor.add_marker(48_000).unwrap(), 0);
    assert_eq!(editor.add_marker(0).unwrap(), 0);
    assert_eq!(editor.add_marker(96_000).unwrap(), 2);
    assert_eq!(positions(&editor), [0, 48_000, 96_000]);
    assert_eq!(names(&editor), ["Маркер 2", "Маркер 1", "Маркер 3"]);
    assert!(editor.is_dirty());
    editor.delete_marker(1).unwrap();
    editor.add_marker(10).unwrap();
    assert_eq!(names(&editor), ["Маркер 2", "Маркер 1", "Маркер 3"]);
}

#[test]
fn a_taken_position_is_rejected() {
    let mut editor = Editor::new();
    editor.add_marker(100).unwrap();
    assert!(matches!(
        editor.add_marker(100),
        Err(EditorError::MarkerExists)
    ));
    assert_eq!(positions(&editor), [100]);
}

#[test]
fn adding_respects_the_limit() {
    let mut editor = Editor::new();
    for position in 0..MAX_MARKERS as u64 {
        editor.add_marker(position).unwrap();
    }
    assert!(matches!(
        editor.add_marker(1_000_000),
        Err(EditorError::Session(SessionError::InvalidMarker { .. }))
    ));
    assert_eq!(editor.session().markers.len(), MAX_MARKERS);
}

#[test]
fn renaming_trims_rejects_blank_and_skips_same_name() {
    let mut editor = Editor::new();
    editor.add_marker(0).unwrap();
    editor.rename_marker(0, "  Припев ").unwrap();
    assert_eq!(names(&editor), ["Припев"]);
    assert!(matches!(
        editor.rename_marker(0, "   "),
        Err(EditorError::EmptyName)
    ));
    editor.rename_marker(0, "Припев").unwrap();
    assert!(editor.undo());
    assert_eq!(names(&editor), ["Маркер 1"]);
}

#[test]
fn moving_resorts_and_returns_the_new_index() {
    let mut editor = Editor::new();
    editor.add_marker(0).unwrap();
    editor.add_marker(100).unwrap();
    assert_eq!(editor.move_marker(0, 200).unwrap(), 1);
    assert_eq!(positions(&editor), [100, 200]);
    assert_eq!(names(&editor), ["Маркер 2", "Маркер 1"]);
    assert!(matches!(
        editor.move_marker(0, 200),
        Err(EditorError::MarkerExists)
    ));
    assert_eq!(editor.move_marker(1, 200).unwrap(), 1);
    assert!(editor.undo());
    assert_eq!(positions(&editor), [0, 100]);
}

#[test]
fn deleting_can_be_undone() {
    let mut editor = Editor::new();
    editor.add_marker(5).unwrap();
    editor.delete_marker(0).unwrap();
    assert!(editor.session().markers.is_empty());
    assert!(editor.undo());
    assert_eq!(positions(&editor), [5]);
}

#[test]
fn a_bad_index_is_reported() {
    let mut editor = Editor::new();
    assert!(matches!(
        editor.delete_marker(0),
        Err(EditorError::NoMarker)
    ));
    assert!(matches!(
        editor.rename_marker(0, "x"),
        Err(EditorError::NoMarker)
    ));
    assert!(matches!(
        editor.move_marker(0, 1),
        Err(EditorError::NoMarker)
    ));
}

#[test]
fn navigation_finds_neighbours_strictly() {
    let mut editor = Editor::new();
    for p in [100, 200, 300] {
        editor.add_marker(p).unwrap();
    }
    assert_eq!(editor.next_marker(0), Some(100));
    assert_eq!(editor.next_marker(100), Some(200));
    assert_eq!(editor.next_marker(300), None);
    assert_eq!(editor.previous_marker(300), Some(200));
    assert_eq!(editor.previous_marker(150), Some(100));
    assert_eq!(editor.previous_marker(100), None);
}
