//! SPEC-021 acceptance tests: notes inside MIDI clips — validation, offline
//! rendering through the built-in synth, and the project file round trip.

use audio_core::graph::Graph;
use audio_core::{
    Clip, MAX_NOTES_PER_CLIP, Note, Project, ProjectError, Session, SessionError, SessionTrack,
    SynthError, render_notes,
};

const SR: u32 = 48_000;

const fn note(pitch: u8, start: u64, length: u64) -> Note {
    Note {
        pitch,
        start,
        length,
        velocity: 100,
    }
}

fn midi_session(notes: Vec<Note>) -> Session {
    let mut clip = Clip::midi(0, SR.into());
    clip.notes = Some(notes);
    let mut track = SessionTrack::instrument("keys");
    track.clips.push(clip);
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    session
}

fn invalid_note(session: &Session) -> usize {
    match session.validate() {
        Err(SessionError::InvalidNote { track, clip, note }) => {
            assert_eq!((track, clip), (0, 0));
            note
        }
        other => panic!("expected InvalidNote, got {other:?}"),
    }
}

// ---- model ----

#[test]
fn a_midi_clip_starts_empty_and_is_valid() {
    let clip = Clip::midi(10, 480);
    assert!(clip.is_midi());
    assert_eq!(clip.notes.as_deref(), Some(&[][..]));
    assert!(clip.source.is_none());
    assert!(!Clip::full(0, 1).is_midi());
    midi_session(Vec::new()).validate().unwrap();
}

#[test]
fn notes_inside_the_clip_are_valid() {
    let last = note(127, u64::from(SR) - 1, 1);
    midi_session(vec![note(0, 0, 100), last])
        .validate()
        .unwrap();
}

#[test]
fn a_note_past_the_clip_end_is_rejected_by_index() {
    let session = midi_session(vec![note(60, 0, 10), note(60, u64::from(SR) - 5, 6)]);
    assert_eq!(invalid_note(&session), 1);
}

#[test]
fn zero_length_zero_velocity_and_out_of_range_pitch_are_rejected() {
    assert_eq!(invalid_note(&midi_session(vec![note(60, 0, 0)])), 0);
    let mut quiet = note(60, 0, 10);
    quiet.velocity = 0;
    assert_eq!(invalid_note(&midi_session(vec![quiet])), 0);
    let mut loud = note(60, 0, 10);
    loud.velocity = 128;
    assert_eq!(invalid_note(&midi_session(vec![loud])), 0);
    assert_eq!(invalid_note(&midi_session(vec![note(128, 0, 10)])), 0);
}

#[test]
fn an_overflowing_note_end_is_rejected() {
    assert_eq!(invalid_note(&midi_session(vec![note(60, u64::MAX, 2)])), 0);
}

#[test]
fn a_clip_cannot_hold_both_a_file_and_notes() {
    let mut session = midi_session(vec![note(60, 0, 10)]);
    session.tracks[0].clips[0].source = Some("a.wav".to_owned());
    assert_eq!(invalid_note(&session), 0);
}

#[test]
fn the_note_limit_is_enforced() {
    let at_limit = vec![note(60, 0, 1); MAX_NOTES_PER_CLIP];
    midi_session(at_limit).validate().unwrap();
    let over = vec![note(60, 0, 1); MAX_NOTES_PER_CLIP + 1];
    assert_eq!(invalid_note(&midi_session(over)), MAX_NOTES_PER_CLIP);
}

#[test]
fn the_error_names_track_clip_and_note() {
    let text = SessionError::InvalidNote {
        track: 1,
        clip: 2,
        note: 3,
    }
    .to_string();
    for part in ["1", "2", "3"] {
        assert!(text.contains(part), "{text}");
    }
}

// ---- rendering ----

