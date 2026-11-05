//! PCM-cache integrity verification and auto-rebuild (RFC-002 §6, SPEC-070).
//!
//! Before trusting `cache/*.pcm`, the loader verifies the cache against
//! `cache/manifest.json` at four levels:
//!
//! * **L0** (always, synchronous) — manifest `format_version`, file existence,
//!   exact size, and the 64-byte `.pcm` header (magic, version, CRC-32,
//!   sample rate / channels / frames).
//! * **L1** (always, background) — source `mtime` plus a sampled xxh3-64 over
//!   the first / middle / last 64 KiB blocks of the `.pcm`.
//! * **L2** (background, once per session and after a crash via `cache/.dirty`)
//!   — full xxh3-128 of every `.pcm`.
//! * **L3** (on demand / after a failure) — blake3 of the `.pcm` plus xxh3-64
//!   of the source FLAC.
//!
//! A file that fails is quarantined (renamed `*.quarantined`) and, when its
//! source FLAC is intact, re-decoded in the background. Everything here runs on
//! the control / loader threads only — never the audio path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::bank_cache::{
    MANIFEST_FORMAT_VERSION, ManifestEntry, PCM_HEADER_LEN, PcmHeader, cache_dir, manifest_path,
    read_manifest, sample_blocks_xxh3_64_file,
};

/// The dirty marker written before a cache build and removed once the manifest
/// is clean, so a crash mid-build triggers a full L2 check next load.
pub const DIRTY_FILE_NAME: &str = ".dirty";

/// How thoroughly the cache is verified (RFC-002 §6.1). Each level is a
/// superset of the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IntegrityLevel {
    /// Manifest, existence, size, header (fast, blocks the load).
    L0,
    /// L0 + mtime + sampled hash (background).
    L1,
    /// L1 + full xxh3-128 (background).
    L2,
    /// L2 + blake3 of the PCM and xxh3-64 of the source (on demand).
    L3,
}

/// Why a single cache entry failed verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// The `.pcm` file does not exist.
    Missing,
    /// The `.pcm` file size differs from `64 + frames × channels × 2`.
    SizeMismatch,
    /// The header is corrupt (bad magic / version / CRC).
    BadHeader,
    /// A header field disagrees with the manifest (rate / channels / frames).
    HeaderFieldMismatch,
    /// The sampled-block hash disagrees (L1).
    SampledHashMismatch,
    /// The full xxh3-128 disagrees (L2).
    FullHashMismatch,
    /// The blake3 disagrees (L3).
    Blake3Mismatch,
    /// The source file's mtime changed or its xxh3 disagrees (L3).
    SourceMismatch,
}

impl IssueKind {
    /// A short human-readable label used in logs and the report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::SizeMismatch => "size-mismatch",
            Self::BadHeader => "bad-header",
            Self::HeaderFieldMismatch => "header-field-mismatch",
            Self::SampledHashMismatch => "sampled-hash-mismatch",
            Self::FullHashMismatch => "full-hash-mismatch",
            Self::Blake3Mismatch => "blake3-mismatch",
            Self::SourceMismatch => "source-mismatch",
        }
    }
}

/// One cache entry that failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityIssue {
    /// The entry's `sample_id`.
    pub sample_id: u32,
    /// Source path (as recorded in the manifest).
    pub src: String,
    /// Cache file name (relative to `cache/`).
    pub cache: String,
    /// What failed.
    pub kind: IssueKind,
    /// Human-readable detail (expected vs actual, or the underlying error).
    pub reason: String,
    /// Whether the source file is present and can be re-decoded.
    pub recoverable: bool,
}

/// The result of [`verify_bank_cache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// The level actually run.
    pub level: IntegrityLevel,
    /// Number of entries checked.
    pub checked: usize,
    /// Number of entries that passed.
    pub ok: usize,
    /// Entries that failed (quarantined by the caller).
    pub issues: Vec<IntegrityIssue>,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

