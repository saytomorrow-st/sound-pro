//! Acceptance tests for clip fades and overlap weights (SPEC-038 §1–§4).

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

#[test]
fn the_upper_clip_wins_away_from_its_edges() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(2_000, 2_000);
    assert_eq!(upper.layer_weight(&lower, 3_000), 1.0);
}

#[test]
fn the_upper_clip_crossfades_in_and_out_over_the_lower_one() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip::full(2_000, 2_000);
    let n = CROSSFADE_SAMPLES;
    assert_eq!(upper.layer_weight(&lower, 2_000), 0.0);
    assert!((upper.layer_weight(&lower, 2_000 + n / 2) - 0.5).abs() < 1e-6);
    assert_eq!(upper.layer_weight(&lower, 2_000 + n), 1.0);
    assert_eq!(upper.layer_weight(&lower, 3_999), 0.0);
    assert!((upper.layer_weight(&lower, 3_999 - n / 2) - 0.5).abs() < 1e-6);
}

#[test]
fn a_short_overlap_crossfades_over_all_of_it() {
    let lower = Clip::full(0, 1_000);
    let upper = Clip::full(900, 1_000);
    // Overlap is 100 samples: the ramp spans exactly those.
    assert_eq!(upper.layer_weight(&lower, 900), 0.0);
    assert!((upper.layer_weight(&lower, 950) - 0.5).abs() < 1e-6);
    assert!((upper.layer_weight(&lower, 999) - 0.99).abs() < 1e-6);
    // The upper end lies outside the lower clip: no fade-out ramp there.
    assert_eq!(upper.layer_weight(&lower, 1_899), 1.0);
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
