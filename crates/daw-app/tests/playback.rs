//! Acceptance tests for live playback without a device (SPEC-016).

use audio_core::session::{Clip, Session, Track as SessionTrack};
use daw_app::playback::{COMMAND_CAPACITY, PlaybackControl, PlaybackEngine, playback};
use daw_app::{SessionJob, TrackSource, build_session_graph};

const RATE: u32 = 48_000;
const LEN: usize = 3_000;

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i + 1) as f32 / 10_000.0).collect()
}

/// One centred track playing `ramp(LEN)` from frame 0.
fn pair(block_frames: usize, start: u64) -> (PlaybackControl, PlaybackEngine) {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, LEN as u64));
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track.clone());
    let graph = build_session_graph(SessionJob {
        session,
        tracks: vec![TrackSource {
            track,
            clips: vec![Box::new(ramp(LEN))],
        }],
        sample_rate: RATE,
        block_frames,
        frames: None,
        output: std::path::PathBuf::new(),
    })
    .unwrap();
    playback(graph, RATE, start, LEN as u64)
}

/// Plays to the end in device buffers of `buffer` frames, stereo.
fn play_all(engine: &mut PlaybackEngine, buffer: usize) -> Vec<f32> {
    let mut all = Vec::new();
    let mut out = vec![1.0f32; buffer * 2];
    for _ in 0..(LEN / buffer + 2) {
        engine.fill(&mut out, 2);
        all.extend_from_slice(&out);
    }
    all
}

fn left(interleaved: &[f32]) -> Vec<f32> {
    interleaved.iter().step_by(2).copied().collect()
}

#[test]
fn the_engine_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<PlaybackEngine>();
}

#[test]
fn a_new_pair_is_stopped_at_its_start() {
    let (control, _engine) = pair(1024, 500);
    assert!(!control.is_playing());
    assert_eq!(control.position(), 500);
    assert_eq!(control.sample_rate(), RATE);
    assert_eq!(control.end(), LEN as u64);
}

#[test]
fn the_start_is_clamped_to_the_end() {
    let (control, _engine) = pair(1024, 99_999);
    assert_eq!(control.position(), LEN as u64);
}

#[test]
fn stopped_playback_is_silent_and_does_not_move() {
    let (control, mut engine) = pair(1024, 0);
    let mut out = vec![1.0f32; 512 * 2];
    engine.fill(&mut out, 2);
    assert!(out.iter().all(|s| *s == 0.0));
    assert_eq!(control.position(), 0);
}

#[test]
fn playback_matches_the_source_for_any_device_buffer() {
    let source = ramp(LEN);
    for (block, buffer) in [
        (1024, 64),
        (1024, 1000),
        (1024, 3000),
        (256, 1000),
        (64, 3000),
    ] {
        let (mut control, mut engine) = pair(block, 0);
        assert!(control.play());
        let out = play_all(&mut engine, buffer);
        let l = left(&out);
        assert_eq!(
            &l[..LEN],
            source.as_slice(),
            "block {block} buffer {buffer}"
        );
        assert!(
            l[LEN..].iter().all(|s| *s == 0.0),
            "silence after end, block {block} buffer {buffer}: {:?}",
            l[LEN..].iter().position(|s| *s != 0.0)
        );
        let r: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(l, r, "centred track is equal in both channels");
    }
}

#[test]
fn reaching_the_end_stops_at_the_end() {
    let (mut control, mut engine) = pair(1024, 0);
    control.play();
    let mut out = vec![0.0f32; 1000 * 2];
    for _ in 0..3 {
        engine.fill(&mut out, 2);
    }
    assert!(!control.is_playing());
    assert_eq!(control.position(), LEN as u64);
    engine.fill(&mut out, 2);
    assert_eq!(control.position(), LEN as u64);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn the_position_advances_while_playing() {
    let (mut control, mut engine) = pair(1024, 0);
    control.play();
    let mut out = vec![0.0f32; 700 * 2];
    engine.fill(&mut out, 2);
    assert!(control.is_playing());
    assert_eq!(control.position(), 700);
}

#[test]
fn stop_keeps_the_position() {
    let (mut control, mut engine) = pair(1024, 0);
    control.play();
    let mut out = vec![0.0f32; 400 * 2];
    engine.fill(&mut out, 2);
    control.stop();
    engine.fill(&mut out, 2);
    assert!(!control.is_playing());
    assert_eq!(control.position(), 400);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn seek_moves_the_playhead_and_playback_continues_from_there() {
    let source = ramp(LEN);
    let (mut control, mut engine) = pair(1024, 0);
    control.seek(2_000);
    control.play();
    let mut out = vec![0.0f32; 100 * 2];
    engine.fill(&mut out, 2);
    assert_eq!(left(&out), &source[2_000..2_100]);
    assert_eq!(control.position(), 2_100);
}

#[test]
fn seek_past_the_end_is_clamped() {
    let (mut control, mut engine) = pair(1024, 0);
    control.seek(1_000_000);
    let mut out = vec![0.0f32; 16 * 2];
    engine.fill(&mut out, 2);
    assert_eq!(control.position(), LEN as u64);
}

#[test]
fn starting_mid_song_plays_from_the_start_position() {
    let source = ramp(LEN);
    let (mut control, mut engine) = pair(1024, 1_500);
    control.play();
    let out = play_all(&mut engine, 500);
    assert_eq!(&left(&out)[..LEN - 1_500], &source[1_500..]);
}

#[test]
fn a_full_queue_reports_false_instead_of_blocking() {
    let (mut control, _engine) = pair(1024, 0);
    for _ in 0..COMMAND_CAPACITY {
        assert!(control.play());
    }
    assert!(!control.play());
}

#[test]
fn zero_channels_or_empty_buffers_are_ignored() {
    let (mut control, mut engine) = pair(1024, 0);
    control.play();
    engine.fill(&mut [], 2);
    engine.fill(&mut [0.0; 8], 0);
    assert_eq!(control.position(), 0);
}

#[test]
fn mono_devices_get_the_left_channel() {
    let source = ramp(LEN);
    let (mut control, mut engine) = pair(1024, 0);
    control.play();
    let mut out = vec![0.0f32; 300];
    engine.fill(&mut out, 1);
    assert_eq!(out, &source[..300]);
}
