//! Acceptance tests for editing buses and sends (SPEC-028).

use audio_core::{Bus, BusSend, Effect, EffectKind, MAX_BUSES, SessionError};
use daw_gui::{Editor, EditorError};

fn editor_with_tracks() -> Editor {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 48_000).unwrap();
    editor.add_midi_track(0, 48_000).unwrap();
    editor
}

fn sends(editor: &Editor, track: usize) -> Vec<BusSend> {
    editor.session().tracks[track].sends.clone()
}

#[test]
fn buses_are_appended_with_unity_gain() {
    let mut editor = editor_with_tracks();
    assert_eq!(editor.add_bus("Verb").unwrap(), 0);
    assert_eq!(editor.add_bus("Delay").unwrap(), 1);
    assert_eq!(
        editor.session().buses,
        vec![Bus::new("Verb"), Bus::new("Delay")]
    );
    assert!(editor.is_dirty());
}

#[test]
fn the_bus_list_is_capped() {
    let mut editor = editor_with_tracks();
    for _ in 0..MAX_BUSES {
        editor.add_bus("b").unwrap();
    }
    assert!(matches!(
        editor.add_bus("b").unwrap_err(),
        EditorError::Session(SessionError::InvalidBus { .. })
    ));
    assert_eq!(editor.session().buses.len(), MAX_BUSES);
}

#[test]
fn a_send_is_added_changed_and_removed_with_level_zero() {
    let mut editor = editor_with_tracks();
    editor.add_bus("Verb").unwrap();
    editor.set_send(0, 0, 0.5).unwrap();
    assert_eq!(sends(&editor, 0), vec![BusSend { bus: 0, level: 0.5 }]);
    editor.set_send(0, 0, 0.25).unwrap();
    assert_eq!(editor.session().tracks[0].send_level(0), 0.25);
    editor.set_send(0, 0, 0.0).unwrap();
    assert!(sends(&editor, 0).is_empty());
    assert!(editor.undo());
    assert_eq!(editor.session().tracks[0].send_level(0), 0.25);
}

#[test]
fn removing_a_bus_drops_its_sends_and_shifts_later_ones() {
    let mut editor = editor_with_tracks();
    for name in ["A", "B", "C"] {
        editor.add_bus(name).unwrap();
    }
    editor.set_send(0, 0, 0.1).unwrap();
    editor.set_send(0, 1, 0.2).unwrap();
    editor.set_send(1, 2, 0.3).unwrap();
    editor.remove_bus(1).unwrap();
    assert_eq!(sends(&editor, 0), vec![BusSend { bus: 0, level: 0.1 }]);
    assert_eq!(sends(&editor, 1), vec![BusSend { bus: 1, level: 0.3 }]);
    assert!(editor.undo());
    assert_eq!(editor.session().buses.len(), 3);
    assert_eq!(editor.session().tracks[0].send_level(1), 0.2);
    assert_eq!(editor.session().tracks[1].send_level(2), 0.3);
}

#[test]
fn bus_gain_and_effects_are_one_undo_step_each() {
    let mut editor = editor_with_tracks();
    editor.add_bus("Verb").unwrap();
    editor.set_bus_gain(0, 0.5).unwrap();
    assert_eq!(editor.add_bus_effect(0, EffectKind::reverb()).unwrap(), 0);
    let mut changed = Effect::new(EffectKind::reverb());
    changed.bypass = true;
    editor.set_bus_effect(0, 0, changed).unwrap();
    assert_eq!(editor.session().buses[0].effects, vec![changed]);
    editor.remove_bus_effect(0, 0).unwrap();
    assert!(editor.session().buses[0].effects.is_empty());
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(
        editor.session().buses[0].effects,
        vec![Effect::new(EffectKind::reverb())]
    );
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.session().buses[0].gain, 1.0);
}

