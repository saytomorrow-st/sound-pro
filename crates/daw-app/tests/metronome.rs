//! Acceptance tests for the metronome click (SPEC-026).

use audio_core::session::{Clip, Session, Track as SessionTrack};
use daw_app::playback::{PlaybackControl, PlaybackEngine, playback};
use daw_app::{SessionJob, TrackSource, build_session_graph};

const RATE: u32 = 48_000;
const LEN: usize = 120_000;
/// 30 ms at 48 kHz.
const CLICK: usize = 1_440;

/// One silent track of `LEN` frames, so only the click is heard.
fn silent(block_frames: usize, start: u64) -> (PlaybackControl, PlaybackEngine) {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, LEN as u64));
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track.clone());
    let graph = build_session_graph(SessionJob {
        session,
        tracks: vec![TrackSource {
            track,
            clips: vec![Box::new(vec![0.0f32; LEN])],
        }],
        sample_rate: RATE,
        block_frames,
        frames: None,
        output: std::path::PathBuf::new(),
    })
    .unwrap();
    playback(graph, RATE, start, LEN as u64)
}

/// Plays `frames` frames in stereo buffers of `buffer` frames; left channel.
fn run(engine: &mut PlaybackEngine, buffer: usize, frames: usize) -> Vec<f32> {
    let mut all = Vec::new();
    let mut out = vec![1.0f32; buffer * 2];
    while all.len() < frames {
        engine.fill(&mut out, 2);
        all.extend(out.iter().step_by(2).copied());
    }
    all.truncate(frames);
    all
}

fn clicking(block: usize, start: u64) -> (PlaybackControl, PlaybackEngine) {
    let (mut control, engine) = silent(block, start);
    control.set_metronome(true);
    control.play();
    (control, engine)
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Frames `[at, at + CLICK)` sound, the rest of the beat is silent.
fn assert_click_at(left: &[f32], at: usize) {
    assert!(left[at] != 0.0, "no click at {at}");
    assert!(
        peak(&left[at..at + CLICK]) > 0.05,
        "click at {at} too quiet"
    );
    assert_eq!(left[at - 1], 0.0, "sound before the click at {at}");
}

#[test]
fn the_metronome_is_off_by_default() {
    let (mut control, mut engine) = silent(1024, 0);
    assert!(!control.metronome());
    control.play();
    assert!(run(&mut engine, 512, 50_000).iter().all(|s| *s == 0.0));
}

#[test]
fn clicks_start_exactly_on_every_beat() {
    let (control, mut engine) = clicking(1024, 0);
    assert!(control.metronome());
    let left = run(&mut engine, 512, 100_000);
    assert!(left[0] != 0.0);
    for beat in 1..4 {
        assert_click_at(&left, beat * 24_000);
    }
    assert!(left[CLICK..24_000].iter().all(|s| *s == 0.0));
}

#[test]
fn the_click_reaches_every_channel() {
    let (_control, mut engine) = clicking(1024, 0);
    let mut out = vec![0.0f32; 64 * 3];
    engine.fill(&mut out, 3);
    assert!(out[0] != 0.0);
    assert_eq!(out[0], out[1]);
    assert_eq!(out[0], out[2]);
}

#[test]
fn the_first_beat_of_a_bar_is_louder() {
    let (_control, mut engine) = clicking(1024, 0);
    let left = run(&mut engine, 512, 100_000);
    let accent = peak(&left[..CLICK]);
    let normal = peak(&left[24_000..24_000 + CLICK]);
    assert!(accent > normal * 1.2, "{accent} vs {normal}");
    let next_bar = peak(&left[96_000..96_000 + CLICK]);
    assert!((next_bar - accent).abs() < 1e-6);
}

#[test]
fn the_meter_moves_beats_and_accents() {
    let (mut control, mut engine) = clicking(1024, 0);
    control.set_meter(90.0, 3);
    let left = run(&mut engine, 512, 100_000);
    // 90 BPM: one beat every 32 000 frames, accent every third beat.
    for beat in 1..3 {
        assert_click_at(&left, beat * 32_000);
    }
    assert!(left[23_000..25_000].iter().all(|s| *s == 0.0));
    let accent = peak(&left[..CLICK]);
    assert!(peak(&left[32_000..32_000 + CLICK]) < accent);
    assert!((peak(&left[96_000..96_000 + CLICK]) - accent).abs() < 1e-6);
}

#[test]
fn a_fractional_beat_length_rounds_up_without_drift() {
    let (mut control, mut engine) = clicking(1024, 0);
    control.set_meter(115.0, 4);
    let left = run(&mut engine, 333, LEN - 2_000);
    let beat = f64::from(RATE) * 60.0 / 115.0;
    for k in 1..4 {
        assert_click_at(&left, (k as f64 * beat).ceil() as usize);
    }
}

#[test]
fn bad_meters_are_ignored() {
    let (mut control, mut engine) = clicking(1024, 0);
    control.set_meter(0.0, 4);
    control.set_meter(f64::NAN, 4);
    control.set_meter(-10.0, 4);
    control.set_meter(120.0, 0);
    let left = run(&mut engine, 512, 50_000);
    assert_click_at(&left, 24_000);
}

#[test]
fn the_click_does_not_depend_on_buffer_or_block_size() {
    let (_c, mut reference) = clicking(1024, 0);
    let expected = run(&mut reference, 1024, 100_000);
    for (block, buffer) in [(64, 64), (128, 1000), (1024, 4096), (512, 17)] {
        let (_c, mut engine) = clicking(block, 0);
        assert_eq!(
            run(&mut engine, buffer, 100_000),
            expected,
            "{block}/{buffer}"
        );
    }
}

#[test]
fn stopped_playback_does_not_click() {
    let (mut control, mut engine) = silent(1024, 0);
    control.set_metronome(true);
    assert!(run(&mut engine, 512, 30_000).iter().all(|s| *s == 0.0));
}

#[test]
fn starting_between_clicks_is_silent_until_the_next_beat() {
    let (_control, mut engine) = clicking(1024, 2_000);
    let left = run(&mut engine, 512, 30_000);
    assert!(left[..22_000].iter().all(|s| *s == 0.0));
    assert!(left[22_000] != 0.0);
}

#[test]
fn turning_the_metronome_off_while_playing_silences_it() {
    let (mut control, mut engine) = clicking(1024, 0);
    run(&mut engine, 512, 10_000);
    control.set_metronome(false);
    assert!(!control.metronome());
    assert!(run(&mut engine, 512, 50_000).iter().all(|s| *s == 0.0));
}

#[test]
fn the_click_follows_the_cycle_jump() {
    let (mut control, mut engine) = clicking(1024, 0);
    control.set_cycle(Some((0, 30_000)));
    let left = run(&mut engine, 700, 60_000);
    assert_click_at(&left, 24_000);
    // Position 29 999 is between clicks; the next frame is position 0 again.
    assert_click_at(&left, 30_000);
    assert!((peak(&left[30_000..30_000 + CLICK]) - peak(&left[..CLICK])).abs() < 1e-6);
}
