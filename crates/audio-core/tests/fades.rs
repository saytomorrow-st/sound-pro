//! Acceptance tests for clip fades and overlap weights (SPEC-038 §1–§3, SPEC-044 §2).

use audio_core::{CROSSFADE_SAMPLES, Clip, Project, Session, SessionError, SessionTrack};

const SR: u32 = 48_000;

fn faded(start: u64, length: u64, fade_in: u64, fade_out: u64) -> Clip {
    Clip {
        fade_in_samples: fade_in,
        fade_out_samples: fade_out,
        ..Clip::full(start, length)
    }
}

fn session_with(clip: Clip) -> Session {
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn a_clip_without_fades_has_unit_gain_everywhere() {
    let clip = Clip::full(100, 50);
    for position in 100..150 {
        assert_eq!(clip.fade_gain(position).to_bits(), 1.0_f32.to_bits());
    }
}

#[test]
fn fade_in_ramps_linearly_from_zero() {
    let clip = faded(100, 1_000, 10, 0);
    assert_eq!(clip.fade_gain(100), 0.0);
    assert!((clip.fade_gain(105) - 0.5).abs() < 1e-6);
    assert!((clip.fade_gain(109) - 0.9).abs() < 1e-6);
    assert_eq!(clip.fade_gain(110), 1.0);
}

#[test]
fn fade_out_reaches_zero_on_the_last_sample() {
    let clip = faded(0, 100, 0, 10);
    assert_eq!(clip.fade_gain(89), 1.0);
    assert!((clip.fade_gain(94) - 0.5).abs() < 1e-6);
    assert_eq!(clip.fade_gain(99), 0.0);
}

#[test]
fn touching_fades_take_the_smaller_ramp() {
    let clip = faded(0, 10, 5, 5);
    assert!((clip.fade_gain(4) - 0.8).abs() < 1e-6);
    assert!((clip.fade_gain(5) - 0.8).abs() < 1e-6);
}

#[test]
fn fades_longer_than_the_clip_are_rejected() {
    assert_eq!(
        session_with(faded(0, 10, 6, 5)).validate(),
        Err(SessionError::InvalidFade { track: 0, clip: 0 })
    );
    assert_eq!(
        session_with(faded(0, 10, u64::MAX, 1)).validate(),
        Err(SessionError::InvalidFade { track: 0, clip: 0 })
    );
    assert_eq!(session_with(faded(0, 10, 5, 5)).validate(), Ok(()));
}

// SPEC-044 §2 supersedes the SPEC-038 §4 layer tests: the later start is
// above regardless of list order, and a running-out overlap is one
// crossfade across all of it.

#[test]
fn the_later_start_is_above_regardless_of_list_order() {
    let early = Clip::full(0, 1_000);
    let late = Clip::full(500, 1_000);
    assert!(late.is_above(0, &early, 1));
    assert!(late.is_above(1, &early, 0));
    assert!(!early.is_above(1, &late, 0));
}

#[test]
fn a_clip_inside_another_is_above_it() {
    let outer = Clip::full(0, 10_000);
    let inner = Clip::full(0, 2_000);
    assert!(inner.is_above(0, &outer, 1));
    assert!(!outer.is_above(1, &inner, 0));
}

#[test]
fn identical_spans_fall_back_to_the_list_order() {
    let a = Clip::full(0, 1_000);
    assert!(a.is_above(1, &a, 0));
    assert!(!a.is_above(0, &a, 1));
}

#[test]
fn an_overlap_running_past_the_lower_end_crossfades_over_all_of_it() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(6_000, 10_000);
    assert_eq!(upper.overlap_weight(&lower, 6_000), 0.0);
    assert!((upper.overlap_weight(&lower, 8_000) - 0.5).abs() < 1e-6);
    assert!((upper.overlap_weight(&lower, 9_999) - 0.999_75).abs() < 1e-6);
}

#[test]
fn a_clip_inside_another_ramps_in_and_out_at_its_edges() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(2_000, 2_000);
    let n = CROSSFADE_SAMPLES;
    assert_eq!(upper.overlap_weight(&lower, 2_000), 0.0);
    assert!((upper.overlap_weight(&lower, 2_000 + n / 2) - 0.5).abs() < 1e-6);
    assert_eq!(upper.overlap_weight(&lower, 3_000), 1.0);
    assert_eq!(upper.overlap_weight(&lower, 3_999), 0.0);
}

#[test]
fn a_short_clip_inside_another_ramps_over_its_halves() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(1_000, 100);
    assert_eq!(upper.overlap_weight(&lower, 1_000), 0.0);
    assert!((upper.overlap_weight(&lower, 1_025) - 0.5).abs() < 1e-6);
    assert!((upper.overlap_weight(&lower, 1_049) - 0.98).abs() < 1e-6);
}

#[test]
fn a_shared_start_has_no_ramp_in() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(0, 2_000);
    assert_eq!(upper.overlap_weight(&lower, 0), 1.0);
}

#[test]
fn fades_round_trip_and_old_files_load_with_none() {
    let project = Project {
        name: "p".into(),
        sample_rate: SR,
        tempo_bpm: 120.0,
        params: Vec::new(),
        session: session_with(faded(0, 1_000, 30, 40)),
    };
    let text = project.to_json();
    assert!(text.contains("\"fade_in\": 30"));
    let back = Project::from_json(&text).unwrap();
    let clip = &back.session.tracks[0].clips[0];
    assert_eq!((clip.fade_in_samples, clip.fade_out_samples), (30, 40));

    let plain = Project {
        session: session_with(Clip::full(0, 1_000)),
        ..project
    };
    let text = plain.to_json();
    assert!(!text.contains("fade_in"), "zero fades are not written");
    let clip = Project::from_json(&text).unwrap().session.tracks[0].clips[0].clone();
    assert_eq!((clip.fade_in_samples, clip.fade_out_samples), (0, 0));
}