impl IntegrityReport {
    /// Whether any entry failed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Verifies the cache of the bank in `dir` up to `level`, returning a report of
/// every entry that failed. `cancel` is polled between entries so a long L2/L3
/// scan can be aborted.
///
/// This reads files and runs only on the control / loader threads.
///
/// # Errors
///
/// A human-readable reason when the cache cannot be verified at all: the
/// manifest is missing/unreadable (no cache) or its `format_version` is
/// incompatible (a full rebuild is required). Per-entry failures are *not*
/// errors — they are collected in [`IntegrityReport::issues`].
pub fn verify_bank_cache(
    dir: &Path,
    level: IntegrityLevel,
    cancel: &Arc<AtomicBool>,
) -> Result<IntegrityReport, String> {
    let t = std::time::Instant::now();
    let mpath = manifest_path(dir);
    let manifest = read_manifest(&mpath).map_err(|e| format!("cache not verifiable: {e}"))?;
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(format!(
            "cache format_version {} != {MANIFEST_FORMAT_VERSION}: full rebuild required",
            manifest.format_version
        ));
    }

    let cdir = cache_dir(dir);
    let mut issues = Vec::new();
    let mut checked = 0usize;

    for entry in &manifest.entries {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        checked += 1;
        verify_entry(dir, &cdir, entry, level, &mut issues);
    }

    Ok(IntegrityReport {
        level,
        checked,
        ok: checked.saturating_sub(issues.len()),
        issues,
        elapsed_ms: t.elapsed().as_millis() as u64,
    })
}

/// Checks one manifest entry at `level`, appending any failure to `issues`.
fn verify_entry(
    dir: &Path,
    cdir: &Path,
    entry: &ManifestEntry,
    level: IntegrityLevel,
    issues: &mut Vec<IntegrityIssue>,
) {
    let path = cdir.join(&entry.cache);
    let recoverable = source_path(dir, &entry.src).is_some_and(|p| p.is_file());

    let push = |issues: &mut Vec<IntegrityIssue>, kind: IssueKind, reason: String| {
        issues.push(IntegrityIssue {
            sample_id: entry.sample_id,
            src: entry.src.clone(),
            cache: entry.cache.clone(),
            kind,
            reason,
            recoverable,
        });
    };

    // L0: existence + size + header.
    let expected_size = PCM_HEADER_LEN as u64
        + entry
            .frames
            .saturating_mul(u64::from(entry.channels))
            .saturating_mul(2);
    let Ok(meta) = std::fs::metadata(&path) else {
        push(
            issues,
            IssueKind::Missing,
            format!("cache file {} does not exist", path.display()),
        );
        return;
    };
    if meta.len() != expected_size {
        push(
            issues,
            IssueKind::SizeMismatch,
            format!("expected {expected_size} bytes, found {}", meta.len()),
        );
        return;
    }
    let header_bytes = match read_header(&path) {
        Ok(b) => b,
        Err(e) => {
            push(issues, IssueKind::BadHeader, e);
            return;
        }
    };
    let Some(header) = PcmHeader::decode(&header_bytes) else {
        push(
            issues,
            IssueKind::BadHeader,
            "corrupt SPCM header".to_owned(),
        );
        return;
    };
    if header.sample_rate != entry.sample_rate
        || header.channels != entry.channels
        || header.frames != entry.frames
    {
        push(
            issues,
            IssueKind::HeaderFieldMismatch,
            format!(
                "header rate/ch/frames {}/{}/{} != manifest {}/{}/{}",
                header.sample_rate,
                header.channels,
                header.frames,
                entry.sample_rate,
                entry.channels,
                entry.frames
            ),
        );
        return;
    }

    // L1: source mtime + sampled hash.
    if level >= IntegrityLevel::L1 {
        if let Some(mtime) = entry_mtime_matches(dir, entry) {
            push(issues, IssueKind::SourceMismatch, mtime);
            return;
        }
        if let Some(sampled) = entry.sampled_xxh3.as_deref() {
            match sample_blocks_xxh3_64_file(&path) {
                Ok((actual, _)) if actual == sampled => {}
                Ok((actual, _)) => push(
                    issues,
                    IssueKind::SampledHashMismatch,
                    format!("sampled xxh3 expected {sampled}, found {actual}"),
                ),
                Err(e) => push(issues, IssueKind::SampledHashMismatch, e),
            }
        }
    }

    // L2: full xxh3-128.
    if level >= IntegrityLevel::L2 {
        match full_xxh3_128_file(&path) {
            Ok(actual) if actual == entry.xxh3_128 => {}
            Ok(actual) => push(
                issues,
                IssueKind::FullHashMismatch,
                format!("xxh3-128 expected {}, found {actual}", entry.xxh3_128),
            ),
            Err(e) => push(issues, IssueKind::FullHashMismatch, e),
        }
    }

    // L3: blake3 of PCM + xxh3-64 of source.
    if level >= IntegrityLevel::L3 {
        if let Some(expected) = entry.blake3.as_deref() {
            match full_blake3_file(&path) {
                Ok(actual) if actual == expected => {}
                Ok(actual) => push(
                    issues,
                    IssueKind::Blake3Mismatch,
                    format!("blake3 expected {expected}, found {actual}"),
                ),
                Err(e) => push(issues, IssueKind::Blake3Mismatch, e),
            }
        }
        if let Some(src_path) = source_path(dir, &entry.src) {
            match crate::bank_cache::xxh3_64_file(&src_path) {
                Ok(actual) if actual == entry.src_xxh3 => {}
                Ok(actual) => push(
                    issues,
                    IssueKind::SourceMismatch,
                    format!("source xxh3 expected {}, found {actual}", entry.src_xxh3),
                ),
                Err(e) => push(issues, IssueKind::SourceMismatch, e),
            }
        }
    }
}

