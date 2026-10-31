//! Acceptance tests for WAV/MP3 import and drag and drop (SPEC-032).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use daw_gui::{Editor, EditorError};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-drop-{tag}-{}", std::process::id()));
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

fn sine_mp3() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../daw-app/tests/data/sine.mp3")
}

fn write_wav(path: &Path, len: usize) {
    let mut buf = AudioBuffer::new(1, len);
    buf.channel_mut(0).fill(0.25);
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

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

#[test]
fn an_mp3_imports_and_is_heard_in_playback_and_export() {
    let dir = Scratch::new("mp3");
    let mut editor = Editor::new();
    let (t, c) = editor.import_wav(&sine_mp3()).unwrap();
    let clip = &editor.session().tracks[t].clips[c];
    assert_eq!(clip.start_samples, 0);
    assert!(clip.length_samples >= 24_000, "{}", clip.length_samples);
    assert_eq!(editor.session().tracks[t].name, "sine");

    let out = dir.join("mix.wav");
    editor.export(&out).unwrap();
    let (_, mix) = read_wav_mono(&out).unwrap();
    assert!(peak(mix.channel(0)) > 0.05);

    let (mut control, mut engine) = editor.playback(0).unwrap();
    control.play();
    let mut buffer = vec![0.0f32; 4_096 * 2];
    engine.fill(&mut buffer, 2);
    assert!(peak(&buffer) > 0.05);
}

#[test]
fn dropping_on_a_track_places_the_clip_under_the_pointer() {
    let dir = Scratch::new("onto");
    let wav = dir.join("a.wav");
    write_wav(&wav, 1_000);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();

    let (added, error) = editor.drop_audio_files(&[sine_mp3()], Some(0), 96_000);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(added, 1);
    let session = editor.session();
    assert_eq!(session.tracks.len(), 1);
    assert_eq!(session.tracks[0].clips.len(), 2);
    assert_eq!(session.tracks[0].clips[1].start_samples, 96_000);
    assert_eq!(editor.selected_clip, Some((0, 1)));
}

#[test]
fn dropping_below_the_tracks_makes_a_new_track() {
    let dir = Scratch::new("below");
    let wav = dir.join("a.wav");
    write_wav(&wav, 1_000);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();

    let (added, error) = editor.drop_audio_files(&[sine_mp3()], None, 4_800);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(added, 1);
    let session = editor.session();
    assert_eq!(session.tracks.len(), 2);
    assert_eq!(session.tracks[1].name, "sine");
    assert_eq!(session.tracks[1].clips[0].start_samples, 4_800);
}

#[test]
fn several_dropped_files_follow_each_other_on_one_track() {
    let dir = Scratch::new("many");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 1_000);
    write_wav(&b, 500);
    let mut editor = Editor::new();

    let (added, error) = editor.drop_audio_files(&[a, b], None, 2_000);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(added, 2);
    let clips = &editor.session().tracks[0].clips;
    assert_eq!(editor.session().tracks.len(), 1);
    assert_eq!(clips[0].start_samples, 2_000);
    assert_eq!(clips[1].start_samples, 3_000);
}

#[test]
fn non_audio_files_are_skipped_and_one_drop_is_one_undo_step_per_file() {
    let dir = Scratch::new("skip");
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, "hello").unwrap();
    let mut editor = Editor::new();

    let (added, error) = editor.drop_audio_files(&[txt], None, 0);
    assert_eq!(added, 0);
    assert!(error.is_none());
    assert!(editor.session().tracks.is_empty());

    editor.drop_audio_files(&[sine_mp3()], None, 0);
    assert_eq!(editor.session().tracks.len(), 1);
    assert!(editor.undo());
    assert!(editor.session().tracks.is_empty());
}

