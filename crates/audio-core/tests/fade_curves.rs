//! Acceptance tests for fade shapes and the layer weight (SPEC-045 §1–§2).

use audio_core::{Clip, Project, Session, SessionError, SessionTrack, shape_fade};

const SR: u32 = 48_000;

fn session_with(clip: Clip) -> Session {
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn a_linear_curve_is_bit_identical_to_the_plain_ramp() {
    for i in 0..=100 {
        let x = i as f32 / 100.0;
        assert_eq!(shape_fade(x, 0.0).to_bits(), x.to_bits());
    }
}

#[test]
fn curves_bend_the_ramp_but_keep_its_ends() {
    for curve in [-1.0, -0.5, 0.5, 1.0] {
        assert_eq!(shape_fade(0.0, curve), 0.0);
        assert_eq!(shape_fade(1.0, curve), 1.0);
    }
    assert!((shape_fade(0.5, 1.0) - 0.5_f32.powf(0.25)).abs() < 1e-6);
    assert!((shape_fade(0.5, -1.0) - 0.0625).abs() < 1e-6);
    assert!(shape_fade(0.5, 0.5) > 0.5);
    assert!(shape_fade(0.5, -0.5) < 0.5);
}

#[test]
fn the_clip_fade_follows_its_curve() {
    let clip = Clip {
        fade_in_samples: 100,
        fade_in_curve: -1.0,
        fade_out_samples: 100,
        fade_out_curve: 1.0,
        ..Clip::full(0, 1_000)
    };
    assert!((clip.fade_gain(50) - 0.0625).abs() < 1e-6);
    assert!((clip.fade_gain(949) - 0.5_f32.powf(0.25)).abs() < 1e-6);
    assert_eq!(clip.fade_gain(500), 1.0);
}

#[test]
fn curves_outside_the_range_are_rejected() {
    for bad in [1.5, -1.01, f32::NAN, f32::INFINITY] {
        let clip = Clip {
            fade_out_curve: bad,
            ..Clip::full(0, 100)
        };
        assert_eq!(
            session_with(clip).validate(),
            Err(SessionError::InvalidFadeCurve { track: 0, clip: 0 })
        );
    }
    let ok = Clip {
        fade_in_curve: -1.0,
        fade_out_curve: 1.0,
        ..Clip::full(0, 100)
    };
    assert_eq!(session_with(ok).validate(), Ok(()));
}

#[test]
fn curves_round_trip_and_linear_ones_are_not_written() {
    let project = Project {
        name: "p".into(),
        sample_rate: SR,
        tempo_bpm: 120.0,
        params: Vec::new(),
        session: session_with(Clip {
            fade_in_samples: 10,
            fade_in_curve: -0.25,
            fade_out_samples: 10,
            fade_out_curve: 0.75,
            ..Clip::full(0, 100)
        }),
    };
    let back = Project::from_json(&project.to_json()).unwrap();
    let clip = &back.session.tracks[0].clips[0];
    assert_eq!((clip.fade_in_curve, clip.fade_out_curve), (-0.25, 0.75));
    let plain = Project {
        session: session_with(Clip::full(0, 100)),
        ..project
    };
    assert!(!plain.to_json().contains("curve"));
}

#[test]
fn without_fades_the_layer_weight_is_the_overlap_weight() {
    let lower = Clip::full(0, 10_000);
    for upper in [Clip::full(6_000, 10_000), Clip::full(2_000, 2_000)] {
        for p in (upper.start_samples..upper.end_samples().min(10_000)).step_by(37) {
            assert_eq!(
                upper.layer_weight(None, Some(&lower), p).to_bits(),
                upper.overlap_weight(&lower, p).to_bits()
            );
        }
    }
}

#[test]
fn an_explicit_fade_in_replaces_the_automatic_ramp() {
    let lower = Clip::full(0, 10_000);
    let upper = Clip {
        fade_in_samples: 4_000,
        ..Clip::full(6_000, 10_000)
    };
    // Explicit 4000-sample fade = the overlap; no squared dip at the middle.
    assert!((upper.layer_weight(None, Some(&lower), 8_000) - 0.5).abs() < 1e-6);
}

#[test]
fn a_crossed_lower_clip_ignores_its_own_fade_out() {
    let lower = Clip {
        fade_out_samples: 4_000,
        ..Clip::full(0, 10_000)
    };
    let upper = Clip::full(6_000, 10_000);
    assert_eq!(lower.layer_weight(Some(&upper), None, 8_000), 1.0);
    // Without a clip over it the fade-out applies as before.
    assert!((lower.layer_weight(None, None, 8_000) - lower.fade_gain(8_000)).abs() < 1e-9);
}
