//! Acceptance tests for editing automation lanes (SPEC-023).

use audio_core::{AutoPoint, Lane, SessionError};
use daw_gui::{Editor, EditorError};

fn editor_with_track() -> Editor {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 48_000).unwrap();
    editor
}

fn p(time: u64, value: f32) -> AutoPoint {
    AutoPoint { time, value }
}

fn lane(editor: &Editor, lane: Lane) -> Vec<AutoPoint> {
    editor.session().tracks[0].automation.lane(lane).to_vec()
}

#[test]
fn points_are_inserted_in_time_order() {
    let mut editor = editor_with_track();
    assert_eq!(editor.add_point(0, Lane::Gain, p(200, 0.5)).unwrap(), 0);
    assert_eq!(editor.add_point(0, Lane::Gain, p(100, 0.2)).unwrap(), 0);
    assert_eq!(editor.add_point(0, Lane::Gain, p(300, 1.0)).unwrap(), 2);
    assert_eq!(
        lane(&editor, Lane::Gain),
        vec![p(100, 0.2), p(200, 0.5), p(300, 1.0)]
    );
    assert!(lane(&editor, Lane::Pan).is_empty());
    assert!(editor.is_dirty());
}

#[test]
fn a_point_at_the_same_time_is_replaced() {
    let mut editor = editor_with_track();
    editor.add_point(0, Lane::Pan, p(100, -0.5)).unwrap();
    assert_eq!(editor.add_point(0, Lane::Pan, p(100, 0.5)).unwrap(), 0);
    assert_eq!(lane(&editor, Lane::Pan), vec![p(100, 0.5)]);
}

#[test]
fn moving_a_point_is_clamped_between_its_neighbours() {
    let mut editor = editor_with_track();
    for point in [p(100, 0.1), p(200, 0.2), p(300, 0.3)] {
        editor.add_point(0, Lane::Gain, point).unwrap();
    }
    assert_eq!(
        editor.move_point(0, Lane::Gain, 1, p(1_000, 0.9)).unwrap(),
        p(299, 0.9)
    );
    assert_eq!(
        editor.move_point(0, Lane::Gain, 1, p(0, 0.9)).unwrap(),
        p(101, 0.9)
    );
    assert_eq!(
        editor.move_point(0, Lane::Gain, 0, p(0, 0.0)).unwrap(),
        p(0, 0.0)
    );
    assert_eq!(
        lane(&editor, Lane::Gain),
        vec![p(0, 0.0), p(101, 0.9), p(300, 0.3)]
    );
}

#[test]
fn each_edit_is_one_undo_step_and_no_op_moves_record_nothing() {
    let mut editor = editor_with_track();
    editor.add_point(0, Lane::Gain, p(100, 0.5)).unwrap();
    editor.move_point(0, Lane::Gain, 0, p(150, 0.25)).unwrap();
    editor.move_point(0, Lane::Gain, 0, p(150, 0.25)).unwrap();
    editor.remove_point(0, Lane::Gain, 0).unwrap();
    assert!(lane(&editor, Lane::Gain).is_empty());
    assert!(editor.undo());
    assert_eq!(lane(&editor, Lane::Gain), vec![p(150, 0.25)]);
    assert!(editor.undo());
    assert_eq!(lane(&editor, Lane::Gain), vec![p(100, 0.5)]);
    assert!(editor.undo());
    assert!(lane(&editor, Lane::Gain).is_empty());
    assert!(editor.redo());
    assert_eq!(lane(&editor, Lane::Gain), vec![p(100, 0.5)]);
}

#[test]
fn clearing_a_lane_removes_every_point() {
    let mut editor = editor_with_track();
    editor.add_point(0, Lane::Pan, p(0, 1.0)).unwrap();
    editor.add_point(0, Lane::Pan, p(10, -1.0)).unwrap();
    editor.clear_lane(0, Lane::Pan).unwrap();
    assert!(lane(&editor, Lane::Pan).is_empty());
    assert!(editor.undo());
    assert_eq!(lane(&editor, Lane::Pan).len(), 2);
}

#[test]
fn bad_indices_are_reported_and_change_nothing() {
    let mut editor = editor_with_track();
    assert!(matches!(
        editor.add_point(5, Lane::Gain, p(0, 0.5)),
        Err(EditorError::NoPoint)
    ));
    assert!(matches!(
        editor.move_point(0, Lane::Gain, 0, p(0, 0.5)),
        Err(EditorError::NoPoint)
    ));
    assert!(matches!(
        editor.remove_point(0, Lane::Pan, 3),
        Err(EditorError::NoPoint)
    ));
    assert!(matches!(
        editor.clear_lane(9, Lane::Pan),
        Err(EditorError::NoPoint)
    ));
    assert!(lane(&editor, Lane::Gain).is_empty());
}

#[test]
fn out_of_range_values_are_session_errors() {
    let mut editor = editor_with_track();
    let before = editor.session().clone();
    assert!(matches!(
        editor.add_point(0, Lane::Gain, p(0, 1.5)),
        Err(EditorError::Session(SessionError::InvalidAutomation {
            track: 0,
            lane: Lane::Gain,
            point: 0
        }))
    ));
    assert!(matches!(
        editor.add_point(0, Lane::Pan, p(0, f32::NAN)),
        Err(EditorError::Session(SessionError::InvalidAutomation {
            track: 0,
            lane: Lane::Pan,
            point: 0
        }))
    ));
    assert_eq!(editor.session(), &before);
}
