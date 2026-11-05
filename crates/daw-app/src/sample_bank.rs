//! Loading a local SFZ sample bank into memory (SPEC-064 / RFC-001 §3.3, §5).
//!
//! This is PR 3 of the sampler slice: it reads the `.sfz` text, resolves and
//! decodes each region's sample (FLAC/WAV/OGG/MP3 through the SPEC-053
//! symphonia decoder), converts it to interleaved `i16`, and produces the
//! [`audio_core::SampleRegion`] list plus the resident `SampleSource` data that
//! [`audio_core::SamplerEngine`] consumes. Loading and decoding happen on the
//! control thread only (I/O and allocation are allowed here); nothing here is
//! ever called from the audio callback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use audio_core::sampler::region::resolve;
use audio_core::sampler::sfz::{IncludeResolver, parse};

/// Default RAM budget for one bank, in MiB (RFC-001 §3.3).
///
/// Osiris (multi-mic, multi-velocity, round-robin, sustain + noise) decodes to
/// ~2.6 GiB of 16-bit PCM; 3072 MiB leaves headroom while still bounding a
/// single bank's memory footprint.
pub const DEFAULT_RAM_BUDGET_MIB: u64 = 3072;

/// Why a bank could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum BankLoadError {
    /// The directory has no `.sfz` file.
    #[error("no .sfz file found in {0}")]
    NoSfz(PathBuf),
    /// The `.sfz` text could not be read.
    #[error("cannot read {0}: {1}")]
    ReadSfz(PathBuf, String),
    /// The `.sfz` text could not be parsed.
    #[error("cannot parse {0}: {1}")]
    Sfz(PathBuf, String),
    /// The decoded samples exceed the configured RAM budget.
    #[error("bank needs {needed_mib} MiB, budget is {budget_mib} MiB")]
    RamBudgetExceeded {
        /// Configured budget, in MiB.
        budget_mib: u64,
        /// What the bank actually needs, in MiB.
        needed_mib: u64,
    },
    /// The RAM budget was zero.
    #[error("RAM budget must be positive")]
    InvalidBudget,
    /// The bank has no usable PCM cache (`cache/manifest.json` is missing or
    /// its format version is incompatible). The caller should build the cache
    /// (or fall back to on-demand FLAC decoding).
    #[error("no usable PCM cache: {0}")]
    NoCache(String),
}

/// A sample the loader could not decode, reported without aborting the bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionFailure {
    /// The `sample` path as written in the `.sfz`.
    pub sample: String,
    /// Why it could not be loaded or decoded.
    pub reason: String,
}

/// A handle describing the on-disk PCM cache a [`LoadedBank`] was read from
/// (RFC-002 §8). The cache lives outside RAM; the handle is only metadata so
/// the caller can report it and the loader can re-open files lazily later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheHandle {
    /// The bank id (e.g. `"osiris"`).
    pub bank_id: String,
    /// Absolute path to the `cache/` directory.
    pub cache_dir: PathBuf,
    /// `Σ entries[].size` — the total `.pcm` bytes on disk.
    pub total_bytes: u64,
    /// Number of cached samples.
    pub samples: usize,
    /// The manifest `format_version`.
    pub format_version: u32,
}

/// A fully loaded bank: resolved regions plus their decoded `i16` samples,
/// ready for [`audio_core::SamplerEngine::new`].
#[derive(Debug)]
pub struct LoadedBank {
    /// Resolved, playable regions (regions with missing samples are omitted).
    pub regions: Vec<audio_core::SampleRegion>,
    /// Decoded samples, indexed by [`audio_core::SampleRegion::sample_index`].
    /// Mono samples are one resident `i16` buffer; stereo samples are
    /// interleaved `[L, R, L, R, …]`.
    pub samples: Vec<audio_core::SampleSource>,
    /// Non-fatal warnings: parser warnings plus loader notes (extra `.sfz`,
    /// truncated regions, …).
    pub warnings: Vec<String>,
    /// Regions whose sample could not be loaded or decoded.
    pub decode_failures: Vec<RegionFailure>,
    /// Actual bytes of sample data held in RAM (resident data only).
    pub ram_bytes: u64,
    /// The PCM cache this bank was read from, when it was loaded from cache
    /// rather than decoded from FLAC/WAV.
    pub cache: Option<CacheHandle>,
    /// For each sample (same index as [`Self::samples`]), the `.pcm` file its
    /// streamed tail is read from (RFC-002 stage 6); `None` for resident
    /// samples. Empty when nothing streams.
    pub stream_files: Vec<Option<PathBuf>>,
}

impl LoadedBank {
    /// Whether any sample is [`audio_core::SampleSource::Streaming`].
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.stream_files.iter().any(Option::is_some)
    }
}

/// Frames of each streamed sample kept resident as its head in Streaming
/// mode (RFC-002 §7): 32 768 frames ≈ 0.68 s at 48 kHz, ~155 MiB for Osiris.
pub const STREAM_HEAD_FRAMES: u64 = 32_768;

/// A filesystem-backed [`IncludeResolver`] rooted at the bank directory.
///
/// `#include`d paths are resolved relative to `root` and confined to it; the
/// parser already rejects absolute paths and `..` (`SfzError::UnsafePath`),
/// but we re-check here and normalise `\` → `/` (defense in depth, and so a
/// Windows-authored SFZ works on Linux and vice versa).
struct DirResolver {
    root: PathBuf,
    /// Folder of the main `.sfz` relative to `root` (`/`-separated, may be
    /// empty); includes and samples are relative to the `.sfz`, not the root.
    base: String,
}

impl IncludeResolver for DirResolver {
    fn resolve(&mut self, path: &str) -> Result<String, String> {
        let joined = join_rel(&self.base, path);
        let full = confine_path(&self.root, &joined)
            .ok_or_else(|| format!("unsafe include path: {path}"))?;
        std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))
    }
}

/// Loads the SFZ bank in `dir`, decoding every sample into `i16` PCM.
///
/// The `.sfz` is found by [`find_sfz`] (searched recursively). `budget_mib`
/// is the RAM budget for the decoded samples; exceeding it is
/// [`BankLoadError::RamBudgetExceeded`] and the whole bank is refused rather
/// than risking an OOM. A region whose sample is missing or undecodable is
/// recorded in [`LoadedBank::decode_failures`] and skipped, so a single broken
/// region does not fail the whole bank.
///
/// Control thread only: this reads files and allocates.
///
/// # Errors
///
/// [`BankLoadError`].
pub fn load_bank(dir: &Path, budget_mib: u64) -> Result<LoadedBank, BankLoadError> {
    load_bank_with_hint(dir, budget_mib, None)
}

