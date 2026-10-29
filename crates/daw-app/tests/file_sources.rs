//! Acceptance tests for clips backed by WAV files (SPEC-014).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::graph::Graph;
use audio_core::session::{Clip, Session, Track as SessionTrack};
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use audio_core::{Project, ProjectError, SessionError};
use daw_app::{LoadError, SessionJob, TrackSource, load_session_project, render_session};

const RATE: u32 = 48_000;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ramp(len: usize, step: f32) -> Vec<f32> {
    (0..len).map(|i| i as f32 * step).collect()
}

fn write_wav(path: &Path, sample_rate: u32, channels: &[Vec<f32>]) {
    let mut buf = AudioBuffer::new(channels.len(), channels[0].len());
    for (ch, data) in channels.iter().enumerate() {
        buf.channel_mut(ch).copy_from_slice(data);
    }
    let spec = WavSpec {
        sample_rate,
        channels: u16::try_from(channels.len()).unwrap(),
    };
    write_wav_mono(path, spec, &buf).unwrap();
}

/// Two tracks; the second clip is trimmed and reads the same file twice.
fn two_track_session() -> Session {
    let mut a = Clip::full(0, 1000);
    a.source = Some("audio/a.wav".to_owned());
    let mut b = Clip::full(300, 500);
    b.offset_samples = 100;
    b.length_samples = 400;
    b.gain = 0.5;
    b.source = Some("audio/b.wav".to_owned());
    let mut again = Clip::full(900, 200);
    again.source = Some("audio/b.wav".to_owned());

    let mut left = SessionTrack::new("left");
    left.pan = -0.5;
    left.clips.push(a);
    let mut right = SessionTrack::new("right");
    right.pan = 0.25;
    right.gain = 0.75;
    right.clips.push(b);
    right.clips.push(again);

    let mut session = Session::new("files", RATE, 120.0);
    session.tracks.push(left);
    session.tracks.push(right);
    session
}

fn save_project(path: &Path, session: Session) {
    let graph = Graph::with_capacity(RATE, 1, 64, 1, 1);
    let project = Project::with_session(Project::capture("files", RATE, 120.0, &graph), session);
    std::fs::write(path, project.to_json()).unwrap();
}

fn render_to(job: SessionJob) -> AudioBuffer {
    let output = job.output.clone();
    render_session(job).unwrap();
    read_wav_mono(output).unwrap().1
}

fn assert_bits_eq(left: &AudioBuffer, right: &AudioBuffer) {
    assert_eq!(left.channels(), right.channels());
    assert_eq!(left.frames(), right.frames());
    for ch in 0..left.channels() {
        for (i, (l, r)) in left.channel(ch).iter().zip(right.channel(ch)).enumerate() {
            assert_eq!(l.to_bits(), r.to_bits(), "ch{ch} frame {i}: {l} vs {r}");
        }
    }
}

#[test]
fn a_project_renders_from_its_files_exactly_like_the_same_samples_in_memory() {
    let dir = Scratch::new("render");
    let a = ramp(1000, 1e-3);
    // Longer than source_length: the tail must be dropped.
    let b = ramp(700, -1e-3);
    write_wav(&dir.join("audio/a.wav"), RATE, std::slice::from_ref(&a));
    write_wav(&dir.join("audio/b.wav"), RATE, std::slice::from_ref(&b));
    let session = two_track_session();
    let project = dir.join("song.json");
    save_project(&project, session.clone());

    let job = load_session_project(&project, dir.join("from-files.wav")).unwrap();
    assert_eq!(job.sample_rate, RATE);
    assert_eq!(job.frames, None);
    assert_eq!(job.session, session);
    let from_files = render_to(job);

    let in_memory = SessionJob {
        tracks: vec![
            TrackSource {
                track: session.tracks[0].clone(),
                clips: vec![Box::new(a)],
            },
            TrackSource {
                track: session.tracks[1].clone(),
                clips: vec![Box::new(b[..500].to_vec()), Box::new(b[..200].to_vec())],
            },
        ],
        session,
        sample_rate: RATE,
        block_frames: 256,
        frames: None,
        output: dir.join("in-memory.wav"),
    };
    let expected = render_to(in_memory);
    assert_eq!(from_files.channels(), 2);
    assert_eq!(from_files.frames(), 1100);
    assert!(from_files.channel(0).iter().any(|s| *s != 0.0));
    assert_bits_eq(&from_files, &expected);
}

#[test]
fn an_absolute_source_path_is_used_as_is() {
    let dir = Scratch::new("absolute");
    let samples = ramp(64, 1e-2);
    let wav = dir.join("audio/abs.wav");
    write_wav(&wav, RATE, std::slice::from_ref(&samples));
    let mut clip = Clip::full(0, 64);
    clip.source = Some(wav.to_str().unwrap().to_owned());
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    let project = dir.join("elsewhere/p.json");
    save_project(&project, session);

    let out = render_to(load_session_project(&project, dir.join("abs-out.wav")).unwrap());
    for ch in 0..2 {
        let bits: Vec<u32> = out.channel(ch).iter().map(|s| s.to_bits()).collect();
        let want: Vec<u32> = samples.iter().map(|s| s.to_bits()).collect();
        assert_eq!(bits, want);
    }
}

