//! Acceptance tests for the editor model behind the window (SPEC-015).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use audio_core::{MAX_TRACKS, Project, SessionError};
use daw_gui::{Editor, EditorError, PEAK_BLOCK, Peaks};

const RATE: u32 = 48_000;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-gui-{tag}-{}", std::process::id()));
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

fn write_wav(path: &Path, sample_rate: u32, samples: &[f32]) {
    let mut buf = AudioBuffer::new(1, samples.len());
    buf.channel_mut(0).copy_from_slice(samples);
    write_wav_mono(
        path,
        WavSpec {
            sample_rate,
            channels: 1,
        },
        &buf,
    )
    .unwrap();
}

/// A valid 32-bit float mono WAV header with no frames.
fn write_empty_wav(path: &Path) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&36_u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&3_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    std::fs::write(path, bytes).unwrap();
}

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i as f32 / len as f32) - 0.5).collect()
}

#[test]
fn a_new_editor_is_an_empty_clean_project() {
    let editor = Editor::new();
    assert!(editor.session().tracks.is_empty());
    assert_eq!(editor.session().sample_rate, RATE);
    assert!(!editor.is_dirty());
    assert!(editor.path().is_none());
    assert_eq!(editor.title(), "Sound Pro — untitled");
}

#[test]
fn importing_a_wav_adds_a_named_track_with_one_full_clip() {
    let dir = Scratch::new("import");
    let wav = dir.join("audio").join("kick drum.wav");
    write_wav(&wav, RATE, &ramp(1000));
    let mut editor = Editor::new();

    assert_eq!(editor.import_wav(&wav).unwrap(), (0, 0));

    let track = &editor.session().tracks[0];
    assert_eq!(track.name, "kick drum");
    let clip = &track.clips[0];
    assert_eq!(
        (clip.start_samples, clip.length_samples, clip.source_length),
        (0, 1000, 1000)
    );
    assert_eq!(clip.name, "kick drum");
    assert_eq!(clip.source.as_deref(), wav.to_str());
    assert!(editor.is_dirty());
    assert_eq!(editor.title(), "Sound Pro — untitled *");
    assert_eq!(editor.peaks(clip).unwrap().frames, 1000);
}

#[test]
fn importing_onto_a_selected_track_appends_after_its_last_clip() {
    let dir = Scratch::new("append");
    let a = dir.join("audio").join("a.wav");
    let b = dir.join("audio").join("b.wav");
    write_wav(&a, RATE, &ramp(300));
    write_wav(&b, RATE, &ramp(200));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();

    assert_eq!(editor.import_wav(&b).unwrap(), (0, 1));
    assert_eq!(editor.session().tracks.len(), 1);
    assert_eq!(editor.session().tracks[0].clips[1].start_samples, 300);

    editor.selected_track = None;
    assert_eq!(editor.import_wav(&b).unwrap(), (1, 0));
    assert_eq!(editor.session().tracks[1].name, "b");
}

#[test]
fn import_errors_leave_the_project_unchanged() {
    let dir = Scratch::new("import-errors");
    let slow = dir.join("audio").join("slow.wav");
    let empty = dir.join("audio").join("empty.wav");
    let missing = dir.join("audio").join("missing.wav");
    write_wav(&slow, 44_100, &ramp(100));
    write_empty_wav(&empty);
    let mut editor = Editor::new();

    assert!(matches!(
        editor.import_wav(&slow),
        Err(EditorError::SampleRate {
            file: 44_100,
            project: RATE,
            ..
        })
    ));
    assert!(matches!(
        editor.import_wav(&empty),
        Err(EditorError::EmptyFile { .. })
    ));
    assert!(matches!(
        editor.import_wav(&missing),
        Err(EditorError::Wav { .. })
    ));
    assert!(editor.session().tracks.is_empty());
    assert!(!editor.is_dirty());
}

#[test]
fn the_track_limit_is_enforced_on_import() {
    let dir = Scratch::new("limit");
    let wav = dir.join("audio").join("x.wav");
    write_wav(&wav, RATE, &ramp(10));
    let mut editor = Editor::new();
    for _ in 0..MAX_TRACKS {
        editor.selected_track = None;
        editor.import_wav(&wav).unwrap();
    }
    editor.selected_track = None;
    assert!(matches!(
        editor.import_wav(&wav),
        Err(EditorError::Session(SessionError::TooManyTracks { .. }))
    ));
    assert_eq!(editor.session().tracks.len(), MAX_TRACKS);
}

#[test]
fn saving_writes_sources_inside_the_project_folder_as_relative_paths() {
    let dir = Scratch::new("save");
    let inside = dir.join("audio").join("in.wav");
    write_wav(&inside, RATE, &ramp(100));
    let outside_dir = Scratch::new("save-outside");
    let outside = outside_dir.join("audio").join("out.wav");
    write_wav(&outside, RATE, &ramp(100));

    let mut editor = Editor::new();
    editor.import_wav(&inside).unwrap();
    editor.selected_track = None;
    editor.import_wav(&outside).unwrap();
    let project = dir.join("song.json");
    editor.save_as(&project).unwrap();

    assert!(!editor.is_dirty());
    assert_eq!(editor.path(), Some(project.as_path()));
    assert_eq!(editor.title(), "Sound Pro — song.json");
    let saved = Project::from_json(&std::fs::read_to_string(&project).unwrap()).unwrap();
    assert_eq!(saved.name, "song");
    assert_eq!(
        saved.session.tracks[0].clips[0].source.as_deref(),
        Some("audio/in.wav")
    );
    assert_eq!(
        saved.session.tracks[1].clips[0].source.as_deref(),
        outside.to_str()
    );
    // In memory the paths stay absolute.
    assert_eq!(
        editor.session().tracks[0].clips[0].source.as_deref(),
        inside.to_str()
    );
}

