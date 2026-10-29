//! Acceptance tests for session rendering (SPEC-010 §3–§6).

use audio_core::buffer::AudioBuffer;
use audio_core::session::{Clip, Session, Track as SessionTrack};
use daw_app::nodes::MonoClip;
use daw_app::{SessionJob, SessionReport, TrackSource, render_session};

/// Scratch file inside the per-process temp directory.
struct Scratch {
    dir: std::path::PathBuf,
    next: usize,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-session-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, next: 0 }
    }

    fn path(&mut self) -> std::path::PathBuf {
        self.next += 1;
        self.dir.join(format!("out{}.wav", self.next))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const RATE: u32 = 48_000;

/// Renders a one-track session whose single clip is `clip` over `source`.
fn render_one(
    scratch: &mut Scratch,
    clip: Clip,
    source: Vec<f32>,
    frames: Option<u64>,
) -> (SessionReport, AudioBuffer) {
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track.clone());
    let output = scratch.path();
    let report = render_session(SessionJob {
        session,
        tracks: vec![TrackSource {
            track,
            clips: vec![Box::new(source)],
        }],
        sample_rate: RATE,
        block_frames: 64,
        frames,
        output: output.clone(),
    })
    .unwrap();
    let (spec, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    assert_eq!(spec.sample_rate, RATE);
    // SPEC-013: a centred track is identical in both channels.
    assert_eq!(spec.channels, 2);
    assert_eq!(buf.channel(0), buf.channel(1));
    (report, buf)
}

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| i as f32 / 1000.0).collect()
}

#[test]
fn a_full_clip_reproduces_its_source_bit_for_bit() {
    let mut scratch = Scratch::new("full");
    let source = ramp(1_000);
    let (report, buf) = render_one(&mut scratch, Clip::full(0, 1_000), source.clone(), None);
    assert_eq!(report.frames, 1_000);
    assert_eq!(report.session_frames, 1_000);
    assert_eq!(report.tracks, 1);
    assert_eq!(buf.frames(), 1_000);
    assert_eq!(buf.channel(0), source.as_slice());
}

#[test]
fn offset_trims_the_head_of_the_source() {
    let mut scratch = Scratch::new("offset");
    let source = ramp(1_000);
    let mut clip = Clip::full(0, 500);
    clip.offset_samples = 250;
    clip.source_length = 1_000;
    let (_, buf) = render_one(&mut scratch, clip, source.clone(), None);
    assert_eq!(buf.frames(), 500);
    // The first rendered frame is source[250], not source[0].
    assert_eq!(buf.channel(0)[0], source[250]);
    assert_eq!(buf.channel(0), &source[250..750]);
}

#[test]
fn length_trims_the_tail_and_leaves_silence_past_the_clip() {
    let mut scratch = Scratch::new("tail");
    let source = ramp(1_000);
    let mut clip = Clip::full(100, 400);
    clip.offset_samples = 10;
    clip.source_length = 1_000;
    // Ask for more frames than the session covers: the rest must be silence.
    let (report, buf) = render_one(&mut scratch, clip, source.clone(), Some(1_000));
    assert_eq!(report.frames, 1_000);
    assert_eq!(report.session_frames, 500);
    assert_eq!(buf.frames(), 1_000);
    assert!(buf.channel(0)[..100].iter().all(|s| *s == 0.0), "lead-in");
    assert_eq!(&buf.channel(0)[100..500], &source[10..410]);
    assert!(
        buf.channel(0)[500..].iter().all(|s| *s == 0.0),
        "no sound after start + length"
    );
}

#[test]
fn a_shorter_frame_budget_trims_the_rendered_silence() {
    let mut scratch = Scratch::new("short");
    let (report, buf) = render_one(&mut scratch, Clip::full(0, 1_000), ramp(1_000), Some(250));
    assert_eq!(report.frames, 250);
    assert_eq!(report.session_frames, 1_000);
    assert_eq!(buf.frames(), 250);
}

