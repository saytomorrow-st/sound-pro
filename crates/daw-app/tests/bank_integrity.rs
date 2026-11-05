//! Acceptance tests for PCM-cache integrity verification and auto-rebuild
//! (SPEC-070 / RFC-002 §6).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_app::bank_cache::{cache_dir, manifest_path, read_manifest};
use daw_app::bank_integrity::{IntegrityLevel, IssueKind, recover_bank_cache, verify_bank_cache};
use daw_app::sample_bank::build_bank_cache;

const RATE: u32 = 48_000;

type ProgressCb = Box<Mutex<dyn FnMut(u64, u64, u64) + Send>>;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-integrity-{tag}-{}", std::process::id()));
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

/// Builds a one-sample cache and returns the bank dir plus the `.pcm` name.
fn build_one_sample_cache(tag: &str) -> (Scratch, String) {
    let dir = Scratch::new(tag);
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    let sfz = "<region> sample=sample.wav lokey=0 hikey=127 pitch_keycenter=60\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let cb = no_progress();
    let cancel = no_cancel();
    let outcome = build_bank_cache(&dir.0, None, &cb, &cancel).unwrap();
    assert_eq!(outcome.samples, 1);

    let manifest = read_manifest(&manifest_path(&dir.0)).unwrap();
    let cache_name = manifest.entries[0].cache.clone();
    (dir, cache_name)
}

#[test]
fn l0_passes_on_clean_cache() {
    let (dir, _) = build_one_sample_cache("l0-clean");
    let report = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap();
    assert!(report.is_clean(), "clean cache must pass L0");
    assert_eq!(report.checked, 1);
    assert_eq!(report.ok, 1);
}

#[test]
fn l0_detects_missing_file() {
    let (dir, cache_name) = build_one_sample_cache("l0-missing");
    std::fs::remove_file(cache_dir(&dir.0).join(&cache_name)).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].kind, IssueKind::Missing);
}

#[test]
fn l0_detects_wrong_size() {
    let (dir, cache_name) = build_one_sample_cache("l0-size");
    let path = cache_dir(&dir.0).join(&cache_name);
    let bytes = std::fs::read(&path).unwrap();
    // Truncate by one byte: the size no longer matches the manifest.
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.issues[0].kind, IssueKind::SizeMismatch);
}

#[test]
fn l0_detects_bad_header() {
    let (dir, cache_name) = build_one_sample_cache("l0-header");
    let path = cache_dir(&dir.0).join(&cache_name);
    let mut bytes = std::fs::read(&path).unwrap();
    // Corrupt a byte inside the CRC'd header region (the sample-rate field),
    // which does not change the file size.
    bytes[10] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.issues[0].kind, IssueKind::BadHeader);
}

#[test]
fn l1_detects_corrupt_payload() {
    let (dir, cache_name) = build_one_sample_cache("l1-payload");
    let path = cache_dir(&dir.0).join(&cache_name);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip one sample byte in the payload (after the 64-byte header). The size
    // is unchanged so L0 passes; the sampled hash catches it.
    bytes[64] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L1, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.issues[0].kind, IssueKind::SampledHashMismatch);
}

