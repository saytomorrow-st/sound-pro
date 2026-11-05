//! Acceptance tests for loading an SFZ sample bank (SPEC-064).

use std::path::{Path, PathBuf};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_app::sample_bank::{BankLoadError, DEFAULT_RAM_BUDGET_MIB, load_bank};

const RATE: u32 = 48_000;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-bank-{tag}-{}", std::process::id()));
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

/// A constant `0.5` tone, so `decode_sample` yields a non-zero `i16`.
fn tone(frames: usize) -> Vec<f32> {
    vec![0.5_f32; frames]
}

/// A one-region bank that maps every key to `sample.wav`.
fn simple_sfz() -> String {
    "<region> sample=sample.wav lokey=0 hikey=127 pitch_keycenter=60\n".to_owned()
}

#[test]
fn loads_a_single_region_bank() {
    let dir = Scratch::new("single");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();

    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    assert_eq!(bank.regions.len(), 1);
    assert_eq!(bank.samples.len(), 1);
    assert!(bank.decode_failures.is_empty());
    // 4800 frames of mono i16 → 9600 bytes.
    assert_eq!(bank.ram_bytes, 9600);
    let region = &bank.regions[0];
    assert_eq!(region.root, 60);
    assert_eq!(region.key_lo, 0);
    assert_eq!(region.key_hi, 127);
    assert_eq!(region.sample_index, 0);
    // Constant 0.5 → 16384 in i16.
    assert_eq!(bank.samples[0].as_i16()[0], 16384);
}

#[test]
fn default_path_resolves_samples() {
    let dir = Scratch::new("defaultpath");
    std::fs::create_dir_all(dir.join("samples")).unwrap();
    write_wav(&dir.join("samples/tone.wav"), RATE, &tone(480));
    let sfz = "<control> default_path=samples\n<region> sample=tone.wav\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    assert_eq!(bank.regions.len(), 1);
    assert!(bank.decode_failures.is_empty());
}

#[test]
fn ram_budget_exceeded_is_a_typed_error() {
    let dir = Scratch::new("budget");
    // 4800 frames = 9600 bytes; budget of 1 MiB is plenty, 0 MiB is rejected.
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("bank.sfz"), simple_sfz()).unwrap();

    assert!(matches!(
        load_bank(&dir.0, 0),
        Err(BankLoadError::InvalidBudget)
    ));

    // A bank needing more than a tiny budget is refused wholesale.
    // 4800 frames * 2 bytes = 9600 bytes; a 1-byte-equivalent budget is 0 MiB
    // after the floor, so force it by loading a larger sample under 1 MiB.
    // Use a sample whose size exceeds a 1 MiB budget minus nothing: 1 MiB is
    // 1_048_576 bytes, i.e. 524_288 frames. Exceed it.
    write_wav(&dir.join("big.wav"), RATE, &tone(524_289));
    let sfz = "<region> sample=big.wav\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();
    let err = load_bank(&dir.0, 1).unwrap_err();
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
fn a_missing_sample_is_skipped_and_reported() {
    let dir = Scratch::new("missing");
    let sfz = "<region> sample=present.wav\n<region> sample=absent.wav\n";
    write_wav(&dir.join("present.wav"), RATE, &tone(480));
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    // Only the present sample resolves.
    assert_eq!(bank.regions.len(), 1);
    assert_eq!(bank.samples.len(), 1);
    assert_eq!(bank.decode_failures.len(), 1);
    assert_eq!(bank.decode_failures[0].sample, "absent.wav");
    assert!(!bank.decode_failures[0].reason.is_empty());
}

