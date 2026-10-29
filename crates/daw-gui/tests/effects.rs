//! Acceptance tests for editing track effects (SPEC-022).

use audio_core::{Effect, EffectKind, MAX_EFFECTS_PER_TRACK, SessionError};
use daw_gui::{Editor, EditorError};

fn editor_with_track() -> Editor {
    let mut editor = Editor::new();
    editor.add_midi_track(0, 48_000).unwrap();
    editor
}

fn chain(editor: &Editor) -> Vec<Effect> {
    editor.session().tracks[0].effects.clone()
}

fn types(editor: &Editor) -> Vec<&'static str> {
    chain(editor).iter().map(|e| e.kind.type_name()).collect()
}

#[test]
fn effects_are_appended_with_defaults_and_return_their_index() {
    let mut editor = editor_with_track();
    assert_eq!(editor.add_effect(0, EffectKind::eq()).unwrap(), 0);
    assert_eq!(editor.add_effect(0, EffectKind::reverb()).unwrap(), 1);
    assert_eq!(
        chain(&editor),
        vec![
            Effect::new(EffectKind::eq()),
            Effect::new(EffectKind::reverb())
        ]
    );
    assert!(editor.is_dirty());
}

#[test]
fn each_effect_edit_is_one_undo_step() {
    let mut editor = editor_with_track();
    editor.add_effect(0, EffectKind::compressor()).unwrap();
    let mut changed = Effect::new(EffectKind::compressor());
    changed.bypass = true;
    editor.set_effect(0, 0, changed).unwrap();
    assert_eq!(chain(&editor), vec![changed]);
    assert!(editor.undo());
    assert_eq!(chain(&editor), vec![Effect::new(EffectKind::compressor())]);
    assert!(editor.undo());
    assert!(chain(&editor).is_empty());
    assert!(editor.redo());
    assert_eq!(types(&editor), ["compressor"]);
}

#[test]
fn setting_the_same_value_records_nothing() {
    let mut editor = editor_with_track();
    editor.add_effect(0, EffectKind::eq()).unwrap();
    editor
        .set_effect(0, 0, Effect::new(EffectKind::eq()))
        .unwrap();
    assert!(editor.undo());
    assert!(chain(&editor).is_empty());
}

#[test]
fn effects_can_be_reordered_and_removed() {
    let mut editor = editor_with_track();
    editor.add_effect(0, EffectKind::eq()).unwrap();
    editor.add_effect(0, EffectKind::compressor()).unwrap();
    editor.add_effect(0, EffectKind::reverb()).unwrap();
    editor.move_effect(0, 2, 0).unwrap();
    assert_eq!(types(&editor), ["reverb", "eq", "compressor"]);
    editor.move_effect(0, 0, 2).unwrap();
    assert_eq!(types(&editor), ["eq", "compressor", "reverb"]);
    editor.remove_effect(0, 1).unwrap();
    assert_eq!(types(&editor), ["eq", "reverb"]);
    assert!(editor.undo());
    assert_eq!(types(&editor), ["eq", "compressor", "reverb"]);
}

#[test]
fn bad_indices_are_reported_and_change_nothing() {
    let mut editor = editor_with_track();
    editor.add_effect(0, EffectKind::eq()).unwrap();
    let before = editor.session().clone();
    assert!(matches!(
        editor.add_effect(5, EffectKind::eq()),
        Err(EditorError::NoEffect)
    ));
    assert!(matches!(
        editor.remove_effect(0, 1),
        Err(EditorError::NoEffect)
    ));
    assert!(matches!(
        editor.set_effect(0, 3, Effect::new(EffectKind::eq())),
        Err(EditorError::NoEffect)
    ));
    assert!(matches!(
        editor.move_effect(0, 0, 1),
        Err(EditorError::NoEffect)
    ));
    assert_eq!(editor.session(), &before);
}

#[test]
fn a_full_chain_and_out_of_range_values_are_rejected() {
    let mut editor = editor_with_track();
    for _ in 0..MAX_EFFECTS_PER_TRACK {
        editor.add_effect(0, EffectKind::eq()).unwrap();
    }
    let before = editor.session().clone();
    assert!(matches!(
        editor.add_effect(0, EffectKind::reverb()),
        Err(EditorError::Session(SessionError::InvalidEffect {
            track: 0,
            effect
        })) if effect == MAX_EFFECTS_PER_TRACK
    ));
    let bad = Effect::new(EffectKind::Compressor {
        threshold_db: -20.0,
        ratio: 0.5,
        attack_ms: 10.0,
        release_ms: 100.0,
        makeup_db: 0.0,
    });
    assert!(matches!(
        editor.set_effect(0, 0, bad),
        Err(EditorError::Session(SessionError::InvalidEffect { .. }))
    ));
    assert_eq!(editor.session(), &before);
}
