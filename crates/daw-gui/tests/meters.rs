//! Acceptance tests for level meter ballistics (SPEC-042).

use daw_gui::layout::{METER_HOLD_SECONDS, MeterState, MeterZone, meter_zone};

#[test]
fn a_meter_rises_at_once_and_falls_at_26_db_per_second() {
    let mut m = MeterState::default();
    m.update((0.5, 0.25), 0.01);
    assert_eq!(m.level, [0.5, 0.25]);
    m.update((0.0, 0.0), 1.0);
    let db = 20.0 * m.level[0].log10();
    let want = 20.0 * 0.5f32.log10() - 26.0;
    assert!((db - want).abs() < 1e-3, "{db} vs {want}");
    m.update((0.0, 0.0), 10.0);
    assert_eq!(m.level, [0.0, 0.0], "below the fader floor is silence");
}

#[test]
fn the_peak_mark_holds_then_falls_and_the_meter_goes_idle() {
    let mut m = MeterState::default();
    m.update((0.8, 0.8), 0.01);
    let steps = (METER_HOLD_SECONDS / 0.1) as usize - 1;
    for _ in 0..steps {
        m.update((0.0, 0.0), 0.1);
    }
    assert_eq!(m.hold[0], 0.8, "held while under {METER_HOLD_SECONDS} s");
    m.update((0.0, 0.0), 0.1);
    m.update((0.0, 0.0), 0.1);
    assert!(m.hold[0] < 0.8, "falls after the hold");
    assert!(!m.is_idle());
    for _ in 0..100 {
        m.update((0.0, 0.0), 0.1);
    }
    assert!(m.is_idle());
    assert!((m.peak - 0.8).abs() < 1e-6, "the peak stays until reset");
}

#[test]
fn a_louder_hit_restarts_the_hold() {
    let mut m = MeterState::default();
    m.update((0.3, 0.3), 0.01);
    m.update((0.0, 0.0), 1.4);
    m.update((0.6, 0.0), 0.01);
    m.update((0.0, 0.0), 1.4);
    assert_eq!(m.hold[0], 0.6);
}

#[test]
fn the_peak_clips_at_0_dbfs_and_resets() {
    let mut m = MeterState::default();
    m.update((0.5, 0.99), 0.01);
    assert!(!m.clipped());
    m.update((0.5, 1.2), 0.01);
    assert!(m.clipped());
    m.reset_peak();
    assert!(!m.clipped());
    assert_eq!(m.peak, 0.0);
    m.update((f32::NAN, f32::INFINITY), 0.01);
    assert!(!m.clipped(), "garbage is ignored");
}

#[test]
fn meter_colours_follow_the_db_thresholds() {
    assert_eq!(meter_zone(0.0), MeterZone::Safe);
    assert_eq!(meter_zone(0.49), MeterZone::Safe);
    assert_eq!(meter_zone(0.6), MeterZone::Hot);
    assert_eq!(meter_zone(0.999), MeterZone::Hot);
    assert_eq!(meter_zone(1.0), MeterZone::Clip);
}

mod editor {
    use audio_core::buffer::AudioBuffer;
    use audio_core::wav::{WavSpec, write_wav_mono};
    use daw_gui::Editor;

    fn write_wav(path: &std::path::Path, value: f32, len: usize) {
        let mut buf = AudioBuffer::new(1, len);
        buf.channel_mut(0).fill(value);
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 1,
        };
        write_wav_mono(path, spec, &buf).unwrap();
    }

    #[test]
    fn editor_playback_feeds_track_and_master_meters_across_graph_swaps() {
        let dir = std::env::temp_dir().join(format!("daw-gui-meters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav(&a, 0.8, 48_000);
        write_wav(&b, 0.5, 48_000);
        let mut editor = Editor::new();
        editor.import_wav(&a).unwrap();
        editor.selected_track = None;
        editor.import_wav(&b).unwrap();
        editor.track_mut(1).unwrap().mute = true;

        let meters = editor.meters().clone();
        let (mut control, mut engine) = editor.playback(0).unwrap();
        control.play();
        let mut buffer = vec![0.0f32; 512 * 2];
        engine.fill(&mut buffer, 2);
        let (l, r) = meters.take_track(0);
        assert!(l > 0.3 && r > 0.3, "track 0 sounds: {l} {r}");
        assert_eq!(meters.take_track(1), (0.0, 0.0), "muted track");
        let (ml, _) = meters.take_master();
        assert!(ml > 0.3, "master: {ml}");

        editor.track_mut(1).unwrap().mute = false;
        let (graph, _) = editor.playback_graph().unwrap();
        assert!(control.replace_graph(graph).is_ok());
        for _ in 0..4 {
            engine.fill(&mut buffer, 2);
        }
        let (l, _) = meters.take_track(1);
        assert!(l > 0.2, "the new graph meters too: {l}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