/// Reads the 64-byte header of a `.pcm` file.
fn read_header(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut buf = vec![0u8; PCM_HEADER_LEN];
    f.read_exact(&mut buf)
        .map_err(|e| format!("read header {}: {e}", path.display()))?;
    Ok(buf)
}

/// Streaming xxh3-128 of a file (never loads the whole file into memory).
fn full_xxh3_128_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    use xxhash_rust::xxh3::Xxh3;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut h = Xxh3::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:032x}", h.digest128()))
}

/// Streaming blake3 of a file.
fn full_blake3_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut h = blake3::Hasher::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Returns a source-mismatch reason when the source file's mtime differs from
/// the manifest, or `None` when it matches (or cannot be stat-ed).
fn entry_mtime_matches(dir: &Path, entry: &ManifestEntry) -> Option<String> {
    let src = source_path(dir, &entry.src)?;
    let meta = crate::bank_cache::source_meta(&src)?;
    if meta.mtime_ns != entry.mtime_ns {
        return Some(format!(
            "source mtime {} != manifest {}",
            meta.mtime_ns, entry.mtime_ns
        ));
    }
    None
}

/// Resolves a manifest `src` string (which may be absolute or relative) against
/// the bank root `dir`, confining it so it cannot escape the bank.
fn source_path(dir: &Path, src: &str) -> Option<PathBuf> {
    let p = Path::new(src);
    if p.is_absolute() {
        // An absolute path is accepted only if it lives under `dir`.
        let rel = p.strip_prefix(dir).ok()?;
        let joined = dir.join(rel);
        return Some(joined);
    }
    crate::sample_bank::confine_path(dir, src)
}

/// Marks the cache dirty (before a build) or clean (after), and reports whether
/// the marker is present (used to force a full L2 check after a crash).
#[must_use]
pub fn dirty_marker_path(bank_dir: &Path) -> PathBuf {
    cache_dir(bank_dir).join(DIRTY_FILE_NAME)
}

