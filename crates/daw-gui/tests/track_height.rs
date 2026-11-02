//! Acceptance tests for per-track height layout (SPEC-056 §1–2).
//!
//! The pure vertical-layout functions compute row offsets, total height and
//! y→row hit-tests from a per-track height vector, keeping folder headers at
//! a fixed compact height.

use daw_gui::Editor;
use daw_gui::track_rows::{
    DEFAULT_TRACK_HEIGHT, FOLDER_ROW_HEIGHT, MAX_TRACK_HEIGHT, MIN_TRACK_HEIGHT, row_at_y,
    row_height, row_heights, row_y, total_height, track_height,
};

fn editor_with(count: usize) -> Editor {
    let mut editor = Editor::new();
    for _ in 0..count {
        editor.add_midi_track(0, 48_000).unwrap();
    }
    editor
}

#[test]
fn unknown_track_falls_back_to_default() {
    assert_eq!(track_height(&[], 0), DEFAULT_TRACK_HEIGHT);
    assert_eq!(track_height(&[], 7), DEFAULT_TRACK_HEIGHT);
}

#[test]
fn track_height_is_clamped_to_bounds() {
    assert_eq!(track_height(&[1.0], 0), MIN_TRACK_HEIGHT);
    assert_eq!(track_height(&[10_000.0], 0), MAX_TRACK_HEIGHT);
    assert_eq!(track_height(&[100.0], 0), 100.0);
}

#[test]
fn no_tracks_has_no_rows() {
    let editor = editor_with(0);
    let session = editor.session();
    assert_eq!(row_heights(&[], session), Vec::<f32>::new());
    assert_eq!(total_height(&[], session), 0.0);
    assert_eq!(row_at_y(&[], session, 0.0), None);
}

#[test]
fn uniform_heights_stack_vertically() {
    let editor = editor_with(3);
    let session = editor.session();
    let heights = vec![72.0, 72.0, 72.0];
    assert_eq!(row_y(&heights, session, 0), 0.0);
    assert_eq!(row_y(&heights, session, 1), 72.0);
    assert_eq!(row_y(&heights, session, 2), 144.0);
    assert_eq!(total_height(&heights, session), 216.0);
}

#[test]
fn variable_heights_offset_each_row() {
    let editor = editor_with(3);
    let session = editor.session();
    let heights = vec![40.0, 200.0, 90.0];
    assert_eq!(row_y(&heights, session, 0), 0.0);
    assert_eq!(row_y(&heights, session, 1), 40.0);
    assert_eq!(row_y(&heights, session, 2), 240.0);
    assert_eq!(total_height(&heights, session), 330.0);
    assert_eq!(row_height(&heights, session, 1), 200.0);
}

#[test]
fn hit_test_lands_in_the_right_row() {
    let editor = editor_with(3);
    let session = editor.session();
    let heights = vec![40.0, 200.0, 90.0];
    // Above the first row.
    assert_eq!(row_at_y(&heights, session, -1.0), None);
    // Boundaries are exclusive on the bottom.
    assert_eq!(row_at_y(&heights, session, 0.0), Some(0));
    assert_eq!(row_at_y(&heights, session, 39.9), Some(0));
    assert_eq!(row_at_y(&heights, session, 40.0), Some(1));
    assert_eq!(row_at_y(&heights, session, 239.9), Some(1));
    assert_eq!(row_at_y(&heights, session, 240.0), Some(2));
    assert_eq!(row_at_y(&heights, session, 329.9), Some(2));
    // Below the last row.
    assert_eq!(row_at_y(&heights, session, 330.0), None);
}

#[test]
fn folder_rows_are_compact_and_fixed() {
    let mut editor = editor_with(3);
    // Folder 0 holds tracks 0 and 1; row order is Folder(0), Track(0),
    // Track(1), Track(2).
    editor.create_folder(&[0, 1]).unwrap();
    let session = editor.session();
    let heights = vec![40.0, 200.0, 90.0];
    let hs = row_heights(&heights, session);
    assert_eq!(hs[0], FOLDER_ROW_HEIGHT);
    assert_eq!(hs[1], 40.0);
    assert_eq!(hs[2], 200.0);
    assert_eq!(hs[3], 90.0);
    // The track at visual row 2 is offset by the folder header plus the
    // first child.
    assert_eq!(row_y(&heights, session, 2), FOLDER_ROW_HEIGHT + 40.0);
    assert_eq!(
        total_height(&heights, session),
        FOLDER_ROW_HEIGHT + 40.0 + 200.0 + 90.0
    );
    // The folder header is never hit-tested as a track, but still occupies
    // its row in the vertical layout.
    assert_eq!(
        row_at_y(&heights, session, FOLDER_ROW_HEIGHT * 0.5),
        Some(0)
    );
    assert_eq!(
        row_at_y(&heights, session, FOLDER_ROW_HEIGHT + 40.0 * 0.5),
        Some(1)
    );
}

#[test]
fn a_collapsed_folder_removes_children_from_layout() {
    let mut editor = editor_with(3);
    editor.create_folder(&[0, 1]).unwrap();
    editor.toggle_folder(0).unwrap();
    let session = editor.session();
    let heights = vec![40.0, 200.0, 90.0];
    // Collapsed: Folder(0), Track(2) only.
    let hs = row_heights(&heights, session);
    assert_eq!(hs, vec![FOLDER_ROW_HEIGHT, 90.0]);
    assert_eq!(total_height(&heights, session), FOLDER_ROW_HEIGHT + 90.0);
    assert_eq!(
        row_at_y(&heights, session, FOLDER_ROW_HEIGHT + 1.0),
        Some(1)
    );
    assert_eq!(row_at_y(&heights, session, FOLDER_ROW_HEIGHT + 90.0), None);
}
