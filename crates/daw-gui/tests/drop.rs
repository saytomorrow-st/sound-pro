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

fn target(pointer: Option<(f32, f32)>, audio: &[bool], snap: bool) -> daw_gui::DropTarget {
    daw_gui::drop_target(pointer, 24.0, 80.0, audio, RATE, 100.0, 120.0, snap)
}

#[test]
fn a_drop_on_an_audio_row_goes_to_that_track_under_the_pointer() {
    let audio = [true, true, true];
    // Row 1 spans y = 24 + 80 .. 24 + 160.
    let t = target(Some((130.0, 24.0 + 80.0 + 10.0)), &audio, false);
    assert_eq!(t.track, Some(1));
    assert_eq!(t.start, daw_gui::x_to_samples(130.0, RATE, 100.0));
    let last = target(Some((10.0, 24.0 + 2.0 * 80.0 + 79.0)), &audio, false);
    assert_eq!(last.track, Some(2));
}

#[test]
fn a_drop_snaps_to_the_beat_unless_free() {
    let audio = [true];
    let free = target(Some((130.0, 40.0)), &audio, false);
    let snapped = target(Some((130.0, 40.0)), &audio, true);
    assert_eq!(snapped.track, Some(0));
    assert_eq!(
        snapped.start,
        daw_gui::snap_to_beat(free.start, RATE, 120.0)
    );
}

#[test]
fn a_drop_below_the_last_track_or_on_the_ruler_makes_a_new_track() {
    let audio = [true, true];
    assert_eq!(
        target(Some((50.0, 24.0 + 2.0 * 80.0 + 1.0)), &audio, false).track,
        None
    );
    assert_eq!(target(Some((50.0, 500.0)), &audio, false).track, None);
    assert_eq!(target(Some((50.0, 10.0)), &audio, false).track, None);
    assert_eq!(target(Some((50.0, 24.0)), &audio, false).track, Some(0));
    assert_eq!(target(Some((50.0, 24.0)), &[], false).track, None);
}

#[test]
fn a_drop_on_an_instrument_row_or_without_a_pointer_makes_a_new_track() {
    let rows = [true, false];
    assert_eq!(
        target(Some((50.0, 24.0 + 80.0 + 5.0)), &rows, false).track,
        None
    );
    assert_eq!(
        target(None, &rows, true),
        daw_gui::DropTarget {
            track: None,
            start: 0
        }
    );
    assert_eq!(target(Some((f32::NAN, 40.0)), &rows, true).track, None);
}

#[test]
fn a_negative_x_drops_at_zero() {
    let t = target(Some((-30.0, 40.0)), &[true], true);
    assert_eq!((t.track, t.start), (Some(0), 0));
}

#[test]
fn native_pointer_conversion_scales_by_pixels_per_point() {
    // A 2x (HiDPI) screen: the same physical cursor distance maps to half the
    // egui points. A 100px cursor at a 2x scale is 50 points in.
    let local = daw_gui::native_pointer_to_timeline(
        (300.0, 200.0), // native cursor, monitor space
        (100.0, 100.0), // window origin in native px
        2.0,
        (0.0, 0.0),
    );
    assert_eq!(local, (100.0, 50.0));
}

#[test]
fn native_pointer_conversion_is_independent_of_window_position() {
    // The same cursor offset from the window must give the same timeline point
    // whether the window sits at (10, 10) or (1500, 900) on the desktop.
    let origin = (1500.0, 900.0);
    let local = daw_gui::native_pointer_to_timeline(
        (origin.0 + 240.0, origin.1 + 120.0),
        origin,
        1.0,
        (0.0, 0.0),
    );
    assert_eq!(local, (240.0, 120.0));
}

#[test]
fn native_pointer_conversion_subtracts_the_timeline_area_origin() {
    // The timeline does not start at the window's top-left: its own origin must
    // be subtracted so the drop lands on the timeline, not the window frame.
    let local = daw_gui::native_pointer_to_timeline((500.0, 400.0), (0.0, 0.0), 1.0, (40.0, 24.0));
    assert_eq!(local, (460.0, 376.0));
}

