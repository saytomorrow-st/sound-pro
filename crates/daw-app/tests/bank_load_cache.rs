//! Acceptance tests for loading a bank from its PCM cache and for the
//! MemAvailable RAM budget (SPEC-071 / RFC-002 §7–§8).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_app::bank_cache::cache_dir;
use daw_app::sample_bank::{
    BankLoadError, build_bank_cache, load_bank, load_bank_auto_with_budget, load_bank_from_cache,
};

const RATE: u32 = 48_000;

type ProgressCb = Box<Mutex<dyn FnMut(u64, u64, u64) + Send>>;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-loadcache-{tag}-{}", std::process::id()));
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

fn write_wav(path: &Path, sample_rate: u32, data: &[f32]) {
    let mut buf = AudioBuffer::new(1, data.len());
    buf.channel_mut(0).copy_from_slice(data);
    let spec = WavSpec {
        sample_rate,
        channels: 1,
    };
    write_wav_mono(path, spec, &buf).unwrap();
}

fn tone(frames: usize) -> Vec<f32> {
    vec![0.5_f32; frames]
}

fn no_progress() -> ProgressCb {
    Box::new(Mutex::new(|_: u64, _: u64, _: u64| {}))
}

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn simple_sfz() -> String {
    "<region> sample=sample.wav lokey=0 hikey=127 pitch_keycenter=60\n".to_owned()
}

/// Builds the cache for a one-sample bank and returns the scratch dir.
fn build_cache(tag: &str) -> Scratch {
    let dir = Scratch::new(tag);
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();
    build_bank_cache(&dir.0, None, &no_progress(), &no_cancel()).unwrap();
    dir
}

#[test]
fn load_bank_from_cache_matches_flac_bit_for_bit() {
    let dir = build_cache("bitforbit");
    let from_cache = load_bank_from_cache(&dir.0, None, 128).unwrap();
    let from_flac = load_bank(&dir.0, 128).unwrap();

    assert!(from_cache.cache.is_some(), "cache handle must be set");
    let cache = from_cache.cache.as_ref().unwrap();
    assert_eq!(cache.bank_id, dir.0.file_name().unwrap().to_string_lossy());
    assert_eq!(cache.samples, 1);

    assert_eq!(from_cache.samples.len(), from_flac.samples.len());
    for (a, b) in from_cache.samples.iter().zip(&from_flac.samples) {
        assert_eq!(a.as_i16(), b.as_i16(), "PCM cache must match FLAC decode");
    }
    assert_eq!(from_cache.ram_bytes, from_flac.ram_bytes);
    assert!(from_cache.decode_failures.is_empty());
}

#[test]
fn load_bank_from_cache_returns_no_cache_without_manifest() {
    let dir = Scratch::new("nocache");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();

    match load_bank_from_cache(&dir.0, None, 128) {
        Err(BankLoadError::NoCache(_)) => {}
        other => panic!("expected NoCache, got {other:?}"),
    }
}

#[test]
fn load_bank_auto_builds_cache_then_loads() {
    let dir = Scratch::new("auto-build");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();

    // No cache exists yet; `load_bank_auto_with_budget` must build it in the
    // background then load from it.
    let bank = load_bank_auto_with_budget(&dir.0, None, 128).unwrap();
    assert!(bank.cache.is_some());
    assert!(cache_dir(&dir.0).join("000000.pcm").exists());
    assert_eq!(bank.samples[0].as_i16()[0], 16384);
}

#[test]
fn load_bank_auto_reuses_existing_cache() {
    let dir = build_cache("auto-reuse");
    let bank = load_bank_auto_with_budget(&dir.0, None, 128).unwrap();
    assert!(bank.cache.is_some());
    assert_eq!(bank.samples.len(), 1);
}

#[test]
fn load_bank_from_cache_respects_ram_budget() {
    // 524_289 frames * 2 bytes = 1_048_578 bytes > 1 MiB (1_048_576 bytes).
    let dir = Scratch::new("budget");
    write_wav(&dir.join("sample.wav"), RATE, &tone(524_289));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();
    build_bank_cache(&dir.0, None, &no_progress(), &no_cancel()).unwrap();

    let err = load_bank_from_cache(&dir.0, None, 1).unwrap_err();
    match err {
        BankLoadError::RamBudgetExceeded {
            budget_mib,
            needed_mib,
        } => {
            assert_eq!(budget_mib, 1);
            assert!(needed_mib > 1);
        }
        other => panic!("expected RamBudgetExceeded, got {other:?}"),
    }
}

#[test]
fn load_bank_from_cache_rejects_zero_budget() {
    let dir = build_cache("zero-budget");
    assert!(matches!(
        load_bank_from_cache(&dir.0, None, 0),
        Err(BankLoadError::InvalidBudget)
    ));
}
