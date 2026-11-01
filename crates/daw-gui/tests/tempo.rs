//! Acceptance tests for the tempo track (SPEC-054).
//!
//! The GUI renders tempo points as draggable flags on a lane; the first point
//! (tick 0) is the project tempo and is a fixed anchor that must not be moved
//! or removed. These tests pin the model behaviour behind those interactions
//! so a drag on the first flag, or a moved point that reverts, fails here.

use daw_gui::Editor;

/// The first tempo point (index 0, tick 0) is the project tempo: it cannot be
/// moved or removed, matching the fixed anchor drawn in the lane.
#[test]
fn first_tempo_point_is_a_fixed_anchor() {
    let mut editor = Editor::new();
    assert_eq!(editor.tempo_point_count(), 1);
    let first = editor.tempo_point(0).unwrap();
    assert_eq!(first.position_ticks, 0);
    assert!(!editor.move_tempo_point(0, 480));
    assert!(!editor.remove_tempo_point(0));
    assert_eq!(editor.tempo_point_count(), 1);
    // The headline tempo still lives at the anchor.
    assert_eq!(
        editor.tempo_point(0).unwrap().bpm,
        editor.session().tempo_bpm
    );
}

/// Moving a non-first point commits (it stays where it was dragged) and is
/// undoable, so a drag never silently reverts (regression for the tempo flag
/// snapping back after release).
#[test]
fn moving_a_tempo_point_commits_and_is_undoable() {
    let mut editor = Editor::new();
    let first = editor.add_tempo_point(480, 140.0).unwrap();
    let second = editor.add_tempo_point(960, 90.0).unwrap();
    assert_eq!((first, second), (1, 2));
    assert_eq!(editor.tempo_point(second).unwrap().position_ticks, 960);

    assert!(editor.move_tempo_point(second, 1_200));
    assert_eq!(editor.tempo_point(second).unwrap().position_ticks, 1_200);
    assert!(editor.is_dirty());

    // Undo restores the original position; redo moves it back.
    assert!(editor.undo());
    assert_eq!(editor.tempo_point(second).unwrap().position_ticks, 960);
    assert!(editor.redo());
    assert_eq!(editor.tempo_point(second).unwrap().position_ticks, 1_200);
}

/// The scalar tempo and the tempo-map BPM range are one and the same
/// (`30..=300`), so a headline tempo at the boundary still builds a valid
/// tempo map instead of being rejected downstream.
#[test]
fn scalar_tempo_matches_the_tempo_map_range() {
    let mut editor = Editor::new();
    let rate = editor.session().sample_rate;
    for boundary in [30.0, 300.0] {
        editor.set_tempo(boundary);
        assert_eq!(editor.session().tempo_bpm, boundary);
        assert!(editor.tempo_map(rate).is_ok());
    }
    // Values outside the range clamp into it and stay valid.
    editor.set_tempo(5.0);
    assert_eq!(editor.session().tempo_bpm, 30.0);
    assert!(editor.tempo_map(rate).is_ok());
    editor.set_tempo(5_000.0);
    assert_eq!(editor.session().tempo_bpm, 300.0);
    assert!(editor.tempo_map(rate).is_ok());
}

/// A tempo point's BPM is clamped to the same `30..=300` range, so no point
/// can be created that the map builder later rejects.
#[test]
fn tempo_point_bpm_is_clamped_to_the_map_range() {
    let mut editor = Editor::new();
    assert_eq!(editor.add_tempo_point(480, 20.0), None);
    assert_eq!(editor.add_tempo_point(480, 999.0), None);
    let idx = editor.add_tempo_point(480, 140.0).unwrap();
    assert!(!editor.set_tempo_point_bpm(idx, 20.0));
    assert!(!editor.set_tempo_point_bpm(idx, 999.0));
    assert!(editor.set_tempo_point_bpm(idx, 30.0));
    assert!(editor.set_tempo_point_bpm(idx, 300.0));
    assert!(editor.tempo_map(editor.session().sample_rate).is_ok());
}

/// The editor tempo map (used by the lane's tick/sample math) matches the
/// session's combined points: index 0 is the headline tempo, the rest follow
/// in order.
#[test]
fn tempo_points_mirror_the_combined_map() {
    let mut editor = Editor::new();
    editor.set_tempo(120.0);
    editor.add_tempo_point(480, 140.0).unwrap();
    editor.add_tempo_point(960, 90.0).unwrap();
    let rate = editor.session().sample_rate;
    let map = editor.tempo_map(rate).unwrap();

    assert_eq!(map.points().len(), 3);
    assert_eq!(editor.tempo_point_count(), 3);
    assert_eq!(map.points()[0].position_ticks, 0);
    assert_eq!(map.points()[0].bpm, 120.0);
    assert_eq!(map.points()[1].position_ticks, 480);
    assert_eq!(map.points()[1].bpm, 140.0);
    assert_eq!(map.points()[2].position_ticks, 960);
    assert_eq!(map.points()[2].bpm, 90.0);
}