#[test]
fn two_regions_on_one_file_decode_once() {
    let dir = Scratch::new("dedup");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    // Two regions (velocity layers / round robins) on the same file.
    let sfz = "<region> sample=sample.wav lokey=0 hikey=127 lovel=1 hivel=64\n\
               <region> sample=sample.wav lokey=0 hikey=127 lovel=65 hivel=127\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    // Two playable regions, but the sample is decoded exactly once.
    assert_eq!(bank.regions.len(), 2);
    assert_eq!(bank.samples.len(), 1);
    // Both regions reference the same decoded sample.
    assert_eq!(bank.regions[0].sample_index, 0);
    assert_eq!(bank.regions[1].sample_index, 0);
    // RAM is counted once, not twice.
    assert_eq!(bank.ram_bytes, 9600);
    assert!(bank.decode_failures.is_empty());
}

#[test]
fn a_directory_without_an_sfz_is_an_error() {
    let dir = Scratch::new("nosfz");
    assert!(matches!(
        load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB),
        Err(BankLoadError::NoSfz(_))
    ));
}

#[test]
fn nested_sfz_is_found_and_hint_beats_search() {
    use daw_app::sample_bank::load_bank_with_hint;

    let dir = Scratch::new("hint");
    let nested = dir.join("Pack").join("Programs");
    std::fs::create_dir_all(&nested).unwrap();
    write_wav(&nested.join("sample.wav"), RATE, &tone(4800));
    let two = "<region> sample=sample.wav lokey=0 hikey=63 pitch_keycenter=60\n\
               <region> sample=sample.wav lokey=64 hikey=127 pitch_keycenter=70\n";
    std::fs::write(nested.join("main.sfz"), two).unwrap();
    // A shallower decoy that plain search would prefer.
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(dir.join("decoy.sfz"), simple_sfz()).unwrap();

    let searched = load_bank_with_hint(&dir.0, DEFAULT_RAM_BUDGET_MIB, None).unwrap();
    assert_eq!(searched.regions.len(), 1, "search picks the shallowest sfz");

    let hinted = load_bank_with_hint(
        &dir.0,
        DEFAULT_RAM_BUDGET_MIB,
        Some("Pack/Programs/main.sfz"),
    )
    .unwrap();
    assert_eq!(hinted.regions.len(), 2, "the hint wins over the search");

    let escaping = load_bank_with_hint(&dir.0, DEFAULT_RAM_BUDGET_MIB, Some("../outside.sfz"));
    assert_eq!(
        escaping.unwrap().regions.len(),
        1,
        "escaping hint is ignored"
    );
}

#[test]
fn search_prefers_non_retuned_sfz_deterministically() {
    use daw_app::sample_bank::load_bank_with_hint;

    let dir = Scratch::new("retuned");
    let sub = dir.join("Grand");
    std::fs::create_dir_all(&sub).unwrap();
    write_wav(&sub.join("sample.wav"), RATE, &tone(4800));
    std::fs::write(sub.join("A-Retuned.sfz"), simple_sfz()).unwrap();
    let two = "<region> sample=sample.wav lokey=0 hikey=63 pitch_keycenter=60\n\
               <region> sample=sample.wav lokey=64 hikey=127 pitch_keycenter=70\n";
    std::fs::write(sub.join("Z-Standard.sfz"), two).unwrap();

    let bank = load_bank_with_hint(&dir.0, DEFAULT_RAM_BUDGET_MIB, None).unwrap();
    assert_eq!(bank.regions.len(), 2);
}

#[test]
fn timestamp_is_valid_rfc3339_utc() {
    use daw_app::bank_library::{rfc3339_utc, timestamp};

    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(1_793_786_654), "2026-11-04T10:04:14Z");
    let now = timestamp();
    let b = now.as_bytes();
    assert_eq!(b.len(), 20, "{now}");
    assert!(b[..4].iter().all(u8::is_ascii_digit), "{now}");
    assert_eq!(
        (b[4], b[7], b[10], b[13], b[16], b[19]),
        (b'-', b'-', b'T', b':', b':', b'Z')
    );
}

// ---------------------------------------------------------------------------
// Stage 2: PCM cache generation (RFC-002 §5.3).
// ---------------------------------------------------------------------------