/// [`load_bank`] preferring the catalog's `sfz` path `hint` (relative to
/// `dir`, `/`-separated). A hint that is unsafe or names no file falls back to
/// the recursive search, so a bank installed by an older version still loads.
///
/// Logs timings and the outcome of every attempt via [`crate::logging`].
///
/// # Errors
///
/// [`BankLoadError`].
pub fn load_bank_with_hint(
    dir: &Path,
    budget_mib: u64,
    hint: Option<&str>,
) -> Result<LoadedBank, BankLoadError> {
    let bank_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let t_total = std::time::Instant::now();
    crate::log_info!("bank {bank_id}: load start ({})", dir.display());
    match load_bank_with_hint_impl(dir, budget_mib, hint, &bank_id) {
        Ok(bank) => {
            let ram_mib = bank.ram_bytes.div_ceil(1024 * 1024);
            crate::log_info!(
                "bank {bank_id}: loaded in {} ms: {} regions, {} samples, {ram_mib} MiB, {} decode failures",
                t_total.elapsed().as_millis(),
                bank.regions.len(),
                bank.samples.len(),
                bank.decode_failures.len()
            );
            for f in bank.decode_failures.iter().take(20) {
                crate::log_warn!("bank {bank_id}: decode failure {}: {}", f.sample, f.reason);
            }
            for w in bank.warnings.iter().take(20) {
                crate::log_debug!("bank {bank_id}: sfz warning {w}");
            }
            Ok(bank)
        }
        Err(e) => {
            crate::log_error!(
                "bank {bank_id}: load failed after {} ms: {e}",
                t_total.elapsed().as_millis()
            );
            Err(e)
        }
    }
}

/// Actual bank loading logic for [`load_bank_with_hint`]; the public wrapper
/// adds the timing/outcome logging.
fn load_bank_with_hint_impl(
    dir: &Path,
    budget_mib: u64,
    hint: Option<&str>,
    bank_id: &str,
) -> Result<LoadedBank, BankLoadError> {
    if budget_mib == 0 {
        return Err(BankLoadError::InvalidBudget);
    }

    let t_start = std::time::Instant::now();
    let sfz = find_sfz(dir, hint)?;
    let base = sfz
        .parent()
        .and_then(|p| p.strip_prefix(dir).ok())
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    let text = std::fs::read_to_string(&sfz)
        .map_err(|e| BankLoadError::ReadSfz(sfz.clone(), e.to_string()))?;

    let mut resolver = DirResolver {
        root: dir.to_path_buf(),
        base: base.clone(),
    };
    let doc =
        parse(&text, &mut resolver).map_err(|e| BankLoadError::Sfz(sfz.clone(), e.to_string()))?;

    let warnings: Vec<String> = doc
        .warnings
        .iter()
        .map(|w| format!("byte {}: {}", w.at, w.message))
        .collect();

    let budget_bytes = budget_mib.saturating_mul(1024 * 1024);
    let mut regions: Vec<audio_core::SampleRegion> = Vec::new();
    let mut samples: Vec<audio_core::SampleSource> = Vec::new();
    let mut ram_bytes: u64 = 0;

    // Resolve every region's path; unique files are decoded once (velocity
    // layers, round robins and release groups share files), in first-seen order.
    let (unique, plan, mut decode_failures) = resolve_unique_samples(dir, &doc, &base);
    crate::log_info!(
        "bank {bank_id}: sfz read+parse {} ms ({} regions, {} unique samples)",
        t_start.elapsed().as_millis(),
        doc.regions.len(),
        unique.len()
    );

    let t_decode = std::time::Instant::now();
    let mut decoded = decode_all(&unique, budget_bytes, &bank_id);
    crate::log_info!(
        "bank {bank_id}: decode {} ms",
        t_decode.elapsed().as_millis()
    );

    // Assemble in region order so the result is independent of thread timing.
    let mut slot_of: Vec<Option<(usize, u8)>> = vec![None; unique.len()];
    let mut slot_failure: Vec<Option<String>> = vec![None; unique.len()];
    for (region, idx) in doc.regions.iter().zip(&plan) {
        let Some(idx) = *idx else { continue };
        let sample_path = region.sample.as_deref().unwrap_or_default();
        if slot_of[idx].is_none() && slot_failure[idx].is_none() {
            match decoded[idx].take() {
                Some(Ok((data, channels))) if !data.is_empty() => {
                    let bytes = (data.len() as u64).saturating_mul(2);
                    if ram_bytes.saturating_add(bytes) > budget_bytes {
                        let needed_mib = ram_bytes.saturating_add(bytes).div_ceil(1024 * 1024);
                        return Err(BankLoadError::RamBudgetExceeded {
                            budget_mib,
                            needed_mib,
                        });
                    }
                    ram_bytes += bytes;
                    slot_of[idx] = Some((samples.len(), channels));
                    samples.push(audio_core::SampleSource::resident(Arc::from(data)));
                }
                Some(Ok(_)) => slot_failure[idx] = Some("sample is empty".to_owned()),
                Some(Err(reason)) => slot_failure[idx] = Some(reason),
                None => {
                    return Err(BankLoadError::RamBudgetExceeded {
                        budget_mib,
                        needed_mib: ram_bytes.div_ceil(1024 * 1024).saturating_add(1),
                    });
                }
            }
        }
        if let Some((sample_index, channels)) = slot_of[idx] {
            if let Some(r) = resolve(region, 48_000, sample_index, channels) {
                regions.push(r);
            }
        } else if let Some(reason) = &slot_failure[idx] {
            decode_failures.push(RegionFailure {
                sample: sample_path.to_owned(),
                reason: reason.clone(),
            });
        }
    }

    Ok(LoadedBank {
        regions,
        samples,
        warnings,
        decode_failures,
        ram_bytes,
        cache: None,
        stream_files: Vec::new(),
    })
}