#[test]
fn a_saved_project_reopens_with_its_waveforms() {
    let dir = Scratch::new("reopen");
    let wav = dir.join("audio").join("a.wav");
    write_wav(&wav, RATE, &ramp(600));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.track_mut(0).unwrap().pan = -0.25;
    let project = dir.join("song.json");
    editor.save_as(&project).unwrap();

    let mut reopened = Editor::new();
    reopened.open(&project).unwrap();

    assert!(!reopened.is_dirty());
    let clip = &reopened.session().tracks[0].clips[0];
    assert_eq!(clip.source.as_deref(), wav.to_str());
    assert_eq!(reopened.session().tracks[0].pan, -0.25);
    assert_eq!(reopened.peaks(clip).unwrap().frames, 600);
}

#[test]
fn a_clip_whose_file_is_gone_opens_without_a_waveform() {
    let dir = Scratch::new("gone");
    let wav = dir.join("audio").join("a.wav");
    write_wav(&wav, RATE, &ramp(100));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    let project = dir.join("song.json");
    editor.save_as(&project).unwrap();
    std::fs::remove_file(&wav).unwrap();

    let mut reopened = Editor::new();
    reopened.open(&project).unwrap();
    let clip = &reopened.session().tracks[0].clips[0];
    assert!(reopened.peaks(clip).is_none());
}

#[test]
fn a_bad_project_file_keeps_the_current_project() {
    let dir = Scratch::new("bad-open");
    let wav = dir.join("audio").join("a.wav");
    write_wav(&wav, RATE, &ramp(100));
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();

    assert!(matches!(
        editor.open(&bad),
        Err(EditorError::Project { .. })
    ));
    assert!(matches!(
        editor.open(&dir.join("missing.json")),
        Err(EditorError::Read { .. })
    ));
    assert_eq!(editor.session().tracks.len(), 1);
    assert!(editor.is_dirty());
}

#[test]
fn deleting_clips_and_tracks_marks_the_project_changed() {
    let dir = Scratch::new("delete");
    let wav = dir.join("audio").join("a.wav");
    write_wav(&wav, RATE, &ramp(100));
    let mut editor = Editor::new();
    editor.import_wav(&wav).unwrap();
    editor.import_wav(&wav).unwrap();
    editor.save_as(&dir.join("p.json")).unwrap();

    editor.delete_clip(0, 5);
    editor.delete_track(9);
    assert!(!editor.is_dirty());

    editor.delete_clip(0, 0);
    assert!(editor.is_dirty());
    assert_eq!(editor.session().tracks[0].clips.len(), 1);
    assert_eq!(editor.selected_clip, None);

    editor.delete_track(0);
    assert!(editor.session().tracks.is_empty());
}

#[test]
fn master_gain_is_clamped() {
    let mut editor = Editor::new();
    editor.set_master_gain(3.0);
    assert_eq!(editor.session().master_gain, 1.0);
    editor.set_master_gain(f32::NAN);
    assert_eq!(editor.session().master_gain, 1.0);
    editor.set_master_gain(0.5);
    assert_eq!(editor.session().master_gain, 0.5);
    assert!(editor.is_dirty());
}

#[test]
fn export_matches_rendering_the_saved_project_from_the_command_line() {
    let dir = Scratch::new("export");
    let a = dir.join("audio").join("a.wav");
    let b = dir.join("audio").join("b.wav");
    write_wav(&a, RATE, &ramp(700));
    write_wav(&b, RATE, &ramp(300));
    let mut editor = Editor::new();
    editor.import_wav(&a).unwrap();
    editor.selected_track = None;
    editor.import_wav(&b).unwrap();
    editor.track_mut(1).unwrap().pan = 0.5;
    let project = dir.join("song.json");
    editor.save_as(&project).unwrap();

    let from_editor = dir.join("editor.wav");
    let report = editor.export(&from_editor).unwrap();
    assert_eq!(report.frames, 700);

    let from_file = dir.join("file.wav");
    let job = daw_app::load_session_project(&project, from_file.clone()).unwrap();
    daw_app::render_session(job).unwrap();

    let (_, ours) = read_wav_mono(&from_editor).unwrap();
    let (_, theirs) = read_wav_mono(&from_file).unwrap();
    assert_eq!(ours.channel(0), theirs.channel(0));
    assert_eq!(ours.channel(1), theirs.channel(1));
}

#[test]
fn exporting_an_empty_project_is_an_error() {
    let dir = Scratch::new("export-empty");
    assert!(matches!(
        Editor::new().export(&dir.join("x.wav")),
        Err(EditorError::Empty)
    ));
}

#[test]
fn peaks_summarise_blocks_and_the_short_tail() {
    let mut samples = vec![0.0_f32; PEAK_BLOCK + 10];
    samples[3] = 0.75;
    samples[7] = -0.5;
    samples[PEAK_BLOCK + 2] = -0.25;
    let peaks = Peaks::from_samples(&samples);

    assert_eq!(peaks.min, vec![-0.5, -0.25]);
    assert_eq!(peaks.max, vec![0.75, 0.0]);
    assert_eq!(peaks.frames, (PEAK_BLOCK + 10) as u64);
    assert_eq!(peaks.range(0, 1), Some((-0.5, 0.75)));
    assert_eq!(peaks.range(PEAK_BLOCK as u64, 10_000), Some((-0.25, 0.0)));
    assert_eq!(peaks.range(10_000, 20_000), None);
    assert_eq!(Peaks::from_samples(&[]).range(0, 10), None);
}
