//! Acceptance tests for the session model (SPEC-010 §1–2).
//!
//! Every rule in the SPEC-010 validation table gets a test here, and the two
//! length helpers get their own: those are the numbers the renderer trusts
//! for the default frame count, so an off-by-one would silently truncate the
//! mix.

use audio_core::{Clip, MAX_CLIPS_PER_TRACK, MAX_TRACKS, Session, SessionError, SessionTrack};

const SR: u32 = 48_000;

fn clip(start: u64, length: u64, source: u64) -> Clip {
    Clip {
        start_samples: start,
        length_samples: length,
        offset_samples: 0,
        source_length: source,
        gain: 1.0,
        name: String::new(),
        source: None,
        notes: None,
        fade_in_samples: 0,
        fade_out_samples: 0,
        fade_in_curve: 0.0,
        fade_out_curve: 0.0,
    }
}

fn session_with(clips: Vec<Clip>) -> Session {
    let mut track = SessionTrack::new("t");
    track.clips = clips;
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    session
}

fn error_of(session: &Session) -> SessionError {
    session.validate().expect_err("session should not validate")
}

#[test]
fn an_empty_session_is_valid_and_has_no_length() {
    let session = Session::new("empty", SR, 120.0);
    assert!(session.tracks.is_empty());
    assert_eq!(session.validate(), Ok(()));
    assert_eq!(session.length_samples(), 0);
    assert_eq!(session.track_length_samples(0), 0);
}

#[test]
fn a_new_untitled_session_is_named_untitled_and_keeps_the_tempo() {
    let session = Session::new_untitled(SR, 96.5);
    assert_eq!(session.name, "untitled");
    assert_eq!(session.sample_rate, SR);
    assert_eq!(session.tempo_bpm, 96.5);
    assert!(session.tracks.is_empty());
}

#[test]
fn a_zero_sample_rate_is_rejected() {
    let mut session = Session::new("s", SR, 120.0);
    session.sample_rate = 0;
    assert_eq!(error_of(&session), SessionError::InvalidSampleRate);
}

#[test]
fn a_non_positive_tempo_is_rejected() {
    for tempo in [0.0, -1.0] {
        let mut session = Session::new("s", SR, 120.0);
        session.tempo_bpm = tempo;
        assert_eq!(error_of(&session), SessionError::InvalidTempo);
    }
}

#[test]
fn a_non_finite_tempo_is_rejected() {
    let mut session = Session::new("s", SR, 120.0);
    session.tempo_bpm = f64::NAN;
    assert_eq!(error_of(&session), SessionError::InvalidTempo);
}

#[test]
fn names_longer_than_128_bytes_are_rejected() {
    let long = "x".repeat(129);
    let exact = "x".repeat(128);

    let mut session = Session::new(&long, SR, 120.0);
    assert_eq!(error_of(&session), SessionError::NameTooLong);
    session.name = exact.clone();
    assert_eq!(session.validate(), Ok(()));

    let mut session = session_with(vec![clip(0, 10, 10)]);
    session.tracks[0].name = long.clone();
    assert_eq!(error_of(&session), SessionError::NameTooLong);
    session.tracks[0].name = exact.clone();
    assert_eq!(session.validate(), Ok(()));

    let mut session = session_with(vec![clip(0, 10, 10)]);
    session.tracks[0].clips[0].name = long;
    assert_eq!(error_of(&session), SessionError::NameTooLong);
}

#[test]
fn a_clip_of_zero_length_is_rejected() {
    let session = session_with(vec![clip(100, 0, 100)]);
    assert_eq!(
        error_of(&session),
        SessionError::EmptyClip { track: 0, clip: 0 }
    );
}

#[test]
fn a_clip_without_content_is_rejected() {
    let session = session_with(vec![clip(0, 0, 0)]);
    assert_eq!(
        error_of(&session),
        SessionError::EmptyClip { track: 0, clip: 0 }
    );

    // A clip with visible length but no content behind it is a different error.
    let session = session_with(vec![clip(0, 5, 0)]);
    assert_eq!(
        error_of(&session),
        SessionError::EmptySource { track: 0, clip: 0 }
    );
}