/// Loads a bank from its prebuilt PCM cache (RFC-002 §8 step 2), reading each
/// `cache/*.pcm` into [`audio_core::SampleSource::Resident`] without decoding
/// FLAC. This is the fast, low-RAM path: only the resident `i16` data is held
/// in memory (Salamander ~1.1 GiB, Osiris ~2.6 GiB), and the loader honours the
/// RAM budget.
///
/// Before reading, the cache is verified at **L0** (RFC-002 §6): manifest
/// `format_version`, file existence, exact size, and the `SPCM` header. Any
/// failing entry is quarantined and re-decoded from its source via
/// [`crate::bank_integrity::recover_bank_cache`]; an entry whose source is
/// missing is skipped into [`LoadedBank::decode_failures`]. L1/L2 run in the
/// background, started by the caller ([`spawn_background_verify`]).
///
/// Control thread only: this reads files and allocates.
///
/// # Errors
///
/// [`BankLoadError::NoCache`] when there is no cache to load (missing manifest
/// or incompatible `format_version`); [`BankLoadError::RamBudgetExceeded`] when
/// the resident data exceeds `budget_mib`; other [`BankLoadError`] on SFZ or
/// decode problems.
pub fn load_bank_from_cache(
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
) -> Result<LoadedBank, BankLoadError> {
    load_bank_from_cache_mode(dir, hint, budget_mib, false)
}

/// Loads a bank from its PCM cache in **Streaming** mode (RFC-002 §3, stage
/// 6): every stereo sample longer than [`STREAM_HEAD_FRAMES`] keeps only its
/// head resident ([`audio_core::SampleSource::Streaming`]) and records its
/// `.pcm` in [`LoadedBank::stream_files`]; the tail is read by
/// [`crate::stream_loader::StreamLoader`]. Mono and short samples stay
/// resident. `budget_mib` bounds the resident data (heads included).
///
/// Control thread only.
///
/// # Errors
///
/// As [`load_bank_from_cache`].
pub fn load_bank_from_cache_streaming(
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
) -> Result<LoadedBank, BankLoadError> {
    load_bank_from_cache_mode(dir, hint, budget_mib, true)
}

#[allow(clippy::too_many_lines)]
fn load_bank_from_cache_mode(
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
    streaming: bool,
) -> Result<LoadedBank, BankLoadError> {
    if budget_mib == 0 {
        return Err(BankLoadError::InvalidBudget);
    }
    let bank_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());

    // L0: verify the cache synchronously. A missing/incompatible manifest is a
    // "no cache" condition the caller turns into a background build.
    let cancel = Arc::new(AtomicBool::new(false));
    let report = crate::bank_integrity::verify_bank_cache(
        dir,
        crate::bank_integrity::IntegrityLevel::L0,
        &cancel,
    )
    .map_err(BankLoadError::NoCache)?;
    if !report.issues.is_empty() {
        crate::log_warn!(
            "bank {bank_id}: {}/{} cached samples failed L0, rebuilding",
            report.issues.len(),
            report.checked
        );
        let recover = crate::bank_integrity::recover_bank_cache(dir, &report.issues, &cancel)
            .map_err(|e| BankLoadError::NoCache(format!("recover failed: {e}")))?;
        if recover.unrecoverable > 0 {
            crate::log_warn!(
                "bank {bank_id}: {} cached samples unrecoverable (source missing)",
                recover.unrecoverable
            );
        }
    }

    // Re-read the manifest and resolve the SFZ/regions exactly as the FLAC
    // loader does, so `sample_index` and ordering match bit-for-bit.
    let manifest = crate::bank_cache::read_manifest(&crate::bank_cache::manifest_path(dir))
        .map_err(|e| BankLoadError::NoCache(format!("manifest unreadable after verify: {e}")))?;

    let sfz = find_sfz(dir, hint)?;
    let base = sfz
        .parent()
        .and_then(|p| p.strip_prefix(dir).ok())
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    let text = std::fs::read_to_string(&sfz)
        .map_err(|e| BankLoadError::ReadSfz(sfz.clone(), e.to_string()))?;
    let mut resolver = DirResolver {
        root: dir.to_path_buf(),
        base: base.clone(),
    };
    let doc =
        parse(&text, &mut resolver).map_err(|e| BankLoadError::Sfz(sfz.clone(), e.to_string()))?;

    let warnings: Vec<String> = doc
        .warnings
        .iter()
        .map(|w| format!("byte {}: {}", w.at, w.message))
        .collect();

    let (unique, plan, mut decode_failures) = resolve_unique_samples(dir, &doc, &base);

    // Map each manifest entry by its stable `src` key (RFC-002 §5.2), so a
    // changed `sample_id` ordering still resolves by path.
    let entry_by_src: HashMap<String, crate::bank_cache::ManifestEntry> = manifest
        .entries
        .into_iter()
        .map(|e| (e.src.clone(), e))
        .collect();

    let budget_bytes = budget_mib.saturating_mul(1024 * 1024);
    let cdir = crate::bank_cache::cache_dir(dir);
    let mut regions: Vec<audio_core::SampleRegion> = Vec::new();
    let mut samples: Vec<audio_core::SampleSource> = Vec::new();
    let mut ram_bytes: u64 = 0;
    let mut slot_of: Vec<Option<(usize, u8)>> = vec![None; unique.len()];
    let mut slot_failure: Vec<Option<String>> = vec![None; unique.len()];
    let mut stream_files: Vec<Option<PathBuf>> = Vec::new();

    for (region, idx) in doc.regions.iter().zip(&plan) {
        let Some(idx) = *idx else { continue };
        let sample_path = region.sample.as_deref().unwrap_or_default();
        if slot_of[idx].is_none() && slot_failure[idx].is_none() {
            let src = unique[idx].to_string_lossy().into_owned();
            match entry_by_src.get(&src) {
                Some(entry) => {
                    let pcm_path = cdir.join(&entry.cache);
                    let stream =
                        streaming && entry.channels == 2 && entry.frames > STREAM_HEAD_FRAMES;
                    let read = if stream {
                        crate::bank_cache::read_pcm_head(&pcm_path, STREAM_HEAD_FRAMES)
                    } else {
                        crate::bank_cache::read_pcm_file(&pcm_path)
                    };
                    match read {
                        Ok((header, data)) if stream && !data.is_empty() => {
                            let bytes = (data.len() as u64).saturating_mul(2);
                            if ram_bytes.saturating_add(bytes) > budget_bytes {
                                let needed_mib =
                                    ram_bytes.saturating_add(bytes).div_ceil(1024 * 1024);
                                return Err(BankLoadError::RamBudgetExceeded {
                                    budget_mib,
                                    needed_mib,
                                });
                            }
                            ram_bytes += bytes;
                            let sample_index = samples.len();
                            slot_of[idx] = Some((sample_index, 2));
                            let file_id = u32::try_from(sample_index).unwrap_or(u32::MAX);
                            let head = audio_core::SampleHead::new(
                                Arc::from(data),
                                header.frames,
                                2,
                                file_id,
                            );
                            samples.push(audio_core::SampleSource::streaming(head));
                            stream_files.resize(sample_index, None);
                            stream_files.push(Some(pcm_path));
                        }
                        Ok((_, data)) if !data.is_empty() => {
                            let bytes = (data.len() as u64).saturating_mul(2);
                            if ram_bytes.saturating_add(bytes) > budget_bytes {
                                let needed_mib =
                                    ram_bytes.saturating_add(bytes).div_ceil(1024 * 1024);
                                return Err(BankLoadError::RamBudgetExceeded {
                                    budget_mib,
                                    needed_mib,
                                });
                            }
                            ram_bytes += bytes;
                            let channels = entry.channels as u8;
                            slot_of[idx] = Some((samples.len(), channels));
                            samples.push(audio_core::SampleSource::resident(Arc::from(data)));
                        }
                        Ok(_) => slot_failure[idx] = Some("sample is empty".to_owned()),
                        Err(e) => slot_failure[idx] = Some(e),
                    }
                }
                None => slot_failure[idx] = Some("no cache entry".to_owned()),
            }
        }
        if let Some((sample_index, channels)) = slot_of[idx] {
            if let Some(r) = resolve(region, 48_000, sample_index, channels) {
                regions.push(r);
            }
        } else if let Some(reason) = &slot_failure[idx] {
            decode_failures.push(RegionFailure {
                sample: sample_path.to_owned(),
                reason: reason.clone(),
            });
        }
    }

    let cache = Some(CacheHandle {
        bank_id: bank_id.clone(),
        cache_dir: cdir,
        total_bytes: manifest.total_bytes,
        samples: samples.len(),
        format_version: manifest.format_version,
    });
    if !stream_files.is_empty() {
        stream_files.resize(samples.len(), None);
    }

    Ok(LoadedBank {
        regions,
        samples,
        warnings,
        decode_failures,
        ram_bytes,
        cache,
        stream_files,
    })
}

