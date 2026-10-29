//! Acceptance tests for the main window layout (SPEC-034).

use daw_gui::layout::{Bottom, Layout, db_label, lcd_position, pan_label};

const RATE: u32 = 48_000;

#[test]
fn default_layout_shows_inspector_only() {
    let layout = Layout::default();
    assert!(layout.inspector);
    assert!(!layout.library);
    assert!(!layout.browser);
    assert_eq!(layout.bottom, None);
}

#[test]
fn bottom_toggle_switches_and_hides() {
    let mut layout = Layout::default();
    layout.toggle_bottom(Bottom::Mixer);
    assert_eq!(layout.bottom, Some(Bottom::Mixer));
    layout.toggle_bottom(Bottom::Editor);
    assert_eq!(layout.bottom, Some(Bottom::Editor));
    layout.toggle_bottom(Bottom::Editor);
    assert_eq!(layout.bottom, None);
}

#[test]
fn opening_clip_shows_editor_even_when_mixer_open() {
    let mut layout = Layout::default();
    layout.toggle_bottom(Bottom::Mixer);
    layout.show_editor();
    assert_eq!(layout.bottom, Some(Bottom::Editor));
    layout.show_editor();
    assert_eq!(layout.bottom, Some(Bottom::Editor));
}

#[test]
fn lcd_starts_at_one() {
    assert_eq!(lcd_position(0, RATE, 120.0, 4), [1, 1, 1, 1]);
}

#[test]
fn lcd_counts_beats_sixteenths_and_ticks() {
    // 120 BPM: one beat = 0.5 s = 24000 samples, a sixteenth = 6000.
    assert_eq!(lcd_position(24_000, RATE, 120.0, 4), [1, 2, 1, 1]);
    assert_eq!(lcd_position(6_000, RATE, 120.0, 4), [1, 1, 2, 1]);
    assert_eq!(lcd_position(6_025, RATE, 120.0, 4), [1, 1, 2, 2]);
    assert_eq!(lcd_position(4 * 24_000, RATE, 120.0, 4), [2, 1, 1, 1]);
    assert_eq!(lcd_position(5_999, RATE, 120.0, 4), [1, 1, 1, 240]);
}

#[test]
fn lcd_follows_time_signature() {
    assert_eq!(lcd_position(3 * 24_000, RATE, 120.0, 3), [2, 1, 1, 1]);
    assert_eq!(lcd_position(3 * 24_000, RATE, 120.0, 0), [1, 4, 1, 1]);
}

#[test]
fn lcd_survives_bad_input() {
    assert_eq!(lcd_position(24_000, RATE, f64::NAN, 4), [1, 2, 1, 1]);
    assert_eq!(lcd_position(24_000, RATE, 0.0, 4), [1, 2, 1, 1]);
    assert_eq!(lcd_position(0, 0, 120.0, 4), [1, 1, 1, 1]);
}

#[test]
fn db_label_values() {
    assert_eq!(db_label(1.0), "0.0");
    assert_eq!(db_label(0.0), "-∞");
    assert_eq!(db_label(f32::NAN), "-∞");
    assert_eq!(db_label(0.5), "-6.0");
}

#[test]
fn pan_label_values() {
    assert_eq!(pan_label(0.0), "C");
    assert_eq!(pan_label(-1.0), "L64");
    assert_eq!(pan_label(1.0), "R64");
    assert_eq!(pan_label(0.5), "R32");
    assert_eq!(pan_label(f32::NAN), "C");
    assert_eq!(pan_label(5.0), "R64");
}