#[test]
fn native_pointer_conversion_ignores_window_size() {
    // A drop must land at the same place no matter how large the window is:
    // only the window origin (its `.min`) and the scale factor matter. The
    // old bug placed the region depending on screen/window size.
    let origin = (100.0, 50.0);
    let scale = 1.5;
    let cursor = (origin.0 + 180.0, origin.1 + 90.0);
    let expected = daw_gui::native_pointer_to_timeline(cursor, origin, scale, (0.0, 0.0));
    // Three very different window sizes all share the same top-left origin.
    for _size in [(1920.0, 1080.0), (800.0, 600.0), (2560.0, 1440.0)] {
        let local = daw_gui::native_pointer_to_timeline(cursor, origin, scale, (0.0, 0.0));
        assert_eq!(local, expected);
    }
}

#[test]
fn native_pointer_conversion_falls_back_on_a_bad_scale() {
    // A non-finite or non-positive scale must not produce NaN/inf or divide by
    // zero; it behaves as a 1x screen.
    for bad in [f32::NAN, f32::INFINITY, 0.0, -2.0] {
        let local =
            daw_gui::native_pointer_to_timeline((120.0, 80.0), (20.0, 30.0), bad, (0.0, 0.0));
        assert_eq!(local, (100.0, 50.0), "scale {bad}");
    }
}

#[test]
fn a_scaled_window_places_the_clip_under_the_cursor() {
    // End-to-end: `XQueryPointer` returns *window-relative* native pixels, so
    // only the scale factor and the timeline's own top-left corner (area.min)
    // matter — never the window size or its desktop position. This is the
    // actual conversion `handle_drop` performs on X11.
    let audio = [true, true];
    let scale = 2.0;
    // Cursor over row 1 (y = 24 + 80 + 10 = 114 points from the window top),
    // at x = 130 points. The window-relative native pixels come from
    // `XQueryPointer`; only the scale factor matters, never the window size.
    let win_x = 130.0 * scale;
    let win_y = 114.0 * scale;
    let local = daw_gui::native_pointer_to_timeline((win_x, win_y), (0.0, 0.0), scale, (0.0, 0.0));
    let t = daw_gui::drop_target(Some(local), 24.0, 80.0, &audio, RATE, 100.0, 120.0, false);
    assert_eq!(t.track, Some(1));
    assert_eq!(t.start, daw_gui::x_to_samples(130.0, RATE, 100.0));
}

#[test]
fn the_os_cursor_drop_does_not_depend_on_window_size() {
    // Bug: the region used to land somewhere that changed with the screen/window
    // size. Because `XQueryPointer` reports window-relative pixels, the same
    // window-relative cursor maps to the same timeline point at every window
    // size (only the scale matters).
    let audio = [true];
    let scale = 1.5;
    // Cursor at 90 points in from the window left, on row 0 (y = 24 + 10 = 34).
    let win = (90.0 * scale, 34.0 * scale);
    let expected = daw_gui::native_pointer_to_timeline(win, (0.0, 0.0), scale, (0.0, 0.0));
    for _size in [(1920.0, 1080.0), (800.0, 600.0), (2560.0, 1440.0)] {
        let local = daw_gui::native_pointer_to_timeline(win, (0.0, 0.0), scale, (0.0, 0.0));
        let t = daw_gui::drop_target(Some(local), 24.0, 80.0, &audio, RATE, 100.0, 120.0, false);
        assert_eq!(t.track, Some(0));
        assert_eq!(local, expected);
    }
}

#[test]
fn a_scaled_drop_onto_an_existing_track_with_regions_stays_on_it() {
    // Bug #2: a drop onto a track that already has regions must not create a
    // new track. The cursor (scaled, with a window origin) maps to row 0, and
    // the file lands as a second clip on that same track.
    let dir = Scratch::new("scaled-existing");
    let first = dir.join("a.wav");
    write_wav(&first, 4800);
    let wav = dir.join("d.wav");
    write_wav(&wav, 4800);
    let mut editor = Editor::new();
    editor.import_audio_at(&first, None, None).unwrap();
    let audio: Vec<bool> = editor
        .session()
        .tracks
        .iter()
        .map(|t| t.kind == audio_core::session::TrackKind::Audio)
        .collect();

    let scale = 1.5;
    // Window-relative native cursor over row 0 (y = 24 + 10 = 34 points) at
    // x = 200 points. The window origin is already baked into `XQueryPointer`,
    // so nothing is subtracted but the scale.
    let win = (200.0 * scale, 34.0 * scale);
    let local = daw_gui::native_pointer_to_timeline(win, (0.0, 0.0), scale, (0.0, 0.0));
    let t = daw_gui::drop_target(Some(local), 24.0, 80.0, &audio, RATE, 100.0, 120.0, true);
    assert_eq!(t.track, Some(0), "the cursor is over the existing track");
    let (index, _) = editor
        .import_audio_at(&wav, t.track, Some(t.start))
        .unwrap();
    assert_eq!(editor.session().tracks.len(), 1, "no new track is created");
    assert_eq!(index, 0);
    assert_eq!(editor.session().tracks[0].clips.len(), 2);
}

