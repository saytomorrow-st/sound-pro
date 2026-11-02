//! Acceptance tests for track/folder drag-reorder and group drop into a
//! folder (SPEC-057 §1–§4).
//!
//! Every operation is a single undo step, preserves the relative order of a
//! moved group, and remaps folder children correctly.

use audio_core::session::Session;
use daw_gui::Editor;
use daw_gui::track_rows::Row;

fn editor_with(count: usize) -> Editor {
    let mut editor = Editor::new();
    editor
        .add_tracks(audio_core::TrackKind::Audio, count, "t")
        .unwrap();
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

// --- Group move (move_tracks) -------------------------------------------

#[test]
fn moving_a_group_preserves_relative_order() {
    let mut editor = editor_with(5);
    // Move tracks 1 and 3 (in order) to position 0.
    editor.move_tracks(&[3, 1], 0).unwrap();
    let n = names(&editor);
    // Original names: t 1..t 5. After moving {1,3} to front: t 2, t 4, t 1, t 3, t 5.
    assert_eq!(n, ["t 2", "t 4", "t 1", "t 3", "t 5"]);
}

#[test]
fn moving_a_group_is_one_undo_step() {
    let mut editor = editor_with(4);
    editor.move_tracks(&[0, 2], 3).unwrap();
    let moved = names(&editor);
    assert!(editor.undo());
    assert_eq!(names(&editor), ["t 1", "t 2", "t 3", "t 4"]);
    assert!(editor.redo());
    assert_eq!(names(&editor), moved);
}

#[test]
fn moving_a_group_remaps_folder_children() {
    let mut editor = editor_with(4);
    let folder = editor.create_folder(&[0, 2]).unwrap();
    // Move the top-level block {1,3} before everything.
    editor.move_tracks(&[1, 3], 0).unwrap();
    // Old 0 -> 2, old 2 -> 3 (they are the folder's children).
    assert_eq!(editor.session().folder_children[folder], vec![2, 3]);
}

#[test]
fn moving_an_empty_or_out_of_range_group_is_rejected() {
    let mut editor = editor_with(2);
    assert!(editor.move_tracks(&[], 0).is_ok());
    assert!(editor.move_tracks(&[9], 0).is_err());
    assert!(editor.move_tracks(&[0], 5).is_err());
}

// --- Move into folder (move_tracks_to_folder) ----------------------------

#[test]
fn moving_tracks_into_a_folder_transfers_between_folders() {
    let mut editor = editor_with(4);
    let a = editor.create_folder(&[1]).unwrap();
    let b = editor.create_empty_folder().unwrap();
    // Track 1 is already in folder `a`; moving it to `b` transfers it.
    editor.move_tracks_to_folder(b, &[1]).unwrap();
    assert!(editor.session().folder_children[a].is_empty());
    assert_eq!(editor.session().folder_children[b], vec![1]);
}

#[test]
fn moving_a_group_into_a_folder_is_one_undo_step() {
    let mut editor = editor_with(4);
    let folder = editor.create_empty_folder().unwrap();
    editor.move_tracks_to_folder(folder, &[0, 2, 3]).unwrap();
    assert_eq!(editor.session().folder_children[folder], vec![0, 2, 3]);
    assert!(editor.undo());
    assert!(editor.session().folder_children[folder].is_empty());
    assert!(editor.redo());
    assert_eq!(editor.session().folder_children[folder], vec![0, 2, 3]);
}

// --- Remove from folder + reorder (drag out of a folder) -----------------

#[test]
fn removing_a_track_from_a_folder_and_reordering_is_two_ops_grouped() {
    let mut editor = editor_with(4);
    let folder = editor.create_folder(&[0]).unwrap();
    // Simulate dragging track 0 out of the folder to the very end, one undo
    // step (remove from folder, then reorder).
    editor.begin_group();
    editor.remove_tracks_from_folder(&[0]).unwrap();
    editor.move_tracks(&[0], 4).unwrap();
    editor.end_group();
    assert!(editor.session().folder_children[folder].is_empty());
    assert_eq!(names(&editor), ["t 2", "t 3", "t 4", "t 1"]);
    // One undo step restores both the folder membership and the order.
    assert!(editor.undo());
    assert_eq!(editor.session().folder_children[folder], vec![0]);
    assert_eq!(names(&editor), ["t 1", "t 2", "t 3", "t 4"]);
}

// --- Folder multi-select (select_folder toggle/range) --------------------

#[test]
fn ctrl_click_adds_folders_to_selection() {
    let mut editor = editor_with(1);
    for _ in 0..3 {
        editor.create_empty_folder().unwrap();
    }
    editor.select_folder(Some(0), false, false);
    editor.select_folder(Some(2), true, false);
    assert!(editor.is_folder_selected(0));
    assert!(editor.is_folder_selected(2));
    assert_eq!(editor.selected_folders(), vec![0, 2]);
}

#[test]
fn ctrl_click_toggles_a_folder_off() {
    let mut editor = editor_with(1);
    editor.create_empty_folder().unwrap();
    editor.create_empty_folder().unwrap();
    editor.select_folder(Some(0), false, false);
    editor.select_folder(Some(1), true, false);
    editor.select_folder(Some(1), true, false);
    assert_eq!(editor.selected_folders(), vec![0]);
}

#[test]
fn shift_click_selects_a_folder_range() {
    let mut editor = editor_with(1);
    for _ in 0..4 {
        editor.create_empty_folder().unwrap();
    }
    editor.select_folder(Some(1), false, false);
    editor.select_folder(Some(3), false, true);
    assert_eq!(editor.selected_folders(), vec![1, 2, 3]);
}

#[test]
fn selecting_a_folder_clears_track_selection() {
    let mut editor = editor_with(2);
    let folder = editor.create_empty_folder().unwrap();
    editor.select_track(0, false, false);
    editor.select_folder(Some(folder), false, false);
    assert!(editor.is_folder_selected(folder));
    assert!(editor.selected_track.is_none());
    assert!(editor.selected_tracks().is_empty());
}

// --- Pure insertion-position helpers -------------------------------------

#[test]
fn track_index_at_insertion_ignores_folder_header_rows() {
    let mut session = Session::new("s", 48_000, 120.0);
    for n in 0..3 {
        session
            .tracks
            .push(audio_core::SessionTrack::new(&format!("t{n}")));
    }
    session.folders.push(audio_core::TrackFolder::new("F"));
    session.folder_children.push(vec![0, 2]);
    // visible_rows: Folder(0), Track(0), Track(1), Track(2).
    assert_eq!(
        daw_gui::track_rows::visible_rows(&session),
        vec![Row::Folder(0), Row::Track(0), Row::Track(1), Row::Track(2)]
    );
    // Dropping just above the folder header lands at track 0.
    assert_eq!(
        daw_gui::track_rows::track_index_at_insertion(&session, 0),
        0
    );
    // Dropping between the folder header and its first child lands at track 0.
    assert_eq!(
        daw_gui::track_rows::track_index_at_insertion(&session, 1),
        0
    );
    // Dropping after all rows lands after every track.
    assert_eq!(
        daw_gui::track_rows::track_index_at_insertion(&session, 4),
        3
    );
}