#[test]
fn a_stereo_file_is_averaged_to_mono() {
    let dir = Scratch::new("stereo");
    let left = vec![0.5_f32, 0.25, -1.0, 0.0];
    let right = vec![0.25_f32, 0.25, 1.0, 0.5];
    write_wav(&dir.join("audio/st.wav"), RATE, &[left, right]);
    let mut clip = Clip::full(0, 4);
    clip.source = Some("audio/st.wav".to_owned());
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    let project = dir.join("p.json");
    save_project(&project, session);

    let out = render_to(load_session_project(&project, dir.join("st-out.wav")).unwrap());
    assert_eq!(out.channel(0), &[0.375, 0.25, 0.0, 0.25]);
    assert_eq!(out.channel(1), out.channel(0));
}

fn one_clip_project(dir: &Scratch, source: Option<&str>, source_length: u64) -> PathBuf {
    let mut clip = Clip::full(0, source_length);
    clip.source = source.map(str::to_owned);
    let mut track = SessionTrack::new("t");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    let project = dir.join("p.json");
    save_project(&project, session);
    project
}

#[test]
fn a_missing_file_is_reported_with_its_resolved_path() {
    let dir = Scratch::new("missing");
    let project = one_clip_project(&dir, Some("audio/nope.wav"), 10);
    let err = load_session_project(&project, dir.join("o.wav")).unwrap_err();
    match &err {
        LoadError::Wav { path, .. } => assert_eq!(path, &dir.join("audio/nope.wav")),
        other => panic!("expected Wav, got {other:?}"),
    }
    assert!(err.to_string().contains("nope.wav"), "{err}");
}

#[test]
fn a_file_at_another_sample_rate_is_converted_on_load() {
    // SPEC-019: a 44.1 kHz constant renders as the same constant at 48 kHz.
    let dir = Scratch::new("rate");
    write_wav(&dir.join("audio/x.wav"), 44_100, &[vec![0.5; 4410]]);
    let project = one_clip_project(&dir, Some("audio/x.wav"), 4800);
    let out = render_to(load_session_project(&project, dir.join("o.wav")).unwrap());
    assert_eq!(out.frames(), 4800);
    for (i, &v) in out.channel(0).iter().enumerate().skip(500).take(3800) {
        assert!((v - 0.5).abs() < 1e-3, "frame {i}: {v}");
    }
}

#[test]
fn a_converted_file_shorter_than_source_length_is_rejected() {
    let dir = Scratch::new("rate-short");
    write_wav(&dir.join("audio/x.wav"), 44_100, &[vec![0.5; 4410]]);
    let project = one_clip_project(&dir, Some("audio/x.wav"), 4801);
    let err = load_session_project(&project, dir.join("o.wav")).unwrap_err();
    assert!(
        matches!(
            err,
            LoadError::SourceTooShort {
                file_frames: 4800,
                source_length: 4801,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_file_shorter_than_source_length_is_rejected() {
    let dir = Scratch::new("short");
    write_wav(&dir.join("audio/x.wav"), RATE, &[vec![0.1; 9]]);
    let project = one_clip_project(&dir, Some("audio/x.wav"), 10);
    let err = load_session_project(&project, dir.join("o.wav")).unwrap_err();
    assert!(
        matches!(
            err,
            LoadError::SourceTooShort {
                file_frames: 9,
                source_length: 10,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_clip_without_a_source_cannot_be_loaded() {
    let dir = Scratch::new("nosource");
    let project = one_clip_project(&dir, None, 10);
    let err = load_session_project(&project, dir.join("o.wav")).unwrap_err();
    assert!(
        matches!(err, LoadError::MissingSource { track: 0, clip: 0 }),
        "{err:?}"
    );
}

#[test]
fn unreadable_and_invalid_projects_are_reported() {
    let dir = Scratch::new("badproject");
    let err = load_session_project(&dir.join("absent.json"), dir.join("o.wav")).unwrap_err();
    assert!(matches!(err, LoadError::Read { .. }), "{err:?}");

    let project = one_clip_project(&dir, Some("audio/x.wav"), 10);
    let text = std::fs::read_to_string(&project)
        .unwrap()
        .replace("\"audio/x.wav\"", "\"\"");
    std::fs::write(&project, text).unwrap();
    let err = load_session_project(&project, dir.join("o.wav")).unwrap_err();
    assert!(
        matches!(
            err,
            LoadError::Project {
                source: ProjectError::Session(SessionError::InvalidSource { track: 0, clip: 0 }),
                ..
            }
        ),
        "{err:?}"
    );
}
