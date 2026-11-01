//! Acceptance tests for transport and ruler geometry (SPEC-040).

use daw_gui::timeline::{
    CYCLE_EDGE_PX, CycleHit, bar_samples, cycle_hit, move_cycle, resize_cycle, step_bar,
};

#[test]
fn cycle_hit_finds_edges_body_and_empty() {
    let c = Some((100.0, 300.0));
    assert_eq!(cycle_hit(100.0, c), CycleHit::Start);
    assert_eq!(cycle_hit(100.0 - CYCLE_EDGE_PX + 1.0, c), CycleHit::Start);
    assert_eq!(cycle_hit(300.0, c), CycleHit::End);
    assert_eq!(cycle_hit(200.0, c), CycleHit::Body);
    assert_eq!(cycle_hit(50.0, c), CycleHit::Empty);
    assert_eq!(cycle_hit(400.0, c), CycleHit::Empty);
    assert_eq!(cycle_hit(200.0, None), CycleHit::Empty);
}

#[test]
fn a_narrow_cycle_keeps_both_edges_reachable() {
    let c = Some((100.0, 104.0));
    assert_eq!(cycle_hit(101.0, c), CycleHit::Start);
    assert_eq!(cycle_hit(103.5, c), CycleHit::End);
}

#[test]
fn bar_length_follows_tempo_and_meter() {
    let map = audio_core::TempoMap::single_or_default(120.0, 48_000);
    assert_eq!(bar_samples(&map, 4), 96_000);
    assert_eq!(bar_samples(&map, 3), 72_000);
    let nan = audio_core::TempoMap::single_or_default(f64::NAN, 48_000);
    assert_eq!(bar_samples(&nan, 0), 96_000);
}

#[test]
fn stepping_lands_on_bar_lines() {
    let bar = 1000;
    assert_eq!(step_bar(0, bar, true), 1000);
    assert_eq!(step_bar(1500, bar, true), 2000);
    assert_eq!(step_bar(2000, bar, true), 3000);
    assert_eq!(step_bar(1500, bar, false), 1000);
    assert_eq!(step_bar(2000, bar, false), 1000);
    assert_eq!(step_bar(0, bar, false), 0);
}

#[test]
fn moving_a_cycle_keeps_its_length_and_stops_at_zero() {
    assert_eq!(move_cycle(1000, 3000, 500), (1500, 3500));
    assert_eq!(move_cycle(1000, 3000, -5000), (0, 2000));
}

#[test]
fn resizing_swaps_crossed_edges_and_keeps_a_minimum() {
    assert_eq!(
        resize_cycle(1000, 3000, CycleHit::End, 4000, 10),
        (1000, 4000)
    );
    assert_eq!(
        resize_cycle(1000, 3000, CycleHit::Start, 500, 10),
        (500, 3000)
    );
    assert_eq!(
        resize_cycle(1000, 3000, CycleHit::End, 200, 10),
        (200, 1000)
    );
    assert_eq!(
        resize_cycle(1000, 3000, CycleHit::End, 1000, 10),
        (1000, 1010)
    );
    assert_eq!(
        resize_cycle(1000, 3000, CycleHit::Body, 9, 10),
        (1000, 3000)
    );
}

#[test]
fn piano_zoom_is_clamped_and_ignores_bad_factors() {
    use daw_gui::timeline::{PIANO_ZOOM_MAX, PIANO_ZOOM_MIN, piano_zoom};
    assert!((piano_zoom(1.0, 2.0) - 2.0).abs() < 1e-6);
    assert!((piano_zoom(6.0, 4.0) - PIANO_ZOOM_MAX).abs() < 1e-6);
    assert!((piano_zoom(0.5, 0.1) - PIANO_ZOOM_MIN).abs() < 1e-6);
    assert!((piano_zoom(1.5, f32::NAN) - 1.5).abs() < 1e-6);
    assert!((piano_zoom(1.5, 0.0) - 1.5).abs() < 1e-6);
}

#[test]
fn keyed_zoom_keeps_the_time_under_the_pointer() {
    use daw_gui::timeline::keyed_zoom_offset;
    let (fixed, offset, anchor) = (88.0, 100.0, 300.0);
    let new = keyed_zoom_offset(1.0, 2.0, offset, anchor, fixed);
    // Time under the pointer before: (offset + anchor - fixed) / 1.0;
    // after: (new + anchor - fixed) / 2.0 — must match.
    let before = offset + anchor - fixed;
    let after = (new + anchor - fixed) / 2.0;
    assert!((before - after).abs() < 1e-3);
    // Pointer over the keyboard: no jump.
    assert!((keyed_zoom_offset(1.0, 2.0, 0.0, 40.0, fixed) - 0.0).abs() < 1e-6);
    assert!((keyed_zoom_offset(0.0, 2.0, 50.0, 40.0, fixed) - 50.0).abs() < 1e-6);
}