/// Loads a bank preferring its PCM cache, transparently building the cache in
/// the background when it is missing (a bank installed by an older version) and
/// falling back to on-demand FLAC decoding if the cache cannot be used
/// (RFC-002 §8 step 5/6).
///
/// A cached bank whose resident data exceeds the budget is reloaded in
/// Streaming mode ([`load_bank_from_cache_streaming`], RFC-002 stage 6):
/// heads in RAM, tails from disk.
///
/// This is the convenience entry used by the app: it computes the effective RAM
/// budget from [`crate::meminfo::effective_budget_mib`], logs RSS before/after,
/// and starts the background L1/L2 integrity check.
///
/// Control thread only.
///
/// # Errors
///
/// [`BankLoadError`] when the bank cannot be loaded from cache or FLAC.
pub fn load_bank_auto(dir: &Path, hint: Option<&str>) -> Result<LoadedBank, BankLoadError> {
    let budget_mib = crate::meminfo::effective_budget_mib(DEFAULT_RAM_BUDGET_MIB);
    load_bank_auto_with_budget(dir, hint, budget_mib)
}

/// [`load_bank_auto`], additionally reporting cache-build progress through
/// `progress` (when `Some`) so a UI can show a live indicator while the
/// background cache is built (SPEC-075). `progress` may be shared across
/// threads and is only ever written via lock-free atomic stores.
pub fn load_bank_auto_with_progress(
    dir: &Path,
    hint: Option<&str>,
    progress: Option<&CacheBuildProgress>,
) -> Result<LoadedBank, BankLoadError> {
    let budget_mib = crate::meminfo::effective_budget_mib(DEFAULT_RAM_BUDGET_MIB);
    load_bank_auto_with_budget_and_progress(dir, hint, budget_mib, progress)
}

/// [`load_bank_auto`] with an explicit budget, so tests can force a low budget.
pub fn load_bank_auto_with_budget(
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
) -> Result<LoadedBank, BankLoadError> {
    load_bank_auto_with_budget_and_progress(dir, hint, budget_mib, None)
}

/// [`load_bank_auto_with_budget`], optionally reporting cache-build progress
/// through `progress` (see [`load_bank_auto_with_progress`]).
fn load_bank_auto_with_budget_and_progress(
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
    progress: Option<&CacheBuildProgress>,
) -> Result<LoadedBank, BankLoadError> {
    let bank_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let rss_before = crate::meminfo::current_rss_mib();
    crate::log_info!(
        "bank {bank_id}: auto-load, budget {budget_mib} MiB, RSS before {} MiB",
        rss_before.map_or("?".to_owned(), |m| m.to_string())
    );

    let result = match load_cache_or_stream(&bank_id, dir, hint, budget_mib) {
        Ok(bank) => Ok(bank),
        Err(BankLoadError::NoCache(reason)) => {
            crate::log_info!(
                "bank {bank_id}: no usable cache ({reason}); building cache in background"
            );
            let cancel = Arc::new(AtomicBool::new(false));
            let progress_bank = bank_id.clone();
            let progress_shared = progress.cloned();
            let progress = std::sync::Mutex::new(move |done: u64, total: u64, bytes: u64| {
                if let Some(shared) = &progress_shared {
                    shared.report(done, total);
                }
                if done % 100 == 0 || done == total {
                    crate::log_info!(
                        "bank {progress_bank}: building cache {done}/{total} samples, {} MiB",
                        bytes.div_ceil(1024 * 1024)
                    );
                }
            });
            match build_bank_cache(dir, hint, &progress, &cancel) {
                Ok(_) => load_cache_or_stream(&bank_id, dir, hint, budget_mib),
                Err(e) => {
                    crate::log_warn!(
                        "bank {bank_id}: cache build failed ({e}); falling back to FLAC decode"
                    );
                    load_bank_with_hint(dir, budget_mib, hint)
                }
            }
        }
        Err(e) => Err(e),
    };

    let rss_after = crate::meminfo::current_rss_mib();
    match (&result, rss_after) {
        (Ok(bank), Some(after)) => crate::log_info!(
            "bank {bank_id}: loaded {} MiB resident{}, RSS after {} MiB",
            bank.ram_bytes.div_ceil(1024 * 1024),
            if bank.is_streaming() {
                " (streaming)"
            } else {
                ""
            },
            after
        ),
        (Err(_), Some(after)) => {
            crate::log_warn!("bank {bank_id}: load failed, RSS after {} MiB", after)
        }
        _ => {}
    }

    result
}

