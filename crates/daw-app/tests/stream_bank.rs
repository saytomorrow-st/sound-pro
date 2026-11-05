//! Acceptance tests for Streaming banks (SPEC-073 / RFC-002 stage 6): the
//! streaming cache load, the budget fallback in `load_bank_auto`, the disk
//! loader and the streamed `SamplerInstrument` render.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use audio_core::wav::{BitDepth, write_wav_pcm};
use audio_core::{
    Interpolation, MidiEvent, MidiMessage, SamplerEngine, Schedule, ScheduleSpec, ScheduledEvent,
};
use daw_app::instrument::{Instrument, SamplerInstrument};
use daw_app::sample_bank::{
    LoadedBank, build_bank_cache, load_bank_auto_with_budget, load_bank_from_cache,
    load_bank_from_cache_streaming,
};
use daw_app::stream_loader::StreamLoader;

const RATE: u32 = 48_000;

type ProgressCb = Box<Mutex<dyn FnMut(u64, u64, u64) + Send>>;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-stream-{tag}-{}", std::process::id()));
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

fn no_progress() -> ProgressCb {
    Box::new(Mutex::new(|_: u64, _: u64, _: u64| {}))
}

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// A decaying stereo chirp-ish signal, distinct per channel, so a misplaced
/// block or a wrong channel order changes the output.
#[allow(clippy::cast_precision_loss)]
fn stereo_signal(frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = i as f32 / RATE as f32;
        let env = (-t * 0.8).exp() * 0.5;
        out.push((std::f32::consts::TAU * 220.0 * t).sin() * env);
        out.push((std::f32::consts::TAU * 331.0 * t + 0.3).sin() * env);
    }
    out
}

/// Writes a one-sample stereo one-shot bank of `frames` and builds its cache.
fn build_bank(tag: &str, frames: usize) -> Scratch {
    let dir = Scratch::new(tag);
    write_wav_pcm(
        dir.join("sample.wav"),
        RATE,
        2,
        BitDepth::I16,
        &stereo_signal(frames),
    )
    .unwrap();
    std::fs::write(
        dir.join("bank.sfz"),
        "<region> sample=sample.wav lokey=0 hikey=127 pitch_keycenter=60 loop_mode=one_shot\n",
    )
    .unwrap();
    build_bank_cache(&dir.0, None, &no_progress(), &no_cancel()).unwrap();
    dir
}

fn schedule(notes: &[(u64, u8)], length: u64) -> Schedule {
    let events = notes
        .iter()
        .map(|&(sample, note)| ScheduledEvent {
            sample,
            source: MidiEvent {
                track: 0,
                tick: 0,
                order_in_track: 1,
                message: MidiMessage::NoteOn {
                    channel: 0,
                    note,
                    velocity: 100,
                },
            },
        })
        .collect();
    Schedule::new(ScheduleSpec {
        events,
        length_samples: length,
    })
}

fn engine_for(bank: &LoadedBank) -> SamplerEngine {
    SamplerEngine::new_with(
        RATE,
        bank.regions.clone(),
        bank.samples.clone(),
        8,
        Interpolation::Hermite4,
    )
    .unwrap()
}

/// Renders `frames` in 512-frame blocks, pumping `loader` before each block.
fn render(
    engine: &mut SamplerEngine,
    mut loader: Option<&mut StreamLoader>,
    sched: &Schedule,
    frames: usize,
) -> Vec<f32> {
    const BLOCK: usize = 512;
    let mut cursor = sched.cursor();
    let mut out = Vec::with_capacity(frames * 2);
    let mut l = vec![0.0_f32; BLOCK];
    let mut r = vec![0.0_f32; BLOCK];
    let mut position = 0_u64;
    while out.len() < frames * 2 {
        if let Some(loader) = loader.as_deref_mut() {
            loader.pump();
        }
        engine.render(sched, &mut cursor, position, BLOCK, &mut l, &mut r);
        for i in 0..BLOCK {
            out.push(l[i]);
            out.push(r[i]);
        }
        position += BLOCK as u64;
    }
    out
}

const FRAMES: usize = 60_000;

#[test]
fn streaming_load_keeps_heads_and_points_tails_at_pcm() {
    let dir = build_bank("load", FRAMES);
    let resident = load_bank_from_cache(&dir.0, None, 128).unwrap();
    let streaming = load_bank_from_cache_streaming(&dir.0, None, 128).unwrap();

    assert!(!resident.is_streaming());
    assert!(streaming.is_streaming());
    assert_eq!(streaming.regions.len(), resident.regions.len());
    let path = streaming.stream_files[0].as_ref().expect("tail file");
    assert!(path.exists(), "{}", path.display());
    let source = &streaming.samples[0];
    assert!(source.is_streaming());
    let head = source.head().expect("head");
    assert_eq!(head.total_frames(), FRAMES as u64);
    assert!(
        streaming.ram_bytes < resident.ram_bytes,
        "{} vs {}",
        streaming.ram_bytes,
        resident.ram_bytes
    );
}

#[test]
fn auto_load_over_budget_falls_back_to_streaming() {
    // 300k stereo i16 frames ≈ 1.14 MiB resident > 1 MiB budget; the head fits.
    let dir = build_bank("auto", 300_000);
    let bank = load_bank_auto_with_budget(&dir.0, None, 1).unwrap();
    assert!(bank.is_streaming());
    assert!(bank.samples[0].is_streaming());

    let small = build_bank("auto-small", FRAMES);
    let bank = load_bank_auto_with_budget(&small.0, None, 128).unwrap();
    assert!(!bank.is_streaming(), "a bank within budget stays resident");
}