/// Creates the `cache/.dirty` marker. Called before a cache build starts.
pub fn mark_dirty(bank_dir: &Path) {
    let p = dirty_marker_path(bank_dir);
    if let Some(parent) = p.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        crate::log_warn!("mark_dirty {}: {e}", parent.display());
        return;
    }
    if let Err(e) = std::fs::write(&p, b"") {
        crate::log_warn!("mark_dirty {}: {e}", p.display());
    }
}

/// Removes the `cache/.dirty` marker. Called after a cache build finishes
/// cleanly.
pub fn clear_dirty(bank_dir: &Path) {
    let p = dirty_marker_path(bank_dir);
    let _ = std::fs::remove_file(&p);
}

/// Whether the `cache/.dirty` marker is present (a previous build was
/// interrupted).
#[must_use]
pub fn is_dirty(bank_dir: &Path) -> bool {
    dirty_marker_path(bank_dir).exists()
}

/// Renames a quarantined `.pcm` aside so it cannot be played, returning the
/// quarantined path. The rebuild overwrites the original name.
pub fn quarantine(cdir: &Path, cache_name: &str) {
    let path = cdir.join(cache_name);
    let q = cdir.join(format!("{cache_name}.quarantined"));
    if let Err(e) = std::fs::rename(&path, &q) {
        crate::log_warn!("quarantine {} -> {}: {e}", path.display(), q.display());
    }
}

/// The result of [`recover_bank_cache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverReport {
    /// Number of entries rebuilt from their source.
    pub recovered: usize,
    /// Number of entries that could not be rebuilt (source missing/mismatch).
    pub unrecoverable: usize,
}

/// Re-decodes the source of every entry in `issues` into `cache/*.pcm` and
/// updates the manifest (RFC-002 §6.3). Runs on 1–2 low-priority worker threads.
///
/// An entry whose source is missing, or whose source xxh3 no longer matches
/// the manifest, is left quarantined and counted as unrecoverable (the UI then
/// offers "download the bank again").
///
/// # Errors
///
/// A human-readable reason when the manifest cannot be re-read or re-written.
pub fn recover_bank_cache(
    dir: &Path,
    issues: &[IntegrityIssue],
    cancel: &Arc<AtomicBool>,
) -> Result<RecoverReport, String> {
    let bank_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let mpath = manifest_path(dir);
    let mut manifest = read_manifest(&mpath)?;
    let cdir = cache_dir(dir);

    let mut recovered = 0usize;
    let mut unrecoverable = 0usize;

    for issue in issues {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let Some(src) = source_path(dir, &issue.src) else {
            crate::log_warn!(
                "bank {bank_id}: cannot resolve source {} for {}: unrecoverable",
                issue.src,
                issue.cache
            );
            unrecoverable += 1;
            continue;
        };
        if !src.is_file() {
            crate::log_warn!(
                "bank {bank_id}: source {} missing for {}: unrecoverable",
                src.display(),
                issue.cache
            );
            unrecoverable += 1;
            continue;
        }

        quarantine(&cdir, &issue.cache);

        match crate::sample_bank::build_one_pcm(&src, issue.sample_id as usize, &cdir) {
            Ok(entry) => {
                if let Some(slot) = manifest
                    .entries
                    .iter_mut()
                    .find(|e| e.sample_id == issue.sample_id)
                {
                    *slot = entry;
                } else {
                    manifest.entries.push(entry);
                }
                recovered += 1;
            }
            Err(e) => {
                crate::log_error!("bank {bank_id}: rebuild {} failed: {e}", issue.cache);
                unrecoverable += 1;
            }
        }
    }

    manifest.total_bytes = manifest.entries.iter().map(|e| e.size).sum();
    crate::bank_cache::write_manifest(&mpath, &manifest)?;
    clear_dirty(dir);

    crate::log_info!(
        "bank {bank_id}: integrity recovery: {recovered} rebuilt, {unrecoverable} unrecoverable"
    );
    Ok(RecoverReport {
        recovered,
        unrecoverable,
    })
}
