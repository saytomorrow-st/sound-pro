//! Acceptance tests for mix export with bit depth, sample rate and
//! normalisation (SPEC-052).

use audio_core::session::{Clip, Session, Track as SessionTrack};
use audio_core::wav::{BitDepth, read_wav_mono};
use daw_app::{ExportSpec, SessionJob, TrackSource, render_export, render_session};

/// Scratch file inside the per-process temp directory.
struct Scratch {
    dir: std::path::PathBuf,
    next: usize,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-export-{tag}-{}", std::process::id()));
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

/// A one-track session whose clip plays `source` from frame 0.
fn job(source: Vec<f32>, output: std::path::PathBuf) -> SessionJob {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, source.len() as u64));
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track.clone());
    SessionJob {
        session,
        tracks: vec![TrackSource {
            track,
            clips: vec![Box::new(source)],
        }],
        sample_rate: RATE,
        block_frames: 64,
        frames: None,
        output,
    }
}

/// A sine at `freq_hz` of `frames` frames, as a plain sample vector.
fn sine_vec(freq_hz: f64, frames: usize) -> Vec<f32> {
    audio_core::wav::sine(freq_hz, RATE, 0, frames)
}

/// Reads a WAV's integer samples back as `f32` in `[-1, 1]`.
fn read_pcm(path: &std::path::Path, bits: u16) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.bits_per_sample, bits);
    let scale = (1 << (bits - 1)) as f32;
    match bits {
        16 => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / scale)
            .collect(),
        24 => reader
            .samples::<i32>()
            .map(|s| s.unwrap() as f32 / scale)
            .collect(),
        _ => unreachable!(),
    }
}

#[test]
fn writes_16_bit_pcm() {
    let mut scratch = Scratch::new("i16");
    let source = vec![0.5_f32; 1_000];
    let output = scratch.path();
    let report = render_export(
        job(source.clone(), output.clone()),
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: RATE,
            normalize: None,
        },
    )
    .unwrap();
    assert_eq!(report.frames, 1_000);
    let samples = read_pcm(&output, 16);
    assert_eq!(samples.len(), 2_000); // stereo interleaved
    // 0.5 * 32767 = 16383.5 -> round -> 16384; /32768 = 0.5.
    assert!(samples.iter().all(|&s| (s - 0.5).abs() < 1e-4));
}

#[test]
fn writes_24_bit_pcm() {
    let mut scratch = Scratch::new("i24");
    let source = vec![0.25_f32; 1_000];
    let output = scratch.path();
    render_export(
        job(source, output.clone()),
        ExportSpec {
            bit_depth: BitDepth::I24,
            sample_rate: RATE,
            normalize: None,
        },
    )
    .unwrap();
    let samples = read_pcm(&output, 24);
    assert_eq!(samples.len(), 2_000);
    assert!(samples.iter().all(|&s| (s - 0.25).abs() < 1e-4));
}

#[test]
fn resamples_to_a_different_rate() {
    let mut scratch = Scratch::new("rate");
    let source = sine_vec(440.0, RATE as usize); // one second
    let output = scratch.path();
    let report = render_export(
        job(source, output.clone()),
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: 44_100,
            normalize: None,
        },
    )
    .unwrap();
    // One second of 44.1 kHz stereo: 44100 frames.
    assert_eq!(report.frames, 44_100);
    let spec = read_wav_mono(&output).unwrap().0;
    assert_eq!(spec.sample_rate, 44_100);
    assert_eq!(spec.channels, 2);
}

#[test]
fn normalizes_to_a_target_peak() {
    let mut scratch = Scratch::new("norm");
    // A quiet source: peak 0.25.
    let source = sine_vec(440.0, RATE as usize)
        .into_iter()
        .map(|s| s * 0.25)
        .collect::<Vec<f32>>();
    let output = scratch.path();
    render_export(
        job(source, output.clone()),
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: RATE,
            normalize: Some(-6.0),
        },
    )
    .unwrap();
    let samples = read_pcm(&output, 16);
    let peak = samples.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
    // Target -6 dBFS = 10^(-6/20) ≈ 0.5012.
    assert!((peak - 0.501_187).abs() < 1e-3, "peak was {peak}");
}

#[test]
fn silence_is_not_divided_by_zero() {
    let mut scratch = Scratch::new("silence");
    let source = vec![0.0_f32; 1_000];
    let output = scratch.path();
    let report = render_export(
        job(source, output.clone()),
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: RATE,
            normalize: Some(-1.0),
        },
    )
    .unwrap();
    assert_eq!(report.frames, 1_000);
    let samples = read_pcm(&output, 16);
    assert!(samples.iter().all(|&s| s == 0.0));
}

#[test]
fn zero_sample_rate_is_rejected() {
    let mut scratch = Scratch::new("zerorate");
    let source = vec![0.5_f32; 100];
    let output = scratch.path();
    let err = render_export(
        job(source, output),
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: 0,
            normalize: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("sample rate"));
}

#[test]
fn export_matches_render_session_pcm() {
    // The master mix at equal rate, without normalisation, must quantise to
    // the same integer samples whether written through `render_session`'s
    // float path or `render_export`'s integer path.
    let mut scratch = Scratch::new("match");
    let source = sine_vec(660.0, 2_000);

    let float_out = scratch.path();
    let float_job = job(source.clone(), float_out.clone());
    render_session(float_job).unwrap();

    let pcm_out = scratch.path();
    let pcm_job = job(source, pcm_out.clone());
    render_export(
        pcm_job,
        ExportSpec {
            bit_depth: BitDepth::I16,
            sample_rate: RATE,
            normalize: None,
        },
    )
    .unwrap();

    let (_spec, float_buf) = read_wav_mono(&float_out).unwrap();
    let pcm = read_pcm(&pcm_out, 16);

    // Compare the float mix quantised to 16 bit against the PCM export.
    let quantise =
        |s: f32| -> f32 { (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16 as f32 / 32_768.0 };
    for (i, &s) in pcm.iter().enumerate() {
        let ch = i % 2;
        let frame = i / 2;
        let expected = quantise(float_buf.channel(ch)[frame]);
        assert!(
            (s - expected).abs() < 1e-4,
            "frame {frame} ch {ch}: {s} vs {expected}"
        );
    }
}