#[test]
fn a_clip_reaching_past_its_content_is_rejected() {
    // offset 10 + length 10 = 20 > source 19: one frame too far.
    let mut only = clip(0, 10, 19);
    only.offset_samples = 10;
    let session = session_with(vec![only]);
    assert_eq!(
        error_of(&session),
        SessionError::ClipOutOfSource { track: 0, clip: 0 }
    );

    // Exactly at the end is fine.
    let mut exact = clip(0, 10, 20);
    exact.offset_samples = 10;
    assert_eq!(session_with(vec![exact]).validate(), Ok(()));
}

#[test]
fn a_clip_whose_timeline_end_overflows_is_rejected() {
    let session = session_with(vec![clip(u64::MAX, 1, 10)]);
    assert_eq!(
        error_of(&session),
        SessionError::TimelineOverflow { track: 0, clip: 0 }
    );
}

#[test]
fn an_out_of_range_track_gain_is_rejected() {
    for gain in [-0.001, 1.5, f32::NAN, f32::INFINITY] {
        let mut session = session_with(vec![clip(0, 10, 10)]);
        session.tracks[0].gain = gain;
        assert_eq!(
            error_of(&session),
            SessionError::InvalidGain { track: 0 },
            "gain {gain}"
        );
    }
    for gain in [0.0, 0.5, 1.0] {
        let mut session = session_with(vec![clip(0, 10, 10)]);
        session.tracks[0].gain = gain;
        assert_eq!(session.validate(), Ok(()), "gain {gain}");
    }
}

#[test]
fn an_out_of_range_clip_gain_is_rejected() {
    for gain in [-0.5, 2.0, f32::NAN] {
        let mut only = clip(0, 10, 10);
        only.gain = gain;
        let session = session_with(vec![only]);
        assert_eq!(
            error_of(&session),
            SessionError::InvalidClipGain { track: 0, clip: 0 },
            "gain {gain}"
        );
    }
}

#[test]
fn the_limits_are_32_tracks_and_128_clips() {
    let mut session = session_with(vec![]);
    session.tracks = (0..MAX_TRACKS)
        .map(|i| SessionTrack::new(&format!("t{i}")))
        .collect();
    assert_eq!(session.validate(), Ok(()));
    session.tracks.push(SessionTrack::new("one too many"));
    assert_eq!(
        error_of(&session),
        SessionError::TooManyTracks { max: MAX_TRACKS }
    );

    let mut session = session_with(vec![clip(0, 10, 10); MAX_CLIPS_PER_TRACK]);
    assert_eq!(session.validate(), Ok(()));
    session.tracks[0].clips.push(clip(20, 10, 10));
    assert_eq!(
        error_of(&session),
        SessionError::TooManyClips {
            track: 0,
            max: MAX_CLIPS_PER_TRACK,
        }
    );
}

#[test]
fn the_error_reports_the_position_in_the_model() {
    let mut first = SessionTrack::new("a");
    first.clips.push(clip(0, 10, 10));
    let mut second = SessionTrack::new("b");
    second.clips.push(clip(0, 10, 10));
    second.clips.push(clip(0, 0, 10));
    let mut session = Session::new("s", SR, 120.0);
    session.tracks = vec![first, second];
    assert_eq!(
        error_of(&session),
        SessionError::EmptyClip { track: 1, clip: 1 }
    );
}

#[test]
fn the_session_length_is_the_end_of_the_last_clip() {
    let mut session = session_with(vec![clip(0, 100, 100), clip(500, 250, 250)]);
    session.tracks.push(SessionTrack::new("silent"));
    assert_eq!(session.length_samples(), 750);
    assert_eq!(session.track_length_samples(0), 750);
    // An empty track contributes nothing; neither does an index past the end.
    assert_eq!(session.track_length_samples(1), 0);
    assert_eq!(session.track_length_samples(2), 0);
}

#[test]
fn a_clip_starting_past_the_others_extends_the_session() {
    let session = session_with(vec![clip(10_000, 1, 1)]);
    assert_eq!(session.length_samples(), 10_001);
}

