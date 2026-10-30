//! Acceptance tests for fade shapes and fade rules on split (SPEC-045 §1, §3).

use std::path::{Path, PathBuf};

use audio_core::Clip;
use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_gui::{Editor, EditorError};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-rules-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
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
    let spec = WavSpec {
        sample_rate: 48_000,
        channels: 1,
    };
    write_wav_mono(path, spec, &buf).unwrap();
}

fn one_clip(dir: &Scratch) -> Editor {
    let wav = dir.0.join("a.wav");
    write_wav(&wav, &vec![0.5; 1_000]);
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor
}

fn clip(editor: &Editor, index: usize) -> &Clip {
    &editor.session().tracks[0].clips[index]
}

#[test]
fn fade_curves_are_clamped_and_undoable() {
    let dir = Scratch::new("curves");
    let mut editor = one_clip(&dir);
    editor.set_fade_curves(0, 0, -3.0, f32::NAN).unwrap();
    assert_eq!(
        (
            clip(&editor, 0).fade_in_curve,
            clip(&editor, 0).fade_out_curve
        ),
        (-1.0, 0.0)
    );
    editor.set_fade_curves(0, 0, 0.5, 2.0).unwrap();
    assert_eq!(
        (
            clip(&editor, 0).fade_in_curve,
            clip(&editor, 0).fade_out_curve
        ),
        (0.5, 1.0)
    );
    editor.undo();
    assert_eq!(clip(&editor, 0).fade_in_curve, -1.0);
    assert!(matches!(
        editor.set_fade_curves(0, 5, 0.0, 0.0),
        Err(EditorError::NoClip)
    ));
}

#[test]
fn unchanged_curves_add_no_undo_step() {
    let dir = Scratch::new("same");
    let mut editor = one_clip(&dir);
    editor.set_fade_curves(0, 0, 0.5, 0.5).unwrap();
    editor.set_fade_curves(0, 0, 0.5, 0.5).unwrap();
    editor.undo();
    assert_eq!(clip(&editor, 0).fade_in_curve, 0.0);
}

#[test]
fn each_part_of_a_split_keeps_only_its_outer_fade() {
    let dir = Scratch::new("split");
    let mut editor = one_clip(&dir);
    editor.set_fades(0, 0, 100, 200).unwrap();
    editor.set_fade_curves(0, 0, -0.5, 0.75).unwrap();
    let (_, right) = editor.split_clip(0, 0, 500).unwrap();
    let (l, r) = (clip(&editor, right - 1), clip(&editor, right));
    assert_eq!((l.fade_in_samples, l.fade_out_samples), (100, 0));
    assert_eq!((l.fade_in_curve, l.fade_out_curve), (-0.5, 0.0));
    assert_eq!((r.fade_in_samples, r.fade_out_samples), (0, 200));
    assert_eq!((r.fade_in_curve, r.fade_out_curve), (0.0, 0.75));
}

#[test]
fn splitting_after_a_crossfade_leaves_the_crossfade_alone() {
    let dir = Scratch::new("xsplit");
    let mut editor = one_clip(&dir);
    let (_, right) = editor.split_clip(0, 0, 500).unwrap();
    let (_, right) = editor.move_clip(0, right, 400, 0).unwrap();
    let left = usize::from(right == 0);
    editor.crossfade(0, left).unwrap();
    // Cut the late clip well after the 100-sample crossfade.
    let late_start = clip(&editor, right).start_samples;
    editor.split_clip(0, right, late_start + 300).unwrap();
    let clips = &editor.session().tracks[0].clips;
    let early = clips.iter().find(|c| c.start_samples == 0).unwrap();
    let late = clips.iter().find(|c| c.start_samples == 400).unwrap();
    let tail = clips.iter().find(|c| c.start_samples == 700).unwrap();
    assert_eq!(early.fade_out_samples, 100);
    assert_eq!(late.fade_in_samples, 100);
    assert_eq!(late.fade_out_samples, 0);
    assert_eq!((tail.fade_in_samples, tail.fade_out_samples), (0, 0));
}

#[test]
fn only_edges_outside_other_clips_get_a_fade_handle() {
    use daw_gui::layout::edges_in_overlap;
    let clips = vec![
        Clip::full(0, 1_000),
        Clip::full(600, 1_000),
        Clip::full(1_600, 500),
    ];
    assert_eq!(edges_in_overlap(&clips, 0), (false, true));
    assert_eq!(edges_in_overlap(&clips, 1), (true, false));
    // Touching is not overlapping.
    assert_eq!(edges_in_overlap(&clips, 2), (false, false));
    assert_eq!(edges_in_overlap(&clips, 9), (false, false));
}
