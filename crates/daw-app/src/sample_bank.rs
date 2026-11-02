//! Loading a local SFZ sample bank into memory (SPEC-064 / RFC-001 §3.3, §5).
//!
//! This is PR 3 of the sampler slice: it reads the `.sfz` text, resolves and
//! decodes each region's sample (FLAC/WAV/OGG/MP3 through the SPEC-053
//! symphonia decoder), converts it to interleaved `i16`, and produces the
//! [`audio_core::SampleRegion`] list plus the `Arc<[i16]>` sample data that
//! [`audio_core::SamplerEngine`] consumes. Loading and decoding happen on the
//! control thread only (I/O and allocation are allowed here); nothing here is
//! ever called from the audio callback.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use audio_core::sampler::region::resolve;
use audio_core::sampler::sfz::{IncludeResolver, parse};

/// Default RAM budget for one bank, in MiB (RFC-001 §3.3).
pub const DEFAULT_RAM_BUDGET_MIB: u64 = 512;

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
    /// Mono samples are one `Arc<[i16]>`; stereo samples are interleaved
    /// `[L, R, L, R, …]`.
    pub samples: Vec<Arc<[i16]>>,
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
/// `#include`d paths are resolved relative to `root`; the parser already
/// rejects absolute paths and `..` (`SfzError::UnsafePath`), so a path that
/// reaches here is a plain relative path.
struct DirResolver {
    root: PathBuf,
}

impl IncludeResolver for DirResolver {
    fn resolve(&mut self, path: &str) -> Result<String, String> {
        let full = self.root.join(path);
        std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))
    }
}

/// Loads the SFZ bank in `dir`, decoding every sample into `i16` PCM.
///
/// The only `.sfz` file in `dir` is used (the first, if several). `budget_mib`
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
    if budget_mib == 0 {
        return Err(BankLoadError::InvalidBudget);
    }

    let sfz = find_sfz(dir)?;
    let text = std::fs::read_to_string(&sfz)
        .map_err(|e| BankLoadError::ReadSfz(sfz.clone(), e.to_string()))?;

    let mut resolver = DirResolver {
        root: dir.to_path_buf(),
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
    let mut samples: Vec<Arc<[i16]>> = Vec::new();
    let mut decode_failures: Vec<RegionFailure> = Vec::new();
    let mut ram_bytes: u64 = 0;

    for region in &doc.regions {
        let Some(sample_path) = region.sample.as_deref() else {
            decode_failures.push(RegionFailure {
                sample: String::new(),
                reason: "region has no sample".to_owned(),
            });
            continue;
        };
        let full = resolve_sample_path(dir, doc.default_path.as_deref(), sample_path);
        match decode_sample(&full) {
            Ok((data, channels)) if !data.is_empty() => {
                // `data` is interleaved i16; one i16 is two bytes (RFC-001 §3.3).
                let bytes = (data.len() as u64).saturating_mul(2);
                if ram_bytes.saturating_add(bytes) > budget_bytes {
                    let needed_mib = ram_bytes.saturating_add(bytes).div_ceil(1024 * 1024);
                    return Err(BankLoadError::RamBudgetExceeded {
                        budget_mib,
                        needed_mib,
                    });
                }
                ram_bytes += bytes;
                let sample_index = samples.len();
                samples.push(Arc::from(data));
                if let Some(r) = resolve(region, 48_000, sample_index, channels) {
                    regions.push(r);
                }
            }
            Ok((_, _)) => {
                decode_failures.push(RegionFailure {
                    sample: sample_path.to_owned(),
                    reason: "sample is empty".to_owned(),
                });
            }
            Err(reason) => {
                decode_failures.push(RegionFailure {
                    sample: sample_path.to_owned(),
                    reason,
                });
            }
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

/// Finds the `.sfz` in `dir`. Several files produce an error-free result using
/// the first; the caller does not need to know which.
fn find_sfz(dir: &Path) -> Result<PathBuf, BankLoadError> {
    let mut found: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sfz") && found.is_none() {
                found = Some(path);
            }
        }
    }
    found.ok_or_else(|| BankLoadError::NoSfz(dir.to_path_buf()))
}

/// Resolves a region's `sample` path: `default_path` (if any) then the bank
/// directory, keeping the result inside `dir` where possible.
fn resolve_sample_path(dir: &Path, default_path: Option<&str>, sample: &str) -> PathBuf {
    if let Some(base) = default_path {
        let candidate = dir.join(base).join(sample);
        if candidate.is_file() {
            return candidate;
        }
    }
    dir.join(sample)
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
}
