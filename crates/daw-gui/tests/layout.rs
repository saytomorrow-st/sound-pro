//! Acceptance tests for the main window layout (SPEC-034).

use daw_gui::layout::{
    Bottom, FADER_FLOOR_DB, Layout, PALETTE_SIZE, db_label, display_color, fader_gain,
    fader_position, lcd_position, pan_angle, pan_label, region_colors, shade,
};

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

#[test]
fn tracks_get_palette_colors_by_position() {
    assert_eq!(display_color(Some(3), 0), 3);
    assert_eq!(display_color(None, 0), 0);
    assert_eq!(display_color(None, 5), 5);
    assert_eq!(display_color(None, 9), 1);
    assert_eq!(display_color(Some(200), 2), 2);
    for i in 0..40 {
        assert!(display_color(None, i) < PALETTE_SIZE);
    }
}

#[test]
fn shade_darkens_and_lightens() {
    assert_eq!(shade([200, 100, 50], 1.0), [200, 100, 50]);
    assert_eq!(shade([200, 100, 50], 0.5), [100, 50, 25]);
    assert_eq!(shade([200, 100, 50], 0.0), [0, 0, 0]);
    assert_eq!(shade([200, 100, 50], 2.0), [255, 255, 255]);
    assert_eq!(shade([200, 100, 50], f32::NAN), [200, 100, 50]);
    assert_eq!(shade([0, 0, 0], 1.5), [128, 128, 128]);
}

#[test]
fn region_header_darker_than_body_and_detail_darker_still() {
    let c = region_colors([80, 160, 220], false);
    assert_eq!(c.body, [80, 160, 220]);
    for ch in 0..3 {
        assert!(c.header[ch] < c.body[ch]);
        assert!(c.detail[ch] < c.header[ch]);
    }
    let muted = region_colors([80, 160, 220], true);
    assert_eq!(muted.body[0], muted.body[1]);
    assert_eq!(muted.body[1], muted.body[2]);
}

#[test]
fn fader_uses_decibel_scale() {
    assert!((fader_position(1.0) - 1.0).abs() < 1e-6);
    assert!(fader_position(0.0).abs() < 1e-6);
    assert!(fader_position(f32::NAN).abs() < 1e-6);
    assert!((fader_position(2.0) - 1.0).abs() < 1e-6);
    let minus6 = fader_position(0.5);
    assert!((minus6 - 0.9).abs() < 0.01, "{minus6}");
    let floor = 10f32.powf(FADER_FLOOR_DB / 20.0);
    assert!(fader_position(floor * 0.5).abs() < 1e-6);
}

#[test]
fn fader_gain_inverts_position() {
    assert!(fader_gain(0.0).abs() < 1e-9);
    assert!((fader_gain(1.0) - 1.0).abs() < 1e-6);
    assert!(fader_gain(f32::NAN).abs() < 1e-9);
    for gain in [0.01_f32, 0.1, 0.25, 0.5, 0.8, 1.0] {
        let back = fader_gain(fader_position(gain));
        assert!(
            (back - gain).abs() < 1e-4 * gain.max(0.01),
            "{gain} -> {back}"
        );
    }
}

#[test]
fn pan_knob_angle() {
    assert!(pan_angle(0.0).abs() < 1e-6);
    assert!((pan_angle(-1.0) + 135f32.to_radians()).abs() < 1e-6);
    assert!((pan_angle(1.0) - 135f32.to_radians()).abs() < 1e-6);
    assert!((pan_angle(9.0) - 135f32.to_radians()).abs() < 1e-6);
    assert!(pan_angle(f32::NAN).abs() < 1e-6);
}

#[test]
fn zoom_keeps_the_anchor_in_place() {
    use daw_gui::layout::zoom_offset;
    // The point 300 px into the view sits at content x = 500 before; at
    // double zoom it is at 1000, so the view must start at 700.
    assert!((zoom_offset(100.0, 200.0, 200.0, 300.0) - 700.0).abs() < 1e-3);
    // Zooming back out returns to the original offset.
    assert!((zoom_offset(200.0, 100.0, 700.0, 300.0) - 200.0).abs() < 1e-3);
    // An anchor at the left edge with no scroll stays at zero.
    assert!(zoom_offset(100.0, 400.0, 0.0, 0.0).abs() < 1e-3);
}

#[test]
fn zoom_offset_never_goes_negative_or_nan() {
    use daw_gui::layout::zoom_offset;
    assert!(zoom_offset(100.0, 10.0, 50.0, 400.0) >= 0.0);
    assert!((zoom_offset(0.0, 10.0, 50.0, 400.0) - 50.0).abs() < 1e-3);
    assert!((zoom_offset(f32::NAN, 10.0, 50.0, 400.0) - 50.0).abs() < 1e-3);
}

#[test]
fn slider_zoom_anchors_to_a_visible_playhead() {
    use daw_gui::layout::slider_anchor;
    assert!((slider_anchor(450.0, 200.0, 800.0) - 250.0).abs() < 1e-3);
    // Off screen: the middle of the view.
    assert!((slider_anchor(50.0, 200.0, 800.0) - 400.0).abs() < 1e-3);
    assert!((slider_anchor(5_000.0, 200.0, 800.0) - 400.0).abs() < 1e-3);
}
