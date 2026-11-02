//! Acceptance tests for track colour following the track across every
//! structural operation (SPEC-059 §1).
//!
//! Colour lives on `Track.color` in the project model, so it must travel with
//! the track — never by position — through reorder, duplicate, delete and
//! folder add/remove. This is what the owner reported as "цвет дорожки не
//! переносится": the model is the source of truth, and these tests pin that
//! the colour moves with the track rather than being re-derived from its slot.

use audio_core::TrackKind;
use daw_gui::Editor;

fn editor_with(count: usize) -> Editor {
    let mut editor = Editor::new();
    editor.add_tracks(TrackKind::Audio, count, "t").unwrap();
    editor
}

fn colors(editor: &Editor) -> Vec<Option<u8>> {
    editor.session().tracks.iter().map(|t| t.color).collect()
}

#[test]
fn move_tracks_keeps_color_with_each_track() {
    let mut editor = editor_with(4);
    editor.set_track_color(0, Some(2)).unwrap();
    editor.set_track_color(1, Some(5)).unwrap();
    editor.set_track_color(2, Some(0)).unwrap();
    editor.set_track_color(3, Some(7)).unwrap();
    // Move the block {1,3} to the front.
    editor.move_tracks(&[1, 3], 0).unwrap();
    // New order: t 1(color 5), t 3(color 7), t 0(color 2), t 2(color 0).
    assert_eq!(
        colors(&editor),
        [Some(5), Some(7), Some(2), Some(0)],
        "colour must travel with the track, not with the slot"
    );
}

#[test]
fn move_track_keeps_color_with_the_track() {
    let mut editor = editor_with(3);
    editor.set_track_color(0, Some(2)).unwrap();
    editor.set_track_color(2, Some(6)).unwrap();
    editor.move_track(0, 2).unwrap();
    // t 0 -> end: [t 1, t 2, t 0]. Untouched t 1 keeps the palette colour it
    // showed at slot 1 (SPEC-060: defaults are pinned, not slot-derived).
    assert_eq!(colors(&editor), [Some(1), Some(6), Some(2)]);
}

#[test]
fn duplicate_track_copies_color_with_the_track() {
    let mut editor = editor_with(2);
    editor.set_track_color(0, Some(3)).unwrap();
    let copy = editor.duplicate_track(0).unwrap();
    assert_eq!(copy, 1);
    assert_eq!(editor.session().tracks[0].color, Some(3));
    assert_eq!(editor.session().tracks[1].color, Some(3));
}

#[test]
fn delete_track_keeps_remaining_colors_on_their_tracks() {
    let mut editor = editor_with(3);
    editor.set_track_color(0, Some(1)).unwrap();
    editor.set_track_color(1, Some(4)).unwrap();
    editor.set_track_color(2, Some(6)).unwrap();
    editor.delete_track(1);
    assert_eq!(colors(&editor), [Some(1), Some(6)]);
}

#[test]
fn moving_tracks_into_a_folder_keeps_color() {
    let mut editor = editor_with(3);
    editor.set_track_color(0, Some(2)).unwrap();
    editor.set_track_color(1, Some(5)).unwrap();
    let folder = editor.create_folder(&[0]).unwrap();
    assert_eq!(folder, 0);
    editor.move_tracks_to_folder(0, &[1]).unwrap();
    // Track 1 joins track 0 in the folder; colour rides on each track.
    assert_eq!(colors(&editor), [Some(2), Some(5), Some(2)]);
    assert_eq!(editor.session().folder_children[0], vec![0, 1]);
}

#[test]
fn moving_tracks_out_of_a_folder_keeps_color() {
    let mut editor = editor_with(3);
    editor.set_track_color(0, Some(2)).unwrap();
    editor.set_track_color(1, Some(5)).unwrap();
    let folder = editor.create_folder(&[0, 1]).unwrap();
    assert_eq!(folder, 0);
    // Drag track 0 out to the top level (below the folder's children).
    editor.move_tracks(&[0], 2).unwrap();
    assert_eq!(colors(&editor), [Some(5), Some(2), Some(2)]);
}
