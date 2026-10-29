//! SPEC-040 §2: clip stacking order on a track.

use audio_core::session::Clip;
use daw_gui::{Editor, EditorError};

fn three() -> Editor {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 1_000).unwrap();
    let track = editor.track_mut(0).unwrap();
    track.clips.push(Clip::midi(100, 1_000));
    track.clips.push(Clip::midi(200, 1_000));
    editor
}

fn starts(editor: &Editor) -> Vec<u64> {
    editor.session().tracks[0]
        .clips
        .iter()
        .map(|c| c.start_samples)
        .collect()
}

#[test]
fn bring_to_front_puts_the_clip_last() {
    let mut editor = three();
    assert_eq!(editor.bring_to_front(0, 0).unwrap(), 2);
    assert_eq!(starts(&editor), [100, 200, 0]);
    assert_eq!(editor.selected_clip, Some((0, 2)));
    assert!(editor.undo());
    assert_eq!(starts(&editor), [0, 100, 200]);
}

#[test]
fn send_to_back_puts_the_clip_first() {
    let mut editor = three();
    assert_eq!(editor.send_to_back(0, 2).unwrap(), 0);
    assert_eq!(starts(&editor), [200, 0, 100]);
    assert_eq!(editor.selected_clip, Some((0, 0)));
}

#[test]
fn a_clip_already_in_place_adds_no_undo_step() {
    let mut editor = three();
    let undo_before = editor.can_undo();
    assert_eq!(editor.bring_to_front(0, 2).unwrap(), 2);
    assert_eq!(editor.send_to_back(0, 0).unwrap(), 0);
    assert_eq!(editor.can_undo(), undo_before);
    assert_eq!(starts(&editor), [0, 100, 200]);
}

#[test]
fn bad_indices_are_errors() {
    let mut editor = three();
    assert!(matches!(
        editor.bring_to_front(0, 3),
        Err(EditorError::NoClip)
    ));
    assert!(matches!(
        editor.send_to_back(1, 0),
        Err(EditorError::NoClip)
    ));
}