/// [`load_bank_from_cache`], retried in Streaming mode when the resident data
/// exceeds `budget_mib`.
fn load_cache_or_stream(
    bank_id: &str,
    dir: &Path,
    hint: Option<&str>,
    budget_mib: u64,
) -> Result<LoadedBank, BankLoadError> {
    match load_bank_from_cache(dir, hint, budget_mib) {
        Err(BankLoadError::RamBudgetExceeded { needed_mib, .. }) => {
            crate::log_info!(
                "bank {bank_id}: resident needs >{needed_mib} MiB of {budget_mib} MiB budget; \
                 loading in streaming mode"
            );
            load_bank_from_cache_streaming(dir, hint, budget_mib)
        }
        other => other,
    }
}

/// Starts the background L1 (always) and L2 (once per session, or after a crash
/// via `cache/.dirty`) integrity check of a loaded cache (RFC-002 §6.2). Any
/// entry that fails is quarantined and re-decoded from its source; the outcome
/// is logged. Detached, low-priority work — never the audio path.
pub fn spawn_background_verify(dir: PathBuf) {
    std::thread::Builder::new()
        .name("bank-integrity-verify".to_owned())
        .spawn(move || {
            let bank_id = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            let cancel = Arc::new(AtomicBool::new(false));
            let level = if crate::bank_integrity::is_dirty(&dir) {
                crate::bank_integrity::IntegrityLevel::L2
            } else {
                crate::bank_integrity::IntegrityLevel::L1
            };
            match crate::bank_integrity::verify_bank_cache(&dir, level, &cancel) {
                Ok(report) if report.is_clean() => {
                    crate::log_info!(
                        "bank {bank_id}: background integrity {:?} clean ({}/{} samples)",
                        level,
                        report.ok,
                        report.checked
                    );
                }
                Ok(report) => {
                    crate::log_warn!(
                        "bank {bank_id}: background integrity found {} issues, rebuilding",
                        report.issues.len()
                    );
                    if let Err(e) =
                        crate::bank_integrity::recover_bank_cache(&dir, &report.issues, &cancel)
                    {
                        crate::log_error!("bank {bank_id}: background rebuild failed: {e}");
                    }
                }
                Err(e) => crate::log_warn!("bank {bank_id}: background verify skipped: {e}"),
            }
        })
        .ok();
}

/// Deepest folder level searched for a `.sfz` below the bank root.
const MAX_SFZ_DEPTH: usize = 4;

/// Resolves every region's `sample` path against the bank root, returning the
/// unique files in first-seen order (`velocity` layers, round-robins and
/// release groups share files), a per-region index into `unique` (`None` for a
/// region whose path could not be resolved), and the corresponding failures.
fn resolve_unique_samples(
    dir: &Path,
    doc: &audio_core::sampler::sfz::SfzDocument,
    base: &str,
) -> (Vec<PathBuf>, Vec<Option<usize>>, Vec<RegionFailure>) {
    let mut unique: Vec<PathBuf> = Vec::new();
    let mut index_of: HashMap<PathBuf, usize> = HashMap::new();
    let mut plan: Vec<Option<usize>> = Vec::with_capacity(doc.regions.len());
    let mut failures: Vec<RegionFailure> = Vec::new();
    for region in &doc.regions {
        let Some(sample_path) = region.sample.as_deref() else {
            failures.push(RegionFailure {
                sample: String::new(),
                reason: "region has no sample".to_owned(),
            });
            plan.push(None);
            continue;
        };
        let Some(full) = resolve_sample_path(dir, base, doc.default_path.as_deref(), sample_path)
        else {
            failures.push(RegionFailure {
                sample: sample_path.to_owned(),
                reason: format!("sample path escapes the bank root: {sample_path}"),
            });
            plan.push(None);
            continue;
        };
        let idx = *index_of.entry(full.clone()).or_insert_with(|| {
            unique.push(full);
            unique.len() - 1
        });
        plan.push(Some(idx));
    }
    (unique, plan, failures)
}

/// Cache-build progress callback: `(done, total, bytes_written)`, called from
/// the worker threads. The closure must be `Send` because it is invoked from
/// multiple threads; it is held behind a `Mutex` so callers pass a plain
/// `&Mutex<impl FnMut(u64, u64, u64) + Send>`.
pub type CacheProgress<'a> = &'a std::sync::Mutex<dyn FnMut(u64, u64, u64) + Send>;