#[test]
fn bad_values_and_indices_change_nothing() {
    let mut editor = editor_with_tracks();
    editor.add_bus("Verb").unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.set_send(0, 1, 0.5),
        Err(EditorError::NoBus)
    ));
    assert!(matches!(
        editor.set_send(5, 0, 0.5),
        Err(EditorError::NoBus)
    ));
    assert!(editor.set_send(0, 0, 1.5).is_err());
    assert!(editor.set_bus_gain(0, -0.1).is_err());
    assert!(matches!(
        editor.set_bus_gain(1, 0.5),
        Err(EditorError::NoBus)
    ));
    assert!(matches!(editor.remove_bus(3), Err(EditorError::NoBus)));
    assert!(matches!(
        editor.remove_bus_effect(0, 0),
        Err(EditorError::NoBus)
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn setting_the_same_value_records_nothing() {
    let mut editor = editor_with_tracks();
    editor.add_bus("Verb").unwrap();
    editor.set_bus_gain(0, 1.0).unwrap();
    editor.set_send(0, 0, 0.0).unwrap();
    assert!(editor.undo());
    assert!(editor.session().buses.is_empty());
}

// --- SPEC-051: bus->bus sends, mute, solo --------------------------------

fn bus_sends(editor: &Editor, bus: usize) -> Vec<BusSend> {
    editor.session().buses[bus].sends.clone()
}

#[test]
fn a_bus_send_is_added_changed_and_removed() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.add_bus("B").unwrap();
    editor.set_bus_send(0, 1, 0.5).unwrap();
    assert_eq!(bus_sends(&editor, 0), vec![BusSend { bus: 1, level: 0.5 }]);
    editor.set_bus_send(0, 1, 0.25).unwrap();
    assert_eq!(editor.session().buses[0].send_level(1), 0.25);
    editor.set_bus_send(0, 1, 0.0).unwrap();
    assert!(bus_sends(&editor, 0).is_empty());
    assert!(editor.undo());
    assert_eq!(editor.session().buses[0].send_level(1), 0.25);
}

#[test]
fn a_bus_self_send_is_rejected_without_change() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.add_bus("B").unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.set_bus_send(0, 0, 0.5),
        Err(EditorError::Session(SessionError::InvalidSend { .. }))
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn a_bus_send_cycle_is_rejected_without_change() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.add_bus("B").unwrap();
    editor.set_bus_send(0, 1, 1.0).unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.set_bus_send(1, 0, 1.0),
        Err(EditorError::Session(SessionError::InvalidSend { .. }))
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn bus_send_bad_indices_change_nothing() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.add_bus("B").unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.set_bus_send(0, 2, 0.5),
        Err(EditorError::NoBus)
    ));
    assert!(matches!(
        editor.set_bus_send(5, 0, 0.5),
        Err(EditorError::NoBus)
    ));
    assert!(editor.set_bus_send(0, 1, 1.5).is_err());
    assert_eq!(editor.session(), &before);
}

#[test]
fn bus_mute_and_solo_are_one_undo_step_each() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.add_bus("B").unwrap();
    editor.set_bus_mute(0, true).unwrap();
    assert!(editor.session().buses[0].mute);
    editor.set_bus_solo(1, true).unwrap();
    assert!(editor.session().buses[1].solo);
    assert!(editor.undo());
    assert!(!editor.session().buses[1].solo);
    assert!(editor.undo());
    assert!(!editor.session().buses[0].mute);
}

#[test]
fn setting_the_same_bus_flag_records_nothing() {
    let mut editor = editor_with_tracks();
    editor.add_bus("A").unwrap();
    editor.set_bus_mute(0, false).unwrap();
    editor.set_bus_solo(0, false).unwrap();
    assert!(editor.undo());
    assert!(editor.session().buses.is_empty());
}

#[test]
fn removing_a_bus_cleans_bus_sends_and_shifts_them() {
    let mut editor = editor_with_tracks();
    for name in ["A", "B", "C"] {
        editor.add_bus(name).unwrap();
    }
    // A sends to B (to be removed) and to C (to be shifted), C sends to B.
    editor.set_bus_send(0, 1, 0.1).unwrap();
    editor.set_bus_send(0, 2, 0.2).unwrap();
    editor.set_bus_send(2, 1, 0.3).unwrap();
    editor.remove_bus(1).unwrap();
    // The sends to the removed bus B are dropped; A's send to C shifts from
    // bus 2 to bus 1.
    assert_eq!(bus_sends(&editor, 0), vec![BusSend { bus: 1, level: 0.2 }]);
    assert!(bus_sends(&editor, 1).is_empty());
}