#[test]
fn loader_render_is_bit_identical_to_resident() {
    let dir = build_bank("bitident", FRAMES);
    let resident = load_bank_from_cache(&dir.0, None, 128).unwrap();
    let streaming = load_bank_from_cache_streaming(&dir.0, None, 128).unwrap();
    let sched = schedule(&[(0, 60), (1000, 67), (5000, 55)], 200_000);
    let total = FRAMES * 2 + 6000;

    let mut reference = engine_for(&resident);
    let expected = render(&mut reference, None, &sched, total);

    let mut engine = engine_for(&streaming);
    let ends = engine.enable_streaming(audio_core::RING_FRAMES);
    let mut loader = StreamLoader::new(ends, Arc::from(streaming.stream_files.clone()));
    let stats = loader.stats();
    let got = render(&mut engine, Some(&mut loader), &sched, total);

    assert!(stats.blocks() > 0);
    assert_eq!(stats.read_errors(), 0);
    assert_eq!(engine.stream_stats().unwrap().underruns(), 0);
    assert!(expected.iter().any(|s| s.abs() > 0.01));
    let first_diff = expected
        .iter()
        .zip(&got)
        .position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first_diff, None, "streamed render diverged");
}

#[test]
fn missing_tail_file_counts_error_and_fades_out() {
    let dir = build_bank("missing", FRAMES);
    let streaming = load_bank_from_cache_streaming(&dir.0, None, 128).unwrap();
    let files: Arc<[Option<PathBuf>]> = Arc::from(vec![Some(dir.join("does-not-exist.pcm"))]);
    let sched = schedule(&[(0, 60)], 200_000);

    let mut engine = engine_for(&streaming);
    let ends = engine.enable_streaming(audio_core::RING_FRAMES);
    let mut loader = StreamLoader::new(ends, files);
    let stats = loader.stats();
    let out = render(&mut engine, Some(&mut loader), &sched, FRAMES);

    assert!(stats.read_errors() > 0);
    assert_eq!(engine.stream_stats().unwrap().underruns(), 1);
    assert_eq!(engine.active_streams(), 0);
    // After the head and the fade the voice is silent.
    let tail_start = (32_768 + 1024) * 2;
    assert!(out[tail_start..].iter().all(|s| *s == 0.0));
}

#[test]
fn instrument_streaming_render_matches_resident() {
    let dir = build_bank("instrument", FRAMES);
    let resident = SamplerInstrument::new(&load_bank_from_cache(&dir.0, None, 128).unwrap());
    let streaming =
        SamplerInstrument::new(&load_bank_from_cache_streaming(&dir.0, None, 128).unwrap());
    assert!(!resident.is_streaming());
    assert!(streaming.is_streaming());

    let notes = [
        audio_core::Note {
            pitch: 60,
            start: 0,
            length: 50_000,
            velocity: 100,
        },
        audio_core::Note {
            pitch: 64,
            start: 3000,
            length: 70_000,
            velocity: 90,
        },
    ];
    let frames = 90_000;
    let a = resident.render_notes(&notes, RATE, frames).unwrap();
    let b = streaming.render_notes(&notes, RATE, frames).unwrap();
    assert_eq!(a.len(), b.len());
    let first_diff = a
        .iter()
        .zip(&b)
        .position(|(x, y)| x.to_bits() != y.to_bits());
    assert_eq!(first_diff, None);
}

#[test]
fn loader_thread_stops_on_drop() {
    let dir = build_bank("thread", FRAMES);
    let streaming = load_bank_from_cache_streaming(&dir.0, None, 128).unwrap();
    let mut engine = engine_for(&streaming);
    let ends = engine.enable_streaming(audio_core::RING_FRAMES);
    let thread = StreamLoader::new(ends, Arc::from(streaming.stream_files.clone()))
        .spawn()
        .unwrap();
    assert_eq!(thread.stats().read_errors(), 0);
    drop(thread);
    // Dropping joined the thread; the engine keeps working without a loader.
    let sched = schedule(&[(0, 60)], 10_000);
    let out = render(&mut engine, None, &sched, 4096);
    assert!(out.iter().any(|s| s.abs() > 0.01));
}

/// SyncStream (RFC-002 §9, SPEC-074): the offline render serves every
/// request synchronously, so even dense high-pitched polyphony — voices that
/// eat their ring several times faster than real time — never underruns and
/// stays bit-identical to the resident render.
#[test]
fn sync_stream_render_never_underruns_under_polyphony() {
    let dir = build_bank("sync-poly", 200_000);
    let resident = SamplerInstrument::new(&load_bank_from_cache(&dir.0, None, 128).unwrap());
    let streaming =
        SamplerInstrument::new(&load_bank_from_cache_streaming(&dir.0, None, 128).unwrap());
    assert!(streaming.is_streaming());

    let mut notes = Vec::new();
    for (i, pitch) in [48_u8, 60, 67, 72, 79, 84, 91, 96, 103, 108, 115, 120]
        .into_iter()
        .enumerate()
    {
        for repeat in 0..3_u64 {
            notes.push(audio_core::Note {
                pitch,
                start: i as u64 * 700 + repeat * 40_000,
                length: 35_000,
                velocity: 40 + (i as u8 * 7),
            });
        }
    }
    let frames = 160_000;
    let a = resident.render_notes(&notes, RATE, frames).unwrap();
    let b = streaming.render_notes(&notes, RATE, frames).unwrap();
    assert!(a.iter().any(|s| s.abs() > 1e-3));
    let first_diff = a
        .iter()
        .zip(&b)
        .position(|(x, y)| x.to_bits() != y.to_bits());
    assert_eq!(first_diff, None);
}