#[test]
fn two_tracks_are_summed_and_gain_and_mute_apply() {
    // Track A: ones for 100 frames. Track B: twos for the same span.
    let ones = vec![1.0_f32; 100];
    let twos = vec![2.0_f32; 100];

    let mut a = SessionTrack::new("a");
    a.clips.push(Clip::full(0, 100));
    let mut b = SessionTrack::new("b");
    b.clips.push(Clip::full(0, 100));
    b.gain = 0.5;

    let mut session = Session::new("mix", RATE, 120.0);
    session.tracks.push(a.clone());
    session.tracks.push(b.clone());

    let mut scratch = Scratch::new("mix");
    let output = scratch.path();
    let report = render_session(SessionJob {
        session,
        tracks: vec![
            TrackSource {
                track: a.clone(),
                clips: vec![Box::new(ones.clone())],
            },
            TrackSource {
                track: b.clone(),
                clips: vec![Box::new(twos.clone())],
            },
        ],
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output: output.clone(),
    })
    .unwrap();
    assert_eq!(report.tracks, 2);
    let (_, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    // Gain 0.5 on the second track: 1.0 + 2.0 * 0.5 = 2.0.
    for (i, sample) in buf.channel(0).iter().enumerate() {
        assert!((sample - 2.0).abs() < 1e-6, "frame {i}: {sample}");
    }

    // Muting the second track leaves only the first one.
    let mut session = Session::new("mute", RATE, 120.0);
    session.tracks.push(a.clone());
    b.mute = true;
    session.tracks.push(b.clone());
    let output = scratch.path();
    render_session(SessionJob {
        session,
        tracks: vec![
            TrackSource {
                track: a,
                clips: vec![Box::new(ones)],
            },
            TrackSource {
                track: b,
                clips: vec![Box::new(twos)],
            },
        ],
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output: output.clone(),
    })
    .unwrap();
    let (_, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    assert!(buf.channel(0).iter().all(|s| (*s - 1.0).abs() < 1e-6));
}

#[test]
fn an_empty_session_renders_silence_of_the_requested_length() {
    let mut scratch = Scratch::new("empty");
    let session = Session::new("empty", RATE, 120.0);
    let output = scratch.path();
    let report = render_session(SessionJob {
        session,
        tracks: Vec::new(),
        sample_rate: RATE,
        block_frames: 64,
        frames: Some(128),
        output: output.clone(),
    })
    .unwrap();
    assert_eq!(
        report,
        SessionReport {
            frames: 128,
            session_frames: 0,
            tracks: 0
        }
    );
    let (_, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    assert_eq!(buf.frames(), 128);
    assert!(buf.channel(0).iter().all(|s| *s == 0.0));
}

#[test]
fn rendering_the_same_session_twice_is_byte_identical() {
    let build = |frames: Option<u64>| {
        let source = MonoClip::sine(440.0, RATE, 3_000).samples().to_vec();
        let mut track = SessionTrack::new("t");
        let mut clip = Clip::full(500, 2_000);
        clip.offset_samples = 200;
        clip.source_length = 3_000;
        clip.gain = 0.75;
        track.clips.push(clip);
        let mut session = Session::new("det", RATE, 120.0);
        session.tracks.push(track.clone());
        SessionJob {
            session,
            tracks: vec![TrackSource {
                track,
                clips: vec![Box::new(source)],
            }],
            sample_rate: RATE,
            block_frames: 64,
            frames,
            output: std::path::PathBuf::new(),
        }
    };
    let mut scratch = Scratch::new("det");
    let first = scratch.path();
    let second = scratch.path();
    for path in [&first, &second] {
        let mut job = build(None);
        job.output = path.clone();
        render_session(job).unwrap();
    }
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap()
    );
}

#[test]
fn a_track_count_mismatch_is_reported() {
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(SessionTrack::new("t"));
    let err = render_session(SessionJob {
        session,
        tracks: Vec::new(),
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output: std::path::PathBuf::from("/dev/null"),
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        daw_app::SessionError::TrackCountMismatch {
            tracks: 1,
            sources: 0
        }
        .to_string()
    );
}

#[test]
fn a_clip_count_mismatch_is_reported() {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, 100));
    track.clips.push(Clip::full(100, 100));
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track.clone());
    let err = render_session(SessionJob {
        session,
        tracks: vec![TrackSource {
            track,
            clips: vec![Box::new(vec![0.0_f32; 100])],
        }],
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output: std::path::PathBuf::from("/dev/null"),
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        daw_app::SessionError::ClipCountMismatch {
            track: "t".to_owned(),
            clips: 2,
            sources: 1
        }
        .to_string()
    );
}

#[test]
fn a_clip_claiming_more_source_than_exists_renders_silence_past_the_real_end() {
    use audio_core::{Graph, Renderer, Transport, TransportCommand};

    // The model claims 1 000 frames of content; the source holds only 100.
    // `Track` must not trust the claim: past the real end it plays silence.
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, 1_000));
    let source = ramp(100);
    let node = daw_app::nodes::Track::new(&track, vec![Box::new(source.clone())]).unwrap();

    let mut graph = Graph::with_capacity(RATE, 1, 64, 1, 0);
    graph.add_node("t", node).unwrap();
    let mut transport = Transport::new(RATE);
    transport.apply(TransportCommand::Play);
    let mut renderer = Renderer::new(graph, transport, 64);

    let mut rendered = Vec::new();
    let mut mix = AudioBuffer::new(1, 64);
    for _ in 0..(1_024 / 64) {
        renderer.render(64, &mut mix).unwrap();
        rendered.extend_from_slice(&mix.channel(0)[..64]);
    }

    assert_eq!(&rendered[..100], &source[..]);
    assert!(rendered[100..].iter().all(|&s| s == 0.0));
}

