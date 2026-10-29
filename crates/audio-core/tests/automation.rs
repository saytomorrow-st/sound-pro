//! SPEC-023: automation lanes in the session model.

use audio_core::{
    AutoPoint, Automation, Lane, MAX_AUTOMATION_POINTS, Session, SessionError, SessionTrack,
    pan_law,
};

fn p(time: u64, value: f32) -> AutoPoint {
    AutoPoint { time, value }
}

fn session_with(lane: Lane, points: Vec<AutoPoint>) -> Session {
    let mut track = SessionTrack::new("t");
    *track.automation.lane_mut(lane) = points;
    let mut session = Session::new("s", 48_000, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn an_empty_lane_has_no_value() {
    assert_eq!(Automation::value_at(&[], 0), None);
    assert!(SessionTrack::new("t").automation.is_empty());
}

#[test]
fn a_single_point_holds_everywhere() {
    let points = [p(100, 0.5)];
    for time in [0, 100, 1_000_000] {
        assert_eq!(Automation::value_at(&points, time), Some(0.5));
    }
}

#[test]
fn values_are_held_outside_and_linear_between_points() {
    let points = [p(100, 0.0), p(200, 1.0), p(300, 0.5)];
    assert_eq!(Automation::value_at(&points, 0), Some(0.0));
    assert_eq!(Automation::value_at(&points, 100), Some(0.0));
    assert_eq!(Automation::value_at(&points, 150), Some(0.5));
    assert_eq!(Automation::value_at(&points, 200), Some(1.0));
    assert_eq!(Automation::value_at(&points, 250), Some(0.75));
    assert_eq!(Automation::value_at(&points, 300), Some(0.5));
    assert_eq!(Automation::value_at(&points, u64::MAX), Some(0.5));
}

#[test]
fn valid_lanes_pass_validation() {
    let session = session_with(Lane::Gain, vec![p(0, 0.0), p(10, 1.0)]);
    assert_eq!(session.validate(), Ok(()));
    let session = session_with(Lane::Pan, vec![p(0, -1.0), p(10, 1.0)]);
    assert_eq!(session.validate(), Ok(()));
}

#[test]
fn points_must_be_strictly_ordered() {
    for points in [vec![p(10, 0.5), p(10, 0.5)], vec![p(10, 0.5), p(5, 0.5)]] {
        assert_eq!(
            session_with(Lane::Gain, points).validate(),
            Err(SessionError::InvalidAutomation {
                track: 0,
                lane: Lane::Gain,
                point: 1
            })
        );
    }
}

#[test]
fn values_out_of_range_are_rejected() {
    for (lane, value) in [
        (Lane::Gain, 1.5),
        (Lane::Gain, -0.1),
        (Lane::Gain, f32::NAN),
        (Lane::Pan, -2.0),
        (Lane::Pan, f32::INFINITY),
    ] {
        assert_eq!(
            session_with(lane, vec![p(0, value)]).validate(),
            Err(SessionError::InvalidAutomation {
                track: 0,
                lane,
                point: 0
            }),
            "{lane} {value}"
        );
    }
}

#[test]
fn too_many_points_are_rejected() {
    let points = (0..=MAX_AUTOMATION_POINTS as u64)
        .map(|t| p(t, 0.5))
        .collect();
    assert_eq!(
        session_with(Lane::Pan, points).validate(),
        Err(SessionError::InvalidAutomation {
            track: 0,
            lane: Lane::Pan,
            point: MAX_AUTOMATION_POINTS
        })
    );
    let points = (0..MAX_AUTOMATION_POINTS as u64)
        .map(|t| p(t, 0.5))
        .collect();
    assert_eq!(session_with(Lane::Pan, points).validate(), Ok(()));
}

#[test]
fn the_pan_law_matches_the_track_helper() {
    assert_eq!(pan_law(0.0), (1.0, 1.0));
    assert_eq!(pan_law(-1.0), (1.0, 0.0));
    assert_eq!(pan_law(1.0), (0.0, 1.0));
    let mut track = SessionTrack::new("t");
    track.pan = 0.25;
    assert_eq!(track.pan_gains(), pan_law(0.25));
}

#[test]
fn the_error_names_the_lane() {
    let text = SessionError::InvalidAutomation {
        track: 2,
        lane: Lane::Pan,
        point: 3,
    }
    .to_string();
    assert!(text.contains("pan") && text.contains('3'), "{text}");
}
