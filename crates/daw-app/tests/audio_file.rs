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

#[test]
fn the_parallel_resampler_matches_the_plain_one_bit_for_bit() {
    let input: Vec<f32> = (0..300_000)
        .map(|i| ((i as f32) * 0.013).sin() * 0.5)
        .collect();
    for (from, to) in [(44_100, 48_000), (48_000, 44_100), (48_000, 48_000)] {
        let plain = audio_core::resample(&input, from, to).unwrap();
        let fast = daw_app::audio_file::resample_parallel(&input, from, to).unwrap();
        assert_eq!(plain.len(), fast.len());
        assert!(
            plain
                .iter()
                .zip(&fast)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{from} -> {to}"
        );
    }
    assert!(daw_app::audio_file::resample_parallel(&input, 0, 48_000).is_err());
}

#[test]
fn the_header_gives_the_length_without_decoding() {
    for rate in [48_000, 24_000] {
        let probed = daw_app::audio_file::probe_len_at(&sine_mp3(), rate).unwrap();
        let decoded = read_mono_at(&sine_mp3(), rate).unwrap().len() as u64;
        assert!(
            probed.abs_diff(decoded) <= u64::from(rate) / 100,
            "{rate}: probed {probed}, decoded {decoded}"
        );
    }
    let wav = std::env::temp_dir().join(format!("daw-probe-{}.wav", std::process::id()));
    audio_core::wav::write_mono_slice(&wav, 44_100, &vec![0.25; 44_100]).unwrap();
    assert_eq!(
        daw_app::audio_file::probe_len_at(&wav, 44_100).unwrap(),
        44_100
    );
    assert_eq!(
        daw_app::audio_file::probe_len_at(&wav, 48_000).unwrap(),
        audio_core::resampled_len(44_100, 44_100, 48_000).unwrap() as u64
    );
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn a_broken_file_has_no_probed_length() {
    let path = std::env::temp_dir().join(format!("daw-probe-broken-{}.mp3", std::process::id()));
    std::fs::write(&path, b"this is not an mp3 file at all").unwrap();
    assert!(daw_app::audio_file::probe_len_at(&path, 48_000).is_err());
    let wav = path.with_extension("wav");
    std::fs::write(&wav, b"RIFF nonsense").unwrap();
    assert!(daw_app::audio_file::probe_len_at(&wav, 48_000).is_err());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&wav);
}
