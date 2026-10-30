//! Acceptance tests for live level meters (SPEC-042).

use audio_core::buffer::AudioBuffer;
use audio_core::session::{Clip, Session, Track as SessionTrack};
use audio_core::{AutoPoint, Renderer, Transport, TransportCommand};
use daw_app::meters::Meters;
use daw_app::playback::playback;
use daw_app::{SessionJob, TrackSource, build_session_graph, build_session_graph_metered};

const RATE: u32 = 48_000;
const BLOCK: usize = 256;
const LEN: usize = 2_000;

/// A ramp whose largest value is `peak`, at the last frame.
fn ramp(peak: f32) -> Vec<f32> {
    (0..LEN)
        .map(|i| (i + 1) as f32 / LEN as f32 * peak)
        .collect()
}

fn track(gain: f32, pan: f32) -> SessionTrack {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, LEN as u64));
    track.gain = gain;
    track.pan = pan;
    track
}

fn job(session: Session, peaks: &[f32]) -> SessionJob {
    let tracks = session
        .tracks
        .iter()
        .zip(peaks)
        .map(|(track, &peak)| TrackSource {
            track: track.clone(),
            clips: vec![Box::new(ramp(peak))],
        })
        .collect();
    SessionJob {
        session,
        tracks,
        sample_rate: RATE,
        block_frames: BLOCK,
        frames: None,
        output: std::path::PathBuf::new(),
    }
}

fn session(tracks: Vec<SessionTrack>, master: f32) -> Session {
    let mut session = Session::new("s", RATE, 120.0);
    session.master_gain = master;
    session.tracks = tracks;
    session
}

/// Renders the whole session and returns both channels.
fn render(graph: audio_core::Graph) -> Vec<f32> {
    let mut transport = Transport::new(RATE);
    transport.apply(TransportCommand::Play);
    let mut renderer = Renderer::new(graph, transport, BLOCK);
    let mut mix = AudioBuffer::new(2, BLOCK);
    let mut all = Vec::new();
    for _ in 0..LEN.div_ceil(BLOCK) {
        renderer.render(BLOCK, &mut mix).unwrap();
        all.extend_from_slice(mix.channel(0));
        all.extend_from_slice(mix.channel(1));
    }
    all
}

fn close(actual: (f32, f32), expected: (f32, f32)) {
    assert!(
        (actual.0 - expected.0).abs() < 1e-6 && (actual.1 - expected.1).abs() < 1e-6,
        "{actual:?} != {expected:?}"
    );
}

fn tracks_with_buses(with_buses: bool) -> Session {
    let mut first = track(0.5, -0.5);
    let second = track(1.0, 0.0);
    let mut muted = track(1.0, 0.0);
    muted.mute = true;
    if with_buses {
        first.sends.push(audio_core::BusSend { bus: 0, level: 1.0 });
    }
    let mut session = session(vec![first, second, muted], 0.25);
    if with_buses {
        session.buses.push(audio_core::Bus::new("Aux"));
    }
    session
}

#[test]
fn tracks_report_post_fader_levels_without_the_master() {
    for with_buses in [false, true] {
        let meters = Meters::new();
        let job = job(tracks_with_buses(with_buses), &[0.8, 0.6, 0.9]);
        render(build_session_graph_metered(job, Some(&meters)).unwrap());
        // Gain 0.5, pan -0.5: left 1.0, right 0.5 (SPEC-013 pan law).
        close(meters.take_track(0), (0.4, 0.2));
        close(meters.take_track(1), (0.6, 0.6));
        assert_eq!(meters.take_track(2), (0.0, 0.0), "muted track is silent");
        assert_eq!(meters.take_track(3), (0.0, 0.0));
    }
}

#[test]
fn a_meter_bank_does_not_change_the_sound() {
    for with_buses in [false, true] {
        let peaks = [0.8, 0.6, 0.9];
        let plain =
            render(build_session_graph(job(tracks_with_buses(with_buses), &peaks)).unwrap());
        let meters = Meters::new();
        let metered = render(
            build_session_graph_metered(job(tracks_with_buses(with_buses), &peaks), Some(&meters))
                .unwrap(),
        );
        let bits = |v: &[f32]| v.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&plain), bits(&metered));
    }
}

#[test]
fn automated_gain_and_pan_are_metered() {
    let mut automated = track(1.0, 0.0);
    automated.automation.gain = vec![AutoPoint {
        time: 0,
        value: 0.5,
    }];
    automated.automation.pan = vec![AutoPoint {
        time: 0,
        value: 1.0,
    }];
    let meters = Meters::new();
    let job = job(session(vec![automated], 0.5), &[0.8]);
    render(build_session_graph_metered(job, Some(&meters)).unwrap());
    close(meters.take_track(0), (0.0, 0.4));
}

#[test]
fn the_engine_reports_the_master_level_and_keeps_it_across_swaps() {
    let graph = || build_session_graph(job(session(vec![track(1.0, 0.0)], 0.5), &[0.8])).unwrap();
    let meters = Meters::new();
    let (mut control, engine) = playback(graph(), RATE, 0, LEN as u64);
    let mut engine = engine.with_meters(meters.clone());
    let mut out = vec![0.0f32; BLOCK * 2];
    engine.fill(&mut out, 2);
    assert_eq!(
        meters.take_master(),
        (0.0, 0.0),
        "stopped playback is silent"
    );
    control.play();
    control.replace_graph(graph()).unwrap();
    for _ in 0..LEN.div_ceil(BLOCK) {
        engine.fill(&mut out, 2);
    }
    close(meters.take_master(), (0.4, 0.4));
    assert_eq!(meters.take_master(), (0.0, 0.0));
}
