//! Acceptance tests for reading WAV and MP3 clip files (SPEC-032).

use std::path::{Path, PathBuf};

use daw_app::audio_file::{AudioFileError, is_audio_file, read_audio, read_mono_at};

fn sine_mp3() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/sine.mp3")
}

#[test]
fn audio_files_are_recognised_by_extension() {
    assert!(is_audio_file(Path::new("a.wav")));
    assert!(is_audio_file(Path::new("b.MP3")));
    assert!(is_audio_file(Path::new("dir/c.Mp3")));
    assert!(!is_audio_file(Path::new("song.flac")));
    assert!(!is_audio_file(Path::new("notes.txt")));
    assert!(!is_audio_file(Path::new("noext")));
}

#[test]
fn an_mp3_decodes_to_a_half_second_tone() {
    let (spec, buffer) = read_audio(&sine_mp3()).unwrap();
    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.channels, 1);
    // 0.5 s of audio, plus encoder padding of at most a few MP3 frames.
    let frames = buffer.frames();
    assert!((24_000..=24_000 + 3 * 1_152).contains(&frames), "{frames}");
    let peak = buffer
        .channel(0)
        .iter()
        .fold(0.0f32, |m, &s| m.max(s.abs()));
    // ffmpeg's `sine` source has amplitude 1/8.
    assert!((0.1..=0.15).contains(&peak), "peak {peak}");
}

#[test]
fn an_mp3_is_resampled_to_the_project_rate() {
    let at_48k = read_mono_at(&sine_mp3(), 48_000).unwrap();
    let at_24k = read_mono_at(&sine_mp3(), 24_000).unwrap();
    let expected = at_48k.len() / 2;
    assert!(at_24k.len().abs_diff(expected) <= 1, "{}", at_24k.len());
}

#[test]
fn a_broken_mp3_is_an_error_not_a_panic() {
    let path = std::env::temp_dir().join(format!("daw-broken-{}.mp3", std::process::id()));
    std::fs::write(&path, b"this is not an mp3 file at all").unwrap();
    let result = read_audio(&path);
    let _ = std::fs::remove_file(&path);
    assert!(
        matches!(result, Err(AudioFileError::Mp3(_) | AudioFileError::Empty)),
        "{result:?}"
    );
}