/// What the window computes on each frame of a drag: hover frames feed the
/// tracker, the drop frame asks it where the file goes.
fn frame_target(pointer: Option<(f32, f32)>, audio: &[bool]) -> Option<daw_gui::DropTarget> {
    pointer.map(|p| target(Some(p), audio, true))
}

#[test]
fn a_drop_without_a_pointer_lands_where_the_preview_was_drawn() {
    let audio = [true, true];
    let mut tracker = daw_gui::DropTracker::default();
    // The preview follows the pointer over row 1, about one second in.
    tracker.hover(frame_target(Some((100.0, 24.0 + 80.0 + 10.0)), &audio));
    let drawn = frame_target(Some((100.0, 24.0 + 80.0 + 10.0)), &audio).unwrap();
    // The platform sends no pointer on the frame of the drop.
    let landed = tracker.release(frame_target(None, &audio));
    assert_eq!(landed, drawn);
    assert_eq!(landed.track, Some(1));
    assert_ne!(landed.start, 0, "the drop must not fall back to the start");
}

#[test]
fn the_drop_position_follows_the_last_preview_not_a_fixed_bar() {
    let audio = [true];
    let bar3 = 2 * 4 * 60 * u64::from(RATE) / 120;
    let mut starts = Vec::new();
    for x in [50.0_f32, 300.0, 800.0] {
        let mut tracker = daw_gui::DropTracker::default();
        tracker.hover(frame_target(Some((x, 40.0)), &audio));
        starts.push(tracker.release(None).start);
    }
    assert!(starts.windows(2).all(|w| w[0] < w[1]), "{starts:?}");
    assert!(starts.iter().any(|&s| s != bar3), "{starts:?}");
}

#[test]
fn a_drop_without_any_hover_or_pointer_makes_a_new_track_at_zero() {
    let mut tracker = daw_gui::DropTracker::default();
    assert_eq!(
        tracker.release(None),
        daw_gui::DropTarget {
            track: None,
            start: 0
        }
    );
}

#[test]
fn a_hover_without_a_pointer_keeps_the_previous_preview() {
    let audio = [true, true];
    let mut tracker = daw_gui::DropTracker::default();
    tracker.hover(frame_target(Some((100.0, 24.0 + 85.0)), &audio));
    tracker.hover(frame_target(None, &audio));
    assert_eq!(tracker.release(None).track, Some(1));
}

#[test]
fn a_released_drop_does_not_leak_into_the_next_one() {
    let audio = [true];
    let mut tracker = daw_gui::DropTracker::default();
    tracker.hover(frame_target(Some((100.0, 40.0)), &audio));
    let _ = tracker.release(None);
    assert_eq!(tracker.release(None).track, None);
    tracker.hover(frame_target(Some((100.0, 40.0)), &audio));
    tracker.cancel();
    assert_eq!(tracker.release(None).start, 0);
}

#[test]
fn a_drop_on_an_existing_audio_track_does_not_add_a_track() {
    let dir = Scratch::new("existing-track");
    let wav = dir.join("d.wav");
    write_wav(&wav, 4800);
    let mut editor = Editor::new();
    let first = dir.join("a.wav");
    write_wav(&first, 4800);
    editor.import_audio_at(&first, None, None).unwrap();
    let audio: Vec<bool> = editor
        .session()
        .tracks
        .iter()
        .map(|t| t.kind == audio_core::session::TrackKind::Audio)
        .collect();
    let mut tracker = daw_gui::DropTracker::default();
    tracker.hover(frame_target(Some((200.0, 24.0 + 10.0)), &audio));
    let t = tracker.release(frame_target(None, &audio));
    let (index, _) = editor
        .import_audio_at(&wav, t.track, Some(t.start))
        .unwrap();
    assert_eq!(editor.session().tracks.len(), 1);
    assert_eq!(index, 0);
    assert_eq!(editor.session().tracks[0].clips[1].start_samples, t.start);
}
