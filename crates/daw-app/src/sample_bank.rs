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
}

/// A sample the loader could not decode, reported without aborting the bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionFailure {
    /// The `sample` path as written in the `.sfz`.
    pub sample: String,
    /// Why it could not be loaded or decoded.
    pub reason: String,
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
    /// Actual bytes of sample data held in RAM.
    pub ram_bytes: u64,
}

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
    let mut decode_failures: Vec<RegionFailure> = Vec::new();
    let mut ram_bytes: u64 = 0;

    // Resolve every region's path; unique files are decoded once (velocity
    // layers, round robins and release groups share files), in first-seen order.
    let mut unique: Vec<PathBuf> = Vec::new();
    let mut index_of: HashMap<PathBuf, usize> = HashMap::new();
    let mut plan: Vec<Option<usize>> = Vec::with_capacity(doc.regions.len());
    for region in &doc.regions {
        let Some(sample_path) = region.sample.as_deref() else {
            decode_failures.push(RegionFailure {
                sample: String::new(),
                reason: "region has no sample".to_owned(),
            });
            plan.push(None);
            continue;
        };
        let Some(full) = resolve_sample_path(dir, &base, doc.default_path.as_deref(), sample_path)
        else {
            decode_failures.push(RegionFailure {
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
    })
}

/// Deepest folder level searched for a `.sfz` below the bank root.
const MAX_SFZ_DEPTH: usize = 4;

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
fn confine_path(root: &Path, path: &str) -> Option<PathBuf> {
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
    let (spec, buffer) = crate::audio_file::read_audio(path).map_err(|e| e.to_string())?;
    let channels = usize::from(spec.channels).max(1);
    let frames = buffer.frames();

    // Mono.
    if channels == 1 {
        let mut out = Vec::with_capacity(frames);
        for s in buffer.channel(0) {
            out.push(to_i16(*s));
        }
        return Ok((out, 1));
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
    Ok((out, 2))
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
}
