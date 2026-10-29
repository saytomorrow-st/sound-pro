//! Acceptance tests for the cycle region in the editor (SPEC-026).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::{Editor, EditorError, bars, bars_in};

const RATE: u32 = 48_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("daw-gui-metronome-{tag}-{}", std::process::id()));
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

fn write_wav(path: &Path, len: usize) {
    let mut buf = AudioBuffer::new(1, len);
    for (i, s) in buf.channel_mut(0).iter_mut().enumerate() {
        *s = (i + 1) as f32 / 10_000.0;
    }
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

fn editor(dir: &Scratch) -> Editor {
    let a = dir.join("a.wav");
    write_wav(&a, 60_000);
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    editor
}

#[test]
fn the_metronome_is_off_and_does_not_dirty_the_project() {
    let mut editor = Editor::new();
    assert!(!editor.metronome);
    editor.metronome = true;
    assert!(!editor.is_dirty());
    assert!(!editor.can_undo());
}

#[test]
fn playback_passes_the_metronome_on() {
    let dir = Scratch::new("pass");
    let mut editor = editor(&dir);
    let (control, _engine) = editor.playback(0).unwrap();
    assert!(!control.metronome());
    editor.metronome = true;
    let (control, _engine) = editor.playback(0).unwrap();
    assert!(control.metronome());
}

#[test]
fn playback_clicks_at_the_project_tempo_and_meter() {
    let dir = Scratch::new("meter");
    let mut editor = editor(&dir);
    editor.metronome = true;
    editor.set_tempo(90.0);
    editor.set_time_signature(3, 4).unwrap();
    let (mut with, mut a) = editor.playback(0).unwrap();
    editor.metronome = false;
    let (mut without, mut b) = editor.playback(0).unwrap();
    with.play();
    without.play();
    let mut x = vec![0.0f32; 60_000 * 2];
    let mut y = vec![0.0f32; 60_000 * 2];
    a.fill(&mut x, 2);
    b.fill(&mut y, 2);
    let click: Vec<f32> = x.iter().zip(&y).step_by(2).map(|(p, q)| p - q).collect();
    // 90 BPM: the second beat starts on frame 32 000.
    assert!(click[32_000].abs() > 1e-3);
    assert!(click[31_000..31_999].iter().all(|s| s.abs() < 1e-6));
}

#[test]
fn set_tempo_is_clamped_undoable_and_dirty() {
    let mut editor = Editor::new();
    editor.set_tempo(140.0);
    assert_eq!(editor.session().tempo_bpm, 140.0);
    assert!(editor.is_dirty());
    editor.set_tempo(5.0);
    assert_eq!(editor.session().tempo_bpm, 20.0);
    editor.set_tempo(5_000.0);
    assert_eq!(editor.session().tempo_bpm, 999.0);
    assert!(editor.undo());
    assert_eq!(editor.session().tempo_bpm, 20.0);
    assert!(editor.undo());
    assert!(editor.undo());
    assert_eq!(editor.session().tempo_bpm, 120.0);
    assert!(!editor.is_dirty());
}

#[test]
fn a_bad_or_unchanged_tempo_does_nothing() {
    let mut editor = Editor::new();
    editor.set_tempo(f64::NAN);
    editor.set_tempo(f64::INFINITY);
    editor.set_tempo(120.0);
    assert_eq!(editor.session().tempo_bpm, 120.0);
    assert!(!editor.is_dirty());
    assert!(!editor.can_undo());
}

#[test]
fn set_time_signature_is_undoable() {
    let mut editor = Editor::new();
    editor.set_time_signature(6, 8).unwrap();
    let sig = editor.session().time_signature;
    assert_eq!((sig.numerator, sig.denominator), (6, 8));
    assert!(editor.is_dirty());
    assert!(editor.undo());
    assert_eq!(
        editor.session().time_signature,
        audio_core::TimeSignature::default()
    );
    assert!(!editor.is_dirty());
}

#[test]
fn a_bad_time_signature_is_an_error_and_changes_nothing() {
    let mut editor = Editor::new();
    for (n, d) in [(0, 4), (17, 4), (4, 3), (4, 0)] {
        assert!(matches!(
            editor.set_time_signature(n, d),
            Err(EditorError::InvalidTimeSignature)
        ));
    }
    editor.set_time_signature(4, 4).unwrap();
    assert!(!editor.is_dirty());
    assert!(!editor.can_undo());
}

#[test]
fn tempo_and_time_signature_are_saved() {
    let dir = Scratch::new("save");
    let mut editor = editor(&dir);
    editor.set_tempo(97.5);
    editor.set_time_signature(5, 8).unwrap();
    editor.metronome = true;
    let path = dir.join("p.json");
    editor.save_as(&path).unwrap();
    let mut back = Editor::new();
    back.open(&path).unwrap();
    assert_eq!(back.session().tempo_bpm, 97.5);
    let sig = back.session().time_signature;
    assert_eq!((sig.numerator, sig.denominator), (5, 8));
}

#[test]
fn export_never_contains_the_click() {
    let dir = Scratch::new("export");
    let mut editor = editor(&dir);
    let plain = dir.join("plain.wav");
    editor.export(&plain).unwrap();
    editor.metronome = true;
    let clicked = dir.join("clicked.wav");
    editor.export(&clicked).unwrap();
    assert_eq!(
        std::fs::read(&plain).unwrap(),
        std::fs::read(&clicked).unwrap()
    );
}

#[test]
fn bars_in_counts_the_beats_of_the_bar() {
    // 120 BPM at 48 kHz: one beat is 24 000 frames.
    assert_eq!(bars_in(0, RATE, 120.0, 3), "1.1");
    assert_eq!(bars_in(24_000 * 2, RATE, 120.0, 3), "1.3");
    assert_eq!(bars_in(24_000 * 3, RATE, 120.0, 3), "2.1");
    assert_eq!(bars_in(24_000 * 6, RATE, 120.0, 7), "1.7");
    assert_eq!(bars_in(24_000 * 4, RATE, 120.0, 0), "2.1");
    assert_eq!(
        bars(24_000 * 4, RATE, 120.0),
        bars_in(24_000 * 4, RATE, 120.0, 4)
    );
}
