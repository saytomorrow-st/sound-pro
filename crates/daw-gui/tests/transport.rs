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
    let map = audio_core::TempoMap::single_or_default(120.0, RATE);
    assert_eq!(bars(0, &map), "1.1");
    assert_eq!(bars(24_000, &map), "1.2");
    assert_eq!(bars(23_999, &map), "1.1");
    assert_eq!(bars(96_000, &map), "2.1");
    assert_eq!(bars(96_000 + 72_000, &map), "2.4");
}

/// Regression: a MIDI track must not silence playback (it rendered fine on
/// export but the player stopped).
#[test]
fn playback_with_a_midi_track_sounds_like_the_export() {
    let dir = Scratch::new("midi");
    let a = dir.join("a.wav");
    write_wav(&a, &ramp(4_000));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    let (t, c) = editor.add_midi_track(0, 4_000).unwrap();
    editor
        .add_note(
            t,
            c,
            audio_core::session::Note {
                start: 0,
                length: 3_000,
                pitch: 60,
                velocity: 100,
            },
        )
        .unwrap();

    let out = dir.join("mix.wav");
    editor.export(&out).unwrap();
    let (_, mix) = read_wav_mono(&out).unwrap();

    let (mut control, mut engine) = editor.playback(0).unwrap();
    control.play();
    let mut played = Vec::new();
    let mut buffer = vec![0.0f32; 512 * 2];
    while played.len() < 4_000 * 2 {
        engine.fill(&mut buffer, 2);
        played.extend_from_slice(&buffer);
    }
    for frame in 0..4_000 {
        assert!(
            (played[frame * 2] - mix.channel(0)[frame]).abs() < 1e-6,
            "left {frame}: {} vs {}",
            played[frame * 2],
            mix.channel(0)[frame]
        );
    }
    assert!(!control.is_playing());
    assert_eq!(control.position(), 4_000);
}

/// SPEC-031: a playhead parked at the end must not put a new clip past the
/// sound; otherwise playback starts in silence.
#[test]
fn new_clip_start_returns_to_zero_past_the_end() {
    let dir = Scratch::new("clipstart");
    let a = dir.join("a.wav");
    write_wav(&a, &ramp(4_000));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    assert_eq!(editor.new_clip_start(4_000), 0);
    assert_eq!(editor.new_clip_start(10_000), 0);

    let at = editor.new_clip_start(4_000);
    editor.add_midi_track(at, 4_000).unwrap();
    let (mut control, mut engine) = editor.playback(at).unwrap();
    control.play();
    let mut buffer = vec![0.0f32; 512 * 2];
    let mut loud = 0.0f32;
    for _ in 0..4 {
        engine.fill(&mut buffer, 2);
        loud = buffer.iter().fold(loud, |m, s| m.max(s.abs()));
    }
    assert!(loud > 0.01, "playback is silent");
}

#[test]
fn new_clip_start_snaps_to_the_bar() {
    let mut editor = Editor::new();
    let rate = u64::from(editor.session().sample_rate);
    let bar = rate * 2; // 4/4 at 120 bpm
    assert_eq!(editor.new_clip_start(0), 0);
    assert_eq!(editor.new_clip_start(bar + 5), bar);

    let (t, _) = editor.add_midi_track(0, bar * 4).unwrap();
    assert_eq!(t, 0);
    assert_eq!(editor.new_clip_start(bar * 2 + bar / 2), bar * 2);
    assert_eq!(editor.new_clip_start(bar * 4), 0);
}

/// SPEC-037: a MIDI region made while looping covers the cycle, wherever
/// the moving playhead happens to be.
#[test]
fn new_midi_region_fills_the_active_cycle() {
    let mut editor = Editor::new();
    let rate = u64::from(editor.session().sample_rate);
    let bar = rate * 2; // 4/4 at 120 bpm
    assert_eq!(editor.new_midi_region(bar + 5), (bar, bar * 4));

    editor.set_cycle(bar * 2, bar * 6).unwrap();
    assert_eq!(editor.new_midi_region(bar * 3 + 17), (bar * 2, bar * 4));
    assert_eq!(editor.new_midi_region(0), (bar * 2, bar * 4));

    editor.cycle_enabled = false;
    assert_eq!(editor.new_midi_region(bar * 3 + 17).0, bar * 3);
}
