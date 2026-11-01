//! Acceptance tests for track folders (SPEC-055 §1–2).
//!
//! The model rules (folders hold no audio state, children are validated) and
//! the serialization round-trip / legacy compatibility.

use audio_core::{
    Project, Session, SessionError, SessionTrack, TRACK_COLORS, TrackFolder, TrackKind,
};

const SR: u32 = 48_000;

fn project(session: Session) -> Project {
    Project {
        name: "demo".to_owned(),
        sample_rate: SR,
        tempo_bpm: 120.0,
        params: Vec::new(),
        session,
    }
}

fn session_with_tracks(count: usize) -> Session {
    let mut session = Session::new("s", SR, 120.0);
    for n in 0..count {
        session
            .tracks
            .push(SessionTrack::new(&format!("Дорожка {n}")));
    }
    session
}

#[test]
fn a_new_session_has_no_folders() {
    let session = Session::new("s", SR, 120.0);
    assert!(session.folders.is_empty());
    assert!(session.folder_children.is_empty());
    assert_eq!(session.validate(), Ok(()));
}

#[test]
fn a_folder_holds_no_audio_state_and_does_not_affect_length() {
    let mut session = session_with_tracks(2);
    session.folders.push(TrackFolder::new("Папка"));
    session.folder_children.push(vec![0, 1]);
    assert_eq!(session.validate(), Ok(()));
    // Folders never change the render geometry: length comes from tracks only.
    assert_eq!(session.length_samples(), 0);
}

#[test]
fn a_child_index_out_of_range_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder::new("Папка"));
    session.folder_children.push(vec![1]);
    assert_eq!(
        session.validate(),
        Err(SessionError::FolderChild { folder: 0 })
    );
}

#[test]
fn a_track_in_two_folders_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder::new("A"));
    session.folders.push(TrackFolder::new("B"));
    session.folder_children.push(vec![0]);
    session.folder_children.push(vec![0]);
    assert_eq!(
        session.validate(),
        Err(SessionError::FolderChild { folder: 1 })
    );
}

#[test]
fn a_repeated_child_within_a_folder_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder::new("A"));
    session.folder_children.push(vec![0, 0]);
    assert_eq!(
        session.validate(),
        Err(SessionError::FolderChild { folder: 0 })
    );
}

#[test]
fn a_folder_color_outside_the_palette_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder {
        name: "Папка".to_owned(),
        color: Some(TRACK_COLORS),
        collapsed: false,
    });
    session.folder_children.push(vec![0]);
    assert_eq!(
        session.validate(),
        Err(SessionError::InvalidColor { track: 0 })
    );
}

#[test]
fn a_too_long_folder_name_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder::new(
        &"x".repeat(audio_core::MAX_NAME_BYTES + 1),
    ));
    session.folder_children.push(vec![0]);
    assert_eq!(session.validate(), Err(SessionError::NameTooLong));
}

#[test]
fn a_mismatched_folder_and_child_list_is_rejected() {
    let mut session = session_with_tracks(1);
    session.folders.push(TrackFolder::new("Папка"));
    // folder_children stays empty, so the lengths disagree.
    assert_eq!(
        session.validate(),
        Err(SessionError::FolderChild { folder: 0 })
    );
}

#[test]
fn a_project_without_folders_serializes_without_the_key() {
    let session = session_with_tracks(2);
    let text = project(session).to_json();
    assert!(!text.contains("\"folders\""));
}

#[test]
fn folders_round_trip() {
    let mut session = session_with_tracks(3);
    session.folders.push(TrackFolder {
        name: "Группа".to_owned(),
        color: Some(3),
        collapsed: true,
    });
    session.folder_children.push(vec![0, 2]);
    session.tracks[0].kind = TrackKind::Audio;

    let project = project(session);
    let text = project.to_json();
    assert!(text.contains("\"folders\""));
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session, project.session);
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_legacy_project_without_folders_round_trips_bit_for_bit() {
    // A document hand-written in the pre-folder format must not gain the key.
    let mut session = session_with_tracks(1);
    session.tracks[0].kind = TrackKind::Audio;
    let text = project(session).to_json();
    assert_eq!(Project::from_json(&text).unwrap().to_json(), text);
    assert!(!text.contains("\"folders\""));
}