/// Shared, non-blocking cache-build progress, read by the GUI while a bank's
/// PCM cache is built on a worker thread (SPEC-075).
///
/// `done` counts decoded samples and `total` the unique samples to decode;
/// both are [`Arc`]`<`[`AtomicU64`]`>`s so the struct is cheaply cloneable and
/// can be handed across threads while the UI thread polls [`fraction`] without
/// ever locking or touching the audio path. `fraction()` is `0.0` until `total`
/// is known and `1.0` when every sample has been decoded.
///
/// [`fraction`]: CacheBuildProgress::fraction
#[derive(Debug, Clone, Default)]
pub struct CacheBuildProgress {
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl CacheBuildProgress {
    /// An empty progress tracker (everything zero).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples decoded so far.
    #[must_use]
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Total unique samples to decode (set once the build plan is known).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// `0.0..=1.0`, or `0.0` when `total` is not yet known / zero.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            (self.done() as f64 / total as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// Records a `(done, total)` progress pair from the worker threads.
    fn report(&self, done: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(done, Ordering::Relaxed);
    }
}

/// Why building a bank's PCM cache failed.
#[derive(Debug, thiserror::Error)]
pub enum CacheBuildError {
    /// The bank has no `.sfz` (or it could not be read/parsed).
    #[error("{0}")]
    Load(#[from] BankLoadError),
    /// Not enough free disk space for the decoded cache.
    #[error("not enough disk space: need {needed_mib} MiB, {free_mib} MiB available")]
    NoSpace {
        /// Cache size in MiB.
        needed_mib: u64,
        /// Free space in MiB.
        free_mib: u64,
    },
    /// A sample could not be decoded or its cache file written.
    #[error("cannot build cache: {0}")]
    Io(String),
    /// The build was cancelled.
    #[error("cache build cancelled")]
    Cancelled,
}

/// Outcome of a successful [`build_bank_cache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBuildOutcome {
    /// Number of samples written.
    pub samples: usize,
    /// Total bytes of `.pcm` payload written (including the 64-byte headers).
    pub total_bytes: u64,
    /// Number of regions whose sample could not be resolved/decoded.
    pub failures: usize,
}

/// Decodes every unique sample of `dir`'s bank to `cache/*.pcm` and writes
/// `cache/manifest.json` (RFC-002 §5.3).
///
/// One decoded file is held per worker thread (never the whole bank), matching
/// [`decode_all`]. The source FLACs are the source of truth; the cache is
/// derived data and safe to delete. This runs on the control/loader threads
/// only and reports progress via `progress`.
///
/// # Errors
///
/// [`CacheBuildError`].
pub fn build_bank_cache(
    dir: &Path,
    hint: Option<&str>,
    progress: CacheProgress<'_>,
    cancel: &Arc<AtomicBool>,
) -> Result<CacheBuildOutcome, CacheBuildError> {
    let bank_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let t_start = std::time::Instant::now();
    crate::log_info!("bank {bank_id}: building PCM cache in {}", dir.display());
    crate::bank_integrity::mark_dirty(dir);

    let sfz = find_sfz(dir, hint).map_err(CacheBuildError::Load)?;
    let base = sfz
        .parent()
        .and_then(|p| p.strip_prefix(dir).ok())
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    let text = std::fs::read_to_string(&sfz)
        .map_err(|e| CacheBuildError::Load(BankLoadError::ReadSfz(sfz.clone(), e.to_string())))?;
    let mut resolver = DirResolver {
        root: dir.to_path_buf(),
        base: base.clone(),
    };
    let doc = parse(&text, &mut resolver)
        .map_err(|e| CacheBuildError::Load(BankLoadError::Sfz(sfz.clone(), e.to_string())))?;

    let (unique, _plan, failures) = resolve_unique_samples(dir, &doc, &base);
    if unique.is_empty() {
        return Err(CacheBuildError::Load(BankLoadError::NoSfz(
            dir.to_path_buf(),
        )));
    }

    let cdir = crate::bank_cache::cache_dir(dir);
    let mpath = crate::bank_cache::manifest_path(dir);

    // Free-space pre-check (RFC-002 §5.3): estimate the decoded size from each
    // file's frame count (probed without decoding) assuming the worst case of
    // two channels, add 5% + the 64-byte headers, and refuse up front if the
    // filesystem cannot hold it. A conservative over-estimate is safe: it only
    // ever refuses a build that would have run out of disk mid-way.
    let needed = estimate_cache_bytes(&unique);
    if let Ok(avail) = fs2::available_space(dir) {
        let needed_plus = needed.saturating_mul(105).div_ceil(100);
        if avail < needed_plus {
            crate::log_error!(
                "bank {bank_id}: not enough disk space for cache (need {} MiB, {} MiB free)",
                needed_plus.div_ceil(1024 * 1024),
                avail.div_ceil(1024 * 1024)
            );
            return Err(CacheBuildError::NoSpace {
                needed_mib: needed_plus.div_ceil(1024 * 1024),
                free_mib: avail.div_ceil(1024 * 1024),
            });
        }
    }

    // Decode on up to `available_parallelism` threads; each worker writes its
    // own `.pcm` and records its manifest entry in a shared, index-ordered vec.
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let written_bytes = AtomicU64::new(0);
    let failed = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(unique.len().max(1));
    let entries: Vec<std::sync::Mutex<Option<crate::bank_cache::ManifestEntry>>> =
        unique.iter().map(|_| std::sync::Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(src) = unique.get(i) else { return };
                    match build_one_pcm(src, i, &cdir) {
                        Ok(entry) => {
                            written_bytes.fetch_add(entry.size, Ordering::Relaxed);
                            if let Ok(mut slot) = entries[i].lock() {
                                *slot = Some(entry);
                            }
                        }
                        Err(e) => {
                            // A single un-decodable source must not abort the
                            // whole cache; count it and move on (RFC-002 §5.3).
                            failed.fetch_add(1, Ordering::Relaxed);
                            crate::log_warn!("bank {bank_id}: skipping {}: {e}", src.display());
                        }
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    let total = unique.len();
                    if let Ok(mut cb) = progress.lock() {
                        cb(
                            (n.min(total)) as u64,
                            total as u64,
                            written_bytes.load(Ordering::Relaxed),
                        );
                    }
                }
            });
        }
    });

    if cancel.load(Ordering::Relaxed) {
        return Err(CacheBuildError::Cancelled);
    }

    // Assemble the manifest in sample_id order, skipping failures.
    let mut manifest_entries = Vec::with_capacity(unique.len());
    for slot in entries {
        if let Some(entry) = slot.into_inner().unwrap_or(None) {
            manifest_entries.push(entry);
        }
    }
    let total_bytes = manifest_entries.iter().map(|e| e.size).sum();
    let manifest = crate::bank_cache::Manifest {
        format_version: crate::bank_cache::MANIFEST_FORMAT_VERSION,
        bank_id: bank_id.clone(),
        created: crate::bank_library::timestamp(),
        total_bytes,
        entries: manifest_entries,
    };
    crate::bank_cache::write_manifest(&mpath, &manifest).map_err(CacheBuildError::Io)?;
    crate::bank_integrity::clear_dirty(dir);

    crate::log_info!(
        "bank {bank_id}: cache built in {} ms: {} samples, {} bytes, {} failures",
        t_start.elapsed().as_millis(),
        unique.len(),
        total_bytes,
        failed.load(Ordering::Relaxed)
    );
    Ok(CacheBuildOutcome {
        samples: unique.len() - failed.load(Ordering::Relaxed),
        total_bytes,
        failures: failures.len() + failed.load(Ordering::Relaxed),
    })
}

/// Estimates the decoded `.pcm` size for `paths` in bytes, probing each file's
/// frame count without decoding and assuming the worst case of two channels
/// (`frames × 2 × 2` bytes plus the 64-byte header). Files that cannot be
/// probed are skipped (they will be counted as decode failures later).
fn estimate_cache_bytes(paths: &[PathBuf]) -> u64 {
    paths.iter().fold(0u64, |acc, p| {
        let frames = crate::audio_file::probe_len_at(p, 48_000).unwrap_or(0);
        acc.saturating_add(crate::bank_cache::PCM_HEADER_LEN as u64)
            .saturating_add(frames.saturating_mul(4))
    })
}

