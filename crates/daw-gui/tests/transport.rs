//! Acceptance tests for playback from the editor (SPEC-016).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use daw_gui::{Editor, EditorError, bars, clock, samples_to_x, x_to_samples};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-play-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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

fn write_wav(path: &Path, samples: &[f32]) {
    let mut buf = AudioBuffer::new(1, samples.len());
    buf.channel_mut(0).copy_from_slice(samples);
    write_wav_mono(
        path,
        WavSpec {
            sample_rate: RATE,
            channels: 1,
        },
        &buf,
    )
    .unwrap();
}

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i + 1) as f32 / 10_000.0).collect()
}

#[test]
fn an_empty_project_cannot_play() {
    assert!(matches!(Editor::new().playback(0), Err(EditorError::Empty)));
}

#[test]
fn playback_sounds_like_the_export() {
    let dir = Scratch::new("export");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, &ramp(2_500));
    write_wav(&b, &ramp(900));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    editor.selected_track = None;
    editor.import_wav(&b).unwrap();
    editor.track_mut(1).unwrap().pan = -0.5;
    editor.track_mut(0).unwrap().gain = 0.5;

    let out = dir.join("mix.wav");
    editor.export(&out).unwrap();
    let (_, mix) = read_wav_mono(&out).unwrap();

    let (mut control, mut engine) = editor.playback(0).unwrap();
    assert_eq!(control.end(), 2_500);
    control.play();
    let mut played = Vec::new();
    let mut buffer = vec![0.0f32; 512 * 2];
    while played.len() < 2_500 * 2 {
        engine.fill(&mut buffer, 2);
        played.extend_from_slice(&buffer);
    }
    for frame in 0..2_500 {
        assert_eq!(played[frame * 2], mix.channel(0)[frame], "left {frame}");
        assert_eq!(
            played[frame * 2 + 1],
            mix.channel(1)[frame],
            "right {frame}"
        );
    }
    assert!(!control.is_playing());
    assert_eq!(control.position(), 2_500);
}

#[test]
fn playback_starts_where_asked_or_from_zero_past_the_end() {
    let dir = Scratch::new("start");
    let a = dir.join("a.wav");
    write_wav(&a, &ramp(1_000));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    assert_eq!(editor.playback(400).unwrap().0.position(), 400);
    assert_eq!(editor.playback(1_000).unwrap().0.position(), 0);
    assert_eq!(editor.playback(5_000).unwrap().0.position(), 0);
}

#[test]
fn positions_and_pixels_round_trip() {
    assert!((samples_to_x(48_000, RATE, 100.0) - 100.0).abs() < 1e-4);
    assert!((samples_to_x(24_000, RATE, 50.0) - 25.0).abs() < 1e-4);
    assert_eq!(x_to_samples(100.0, RATE, 100.0), 48_000);
    assert_eq!(x_to_samples(25.0, RATE, 50.0), 24_000);
    for samples in [0_u64, 1_200, 48_000, 1_000_000] {
        let x = samples_to_x(samples, RATE, 120.0);
        assert!(x_to_samples(x, RATE, 120.0).abs_diff(samples) <= 1);
    }
}

#[test]
fn negative_or_broken_offsets_give_zero() {
    assert_eq!(x_to_samples(-10.0, RATE, 100.0), 0);
    assert_eq!(x_to_samples(f32::NAN, RATE, 100.0), 0);
    assert_eq!(x_to_samples(10.0, RATE, 0.0), 0);
}

#[test]
fn the_clock_shows_minutes_seconds_and_milliseconds() {
    assert_eq!(clock(0, RATE), "0:00.000");
    assert_eq!(clock(24_000, RATE), "0:00.500");
    assert_eq!(clock(48_000 * 75 + 48, RATE), "1:15.001");
}

#[test]
fn bars_count_from_one_in_four_four() {
    // 120 BPM: one beat is half a second, one bar two seconds.
    assert_eq!(bars(0, RATE, 120.0), "1.1");
    assert_eq!(bars(24_000, RATE, 120.0), "1.2");
    assert_eq!(bars(23_999, RATE, 120.0), "1.1");
    assert_eq!(bars(96_000, RATE, 120.0), "2.1");
    assert_eq!(bars(96_000 + 72_000, RATE, 120.0), "2.4");
}