use daw_app::sample_bank::{CacheBuildError, build_bank_cache};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

type ProgressCb = Box<Mutex<dyn FnMut(u64, u64, u64) + Send>>;
type ProgressRecord = Arc<Mutex<Option<(u64, u64, u64)>>>;

/// A progress sink that records the last `(done, total, bytes)` triple.
fn recording_progress() -> (ProgressCb, ProgressRecord) {
    let last: ProgressRecord = Arc::new(Mutex::new(None));
    let last2 = Arc::clone(&last);
    (
        Box::new(Mutex::new(move |done, total, bytes| {
            *last2.lock().unwrap() = Some((done, total, bytes));
        })),
        last,
    )
}

#[test]
fn builds_pcm_cache_and_manifest() {
    let dir = Scratch::new("cache-build");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    // Two regions on one sample → exactly one .pcm entry.
    let sfz = "<region> sample=sample.wav lokey=0 hikey=127 pitch_keycenter=60\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let (cb, last) = recording_progress();
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = build_bank_cache(&dir.0, None, &cb, &cancel).unwrap();

    assert_eq!(outcome.samples, 1);
    assert_eq!(outcome.failures, 0);
    // 4800 mono frames → 64-byte header + 4800*2 = 9664 bytes.
    assert_eq!(outcome.total_bytes, 64 + 4800 * 2);

    // Progress was reported to completion.
    let (done, total, _bytes) = last.lock().unwrap().expect("progress called");
    assert_eq!(done, 1);
    assert_eq!(total, 1);

    // The manifest and cache file exist.
    let cache_dir = dir.join("cache");
    let manifest_path = cache_dir.join("manifest.json");
    assert!(manifest_path.is_file(), "manifest.json written");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(parsed["format_version"], 1);
    assert_eq!(parsed["entries"].as_array().unwrap().len(), 1);
    let entry = &parsed["entries"][0];
    assert_eq!(entry["sample_rate"], RATE);
    assert_eq!(entry["channels"], 1);
    assert_eq!(entry["frames"], 4800);
    assert_eq!(entry["size"], 64 + 4800 * 2);
    let cache_name = entry["cache"].as_str().unwrap();
    assert!(cache_dir.join(cache_name).is_file(), ".pcm written");
}

#[test]
fn cache_build_failure_is_reported_not_fatal() {
    // A bank whose only sample is missing: decode fails for that sample, so the
    // build returns Ok with `failures == 1` rather than Err (the cache is
    // derived data and the bank remains playable via on-demand decode).
    let dir = Scratch::new("cache-missing");
    let sfz = "<region> sample=absent.wav\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let cb: ProgressCb = Box::new(Mutex::new(|_: u64, _: u64, _: u64| {}));
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = build_bank_cache(&dir.0, None, &cb, &cancel).unwrap();
    assert_eq!(outcome.samples, 0);
    assert_eq!(outcome.failures, 1);
    assert_eq!(outcome.total_bytes, 0);
}

#[test]
fn cache_build_can_be_cancelled() {
    let dir = Scratch::new("cache-cancel");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    let sfz = "<region> sample=sample.wav lokey=0 hikey=127\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let cb: ProgressCb = Box::new(Mutex::new(|_: u64, _: u64, _: u64| {}));
    let cancel = Arc::new(AtomicBool::new(true));
    let err = build_bank_cache(&dir.0, None, &cb, &cancel).unwrap_err();
    assert!(matches!(err, CacheBuildError::Cancelled));
}

#[test]
fn cache_build_without_sfz_is_an_error() {
    let dir = Scratch::new("cache-nosfz");
    let cb: ProgressCb = Box::new(Mutex::new(|_: u64, _: u64, _: u64| {}));
    let cancel = Arc::new(AtomicBool::new(false));
    let err = build_bank_cache(&dir.0, None, &cb, &cancel).unwrap_err();
    assert!(matches!(err, CacheBuildError::Load(_)), "got {err:?}");
}