#[test]
fn no_notes_render_silence_of_the_requested_length() {
    let out = render_notes(&[], SR, 1_000).unwrap();
    assert_eq!(out.len(), 1_000);
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn a_note_is_silent_before_its_start_and_sounds_after() {
    let out = render_notes(&[note(69, 4_800, 9_600)], SR, 24_000).unwrap();
    assert!(out[..4_800].iter().all(|&s| s == 0.0));
    let peak = out[4_800..14_400]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "peak {peak}");
    assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
}

#[test]
fn the_release_fades_out_after_the_note_ends() {
    // 120 ms release at 48 kHz is 5_760 samples; leave plenty of margin.
    let out = render_notes(&[note(60, 0, 4_800)], SR, 24_000).unwrap();
    assert!(out[20_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn rendering_is_deterministic_and_order_independent() {
    let a = [
        note(60, 0, 9_000),
        note(64, 2_000, 9_000),
        note(67, 4_000, 300),
    ];
    let b = [a[2], a[0], a[1]];
    let first = render_notes(&a, SR, 20_000).unwrap();
    assert_eq!(first, render_notes(&a, SR, 20_000).unwrap());
    assert_eq!(first, render_notes(&b, SR, 20_000).unwrap());
}

#[test]
fn a_repeated_note_retriggers() {
    // Back-to-back notes of the same pitch: the second must still sound.
    let out = render_notes(&[note(60, 0, 4_800), note(60, 4_800, 4_800)], SR, 12_000).unwrap();
    let peak = out[6_000..9_600]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "peak {peak}");
}

#[test]
fn a_zero_sample_rate_is_an_error() {
    assert_eq!(
        render_notes(&[note(60, 0, 10)], 0, 10).unwrap_err(),
        SynthError::InvalidSampleRate
    );
}

// ---- project file ----

fn project_with(session: Session) -> Project {
    Project::with_session(
        Project::capture("demo", SR, 120.0, &Graph::with_capacity(SR, 2, 128, 4, 4)),
        session,
    )
}

#[test]
fn notes_survive_the_project_round_trip() {
    let mut loud = note(72, 100, 200);
    loud.velocity = 127;
    let session = midi_session(vec![note(60, 0, 480), loud]);
    let text = project_with(session.clone()).to_json();
    assert!(text.contains("\"notes\""));
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &session);
    assert_eq!(back.to_json(), text);
}

#[test]
fn an_empty_midi_clip_round_trips() {
    let session = midi_session(Vec::new());
    let text = project_with(session.clone()).to_json();
    assert_eq!(Project::from_json(&text).unwrap().session(), &session);
}

#[test]
fn audio_clips_write_no_notes_key() {
    let mut track = SessionTrack::new("a");
    track.clips.push(Clip::full(0, 10));
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    let text = project_with(session.clone()).to_json();
    assert!(!text.contains("\"notes\""));
    assert_eq!(Project::from_json(&text).unwrap().session(), &session);
}

#[test]
fn an_unknown_key_inside_a_note_is_rejected() {
    let text = project_with(midi_session(vec![note(60, 0, 10)]))
        .to_json()
        .replace("\"velocity\"", "\"vel\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "vel".to_owned()
        }
    );
}

#[test]
fn a_missing_note_field_is_named() {
    let text = project_with(midi_session(vec![note(60, 0, 10)]))
        .to_json()
        .replace(", \"velocity\": 100", "");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::MissingField {
            field: "session.tracks[].clips[].notes[].velocity"
        }
    );
}

#[test]
fn an_invalid_note_in_a_file_is_a_session_error() {
    let text = project_with(midi_session(vec![note(60, 0, 10)]))
        .to_json()
        .replace("\"length\": 10", "\"length\": 0");
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(SessionError::InvalidNote { note: 0, .. })
    ));
}

#[test]
fn a_pitch_above_255_is_rejected_by_the_parser() {
    let text = project_with(midi_session(vec![note(60, 0, 10)]))
        .to_json()
        .replace("\"pitch\": 60", "\"pitch\": 300");
    assert!(Project::from_json(&text).is_err());
}