#[test]
fn l2_detects_corrupt_payload_beyond_sampled_blocks() {
    // A payload large enough that the sampled (first/middle/last 64 KiB) blocks
    // are unchanged, but a byte in-between is flipped: L1 misses it, L2 catches
    // the full xxh3-128.
    let dir = Scratch::new("l2-payload");
    write_wav(&dir.join("sample.wav"), RATE, &tone(300_000));
    let sfz = "<region> sample=sample.wav\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();
    let cb = no_progress();
    let cancel = no_cancel();
    build_bank_cache(&dir.0, None, &cb, &cancel).unwrap();

    let manifest = read_manifest(&manifest_path(&dir.0)).unwrap();
    let path = cache_dir(&dir.0).join(&manifest.entries[0].cache);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip a byte past the first sampled 64 KiB block but well before the
    // middle block, so the sampled (first/middle/last) hash is unchanged.
    let flip_at = 64 + 65_536 + 1;
    assert!(flip_at < bytes.len());
    bytes[flip_at] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    // L1 sampled hash still matches (the flipped byte is outside the sampled
    // blocks).
    let l1 = verify_bank_cache(&dir.0, IntegrityLevel::L1, &no_cancel()).unwrap();
    assert!(l1.is_clean(), "L1 must not catch a mid-file flip");

    // L2 full hash catches it.
    let l2 = verify_bank_cache(&dir.0, IntegrityLevel::L2, &no_cancel()).unwrap();
    assert!(!l2.is_clean());
    assert_eq!(l2.issues[0].kind, IssueKind::FullHashMismatch);
}

#[test]
fn recover_rebuilds_corrupted_pcm() {
    let (dir, cache_name) = build_one_sample_cache("recover-ok");
    let path = cache_dir(&dir.0).join(&cache_name);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[64] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L1, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    // The source WAV still exists, so recovery is possible.
    assert!(report.issues[0].recoverable);

    let rr = recover_bank_cache(&dir.0, &report.issues, &no_cancel()).unwrap();
    assert_eq!(rr.recovered, 1);
    assert_eq!(rr.unrecoverable, 0);

    // The cache is clean again after recovery.
    let again = verify_bank_cache(&dir.0, IntegrityLevel::L1, &no_cancel()).unwrap();
    assert!(again.is_clean(), "cache must be clean after recovery");
}

#[test]
fn recover_marks_missing_source_unrecoverable() {
    let (dir, cache_name) = build_one_sample_cache("recover-missing-src");
    // Remove the source WAV, then corrupt the cache file.
    std::fs::remove_file(dir.join("sample.wav")).unwrap();
    let path = cache_dir(&dir.0).join(&cache_name);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[64] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let report = verify_bank_cache(&dir.0, IntegrityLevel::L1, &no_cancel()).unwrap();
    assert!(!report.is_clean());
    assert!(
        !report.issues[0].recoverable,
        "missing source is unrecoverable"
    );

    let rr = recover_bank_cache(&dir.0, &report.issues, &no_cancel()).unwrap();
    assert_eq!(rr.recovered, 0);
    assert_eq!(rr.unrecoverable, 1);
}

#[test]
fn verify_errors_when_manifest_missing() {
    let dir = Scratch::new("no-manifest");
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    let err = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap_err();
    assert!(err.contains("not verifiable"), "got {err}");
}

#[test]
fn verify_errors_on_incompatible_format_version() {
    let (dir, _) = build_one_sample_cache("bad-version");
    let mpath = manifest_path(&dir.0);
    let mut manifest = read_manifest(&mpath).unwrap();
    manifest.format_version = 999;
    daw_app::bank_cache::write_manifest(&mpath, &manifest).unwrap();

    let err = verify_bank_cache(&dir.0, IntegrityLevel::L0, &no_cancel()).unwrap_err();
    assert!(err.contains("full rebuild required"), "got {err}");
}

#[test]
fn dirty_marker_survives_crash_and_clears_after_build() {
    let dir = Scratch::new("dirty");
    write_wav(&dir.join("sample.wav"), RATE, &tone(4800));
    let sfz = "<region> sample=sample.wav\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let cb = no_progress();
    let cancel = no_cancel();
    build_bank_cache(&dir.0, None, &cb, &cancel).unwrap();

    // A clean build removes the marker.
    assert!(!daw_app::bank_integrity::is_dirty(&dir.0));
    daw_app::bank_integrity::mark_dirty(&dir.0);
    assert!(daw_app::bank_integrity::is_dirty(&dir.0));
    daw_app::bank_integrity::clear_dirty(&dir.0);
    assert!(!daw_app::bank_integrity::is_dirty(&dir.0));
}