/// Decodes one source sample and writes its `.pcm`, returning its manifest
/// entry. `sample_id` is the index in the unique list; the stable key is `src`.
pub(crate) fn build_one_pcm(
    src: &Path,
    sample_id: usize,
    cache_dir: &Path,
) -> Result<crate::bank_cache::ManifestEntry, String> {
    let (data, channels, rate) = decode_sample_with_rate(src)?;
    let channels = u16::from(channels);
    let frames = (data.len() as u64).div_ceil(u64::from(channels).max(1));
    let src_meta = crate::bank_cache::source_meta(src);
    let src_size = src_meta.map(|m| m.size).unwrap_or(0);
    let src_xxh3 = crate::bank_cache::xxh3_64_file(src)?;
    let mtime_ns = src_meta.map(|m| m.mtime_ns).unwrap_or(0);
    let is_flac = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("flac"));
    let header = crate::bank_cache::PcmHeader {
        channels,
        sample_rate: rate,
        frames,
        src_flac_size: if is_flac { src_size } else { 0 },
        src_flac_xxh3: if is_flac {
            u64::from_str_radix(&src_xxh3, 16).unwrap_or(0)
        } else {
            0
        },
    };
    let name = format!("{sample_id:06}.pcm");
    let written = crate::bank_cache::write_pcm_file(cache_dir, &name, &header, &data, true)?;
    Ok(crate::bank_cache::ManifestEntry {
        sample_id: sample_id as u32,
        src: src.to_string_lossy().into_owned(),
        cache: name,
        size: written.size,
        xxh3_128: written.xxh3_128,
        blake3: written.blake3,
        sampled_xxh3: Some(written.sampled_xxh3),
        sample_rate: rate,
        channels,
        frames,
        src_size,
        src_xxh3,
        mtime_ns,
    })
}

/// Whether `dir` holds an `.sfz` that [`load_bank_with_hint`] would pick.
#[must_use]
pub fn has_loadable_sfz(dir: &Path, hint: Option<&str>) -> bool {
    find_sfz(dir, hint).is_ok()
}

/// Finds the `.sfz` of the bank in `dir`.
///
/// 1. The catalog `hint`, when it is confined to `dir` and is a file.
/// 2. Otherwise a breadth-first search up to [`MAX_SFZ_DEPTH`] folders deep.
///    Candidates are ordered deterministically: shallowest first, then files
///    without "retuned" in the name before those with it (Salamander ships a
///    `...Retuned.sfz` next to the standard one; the standard tuning wins),
///    then by file name. Symlinks are not followed.
fn find_sfz(dir: &Path, hint: Option<&str>) -> Result<PathBuf, BankLoadError> {
    if let Some(path) = hint.and_then(|h| confine_path(dir, h))
        && path.is_file()
    {
        return Ok(path);
    }
    let mut candidates: Vec<(usize, bool, String, PathBuf)> = Vec::new();
    let mut level = vec![dir.to_path_buf()];
    for depth in 0..=MAX_SFZ_DEPTH {
        let mut next = Vec::new();
        for folder in &level {
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    next.push(path);
                } else if kind.is_file()
                    && path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("sfz"))
                {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let retuned = name.to_lowercase().contains("retuned");
                    candidates.push((depth, retuned, name, path));
                }
            }
        }
        if !candidates.is_empty() {
            break;
        }
        level = next;
    }
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .map(|(_, _, _, path)| path)
        .ok_or_else(|| BankLoadError::NoSfz(dir.to_path_buf()))
}

/// Joins a `/`-separated folder `base` and `path` (`\` normalised).
fn join_rel(base: &str, path: &str) -> String {
    let path = path.replace('\\', "/");
    if base.is_empty() {
        path
    } else {
        format!("{base}/{path}")
    }
}

/// Resolves a region's `sample` path: `default_path` (if any) then the bank
/// directory, keeping the result inside the **bank root** `dir`.
///
/// SFZ `sample=` paths may legitimately climb *within* the bank (e.g. Osiris
/// uses `sample=../UC/...` to reach a sibling folder under the bank root), but
/// they must never escape the bank root itself. This normalises `\` → `/` and
/// rejects absolute paths, drive letters, and `..` that leave `dir`.
///
/// Returns `None` when the path is unsafe (escapes `dir`); the caller records
/// it as a decode failure rather than reading an arbitrary file.
fn resolve_sample_path(
    dir: &Path,
    base: &str,
    default_path: Option<&str>,
    sample: &str,
) -> Option<PathBuf> {
    // `sample` is relative to `default_path` (a subdirectory of the bank root),
    // or to the bank root when there is no `default_path`. Joining them and
    // confining against `dir` keeps `..` from escaping the bank root.
    let joined = match default_path {
        Some(base) if !base.is_empty() => format!("{base}/{sample}"),
        _ => sample.to_owned(),
    };
    confine_path(dir, &join_rel(base, &joined))
}

/// Joins `path` onto `root`, normalising `\` → `/` and rejecting any path that
/// escapes `root` (absolute, drive-letter, or `..` that climbs above `root`).
/// A leading `..` or an empty path yields `None`.
pub(crate) fn confine_path(root: &Path, path: &str) -> Option<PathBuf> {
    let normalized = path.replace('\\', "/");
    // Absolute (POSIX or Windows-style) and drive letters are never allowed.
    if normalized.starts_with('/') || (normalized.len() >= 2 && normalized.as_bytes()[1] == b':') {
        return None;
    }
    // Track components relative to `root` so that `..` never climbs above it.
    let mut components: Vec<&str> = Vec::new();
    for comp in normalized.split('/') {
        match comp {
            "" | "." => continue,
            // A `..` with nothing to pop climbs above `root`.
            ".." => {
                components.pop()?;
            }
            other => components.push(other),
        }
    }
    let mut out = root.to_path_buf();
    for comp in components {
        out.push(comp);
    }
    Some(out)
}

/// Decodes `path` to interleaved `i16` PCM via the SPEC-053 symphonia decoder.
///
/// WAV/FLAC/OGG/MP3 are decoded to planar `f32` by
/// [`crate::audio_file::read_audio`], then converted to `i16` here: mono stays
/// mono, stereo becomes interleaved `[L, R, …]`, and more than two channels
/// are mixed down to stereo. `f32` → `i16` scales by 32767 and saturates.
///
/// Returns the decoded data and its channel count (`1` or `2`).
///
/// # Errors
///
/// A human-readable reason on any I/O or decode failure.
type Decoded = Result<(Vec<i16>, u8), String>;

