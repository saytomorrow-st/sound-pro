//! Acceptance tests for per-track instrument selection (SPEC-065).

use audio_core::{Clip, Project, Session, SessionTrack, TrackInstrument, TrackKind};

const SR: u32 = 48_000;

fn instrument_track() -> SessionTrack {
    let mut track = SessionTrack::instrument("midi");
    track.clips.push(Clip::full(0, 1_000));
    track
}

fn session_with(track: SessionTrack) -> Session {
    let mut session = Session::new("s", SR, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn a_new_track_defaults_to_builtin_piano() {
    let track = SessionTrack::instrument("midi");
    assert_eq!(track.instrument, TrackInstrument::BuiltinPiano);
    assert_eq!(TrackInstrument::default(), TrackInstrument::BuiltinPiano);
}

#[test]
fn builtin_piano_is_not_written() {
    let project = Project {
        name: "p".into(),
        sample_rate: SR,
        tempo_bpm: 120.0,
        params: Vec::new(),
        session: session_with(instrument_track()),
    };
    let text = project.to_json();
    assert!(
        !text.contains("\"instrument\":"),
        "default instrument is not written"
    );
}

#[test]
fn sampler_round_trips_by_id() {
    let mut track = instrument_track();
    track.instrument = TrackInstrument::Sampler {
        bank_id: "salamander-grand-v3".into(),
    };
    let project = Project {
        name: "p".into(),
        sample_rate: SR,
        tempo_bpm: 120.0,
        params: Vec::new(),
        session: session_with(track),
    };
    let text = project.to_json();
    assert!(
        text.contains("\"instrument\": { \"sampler\": { \"bank\": \"salamander-grand-v3\" } }")
    );

    let back = Project::from_json(&text).unwrap();
    assert_eq!(
        back.session.tracks[0].instrument,
        TrackInstrument::Sampler {
            bank_id: "salamander-grand-v3".into(),
        }
    );
}

#[test]
fn legacy_sampler_path_still_loads() {
    // A pre-redesign document serialised the sampler as a bare folder path.
    let text = r#"{
    "name": "p",
    "sample_rate": 48000,
    "tempo_bpm": 120.0,
    "params": [],
    "session": {
      "name": "s",
      "sample_rate": 48000,
      "tempo_bpm": 120.0,
      "master_gain": 1.0,
      "tracks": [
        {
          "name": "midi",
          "kind": "instrument",
          "instrument": { "sampler": "/home/user/banks/piano" },
          "gain": 1.0,
          "pan": 0.0,
          "mute": false,
          "solo": false,
          "clips": []
        }
      ]
    }
  }"#;

    let project = Project::from_json(text).unwrap();
    assert_eq!(
        project.session.tracks[0].instrument,
        TrackInstrument::Sampler {
            bank_id: "/home/user/banks/piano".into(),
        }
    );
}

#[test]
fn old_file_without_instrument_loads_as_builtin_piano_and_resaves_without_it() {
    // A pre-SPEC-065 instrument track has no `instrument` field.
    let old = r#"{
    "name": "old",
    "sample_rate": 48000,
    "tempo_bpm": 120.0,
    "params": [],
    "session": {
      "name": "old",
      "sample_rate": 48000,
      "tempo_bpm": 120.0,
      "master_gain": 1.0,
      "tracks": [
        {
          "name": "midi",
          "kind": "instrument",
          "gain": 1.0,
          "pan": 0.0,
          "mute": false,
          "solo": false,
          "clips": []
        }
      ]
    }
  }"#;

    let project = Project::from_json(old).unwrap();
    assert_eq!(project.session.tracks[0].kind, TrackKind::Instrument);
    assert_eq!(
        project.session.tracks[0].instrument,
        TrackInstrument::BuiltinPiano
    );

    // Re-saving must not introduce an `instrument` field.
    let resaved = project.to_json();
    assert!(
        !resaved.contains("\"instrument\":"),
        "resaving an old file must not add an instrument field"
    );
    assert_eq!(
        Project::from_json(&resaved).unwrap().session.tracks[0].instrument,
        TrackInstrument::BuiltinPiano
    );
}

#[test]
fn unknown_instrument_key_is_rejected() {
    let text = r#"{
    "name": "p",
    "sample_rate": 48000,
    "tempo_bpm": 120.0,
    "master_gain": 1.0,
    "tracks": [
      {
        "name": "midi",
        "kind": "instrument",
        "instrument": { "download": "https://example.com" },
        "gain": 1.0,
        "pan": 0.0,
        "mute": false,
        "solo": false,
        "clips": []
      }
    ]
  }"#;
    assert!(Project::from_json(text).is_err());
}