/// Renders `tracks` (each one full clip over its source) with `master_gain`.
fn render_mix(
    scratch: &mut Scratch,
    tracks: Vec<(SessionTrack, Vec<f32>)>,
    master_gain: f32,
) -> AudioBuffer {
    let mut session = Session::new("mix", RATE, 120.0);
    session.master_gain = master_gain;
    let mut sources = Vec::new();
    for (mut track, source) in tracks {
        track.clips.push(Clip::full(0, source.len() as u64));
        session.tracks.push(track.clone());
        sources.push(TrackSource {
            track,
            clips: vec![Box::new(source)],
        });
    }
    let output = scratch.path();
    render_session(SessionJob {
        session,
        tracks: sources,
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output: output.clone(),
    })
    .unwrap();
    let (spec, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    assert_eq!(spec.channels, 2);
    buf
}

fn panned(pan: f32) -> SessionTrack {
    let mut track = SessionTrack::new("p");
    track.pan = pan;
    track
}

fn scaled(source: &[f32], gain: f32) -> Vec<f32> {
    source.iter().map(|s| s * gain).collect()
}

#[test]
fn hard_pans_leave_exact_silence_on_the_far_side() {
    let mut scratch = Scratch::new("hardpan");
    let source = ramp(300);
    let left = render_mix(&mut scratch, vec![(panned(-1.0), source.clone())], 1.0);
    assert_eq!(left.channel(0), source.as_slice());
    assert!(left.channel(1).iter().all(|&s| s == 0.0));
    let right = render_mix(&mut scratch, vec![(panned(1.0), source.clone())], 1.0);
    assert!(right.channel(0).iter().all(|&s| s == 0.0));
    assert_eq!(right.channel(1), source.as_slice());
}

#[test]
fn a_half_pan_follows_the_balance_law() {
    let mut scratch = Scratch::new("halfpan");
    let source = ramp(300);
    let buf = render_mix(&mut scratch, vec![(panned(0.5), source.clone())], 1.0);
    assert_eq!(buf.channel(0), scaled(&source, 0.5).as_slice());
    assert_eq!(buf.channel(1), source.as_slice());
}

#[test]
fn solo_silences_the_other_tracks_and_mute_beats_solo() {
    let mut scratch = Scratch::new("solo");
    let a = ramp(200);
    let b: Vec<f32> = ramp(200).iter().map(|s| -s).collect();
    let mut solo_b = SessionTrack::new("b");
    solo_b.solo = true;
    let buf = render_mix(
        &mut scratch,
        vec![
            (SessionTrack::new("a"), a.clone()),
            (solo_b.clone(), b.clone()),
        ],
        1.0,
    );
    assert_eq!(buf.channel(0), b.as_slice());
    assert_eq!(buf.channel(1), b.as_slice());

    solo_b.mute = true;
    let buf = render_mix(
        &mut scratch,
        vec![(SessionTrack::new("a"), a), (solo_b, b)],
        1.0,
    );
    assert!(
        buf.channel(0)
            .iter()
            .chain(buf.channel(1))
            .all(|&s| s == 0.0)
    );
}

#[test]
fn master_gain_scales_both_channels() {
    let mut scratch = Scratch::new("master");
    let source = ramp(200);
    let buf = render_mix(&mut scratch, vec![(panned(0.0), source.clone())], 0.5);
    let half = scaled(&source, 0.5);
    assert_eq!(buf.channel(0), half.as_slice());
    assert_eq!(buf.channel(1), half.as_slice());
    let buf = render_mix(&mut scratch, vec![(panned(-0.5), source)], 0.0);
    assert!(
        buf.channel(0)
            .iter()
            .chain(buf.channel(1))
            .all(|&s| s == 0.0)
    );
}