/// Decodes `paths` on up to `available_parallelism` threads. Slot `i` always
/// holds the result for `paths[i]`; workers stop early once the decoded total
/// passes `budget_bytes` (unfinished slots stay `None`).
fn decode_all(paths: &[PathBuf], budget_bytes: u64, bank_id: &str) -> Vec<Option<Decoded>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    let slots: Vec<Mutex<Option<Decoded>>> = paths.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = AtomicU64::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(paths.len().max(1));
    crate::log_info!(
        "bank {bank_id}: decoding {} files on {threads} threads",
        paths.len()
    );
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if total.load(Ordering::Relaxed) > budget_bytes {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { return };
                    let result = decode_sample(path);
                    if let Ok((data, _)) = &result {
                        total.fetch_add((data.len() as u64).saturating_mul(2), Ordering::Relaxed);
                    }
                    if let Ok(mut slot) = slots[i].lock() {
                        *slot = Some(result);
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 100 == 0 {
                        crate::log_debug!("bank {bank_id}: decoded {n}/{} files", paths.len());
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap_or(None))
        .collect()
}

fn decode_sample(path: &Path) -> Result<(Vec<i16>, u8), String> {
    decode_sample_with_rate(path).map(|(data, channels, _rate)| (data, channels))
}

/// [`decode_sample`] plus the decoded sample rate, needed to write the `.pcm`
/// header (`sample_rate`) during cache generation (RFC-002 §5.3).
fn decode_sample_with_rate(path: &Path) -> Result<(Vec<i16>, u8, u32), String> {
    let (spec, buffer) = crate::audio_file::read_audio(path).map_err(|e| e.to_string())?;
    let channels = usize::from(spec.channels).max(1);
    let frames = buffer.frames();

    // Mono.
    if channels == 1 {
        let mut out = Vec::with_capacity(frames);
        for s in buffer.channel(0) {
            out.push(to_i16(*s));
        }
        return Ok((out, 1, spec.sample_rate));
    }

    // Stereo (or more, mixed down to stereo): interleave left and right.
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        for ch in 0..channels {
            let s = buffer.channel(ch)[frame];
            if ch == 0 {
                left = s;
            } else if ch == 1 {
                right = s;
            } else {
                left += s;
                right += s;
            }
        }
        out.push(to_i16(left));
        out.push(to_i16(right));
    }
    Ok((out, 2, spec.sample_rate))
}

#[inline]
fn to_i16(s: f32) -> i16 {
    let scaled = s.clamp(-1.0, 1.0) * 32767.0;
    scaled.round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_i16_scales_and_saturates() {
        assert_eq!(to_i16(1.0), 32767);
        assert_eq!(to_i16(-1.0), -32767);
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(2.0), 32767);
        assert_eq!(to_i16(-2.0), -32767);
    }

    #[test]
    fn confine_path_normalises_backslashes() {
        let root = Path::new("/bank/root");
        assert_eq!(
            confine_path(root, "samples\\tone.wav"),
            Some(PathBuf::from("/bank/root/samples/tone.wav"))
        );
        assert_eq!(
            confine_path(root, "UC\\Piano\\C4.wav"),
            Some(PathBuf::from("/bank/root/UC/Piano/C4.wav"))
        );
    }

    #[test]
    fn confine_path_allows_climbing_within_root() {
        let root = Path::new("/bank/root");
        // `..` that stays at/above root is allowed (Osiris `sample=../UC/...`),
        // where `..` climbs relative to a subdirectory but never above `root`.
        assert_eq!(
            confine_path(root, "Programs/../UC/Piano/C4.wav"),
            Some(PathBuf::from("/bank/root/UC/Piano/C4.wav"))
        );
        // Climbing back down within the root is fine.
        assert_eq!(
            confine_path(root, "sub/../other.wav"),
            Some(PathBuf::from("/bank/root/other.wav"))
        );
    }

    #[test]
    fn confine_path_rejects_escape_above_root() {
        let root = Path::new("/bank/root");
        assert_eq!(confine_path(root, "../../etc/passwd"), None);
        assert_eq!(confine_path(root, ".."), None);
        assert_eq!(confine_path(root, "sub/../../.."), None);
    }

    #[test]
    fn confine_path_rejects_absolute_and_drive() {
        let root = Path::new("/bank/root");
        assert_eq!(confine_path(root, "/etc/passwd"), None);
        assert_eq!(confine_path(root, "\\etc\\passwd"), None);
        assert_eq!(confine_path(root, "C:\\evil"), None);
        assert_eq!(confine_path(root, "C:/evil"), None);
    }

    #[test]
    fn confine_path_handles_dot_and_empty_components() {
        let root = Path::new("/bank/root");
        assert_eq!(
            confine_path(root, "./samples/./tone.wav"),
            Some(PathBuf::from("/bank/root/samples/tone.wav"))
        );
        assert_eq!(
            confine_path(root, "samples//tone.wav"),
            Some(PathBuf::from("/bank/root/samples/tone.wav"))
        );
        assert_eq!(confine_path(root, ""), Some(root.to_path_buf()));
    }

    #[test]
    fn cache_build_progress_reports_fraction() {
        let p = CacheBuildProgress::new();
        // Total unknown → 0.0, no division by zero.
        assert_eq!(p.fraction(), 0.0);
        assert_eq!(p.done(), 0);
        assert_eq!(p.total(), 0);

        // Mid-build: 2 of 5 samples decoded.
        p.report(2, 5);
        assert_eq!(p.done(), 2);
        assert_eq!(p.total(), 5);
        assert!((p.fraction() - 0.4).abs() < 1e-6);

        // Completion clamps to 1.0.
        p.report(5, 5);
        assert_eq!(p.fraction(), 1.0);
    }

    #[test]
    fn cache_build_progress_is_shared_across_clones() {
        let p = CacheBuildProgress::new();
        let clone = p.clone();
        clone.report(3, 10);
        // Both handles observe the same atomics.
        assert_eq!(p.done(), 3);
        assert_eq!(p.total(), 10);
        assert!((p.fraction() - 0.3).abs() < 1e-6);
    }
}