#[test]
fn a_broken_mp3_reports_an_error_and_changes_nothing() {
    let dir = Scratch::new("broken");
    let bad = dir.join("bad.mp3");
    std::fs::write(&bad, b"definitely not mpeg audio").unwrap();
    let mut editor = Editor::new();

    let (added, error) = editor.drop_audio_files(&[bad], None, 0);
    assert_eq!(added, 0);
    assert!(
        matches!(
            error,
            Some(EditorError::Decode { .. } | EditorError::EmptyFile { .. })
        ),
        "{error:?}"
    );
    assert!(editor.session().tracks.is_empty());
    assert!(!editor.is_dirty());
    assert!(!editor.can_undo());
}

#[test]
fn peaks_fill_in_as_the_decoder_advances() {
    let total = 4 * 256 + 100;
    let mut peaks = daw_gui::Peaks::empty(total as u64);
    let signal: Vec<f32> = (0..total).map(|i| (i % 7) as f32 / 10.0 - 0.3).collect();

    // Half decoded: only blocks the data fully covers are produced.
    let first = peaks.blocks_from(&signal[..600], RATE, RATE, 0);
    assert_eq!(first.len(), 2);
    peaks.set_blocks(0, &first);
    assert_eq!(peaks.range(0, 512), Some((-0.3, 0.3)));

    // The rest continues from the blocks already sent, including the tail.
    let rest = peaks.blocks_from(&signal, RATE, RATE, first.len());
    assert_eq!(rest.len(), 3);
    peaks.set_blocks(first.len(), &rest);
    let whole = daw_gui::Peaks::empty(total as u64);
    let all = whole.blocks_from(&signal, RATE, RATE, 0);
    assert_eq!([first, rest].concat(), all);
}

#[test]
fn peak_blocks_follow_the_source_rate() {
    let peaks = daw_gui::Peaks::empty(48_000);
    let half_rate = vec![0.5f32; 12_000];
    let blocks = peaks.blocks_from(&half_rate, 24_000, RATE, 0);
    // 12 000 source frames cover 24 000 project frames.
    assert_eq!(blocks.len(), 24_000 / 256);
}

#[test]
fn a_cached_file_is_read_without_decoding_and_a_changed_one_is_not() {
    let dir = Scratch::new("cache");
    let wav = dir.join("a.wav");
    write_wav(&wav, 1000);
    let cache = daw_app::pcm_cache::PcmCache::new(dir.join("cache"));
    let mut calls = 0;
    let first =
        daw_gui::read_source_cached(&wav, RATE, Some(&cache), &mut |_, _| calls += 1).unwrap();
    assert_eq!(first.left.len(), 1000);
    // Served from the cache: the progress callback is not called.
    let before = calls;
    let again =
        daw_gui::read_source_cached(&wav, RATE, Some(&cache), &mut |_, _| calls += 1).unwrap();
    assert_eq!(again.left, first.left);
    assert_eq!(calls, before);

    write_wav(&wav, 1500);
    let changed = daw_gui::read_source_cached(&wav, RATE, Some(&cache), &mut |_, _| {}).unwrap();
    assert_eq!(changed.left.len(), 1500);
}

#[test]
fn drop_start_snaps_and_clamps() {
    assert_eq!(daw_gui::drop_start(0.0, RATE, 100.0, 120.0, true), 0);
    let free = daw_gui::drop_start(100.0, RATE, 100.0, 120.0, false);
    assert_eq!(free, RATE as u64);
    let snapped = daw_gui::drop_start(98.0, RATE, 100.0, 120.0, true);
    assert_eq!(snapped % u64::from(RATE / 2), 0);
}

#[test]
fn probe_len_comes_from_the_header_for_mp3_and_wav() {
    let dir = Scratch::new("probe");
    let wav = dir.join("p.wav");
    write_wav(&wav, 2400);
    assert_eq!(daw_gui::probe_len(&wav, RATE).unwrap(), 2400);
    assert!(daw_gui::probe_len(&sine_mp3(), RATE).unwrap() >= 24_000);
}