#[test]
fn a_full_clip_covers_its_whole_source() {
    let clip = Clip::full(480, 1_000);
    assert_eq!(clip.offset_samples, 0);
    assert_eq!(clip.length_samples, 1_000);
    assert_eq!(clip.source_length, 1_000);
    assert_eq!(clip.gain, 1.0);
    assert!(clip.name.is_empty());
    assert!(clip.covers(480));
    assert!(clip.covers(1_479));
    assert!(!clip.covers(479));
    assert!(!clip.covers(1_480));
    assert_eq!(clip.end_samples(), 1_480);
    assert_eq!(clip.source_length, 1_000);
    assert_eq!(clip.gain, 1.0);
}

#[test]
fn the_source_index_follows_the_offset_inside_the_window() {
    let mut clip = Clip::full(100, 50);
    clip.offset_samples = 30;
    // `source_index` assumes `covers`; the pairing of the two is what the
    // mixer node relies on.
    assert!(clip.covers(100) && clip.covers(149));
    assert_eq!(clip.source_index(100), 30);
    assert_eq!(clip.source_index(149), 79);
    assert!(!clip.covers(150));
    assert!(!clip.covers(99));
}

#[test]
fn validation_is_not_shadowed_by_a_clean_display_string() {
    // The CLI prints `Display`; every variant must say something.
    let errors = [
        SessionError::InvalidSampleRate,
        SessionError::InvalidTempo,
        SessionError::NameTooLong,
        SessionError::EmptyClip { track: 3, clip: 7 },
        SessionError::EmptySource { track: 3, clip: 7 },
        SessionError::ClipOutOfSource { track: 3, clip: 7 },
        SessionError::TimelineOverflow { track: 3, clip: 7 },
        SessionError::InvalidGain { track: 3 },
        SessionError::InvalidClipGain { track: 3, clip: 7 },
        SessionError::TooManyTracks { max: 32 },
        SessionError::TooManyClips { track: 3, max: 128 },
    ];
    for error in errors {
        let text = error.to_string();
        assert!(!text.is_empty(), "{error:?}");
    }
    assert!(
        SessionError::EmptyClip { track: 3, clip: 7 }
            .to_string()
            .contains('3')
    );
}

#[test]
fn source_paths_are_checked_after_the_clip_name_and_before_gain() {
    let mut bad = clip(0, 10, 10);
    bad.source = Some(String::new());
    bad.gain = 2.0;
    let session = session_with(vec![bad.clone()]);
    assert_eq!(
        session.validate(),
        Err(SessionError::InvalidSource { track: 0, clip: 0 })
    );
    bad.name = "x".repeat(audio_core::MAX_NAME_BYTES + 1);
    assert_eq!(
        session_with(vec![bad]).validate(),
        Err(SessionError::NameTooLong)
    );
    let mut good = clip(0, 10, 10);
    good.source = Some("a.wav".to_owned());
    assert_eq!(session_with(vec![good]).validate(), Ok(()));
    assert!(
        SessionError::InvalidSource { track: 1, clip: 2 }
            .to_string()
            .contains("source")
    );
}

// ---- SPEC-026 ----

#[test]
fn the_time_signature_defaults_to_four_four() {
    let s = Session::new("s", 48_000, 120.0);
    assert_eq!(s.time_signature, audio_core::TimeSignature::default());
    assert_eq!(s.time_signature.numerator, 4);
    assert_eq!(s.time_signature.denominator, 4);
}

#[test]
fn supported_time_signatures_are_valid_and_others_rejected() {
    use audio_core::TimeSignature;
    let mut s = Session::new("s", 48_000, 120.0);
    for (n, d) in [(1, 4), (3, 4), (6, 8), (7, 8), (16, 16), (2, 2)] {
        s.time_signature = TimeSignature {
            numerator: n,
            denominator: d,
        };
        assert_eq!(s.validate(), Ok(()), "{n}/{d}");
    }
    for (n, d) in [(0, 4), (17, 4), (4, 0), (4, 3), (4, 1), (4, 32)] {
        s.time_signature = TimeSignature {
            numerator: n,
            denominator: d,
        };
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidTimeSignature),
            "{n}/{d}"
        );
    }
}
