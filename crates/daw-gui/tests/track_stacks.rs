//! Acceptance tests for track folders in the editor (SPEC-055 §3–4).
//!
//! Every folder operation is one undo step, leaves the project unchanged on
//! error, and the row layout hides collapsed children without touching the
//! audio state.

use audio_core::session::Session;
use daw_gui::track_rows::{Row, visible_rows};
use daw_gui::{DeleteFolder, Editor, EditorError};

fn editor_with(count: usize) -> Editor {
    let mut editor = Editor::new();
    for _ in 0..count {
        editor.add_midi_track(0, 48_000).unwrap();
    }
    editor
}

fn folder_names(editor: &Editor) -> Vec<String> {
    editor
        .session()
        .folders
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

#[test]
fn creating_a_folder_collects_tracks_in_session_order() {
    let mut editor = editor_with(4);
    let folder = editor.create_folder(&[2, 0, 1]).unwrap();
    assert_eq!(folder, 0);
    assert_eq!(editor.session().folders[0].name, "Папка 1");
    assert_eq!(editor.session().folder_children[0], vec![0, 1, 2]);
    assert_eq!(folder_names(&editor), ["Папка 1"]);
}

#[test]
fn creating_a_folder_from_no_tracks_is_rejected() {
    let mut editor = editor_with(1);
    assert!(matches!(
        editor.create_folder(&[]).unwrap_err(),
        EditorError::EmptyFolder
    ));
    assert!(editor.session().folders.is_empty());
}

#[test]
fn creating_a_folder_from_an_out_of_range_track_is_rejected() {
    let mut editor = editor_with(1);
    assert!(matches!(
        editor.create_folder(&[5]).unwrap_err(),
        EditorError::NoClip
    ));
    assert!(editor.session().folders.is_empty());
}

#[test]
fn gathering_an_already_foldered_track_is_rejected() {
    let mut editor = editor_with(2);
    editor.create_folder(&[0]).unwrap();
    assert!(matches!(
        editor.create_folder(&[0, 1]).unwrap_err(),
        EditorError::TrackInFolder
    ));
    assert_eq!(editor.session().folders.len(), 1);
}

#[test]
fn an_empty_folder_is_created_and_named_uniquely() {
    let mut editor = editor_with(1);
    let a = editor.create_empty_folder().unwrap();
    let b = editor.create_empty_folder().unwrap();
    assert_eq!(folder_names(&editor), ["Папка 1", "Папка 2"]);
    assert!(editor.session().folder_children[a].is_empty());
    assert!(editor.session().folder_children[b].is_empty());
}

#[test]
fn deleting_a_folder_keeps_children_by_default() {
    let mut editor = editor_with(3);
    let folder = editor.create_folder(&[0, 1]).unwrap();
    editor
        .delete_folder(folder, DeleteFolder::KeepChildren)
        .unwrap();
    assert!(editor.session().folders.is_empty());
    assert_eq!(editor.session().tracks.len(), 3);
}

#[test]
fn deleting_a_folder_with_children_removes_them() {
    let mut editor = editor_with(4);
    let folder = editor.create_folder(&[1, 3]).unwrap();
    editor
        .delete_folder(folder, DeleteFolder::DeleteChildren)
        .unwrap();
    assert!(editor.session().folders.is_empty());
    // Tracks 1 and 3 are gone; 0 and 2 remain (2 total).
    assert_eq!(editor.session().tracks.len(), 2);
}

#[test]
fn renaming_a_folder_trims_and_can_be_undone() {
    let mut editor = editor_with(1);
    let folder = editor.create_empty_folder().unwrap();
    editor.rename_folder(folder, "  Барабаны  ").unwrap();
    assert_eq!(editor.session().folders[0].name, "Барабаны");
    assert!(editor.undo());
    assert_eq!(editor.session().folders[0].name, "Папка 1");
}

#[test]
fn setting_a_folder_color_round_trips_through_undo() {
    let mut editor = editor_with(1);
    let folder = editor.create_empty_folder().unwrap();
    editor.set_folder_color(folder, Some(4)).unwrap();
    assert_eq!(editor.session().folders[0].color, Some(4));
    assert!(editor.undo());
    assert_eq!(editor.session().folders[0].color, None);
    assert!(editor.redo());
    assert_eq!(editor.session().folders[0].color, Some(4));
}

#[test]
fn toggling_a_folder_is_display_only_and_undoable() {
    let mut editor = editor_with(1);
    let folder = editor.create_folder(&[0]).unwrap();
    let gain_before = editor.session().tracks[0].gain;
    editor.toggle_folder(folder).unwrap();
    assert!(editor.session().folders[0].collapsed);
    // Collapsing never changes a child's sound (SPEC-055 §3).
    assert_eq!(editor.session().tracks[0].gain, gain_before);
    assert!(editor.undo());
    assert!(!editor.session().folders[0].collapsed);
}

#[test]
fn adding_and_removing_tracks_to_a_folder_is_undoable() {
    let mut editor = editor_with(3);
    let folder = editor.create_empty_folder().unwrap();
    editor.add_tracks_to_folder(folder, &[1, 0]).unwrap();
    assert_eq!(editor.session().folder_children[folder], vec![0, 1]);
    editor.remove_tracks_from_folder(&[0]).unwrap();
    assert_eq!(editor.session().folder_children[folder], vec![1]);
    assert!(editor.undo());
    assert_eq!(editor.session().folder_children[folder], vec![0, 1]);
}

#[test]
fn deleting_a_track_removes_it_from_its_folder_and_reindexes() {
    let mut editor = editor_with(4);
    let folder = editor.create_folder(&[0, 2]).unwrap();
    editor.delete_track(1); // delete a top-level track between the children
    // Child indices shift: 0 stays, 2 becomes 1.
    assert_eq!(editor.session().folder_children[folder], vec![0, 1]);
}

#[test]
fn moving_a_track_reindexes_folder_children() {
    let mut editor = editor_with(3);
    let folder = editor.create_folder(&[0, 2]).unwrap();
    editor.move_track(1, 0).unwrap(); // move top-level track 1 above child 0
    // Track that was 0 is now 1, track 2 is now... recheck via names.
    assert_eq!(editor.session().folder_children[folder], vec![1, 2]);
}

#[test]
fn moving_a_folder_moves_its_children_as_a_block() {
    let mut editor = Editor::new();
    editor
        .add_tracks(audio_core::TrackKind::Audio, 5, "t")
        .unwrap();
    // Folder holds tracks 1 and 3; move the block to start at 0.
    let folder = editor.create_folder(&[1, 3]).unwrap();
    editor.move_folder(folder, 0).unwrap();
    let names: Vec<String> = editor
        .session()
        .tracks
        .iter()
        .map(|t| t.name.clone())
        .collect();
    // Children (old 1, 3) now occupy the first two slots, in order; the rest
    // follow in their original order.
    assert_eq!(names[0], "t 2");
    assert_eq!(names[1], "t 4");
    assert_eq!(names[2], "t 1");
    assert_eq!(names[3], "t 3");
    assert_eq!(names[4], "t 5");
    // Children still address tracks 0 and 1.
    assert_eq!(editor.session().folder_children[folder], vec![0, 1]);
}

#[test]
fn moving_a_folder_is_one_undo_step() {
    let mut editor = Editor::new();
    editor
        .add_tracks(audio_core::TrackKind::Audio, 5, "t")
        .unwrap();
    let folder = editor.create_folder(&[1, 3]).unwrap();
    editor.move_folder(folder, 0).unwrap();
    let before = editor.session().tracks[0].name.clone();
    assert!(editor.undo());
    let after = editor.session().tracks[0].name.clone();
    assert_ne!(before, after);
    assert!(editor.redo());
    assert_eq!(editor.session().tracks[0].name, before);
}

#[test]
fn every_folder_operation_is_one_undo_step() {
    let mut editor = editor_with(2);
    let folder = editor.create_folder(&[0]).unwrap();
    assert!(editor.can_undo());
    editor.rename_folder(folder, "Renamed").unwrap();
    editor.toggle_folder(folder).unwrap();
    // Undo back through toggle -> rename -> create.
    assert!(editor.undo());
    assert!(!editor.session().folders[0].collapsed);
    assert!(editor.undo());
    assert_eq!(editor.session().folders[0].name, "Папка 1");
    assert!(editor.undo());
    assert!(editor.session().folders.is_empty());
}

#[test]
fn visible_rows_put_a_folder_header_above_its_children() {
    let mut session = Session::new("s", 48_000, 120.0);
    for n in 0..4 {
        session
            .tracks
            .push(audio_core::SessionTrack::new(&format!("t{n}")));
    }
    session.folders.push(audio_core::TrackFolder::new("F"));
    session.folder_children.push(vec![0, 2]);

    let rows = visible_rows(&session);
    assert_eq!(
        rows,
        vec![
            Row::Folder(0),
            Row::Track(0),
            Row::Track(1),
            Row::Track(2),
            Row::Track(3)
        ]
    );
}

#[test]
fn a_collapsed_folder_hides_its_children() {
    let mut session = Session::new("s", 48_000, 120.0);
    for n in 0..3 {
        session
            .tracks
            .push(audio_core::SessionTrack::new(&format!("t{n}")));
    }
    session.folders.push(audio_core::TrackFolder {
        name: "F".into(),
        color: None,
        collapsed: true,
    });
    session.folder_children.push(vec![0, 2]);

    let rows = visible_rows(&session);
    assert_eq!(rows, vec![Row::Folder(0), Row::Track(1)]);
    assert_eq!(daw_gui::track_rows::row_of_track(&session, 0), None);
    assert_eq!(daw_gui::track_rows::row_of_track(&session, 1), Some(1));
    assert!(daw_gui::track_rows::is_hidden(&session, 0));
    assert!(!daw_gui::track_rows::is_hidden(&session, 1));
}

#[test]
fn selecting_a_folder_highlights_it_and_clears_track_selection() {
    let mut editor = editor_with(3);
    let folder = editor.create_folder(&[0, 1]).unwrap();
    editor.select_track(2, false, false);
    assert!(editor.is_track_selected(2));

    editor.select_folder(Some(folder));
    assert!(editor.is_folder_selected(folder));
    assert!(!editor.is_track_selected(2));
    assert!(editor.selected_track.is_none());
}

#[test]
fn clearing_folder_selection_does_not_reselect_tracks() {
    let mut editor = editor_with(2);
    let folder = editor.create_folder(&[0]).unwrap();
    editor.select_folder(Some(folder));
    editor.select_folder(None);
    assert!(!editor.is_folder_selected(folder));
    assert!(editor.selected_track.is_none());
}

#[test]
fn selecting_a_track_clears_folder_selection() {
    let mut editor = editor_with(3);
    let folder = editor.create_folder(&[0]).unwrap();
    editor.select_folder(Some(folder));
    assert!(editor.is_folder_selected(folder));

    editor.select_track(2, false, false);
    assert!(!editor.is_folder_selected(folder));
    assert!(editor.is_track_selected(2));
}

#[test]
fn selecting_an_out_of_range_folder_is_ignored() {
    let mut editor = editor_with(1);
    editor.create_folder(&[0]).unwrap();
    editor.select_track(0, false, false);
    editor.select_folder(Some(7));
    assert!(editor.selected_folder.is_none());
    assert!(editor.is_track_selected(0));
}

#[test]
fn adding_a_track_to_a_folder_is_one_undo_step() {
    let mut editor = editor_with(3);
    let folder = editor.create_empty_folder().unwrap();
    editor.add_tracks_to_folder(folder, &[1]).unwrap();
    assert_eq!(editor.session().folder_children[folder], vec![1]);
    assert!(editor.undo());
    assert!(editor.session().folder_children[folder].is_empty());
    assert!(editor.redo());
    assert_eq!(editor.session().folder_children[folder], vec![1]);
}

#[test]
fn adding_a_foldered_track_to_another_folder_is_rejected() {
    let mut editor = editor_with(3);
    let a = editor.create_folder(&[0]).unwrap();
    let b = editor.create_empty_folder().unwrap();
    assert!(matches!(
        editor.add_tracks_to_folder(b, &[0]).unwrap_err(),
        EditorError::TrackInFolder
    ));
    assert!(editor.session().folder_children[a].contains(&0));
    assert!(editor.session().folder_children[b].is_empty());
}
