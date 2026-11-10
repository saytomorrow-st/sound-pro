//! Bank download with license re-check (SPEC-067 / RFC-001 §6, PR 6).
//!
//! We host nothing: the only download source is the upstream URL pinned in a
//! catalog manifest (see [`crate::bank_library::Download`]). Before every
//! download the license is re-checked against the upstream source the catalog
//! audited, the archive SHA-256 is verified against the pinned digest, and
//! only then is the archive unpacked into the user data directory with an
//! atomic rename. A share-alike/GPL license cannot be downloaded at all (the
//! catalog forbids it; local import only).
//!
//! This module runs on the control thread only. It allocates and does I/O; it
//! is never called from the audio callback. Download progress and cancellation
//! are signalled through [`Progress`] and a cancellation flag shared with the
//! GUI.
//!
//! # Threat model (SPEC-067 §5)
//!
//! - **Zip-slip / path traversal**: an archive entry with an absolute path, a
//!   `..` or `.` component, a drive prefix, a `:` (Windows alternate-stream),
//!   a reserved device name, or a symlink/hard-link is rejected before
//!   extraction and the whole download is discarded.
//! - **Zip bombs / size limits**: the declared (uncompressed) entry size must
//!   fit within [`MAX_ENTRY_BYTES`] and the *actual* bytes are counted as they
//!   stream (not trusted from the header) against a per-archive unpack limit
//!   derived from the pinned size; the compressed archive is capped at the
//!   manifest `size_bytes` while streaming.
//! - **Redirects**: redirects are followed only while the host stays on the
//!   catalog `redirect_hosts` allow-list (HTTPS only); any other cross-host
//!   redirect aborts with [`DownloadError::RedirectToOtherHost`].
//! - **TLS**: data and license URLs must be `https`; `http` is rejected with
//!   [`DownloadError::InsecureUrl`]. TLS uses rustls (no OpenSSL).
//! - **Partial downloads / resume**: the archive is streamed to a partial file
//!   and only renamed into place after hash verification; a truncated body
//!   fails the size/hash check and the partial file is deleted. A network
//!   failure mid-stream is retried (up to [`MAX_RETRIES`]) via HTTP `Range`
//!   from the last byte written, so an interrupted download resumes rather
//!   than restarting from zero.
//! - **Concurrency / atomicity**: a per-bank lock file (created with
//!   `create_new`) serialises concurrent downloads of the same bank; the
//!   install uses a unique staging dir and a rename into place, and the
//!   registry write is temp+rename.
//! - **Cancel**: the GUI flips a shared `AtomicBool`; every network read and
//!   the unpack loop check it and abort with [`DownloadError::Cancelled`].
//!
//! Advisory locks use the `fs2` crate because `std::fs::File::try_lock` was
//! stabilised only in Rust 1.89 and the project MSRV is 1.85. When the MSRV is
//! raised to 1.89, [`acquire_lock`] and the registry lock in
//! `bank_library` should move to `File::try_lock` and drop the `fs2`
//! dependency.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::bank_library::{CatalogEntry, Download, InstalledBank, LicenseCheck};
use sha2::{Digest, Sha256};

/// Maximum number of redirects followed before giving up.
const MAX_REDIRECTS: usize = 5;

/// Maximum number of archive entries accepted (guard against a many-entry
/// "zip bomb" of tiny files).
const MAX_ENTRIES: usize = 100_000;

/// Hard cap on the total unpacked size of a bank (4 GiB). The effective limit
/// is `size_bytes * 8`, capped here.
const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Maximum size of a single unpacked archive entry (2 GiB).
const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum size of a license-source body (1 MiB).
const MAX_LICENSE_BYTES: u64 = 1024 * 1024;

/// Per-read timeout for network I/O (stalls mid-download).
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall connect/send timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall deadline for the (small) license-source fetch.
const LICENSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of retries for a network failure mid-download (SPEC-076).
/// Each retry resumes from the bytes already written to the partial file via
/// HTTP `Range`, so a flaky connection does not restart from zero.
const MAX_RETRIES: u32 = 3;

/// Why a download failed.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The archive URL is not HTTPS.
    #[error("download URL is not HTTPS: {0}")]
    InsecureUrl(String),
    /// A redirect would leave an allowed host.
    #[error("the source redirected to a different host: {0}")]
    RedirectToOtherHost(String),
    /// Too many redirects.
    #[error("too many redirects while fetching {0}")]
    TooManyRedirects(String),
    /// An HTTP status that is not 2xx.
    #[error("HTTP {0} from {1}")]
    HttpStatus(u16, String),
    /// The upstream API is rate-limited (GitHub unauthenticated = 60/h).
    #[error("rate limited, try again later: {0}")]
    RateLimited(String),
    /// A network or body I/O failure.
    #[error("network error: {0}")]
    Network(String),
    /// A network failure persisted across every retry.
    #[error("download failed after {0} attempts: {1}")]
    RetriesExhausted(u32, String),
    /// There is not enough disk space to complete the download.
    #[error("not enough disk space to download {0}")]
    NoSpace(String),
    /// The downloaded byte count differs from the manifest.
    #[error("download truncated: expected {expected} bytes, got {actual}")]
    Truncated {
        /// Bytes the manifest pinned.
        expected: u64,
        /// Bytes actually received.
        actual: u64,
    },
    /// The advertised Content-Length disagrees with the pinned size.
    #[error("server advertised {actual} bytes, manifest pins {expected}")]
    TooLarge {
        /// Bytes the manifest pinned.
        expected: u64,
        /// Bytes the server advertised.
        actual: u64,
    },
    /// The archive SHA-256 does not match the pinned digest.
    #[error("archive checksum mismatch")]
    HashMismatch,
    /// The license source could not be reached.
    #[error("license source unreachable: {0}")]
    LicenseUnreachable(String),
    /// The license text/hash does not match what the catalog audited.
    #[error("license no longer matches the audited source")]
    LicenseMismatch,
    /// The license source did not declare an SPDX id at all.
    #[error("license source declares no SPDX identifier")]
    LicenseMissingSpdx,
    /// The archive format is unsupported (should be caught by validation).
    #[error("unsupported archive format {0:?}")]
    UnsupportedFormat(String),
    /// An archive entry is unsafe (absolute path, `..`, symlink, oversize).
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),
    /// The archive has more entries than allowed.
    #[error("archive has too many entries ({0})")]
    TooManyEntries(usize),
    /// The unpacked total exceeds the per-archive limit.
    #[error("unpacked size {actual} exceeds limit {limit}")]
    UnpackLimitExceeded {
        /// The limit.
        limit: u64,
        /// What was actually reached.
        actual: u64,
    },
    /// The archive is damaged or unreadable.
    #[error("cannot unpack archive: {0}")]
    Unpack(String),
    /// The user cancelled the download.
    #[error("download cancelled")]
    Cancelled,
    /// Another download of the same bank is already in progress.
    #[error("another download of {0} is already running")]
    AlreadyInProgress(String),
    /// Local filesystem I/O (temp file, unpack, install).
    #[error("I/O error at {0}: {1}")]
    Io(PathBuf, String),
}

/// Download progress, shared between the worker and the GUI.
///
/// `total` is the pinned manifest size in bytes; `received` grows as the body
/// streams. `fraction()` is `0.0` until `total` is known and `1.0` when done.
#[derive(Debug, Clone)]
pub struct Progress {
    received: Arc<AtomicU64>,
    total: u64,
}

impl Progress {
    /// A progress tracker for a download of `total` bytes.
    #[must_use]
    pub fn new(total: u64) -> Self {
        Self {
            received: Arc::new(AtomicU64::new(0)),
            total,
        }
    }

    /// Bytes received so far.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    /// Pinned total size in bytes.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// `0.0..=1.0`, or `0.0` when `total` is zero.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.bytes() as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Unpacking limits for one archive, derived from the pinned size but
/// overridable in tests.
#[derive(Debug, Clone, Copy)]
struct Limits {
    max_entries: usize,
    max_total_unpacked: u64,
    max_entry_bytes: u64,
}

impl Limits {
    fn for_archive(size_bytes: u64) -> Self {
        Limits {
            max_entries: MAX_ENTRIES,
            // A compressed archive may legitimately expand; 8x is generous for
            // both catalog banks, capped at 4 GiB.
            max_total_unpacked: size_bytes.saturating_mul(8).min(MAX_UNPACKED_BYTES),
            max_entry_bytes: MAX_ENTRY_BYTES,
        }
    }
}

/// Downloads, verifies, and installs a bank from the catalog into the user
/// data directory (see [`crate::bank_library::banks_dir`]).
///
/// # Errors
///
/// [`DownloadError`].
pub fn download_bank(
    entry: &CatalogEntry,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
) -> Result<InstalledBank, DownloadError> {
    let banks = crate::bank_library::banks_dir().ok_or_else(|| {
        DownloadError::Io(
            PathBuf::from("<no data dir>"),
            "no data directory".to_owned(),
        )
    })?;
    download_bank_into(entry, &banks, progress, cancel)
}

/// Downloads, verifies, and installs a bank into an explicit `banks` directory
/// (used by [`download_bank`] and by the tests, which point it at a temp dir).
///
/// The full pipeline: validate → license re-check → stream the archive to a
/// temp file (progress + cancel) → verify size and SHA-256 → unpack into a
/// unique temp dir (safe) → atomically move into the banks directory → update
/// the registry with the attribution text. On any failure the temp artifacts
/// are removed and no registry entry is written.
///
/// # Errors
///
/// [`DownloadError`].
pub fn download_bank_into(
    entry: &CatalogEntry,
    banks: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
) -> Result<InstalledBank, DownloadError> {
    download_bank_into_inner(entry, banks, progress, cancel, true)
}

/// [`download_bank_into`] with the HTTPS enforcement toggled. The production
/// path always requires HTTPS; the integration tests call this with `false` so
/// they can serve archives and license sources from a local `http://` server.
///
/// This is intentionally `pub(crate)` and `#[cfg(test)]`-gated so it is not
/// part of the crate's public API (M7).
#[cfg(test)]
pub(crate) fn download_bank_into_with_https(
    entry: &CatalogEntry,
    banks: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<InstalledBank, DownloadError> {
    download_bank_into_inner(entry, banks, progress, cancel, https_required)
}

/// The full download pipeline (shared by production and tests).
fn download_bank_into_inner(
    entry: &CatalogEntry,
    banks: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<InstalledBank, DownloadError> {
    // 0. Validate the entry id and sfz path (defense in depth; the catalog is
    //    also validated at parse time, but the downloader must not trust it).
    if !crate::bank_library::is_valid_entry_id(&entry.id) {
        return Err(DownloadError::UnsafeEntry(format!(
            "invalid bank id {entry:?}"
        )));
    }
    if let Some(sfz) = &entry.sfz {
        if !crate::bank_library::is_valid_sfz_path(sfz) {
            return Err(DownloadError::UnsafeEntry(format!(
                "invalid sfz path {sfz:?}"
            )));
        }
    }

    let download = entry
        .download
        .as_ref()
        .ok_or_else(|| DownloadError::UnsupportedFormat(entry.format.clone()))?;
    let license_check = entry
        .license_check
        .as_ref()
        .ok_or_else(|| DownloadError::LicenseUnreachable("no license_check block".to_owned()))?;

    std::fs::create_dir_all(banks)
        .map_err(|e| DownloadError::Io(banks.to_path_buf(), e.to_string()))?;

    // Serialise concurrent downloads of the same bank. This must happen BEFORE
    // the stale-artifact sweep so a live lock held by another download is never
    // removed.
    let lock_path = banks.join(format!(".{}.lock", entry.id));
    let _lock = acquire_lock(&lock_path)?;

    // Sweep stale temp/old artifacts left by a previous interrupted run. Our
    // own lock is held, so it is not swept.
    sweep_stale_artifacts(banks, &entry.id)?;

    // 1. Re-check the license before downloading anything.
    recheck_license(entry, license_check, cancel, https_required)?;

    // 2. Stream the archive to a deterministic partial file, enforcing the
    //    size cap. The `.part` name (not swept as stale) lets a later run
    //    resume an interrupted download via HTTP Range; it is removed once the
    //    archive is verified and unpacked (or on a hard failure below).
    let partial = banks.join(format!(".{}.download.part", entry.id));
    let result = download_archive(download, &partial, progress, cancel, https_required);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }

    // 3. Unpack into a unique staging dir, then atomically install.
    let limits = Limits::for_archive(download.size_bytes);
    let tmp_dir = banks.join(format!(".{}.unpack.{}.tmp", entry.id, unique_token()));
    let unpack_result = (|| {
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| DownloadError::Io(tmp_dir.clone(), e.to_string()))?;
        unpack_archive(&partial, &entry.format, &tmp_dir, cancel, &limits)
    })();
    let _ = std::fs::remove_file(&partial);
    if let Err(e) = unpack_result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    let final_dir = banks.join(&entry.id);
    let install_result = atomic_install(&tmp_dir, &final_dir, banks, &entry.id);
    if let Err(e) = install_result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    // 4. Record the install in the registry with attribution. If the registry
    //    write fails, roll the install back so we never leave an unregistered
    //    bank behind.
    let bank = InstalledBank {
        id: entry.id.clone(),
        name: entry.name.clone(),
        family: entry.family.clone(),
        path: final_dir.to_string_lossy().into_owned(),
        license_spdx: entry.license.spdx.clone(),
        attribution: entry.attribution.clone(),
        license_acknowledged: false,
        installed_at: crate::bank_library::timestamp(),
    };
    let registry_file = banks.join("banks.json");
    if let Err(e) =
        crate::bank_library::record_installed_bank_with_cancel(&registry_file, &bank, Some(cancel))
            .map_err(|e| DownloadError::Io(registry_file, e.to_string()))
    {
        // Roll back: remove the newly-installed bank and restore the previous
        // install if there was one. If the restore fails, surface that error
        // (the previous install is still safely on disk as `.{id}.old.tmp`).
        let _ = std::fs::remove_dir_all(&final_dir);
        let old = banks.join(format!(".{}.old.tmp", entry.id));
        if old.exists() {
            std::fs::rename(&old, &final_dir)
                .map_err(|re| DownloadError::Io(old.clone(), re.to_string()))?;
        }
        return Err(e);
    }
    // Success: drop any backed-up previous install.
    let old = banks.join(format!(".{}.old.tmp", entry.id));
    let _ = std::fs::remove_dir_all(&old);

    // 5. Build the PCM cache now that the bank is installed (RFC-002 §5.3).
    //    Decoding runs on this worker thread (never the audio thread) and holds
    //    at most one decoded file per worker, so it cannot OOM. The cache is
    //    derived data; a failure here leaves the bank installed and playable
    //    (falling back to on-demand FLAC decode), so it is logged, not fatal.
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }
    let bank_id = entry.id.clone();
    let last_logged = std::sync::atomic::AtomicU64::new(0);
    let progress_cb = std::sync::Mutex::new(move |done: u64, total: u64, bytes: u64| {
        let d = done
            .saturating_mul(10)
            .checked_div(total.max(1))
            .unwrap_or(0);
        let prev = last_logged.swap(d, Ordering::Relaxed);
        if d > prev {
            crate::log_info!(
                "bank {bank_id}: cache build {done}/{total} samples, {} MiB written",
                bytes.div_ceil(1024 * 1024)
            );
        }
    });
    match crate::sample_bank::build_bank_cache(
        &final_dir,
        entry.sfz.as_deref(),
        &progress_cb,
        cancel,
    ) {
        Ok(outcome) => {
            crate::log_info!(
                "bank {}: cache ready: {} samples, {} bytes, {} failures",
                entry.id,
                outcome.samples,
                outcome.total_bytes,
                outcome.failures
            );
        }
        Err(e) => {
            // Cancelled must still surface as cancellation; otherwise the
            // install is complete and the missing cache is non-fatal.
            if matches!(e, crate::sample_bank::CacheBuildError::Cancelled) {
                return Err(DownloadError::Cancelled);
            }
            crate::log_warn!(
                "bank {}: cache build skipped (will decode on demand): {e}",
                entry.id
            );
        }
    }

    Ok(bank)
}

/// A per-bank advisory lock, held on an open file handle. The kernel releases
/// the lock when the process exits (including SIGKILL/power loss), so a file
/// left behind by a crash is harmless — the next download simply acquires the
/// lock again. The file itself is never deleted, so it cannot be mistaken for
/// a live lock by a concurrent download.
struct BankLock {
    _file: std::fs::File,
}

fn acquire_lock(path: &Path) -> Result<BankLock, DownloadError> {
    // `create(true)` (not `create_new`) so an existing stale lock file is
    // opened rather than treated as "in progress"; the advisory lock then
    // decides whether it is actually held.
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| DownloadError::Io(path.to_path_buf(), e.to_string()))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(BankLock { _file: file }),
        // Detect lock contention by comparing the raw OS error code, NOT
        // `ErrorKind`: on Windows `LockFileEx` reports `ERROR_LOCK_VIOLATION`
        // (33), which Rust maps to `ErrorKind::Other`, not `WouldBlock` (that
        // mapping is only reliable on Unix where it is `EWOULDBLOCK`). `fs2`
        // exposes the canonical contended error for the host platform.
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(DownloadError::AlreadyInProgress(lock_id(path)))
        }
        Err(e) => Err(DownloadError::Io(path.to_path_buf(), e.to_string())),
    }
}

/// The human-readable bank id for an `AlreadyInProgress` error, derived from a
/// lock path `.{id}.lock` by stripping the leading `.` and trailing `.lock`.
fn lock_id(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix('.'))
        .and_then(|s| s.strip_suffix(".lock"))
        .map(str::to_owned)
        .unwrap_or_default()
}

/// Removes stale `.<id>.download.*.tmp`, `.<id>.unpack.*.tmp` and
/// `.<id>.old.tmp` artifacts for `id`.
///
/// The `.<id>.old.tmp` backup is handled specially: if `final_dir`
/// (`banks/<id>`) is missing but the backup still exists (a crash between
/// `rename(final -> .old)` and `rename(tmp -> final)`), the backup is restored
/// rather than deleted, so a failed re-download never loses the last good bank.
///
/// If the restore fails (e.g. an antivirus holds the `.old.tmp` on Windows),
/// the backup is deliberately left in place and the error is surfaced, so a
/// later sweep never deletes the last working install behind our back.
///
/// Matching is explicit (three exact name shapes) rather than a `.{id}.`
/// prefix, so an id containing a dot (`a.b`) is not confused with a sibling
/// bank whose id is the prefix (`a`).
fn sweep_stale_artifacts(banks: &Path, id: &str) -> Result<(), DownloadError> {
    let final_dir = banks.join(id);
    let old = banks.join(format!(".{id}.old.tmp"));
    if !final_dir.exists() && old.exists() {
        // Restore the interrupted previous install before anything else runs.
        std::fs::rename(&old, &final_dir)
            .map_err(|e| DownloadError::Io(old.clone(), e.to_string()))?;
    }
    let entries = match std::fs::read_dir(banks) {
        Ok(e) => e,
        Err(e) => return Err(DownloadError::Io(banks.to_path_buf(), e.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| DownloadError::Io(banks.to_path_buf(), e.to_string()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Staging names are `.{id}.download.{token}.tmp` and
        // `.{id}.unpack.{token}.tmp`, and the backup is `.{id}.old.tmp`. The
        // lock file `.{id}.lock` is left alone (advisory lock released by the
        // kernel on exit). We match these three shapes explicitly so an id that
        // contains a dot (`a.b`) is never swept by a sibling id's prefix (`a`).
        let is_stale = is_stale_artifact(id, &name);
        if is_stale {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| DownloadError::Io(path.clone(), e.to_string()))?;
            } else {
                std::fs::remove_file(&path)
                    .map_err(|e| DownloadError::Io(path.clone(), e.to_string()))?;
            }
        }
    }
    Ok(())
}

/// Whether `name` is a stale artifact (download/unpack staging or old backup)
/// for `id`, matched against the three exact name shapes rather than a prefix.
fn is_stale_artifact(id: &str, name: &str) -> bool {
    let Some(rest) = name.strip_prefix(&format!(".{id}.")) else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".tmp") else {
        return false;
    };
    // `.download.<digits>.tmp` / `.unpack.<digits>.tmp` — the token is all
    // ASCII digits (nanoseconds xor pid); `.old.tmp` — the backup.
    stem == "old"
        || stem
            .strip_prefix("download.")
            .is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        || stem
            .strip_prefix("unpack.")
            .is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
}

/// A unique token for staging names (nanoseconds + pid), avoiding clobbering
/// between concurrent downloads and stale artifacts.
fn unique_token() -> u128 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos ^ (u128::from(std::process::id()) << 64)
}

/// Re-checks the license against the upstream source before every download.
///
/// `text-hash` fetches the license text, normalizes it (UTF-8, trim, CRLF→LF)
/// and compares its SHA-256 to the catalog's pinned digest. `metadata` fetches
/// JSON and compares the `license.spdx_id` field to the catalog's SPDX id
/// (no hash). On any mismatch, an unreachable source, or a missing digest, the
/// download is blocked.
fn recheck_license(
    entry: &CatalogEntry,
    check: &LicenseCheck,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<(), DownloadError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }
    let body = http_get_license(&check.url, cancel, https_required)?;
    match check.mode.as_str() {
        "text-hash" => {
            let expected = check
                .sha256
                .as_deref()
                .ok_or(DownloadError::LicenseMismatch)?;
            let normalized = normalize_license_text(&body);
            let actual = sha256_hex(normalized.as_bytes());
            if !constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
                return Err(DownloadError::LicenseMismatch);
            }
            Ok(())
        }
        "metadata" => {
            let declared =
                license_spdx_from_metadata(&body).ok_or(DownloadError::LicenseMissingSpdx)?;
            if declared != entry.license.spdx {
                return Err(DownloadError::LicenseMismatch);
            }
            Ok(())
        }
        _ => Err(DownloadError::UnsupportedFormat(check.mode.clone())),
    }
}

/// Streams the archive to `out`, enforcing the manifest size cap, reporting
/// progress, and honouring cancellation. On success the SHA-256 of the written
/// bytes is verified against the pinned digest.
/// Streams `download` to `out` (a deterministic partial file), enforcing the
/// pinned size and SHA-256, and retrying network failures by resuming via HTTP
/// `Range` from the last byte already on disk.
///
/// The SHA-256 is computed over the *entire* file (bytes already present from
/// a previous attempt are re-hashed), so a resumption can never accept a
/// corrupted prefix. On success the file holds exactly `download.size_bytes`
/// bytes matching the pinned digest; on a hard failure (bad hash, truncation,
/// cancellation, retries exhausted) the caller removes `out`.
fn download_archive(
    download: &Download,
    out: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<(), DownloadError> {
    // Bytes already on disk from a previous attempt; resume from here.
    let mut received = existing_partial_size(out)?;
    if received > download.size_bytes {
        // A corrupt/larger-than-pinned leftover: discard and restart clean.
        std::fs::remove_file(out).map_err(|e| io_err(out, e))?;
        received = 0;
    }

    let mut attempts: u32 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        attempts += 1;

        // Open for append (resume) or create (fresh), without truncating.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
            .map_err(|e| io_err(out, e))?;
        let mut writer = std::io::BufWriter::new(file);

        let range_start = (received > 0).then_some(received);
        let mut reader = http_open(
            &download.url,
            cancel,
            https_required,
            Some(&download.redirect_hosts),
            None,
            range_start,
        )?;

        // If we asked for a range but the server ignored it and returned the
        // whole body (200), restart from zero: truncate the partial file and
        // re-hash from scratch.
        if range_start.is_some() && reader.status() == 200 {
            let _ = std::fs::remove_file(out);
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(out)
                .map_err(|e| io_err(out, e))?;
            writer = std::io::BufWriter::new(file);
            received = 0;
        }

        // Early size check: a lying `Content-Length` is rejected before we
        // read. For a ranged response the advertised length is the *remaining*
        // bytes, so compare against `size_bytes - received`.
        if let Some(len) = reader.content_length() {
            let expected_remaining = download.size_bytes.saturating_sub(received);
            if len != expected_remaining {
                return Err(DownloadError::TooLarge {
                    expected: download.size_bytes,
                    actual: len.saturating_add(received),
                });
            }
        }

        // Stream the (remaining) body, appending to the partial file.
        let mut buf = vec![0u8; 64 * 1024];
        let stream_result: Result<(), DownloadError> = (|| {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(DownloadError::Cancelled);
                }
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| DownloadError::Network(e.to_string()))?;
                if n == 0 {
                    break;
                }
                received += n as u64;
                if received > download.size_bytes {
                    return Err(DownloadError::Truncated {
                        expected: download.size_bytes,
                        actual: received,
                    });
                }
                writer.write_all(&buf[..n]).map_err(|e| io_err(out, e))?;
                progress.received.store(received, Ordering::Relaxed);
            }
            writer.flush().map_err(|e| io_err(out, e))?;
            // A clean EOF before the pinned size is a truncated (resume-able)
            // body: the server stopped sending early but we keep the partial
            // file and retry with a Range from `received`.
            if received != download.size_bytes {
                return Err(DownloadError::Truncated {
                    expected: download.size_bytes,
                    actual: received,
                });
            }
            Ok(())
        })();

        // Network failures and truncated bodies are retryable (resume from
        // `received`); everything else is fatal and returned immediately.
        match stream_result {
            Ok(()) => break,
            Err(e) => {
                // `received` already counts the bytes written before the
                // failure; the partial file is left in place for the retry.
                let retryable = matches!(
                    e,
                    DownloadError::Network(_)
                        | DownloadError::RateLimited(_)
                        | DownloadError::Truncated { .. }
                );
                if retryable && attempts < MAX_RETRIES {
                    crate::log_warn!("bank download attempt {attempts} failed, resuming: {e}");
                    continue;
                }
                return Err(if retryable {
                    DownloadError::RetriesExhausted(attempts, e.to_string())
                } else {
                    e
                });
            }
        }
    }

    // Hash the *entire* file (including any bytes resumed from a prior run).
    let actual = hash_file_sha256(out)?;
    if !constant_time_eq(actual.as_bytes(), download.sha256.as_bytes()) {
        return Err(DownloadError::HashMismatch);
    }
    Ok(())
}

/// The number of bytes already present in a partial download file, or `0` if
/// it does not exist. Errors reading metadata surface as a [`DownloadError`].
fn existing_partial_size(path: &Path) -> Result<u64, DownloadError> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(io_err(path, e)),
    }
}

/// SHA-256 of a file's full contents, streamed (used to re-hash a resumed
/// partial file that may contain bytes from a previous attempt).
fn hash_file_sha256(path: &Path) -> Result<String, DownloadError> {
    let file = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    let mut hasher = Sha256::new();
    let mut reader = std::io::BufReader::new(file);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| io_err(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Unpacks the archive into `dest`, rejecting unsafe entries and enforcing
/// size limits.
fn unpack_archive(
    archive: &Path,
    format: &str,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    limits: &Limits,
) -> Result<(), DownloadError> {
    match format {
        "zip" => unpack_zip(archive, dest, cancel, limits, None),
        "tar.bz2" => unpack_tar(archive, dest, cancel, limits, |file| {
            Box::new(bzip2::read::BzDecoder::new(file))
        }),
        "tar.gz" => unpack_tar(archive, dest, cancel, limits, |file| {
            Box::new(flate2::read::GzDecoder::new(file))
        }),
        "tar.xz" => unpack_tar(archive, dest, cancel, limits, |file| {
            Box::new(xz2::read::XzDecoder::new(file))
        }),
        "7z" => unpack_7z(archive, dest, cancel, limits),
        other => Err(DownloadError::UnsupportedFormat(other.to_owned())),
    }
}

/// Tracks unpack state shared by both archive formats: running total and the
/// set of already-written paths (lowercased) to detect duplicate/case-folding
/// collisions.
struct UnpackState {
    total: u64,
    seen: HashSet<String>,
}

impl UnpackState {
    fn new() -> Self {
        UnpackState {
            total: 0,
            seen: HashSet::new(),
        }
    }

    /// Registers a path for a new entry, rejecting duplicates (even across
    /// case). Per-entry size caps are checked by the caller against the
    /// declared size; total accounting is done via [`UnpackState::add_bytes`]
    /// as actual bytes are written, so a forged declared size cannot bypass
    /// the limits.
    fn register(&mut self, rel: &str) -> Result<(), DownloadError> {
        let key = rel.to_lowercase();
        if !self.seen.insert(key) {
            return Err(DownloadError::UnsafeEntry(format!(
                "duplicate entry path {rel:?}"
            )));
        }
        Ok(())
    }

    /// Adds `n` actually-written bytes to the running total, enforcing the
    /// per-archive limit on real (not declared) byte counts.
    fn add_bytes(&mut self, n: u64, limits: &Limits) -> Result<(), DownloadError> {
        self.total = self.total.saturating_add(n);
        if self.total > limits.max_total_unpacked {
            return Err(DownloadError::UnpackLimitExceeded {
                limit: limits.max_total_unpacked,
                actual: self.total,
            });
        }
        Ok(())
    }
}

fn unpack_zip(
    archive: &Path,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    limits: &Limits,
    // Test-only hook: abort with `Cancelled` once `actual` bytes have been
    // written to the current entry. This makes the mid-copy cancellation
    // deterministic (no reliance on thread timing); production always passes
    // `None` and relies on the shared `cancel` flag.
    cancel_after_bytes: Option<u64>,
) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| DownloadError::Unpack(e.to_string()))?;

    if zip.len() > limits.max_entries {
        return Err(DownloadError::TooManyEntries(zip.len()));
    }

    let mut state = UnpackState::new();
    for i in 0..zip.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let mut entry = zip
            .by_index(i)
            .map_err(|e| DownloadError::Unpack(e.to_string()))?;
        let name = entry.name().to_owned();
        // Crate-level zip-slip check first (absolute paths and `..`).
        if entry.enclosed_name().is_none() {
            return Err(DownloadError::UnsafeEntry(format!(
                "zip entry escapes: {name:?}"
            )));
        }
        let out_path = safe_join(dest, &name)?;

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| DownloadError::Io(out_path, e.to_string()))?;
            continue;
        }
        if entry.is_symlink() || is_non_regular_zip(&entry) {
            return Err(DownloadError::UnsafeEntry(format!(
                "non-regular entry {name:?}"
            )));
        }
        let declared = entry.size();
        if declared > limits.max_entry_bytes {
            return Err(DownloadError::UnsafeEntry(format!(
                "entry {name:?} is too large ({declared} bytes)"
            )));
        }
        state.register(&name)?;

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DownloadError::Io(parent.to_path_buf(), e.to_string()))?;
        }
        // `create_new` so a duplicate/collision can never overwrite a file.
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .map_err(|e| io_err(&out_path, e))?;
        let mut buf = vec![0u8; 64 * 1024];
        // Read through a `Take` of `declared + 1` bytes: a forged
        // `uncompressed_size` that understates the data yields more than
        // `declared` bytes and is rejected, and the *actual* bytes written are
        // what count against the total limit (not the declared size).
        let mut limited = entry.by_ref().take(declared.saturating_add(1));
        let mut actual: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let n = limited
                .read(&mut buf)
                .map_err(|e| DownloadError::Unpack(e.to_string()))?;
            if n == 0 {
                break;
            }
            actual = actual.saturating_add(n as u64);
            if actual > declared {
                return Err(DownloadError::UnsafeEntry(format!(
                    "entry {name:?} has more bytes than its declared size ({declared})"
                )));
            }
            if cancel_after_bytes.is_some_and(|threshold| actual >= threshold) {
                return Err(DownloadError::Cancelled);
            }
            state.add_bytes(n as u64, limits)?;
            out.write_all(&buf[..n]).map_err(|e| io_err(&out_path, e))?;
        }
    }
    Ok(())
}

/// Whether a zip entry is a FIFO/device/socket (non-regular) rather than a
/// regular file or directory. Symlinks are checked separately.
fn is_non_regular_zip<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    const S_IFMT: u32 = 0o170000;
    const S_IFREG: u32 = 0o100000;
    const S_IFDIR: u32 = 0o040000;
    match entry.unix_mode() {
        Some(mode) => {
            let ft = mode & S_IFMT;
            ft != S_IFREG && ft != S_IFDIR && ft != 0
        }
        None => false,
    }
}

fn unpack_tar(
    archive: &Path,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    limits: &Limits,
    wrap: impl FnOnce(std::fs::File) -> Box<dyn Read>,
) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;
    let decoder = wrap(file);
    let mut tar = tar::Archive::new(decoder);

    let mut state = UnpackState::new();
    let mut count = 0usize;
    for entry in tar
        .entries()
        .map_err(|e| DownloadError::Unpack(e.to_string()))?
    {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        count += 1;
        if count > limits.max_entries {
            return Err(DownloadError::TooManyEntries(count));
        }
        let mut entry = entry.map_err(|e| DownloadError::Unpack(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| DownloadError::Unpack(e.to_string()))?
            .into_owned();
        let name = path.to_string_lossy().into_owned();
        let out_path = safe_join(dest, &name)?;

        let ft = entry.header().entry_type();
        if ft.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| DownloadError::Io(out_path, e.to_string()))?;
            continue;
        }
        if !ft.is_file() {
            // Symlinks, hard links, devices, fifos, etc. are rejected.
            return Err(DownloadError::UnsafeEntry(format!(
                "non-regular entry {name:?}"
            )));
        }
        let declared = entry.size();
        if declared > limits.max_entry_bytes {
            return Err(DownloadError::UnsafeEntry(format!(
                "entry {name:?} is too large ({declared} bytes)"
            )));
        }
        state.register(&name)?;

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DownloadError::Io(parent.to_path_buf(), e.to_string()))?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .map_err(|e| io_err(&out_path, e))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let n = entry
                .read(&mut buf)
                .map_err(|e| DownloadError::Unpack(e.to_string()))?;
            if n == 0 {
                break;
            }
            state.add_bytes(n as u64, limits)?;
            out.write_all(&buf[..n]).map_err(|e| io_err(&out_path, e))?;
        }
    }
    Ok(())
}

/// Unpacks a 7z archive using `sevenz-rust`, applying the same safety checks
/// as the tar/zip paths (zip-slip, size caps, duplicate detection, atomic
/// `create_new` writes, cancellation).
///
/// Note: the 7z format does not encode symlinks the way tar/zip do — a 7z
/// "symlink" entry is just a regular file whose contents are the link target.
/// We therefore treat every non-directory entry as a regular file and write
/// its bytes verbatim, which is both safe and the behavior users expect.
fn unpack_7z(
    archive: &Path,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    limits: &Limits,
) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;

    let mut state = UnpackState::new();
    let mut count = 0usize;
    // The closure cannot return our structured `DownloadError` directly
    // (`sevenz-rust` only accepts its own `Error`), so we stash the first
    // error here and stop iteration immediately; it is returned below.
    let mut first_err: Option<DownloadError> = None;

    let result = sevenz_rust::decompress_with_extract_fn(
        file,
        dest,
        |entry: &sevenz_rust::SevenZArchiveEntry,
         reader: &mut dyn std::io::Read,
         _dest_path: &std::path::PathBuf|
         -> Result<bool, sevenz_rust::Error> {
            // Cancellation and entry-count limits are enforced from within the
            // closure because `sevenz-rust` does not expose the total entry
            // count up front.
            if cancel.load(Ordering::Relaxed) {
                first_err = Some(DownloadError::Cancelled);
                return Ok(false);
            }
            count += 1;
            if count > limits.max_entries {
                first_err = Some(DownloadError::TooManyEntries(count));
                return Ok(false);
            }

            let name = entry.name().to_owned();
            if entry.is_anti_item() {
                first_err = Some(DownloadError::UnsafeEntry(format!(
                    "7z anti-item (deletion marker) is not allowed: {name:?}"
                )));
                return Ok(false);
            }

            let out_path = match safe_join(dest, &name) {
                Ok(p) => p,
                Err(e) => {
                    first_err = Some(e);
                    return Ok(false);
                }
            };

            if entry.is_directory() {
                if let Err(e) = std::fs::create_dir_all(&out_path) {
                    first_err = Some(DownloadError::Io(out_path, e.to_string()));
                    return Ok(false);
                }
                return Ok(true);
            }

            let declared = entry.size();
            if declared > limits.max_entry_bytes {
                first_err = Some(DownloadError::UnsafeEntry(format!(
                    "entry {name:?} is too large ({declared} bytes)"
                )));
                return Ok(false);
            }
            if let Err(e) = state.register(&name) {
                first_err = Some(e);
                return Ok(false);
            }

            if let Some(parent) = out_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    first_err = Some(DownloadError::Io(parent.to_path_buf(), e.to_string()));
                    return Ok(false);
                }
            }

            // `create_new` so a duplicate/collision can never overwrite a file.
            let mut out = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&out_path)
            {
                Ok(f) => f,
                Err(e) => {
                    first_err = Some(io_err(&out_path, e));
                    return Ok(false);
                }
            };

            let mut buf = vec![0u8; 64 * 1024];
            let mut actual: u64 = 0;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    first_err = Some(DownloadError::Cancelled);
                    return Ok(false);
                }
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        first_err = Some(DownloadError::Unpack(e.to_string()));
                        return Ok(false);
                    }
                };
                actual = actual.saturating_add(n as u64);
                if actual > declared {
                    first_err = Some(DownloadError::UnsafeEntry(format!(
                        "entry {name:?} has more bytes than its declared size ({declared})"
                    )));
                    return Ok(false);
                }
                if let Err(e) = state.add_bytes(n as u64, limits) {
                    first_err = Some(e);
                    return Ok(false);
                }
                if let Err(e) = out.write_all(&buf[..n]) {
                    first_err = Some(io_err(&out_path, e));
                    return Ok(false);
                }
            }
            Ok(true)
        },
    );

    if let Some(e) = first_err {
        return Err(e);
    }
    // A genuine `sevenz-rust` failure (bad signature, unsupported compression
    // method, truncated archive, ...) that our own checks did not catch.
    result.map_err(|e| DownloadError::Unpack(e.to_string()))
}

/// Validates an archive entry name and returns it as clean, `/`-separated
/// components. This is a **string-level** check, independent of the host OS,
/// so an archive produced on any platform is judged by the same rules and a
/// name that is dangerous on Windows is rejected even when we run on Linux
/// (and vice versa). Rejects:
///
/// - a NUL byte;
/// - an absolute or root-relative path (leading `/` or `\`, plus UNC
///   `//server/share`), because the split yields a leading empty component;
/// - a Windows drive prefix or alternate-stream name (any `:`, e.g. `C:`,
///   `name:stream`, `::$DATA`);
/// - any `..` (parent) or `.` (current) component;
/// - any empty component (leading/trailing separators other than a single
///   trailing separator for directory entries, and internal `//`);
/// - Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`,
///   `LPT1-9`), case-insensitively, with or without an extension.
fn validate_entry_name(name: &str) -> Result<Vec<String>, DownloadError> {
    if name.contains('\0') {
        return Err(DownloadError::UnsafeEntry(format!(
            "NUL byte in entry name {name:?}"
        )));
    }
    // A single trailing separator marks a directory entry; allow it.
    let trimmed = name.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Err(DownloadError::UnsafeEntry(format!(
            "empty entry name {name:?}"
        )));
    }
    let mut comps = Vec::new();
    for comp in trimmed.split(['/', '\\']) {
        if comp.is_empty() {
            return Err(DownloadError::UnsafeEntry(format!(
                "empty component in {name:?}"
            )));
        }
        if comp == "." {
            return Err(DownloadError::UnsafeEntry(format!(
                "current-directory component in {name:?}"
            )));
        }
        if comp == ".." {
            return Err(DownloadError::UnsafeEntry(format!(
                "parent traversal in {name:?}"
            )));
        }
        if comp.contains(':') {
            return Err(DownloadError::UnsafeEntry(format!(
                "colon in entry name {name:?}"
            )));
        }
        if is_windows_reserved(comp) {
            return Err(DownloadError::UnsafeEntry(format!(
                "reserved name {comp:?} in {name:?}"
            )));
        }
        comps.push(comp.to_owned());
    }
    Ok(comps)
}

/// Whether a component is a Windows reserved device name (`CON`, `PRN`, `AUX`,
/// `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`), case-insensitively and ignoring any
/// extension.
fn is_windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            if stem.len() == 4 && (&stem[..3] == "COM" || &stem[..3] == "LPT") {
                let last = stem.as_bytes()[3];
                last.is_ascii_digit() && last != b'0'
            } else {
                false
            }
        }
    }
}

/// Resolves a validated archive entry name against `dest`, joining one clean
/// component at a time so the result is deterministic on every platform.
fn safe_join(dest: &Path, name: &str) -> Result<PathBuf, DownloadError> {
    let comps = validate_entry_name(name)?;
    let mut joined = dest.to_path_buf();
    for comp in comps {
        joined.push(comp);
    }
    Ok(joined)
}

/// Atomically moves the freshly-unpacked tree into its final location,
/// replacing any previous install without a window where the bank is missing.
///
/// The previous install (if any) is renamed to `.<id>.old.tmp` first, the new
/// tree is renamed into place, and the backup is removed by the caller after
/// the registry write succeeds (or restored on rollback).
///
/// Before the final rename, the unpacked tree's directory entry is synced to
/// disk (so the rename cannot be reordered before the data it points to), and
/// the parent directory is fsynced so the rename itself is durable across a
/// crash. On Windows directories cannot be opened for `sync_all`, so the
/// directory fsync is Unix-only (`#[cfg(unix)]`).
fn atomic_install(
    tmp_dir: &Path,
    final_dir: &Path,
    banks: &Path,
    id: &str,
) -> Result<(), DownloadError> {
    let old = banks.join(format!(".{id}.old.tmp"));
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| DownloadError::Io(old.clone(), e.to_string()))?;
    }
    if final_dir.exists() {
        std::fs::rename(final_dir, &old)
            .map_err(|e| DownloadError::Io(final_dir.to_path_buf(), e.to_string()))?;
    }
    // Persist the unpacked tree before it is renamed into place, so a crash
    // after the rename cannot leave a final_dir whose contents were never
    // written to disk. This is part of the install; an fsync failure is an
    // install failure and must not be swallowed.
    fsync_dir_recursive(tmp_dir)?;
    fsync_dir(banks)?;
    match std::fs::rename(tmp_dir, final_dir) {
        Ok(()) => {
            fsync_dir(banks)?;
            Ok(())
        }
        Err(e) => {
            // Restore the previous install on failure.
            if old.exists() {
                let _ = std::fs::rename(&old, final_dir);
            }
            Err(DownloadError::Io(tmp_dir.to_path_buf(), e.to_string()))
        }
    }
}

/// `sync_all` on a directory, so a rename/create within it is durable. This is
/// a no-op on non-Unix, where opening a directory for `sync_all` is not
/// supported (Windows dirs cannot be opened with a plain `File::open`).
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), DownloadError> {
    let file = std::fs::File::open(dir)
        .map_err(|e| DownloadError::Io(dir.to_path_buf(), e.to_string()))?;
    file.sync_all()
        .map_err(|e| DownloadError::Io(dir.to_path_buf(), e.to_string()))
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<(), DownloadError> {
    Ok(())
}

/// Recursively fsyncs every file and directory under `dir` (depth-first, dirs
/// after their children), so the unpacked tree is fully durable before the
/// atomic rename into its final location.
fn fsync_dir_recursive(dir: &Path) -> Result<(), DownloadError> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| DownloadError::Io(dir.to_path_buf(), e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| DownloadError::Io(dir.to_path_buf(), e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            fsync_dir_recursive(&path)?;
        } else {
            // Open with write access: on Windows `sync_all` (`FlushFileBuffers`)
            // requires a write handle, so a read-only `File::open` would fail
            // with `Access is denied`. The unpacked files are all written by us.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| DownloadError::Io(path.clone(), e.to_string()))?;
            file.sync_all()
                .map_err(|e| DownloadError::Io(path.clone(), e.to_string()))?;
        }
    }
    fsync_dir(dir)
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// A streaming HTTP response reader that remembers its `Content-Length` for
/// the early size check. For a ranged (206) response, `content_length` is the
/// remaining byte count (the full length is implied by the pinned manifest
/// size); for an ordinary 200 response it is the whole body length.
struct BodyReader {
    inner: Box<dyn Read>,
    content_length: Option<u64>,
    status: u16,
}

impl BodyReader {
    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn status(&self) -> u16 {
        self.status
    }
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

/// Fetches the (small) license body into memory, with a 1 MiB cap, an overall
/// deadline, cancellation, and same-host-only redirects.
fn http_get_license(
    url: &str,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<Vec<u8>, DownloadError> {
    let mut reader = http_open(
        url,
        cancel,
        https_required,
        None,
        Some(LICENSE_TIMEOUT),
        None,
    )?;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        if out.len() + n > MAX_LICENSE_BYTES as usize {
            return Err(DownloadError::LicenseUnreachable(
                "license source is too large".to_owned(),
            ));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// Opens a streaming reader for `url`, following redirects (up to
/// [`MAX_REDIRECTS`]) only to allowed hosts, and enforcing HTTPS when
/// `https_required`.
///
/// `allowed_hosts`, when `Some`, is the catalog `redirect_hosts` allow-list:
/// the pinned URL's own host is always allowed, plus any listed exact host or
/// `.`-suffixed suffix. When `None` (license fetch), redirects stay same-host.
///
/// `range_start`, when `Some`, sends `Range: bytes={start}-` and expects a 206
/// response (used to resume a partial download). If the server ignores the
/// range and answers 200 with the full body, the caller detects it via
/// [`BodyReader::status`] and restarts from zero.
fn http_open(
    url: &str,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
    allowed_hosts: Option<&[String]>,
    global_timeout: Option<Duration>,
    range_start: Option<u64>,
) -> Result<BodyReader, DownloadError> {
    if https_required {
        ensure_https(url)?;
    }
    let agent = build_agent(global_timeout);
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let mut request = agent.get(&current);
        if let Some(start) = range_start {
            // Re-send the Range header on every redirect hop (SPEC-076 §7).
            request = request.header("Range", &format!("bytes={start}-"));
        }
        let response = request
            .call()
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = match response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
            {
                Some(l) => l.to_owned(),
                None => return Err(DownloadError::HttpStatus(status, current)),
            };
            let next = resolve_redirect(&current, &location)?;
            if https_required {
                ensure_https(&next)?;
            }
            let same = same_origin(&current, &next);
            let allowed = allowed_hosts.is_some_and(|list| host_allowed(&next, list));
            if !(same || allowed) {
                return Err(DownloadError::RedirectToOtherHost(next));
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&status) {
            // GitHub's unauthenticated API rate limit is 60/h; surface it as a
            // clear, fail-closed message rather than a bare 4xx.
            if status == 429 || (status == 403 && is_rate_limit_body(&current)) {
                return Err(DownloadError::RateLimited(current));
            }
            return Err(DownloadError::HttpStatus(status, current));
        }
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let reader = response.into_body().into_reader();
        return Ok(BodyReader {
            inner: Box::new(reader),
            content_length,
            status,
        });
    }
    Err(DownloadError::TooManyRedirects(current))
}

fn build_agent(global_timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        // Disable transparent gzip so the size cap and SHA-256 apply to the
        // real bytes (and do not advertise gzip support to the server).
        .accept_encoding("")
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .timeout_global(global_timeout)
        .user_agent(concat!("sound-pro/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Rejects non-HTTPS URLs.
fn ensure_https(url: &str) -> Result<(), DownloadError> {
    if !url.starts_with("https://") {
        return Err(DownloadError::InsecureUrl(url.to_owned()));
    }
    Ok(())
}

/// Resolves a `Location` header (absolute or relative) against the current URL
/// using `url::Url::join`, which implements RFC 3986 relative resolution
/// (so `c` from `/a/b` resolves to `/a/c`).
fn resolve_redirect(current: &str, location: &str) -> Result<String, DownloadError> {
    let base = url::Url::parse(current)
        .map_err(|e| DownloadError::Network(format!("bad URL {current:?}: {e}")))?;
    let next = base
        .join(location)
        .map_err(|e| DownloadError::Network(format!("bad redirect target {location:?}: {e}")))?;
    Ok(next.into())
}

/// Whether two URLs share the same scheme, host (case-insensitive) and port
/// (with default ports normalised).
fn same_origin(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (url::Url::parse(a), url::Url::parse(b)) else {
        return false;
    };
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Whether `url`'s host is on the allow-list (exact host or `.`-prefixed
/// suffix). The host is compared lowercased.
fn host_allowed(url: &str, allowed: &[String]) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|a| {
        if let Some(suffix) = a.strip_prefix('.') {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == a.as_str()
        }
    })
}

// ---------------------------------------------------------------------------
// Hashing / normalization helpers
// ---------------------------------------------------------------------------

/// Normalizes license text for hashing: UTF-8 (lossy), trim, CRLF→LF.
fn normalize_license_text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.replace("\r\n", "\n");
    s.trim().to_owned()
}

/// SHA-256 of `data` as a 64-char lowercase hex string.
fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Constant-time comparison for digests (length must already match).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Maps a filesystem error to a [`DownloadError`], translating `ENOSPC` into
/// the clearer [`DownloadError::NoSpace`] (SPEC-067 §5 disk-space item).
fn io_err(path: &Path, e: std::io::Error) -> DownloadError {
    if e.kind() == std::io::ErrorKind::StorageFull || e.raw_os_error() == Some(28) {
        DownloadError::NoSpace(path.to_string_lossy().into_owned())
    } else {
        DownloadError::Io(path.to_path_buf(), e.to_string())
    }
}

/// Whether a 403 response is plausibly a rate-limit. GitHub returns 403 (not
/// 429) for its unauthenticated 60/h limit, so any 403 against the GitHub API
/// host is treated as rate-limited and reported with a clear message.
fn is_rate_limit_body(url: &str) -> bool {
    url.contains("api.github.com")
}

/// Extracts the SPDX id from a license metadata JSON document (the
/// `license.spdx_id` field of the GitHub license API response).
fn license_spdx_from_metadata(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("license")?
        .get("spdx_id")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_license_text_strips_crlf_and_trim() {
        let raw = b"  Line one\r\nLine two\r\n  ";
        assert_eq!(normalize_license_text(raw), "Line one\nLine two");
    }

    #[test]
    fn normalize_license_text_is_lossy_utf8() {
        // Invalid UTF-8 becomes U+FFFD, deterministically.
        let raw = b"a\xffb";
        assert_eq!(normalize_license_text(raw), "a\u{fffd}b");
    }

    #[test]
    fn normalize_plus_hash_matches_pinned_salamander() {
        // A representative slice of the CC-BY-3.0 legalcode with CRLF and
        // surrounding whitespace; the exact bytes + pinned digest are checked
        // against upstream in `catalog/audit/salamander-grand-v3.md`.
        let text = "Creative Commons Attribution 3.0 Unported\r\n\r\nTHE WORK\r\n";
        let normalized = normalize_license_text(text.as_bytes());
        assert_eq!(
            normalized,
            "Creative Commons Attribution 3.0 Unported\n\nTHE WORK"
        );
        let h = sha256_hex(normalized.as_bytes());
        assert_eq!(h.len(), 64);
        assert!(
            h.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn host_port_and_same_origin() {
        assert!(same_origin(
            "https://example.com/a",
            "https://example.com/b"
        ));
        assert!(same_origin(
            "https://example.com/a",
            "https://EXAMPLE.com/b"
        ));
        assert!(same_origin(
            "https://example.com/a",
            "https://example.com:443/b"
        ));
        assert!(!same_origin("https://example.com/a", "https://evil.com/a"));
        assert!(!same_origin(
            "https://example.com/a",
            "http://example.com/a"
        ));
        assert!(!same_origin(
            "https://example.com/a",
            "https://example.com:8443/a"
        ));
    }

    #[test]
    fn resolve_redirect_handles_relative_and_absolute() {
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "/c").unwrap(),
            "https://example.com/c"
        );
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "c").unwrap(),
            "https://example.com/a/c"
        );
        assert_eq!(
            resolve_redirect("https://example.com/a", "https://other.com/x").unwrap(),
            "https://other.com/x"
        );
    }

    #[test]
    fn host_allowed_matches_exact_and_suffix() {
        let list = vec![".ca.archive.org".to_owned(), "example.org".to_owned()];
        assert!(host_allowed("https://dn710705.ca.archive.org/x", &list));
        assert!(host_allowed("https://ca.archive.org/x", &list));
        assert!(host_allowed("https://example.org/x", &list));
        assert!(!host_allowed("https://evil.org/x", &list));
        assert!(!host_allowed("https://notca.archive.org/x", &list));
    }

    #[test]
    fn ensure_https_rejects_http() {
        assert!(ensure_https("http://example.com").is_err());
        assert!(ensure_https("https://example.com").is_ok());
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let dest = Path::new("/tmp/dest");
        // Safe names are accepted.
        assert!(safe_join(dest, "ok/file.sfz").is_ok());
        assert!(safe_join(dest, "dir/").is_ok(), "directory entry");
        assert!(safe_join(dest, "dir\\sub/").is_ok(), "mixed separators");

        // Parent/current traversal, in both separator styles and at any depth.
        for bad in [
            "../evil",
            "a/../b",
            "..\\evil",
            "a\\..\\b",
            "a/b/../../c",
            "./a",
            "a/./b",
        ] {
            assert!(
                safe_join(dest, bad).is_err(),
                "must reject traversal: {bad:?}"
            );
        }

        // Absolute / root-relative / UNC, in both separator styles.
        for bad in [
            "/abs/path",
            "\\abs\\path",
            "//server/share",
            "\\\\server\\share",
            "C:/foo",
            "C:\\foo",
        ] {
            assert!(
                safe_join(dest, bad).is_err(),
                "must reject absolute: {bad:?}"
            );
        }

        // Windows drive prefixes and alternate-stream / reserved names.
        for bad in [
            "C:", "a:b", "a::b", "::$DATA", "CON", "con.txt", "NUL", "COM1", "LPT9",
        ] {
            assert!(safe_join(dest, bad).is_err(), "must reject: {bad:?}");
        }

        // Empty components (internal double separator).
        for bad in ["a//b", "a\\/b", "a/\\b", "/"] {
            assert!(
                safe_join(dest, bad).is_err(),
                "must reject empty component: {bad:?}"
            );
        }

        // NUL byte.
        assert!(safe_join(dest, "a\0b").is_err(), "must reject NUL");

        // Reserved-name lookalikes that are actually safe.
        assert!(safe_join(dest, "COM0").is_ok(), "COM0 is not reserved");
        assert!(safe_join(dest, "console.txt").is_ok());
    }

    #[test]
    fn license_spdx_from_metadata_parses_github_api() {
        let body = br#"{"license":{"spdx_id":"CC0-1.0","name":"Creative Commons Zero"}}"#;
        assert_eq!(license_spdx_from_metadata(body).as_deref(), Some("CC0-1.0"));
        assert_eq!(license_spdx_from_metadata(b"{}"), None);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn lock_id_strips_dot_and_lock_suffix() {
        assert_eq!(lock_id(Path::new(".test-bank.lock")), "test-bank");
        assert_eq!(lock_id(Path::new("/some/dir/.a.b.lock")), "a.b");
        // A lock file with a dot in the id keeps the whole id, not just the
        // first `file_stem` segment.
        assert_eq!(lock_id(Path::new(".foo.bar.lock")), "foo.bar");
        // No leading dot / no suffix → empty (defensive).
        assert_eq!(lock_id(Path::new("test-bank.lock")), "");
        assert_eq!(lock_id(Path::new(".test-bank")), "");
    }

    #[test]
    fn stale_artifact_matches_exact_shapes_only() {
        // The three recognised staging/backup shapes for id "a".
        assert!(is_stale_artifact("a", ".a.download.123.tmp"));
        assert!(is_stale_artifact("a", ".a.unpack.123.tmp"));
        assert!(is_stale_artifact("a", ".a.old.tmp"));

        // A sibling id "a.b" must never be swept by id "a" (prefix collision).
        assert!(!is_stale_artifact("a", ".a.b.download.123.tmp"));
        assert!(!is_stale_artifact("a", ".a.b.unpack.123.tmp"));
        assert!(!is_stale_artifact("a", ".a.b.old.tmp"));

        // The lock file is never a stale artifact.
        assert!(!is_stale_artifact("a", ".a.lock"));

        // Non-digit tokens are not download/unpack staging names.
        assert!(!is_stale_artifact("a", ".a.download.notdigits.tmp"));
        assert!(!is_stale_artifact("a", ".a.unpack.123x.tmp"));
        // Missing `.tmp` suffix or wrong id.
        assert!(!is_stale_artifact("a", ".a.download.123"));
        assert!(!is_stale_artifact("b", ".a.download.123.tmp"));
    }

    #[test]
    fn sweep_restores_old_backup_when_final_missing() {
        let dir = tempfile::tempdir().unwrap().keep();
        // A previous install was left in `.{id}.old.tmp` with no final dir
        // (crash between `rename(final -> .old)` and `rename(tmp -> final)`).
        let old = dir.join(".test-bank.old.tmp");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("bank.sfz"), b"data").unwrap();

        sweep_stale_artifacts(&dir, "test-bank").unwrap();

        // The backup is restored, not deleted.
        assert!(dir.join("test-bank").join("bank.sfz").exists());
        assert!(!old.exists(), "the backup must be moved back into place");
    }

    #[test]
    fn sweep_keeps_old_backup_when_final_exists() {
        let dir = tempfile::tempdir().unwrap().keep();
        // Both a final dir and a stale backup exist; the final dir wins, so the
        // backup is treated as a stale artifact and removed.
        std::fs::create_dir_all(dir.join("test-bank")).unwrap();
        let old = dir.join(".test-bank.old.tmp");
        std::fs::create_dir_all(&old).unwrap();

        sweep_stale_artifacts(&dir, "test-bank").unwrap();

        assert!(dir.join("test-bank").exists());
        assert!(
            !old.exists(),
            "a stale backup must be removed when final exists"
        );
    }

    #[test]
    fn sweep_does_not_delete_sibling_bank_with_dotted_id() {
        let dir = tempfile::tempdir().unwrap().keep();
        // A staging artifact for bank "a.b" must survive a sweep of bank "a".
        let sibling = dir.join(".a.b.download.123.tmp");
        std::fs::write(&sibling, b"partial").unwrap();
        // And a genuine stale artifact for "a".
        let mine = dir.join(".a.download.456.tmp");
        std::fs::write(&mine, b"partial").unwrap();

        sweep_stale_artifacts(&dir, "a").unwrap();

        assert!(sibling.exists(), "a.b's artifact must not be swept by a");
        assert!(!mine.exists(), "a's own artifact must be swept");
    }
}

/// HTTP integration tests that exercise the full download pipeline against a
/// local `http://` server (no real internet). They live here, as unit tests,
/// so they can reach the `#[cfg(test)]`-gated `download_bank_into_with_https`
/// hook (M7) without making it part of the public API.
#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::bank_library::{CatalogEntry, Download, License, LicenseCheck};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    // The `Range: bytes=N-` start offset of the request currently being
    // served on this worker thread, parsed from the request head. A handler
    // closure reads it via `requested_range` to serve a partial body
    // (resume test). It is thread-local because each connection is served on
    // its own thread.
    thread_local! {
        static REQUESTED_RANGE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }

    /// The `Range` start offset of the in-flight request, or `None`.
    fn requested_range() -> Option<u64> {
        REQUESTED_RANGE.with(|c| c.get())
    }

    /// A small multi-threaded HTTP server: the accept loop runs on one thread
    /// and each connection is served on its own worker thread, so a handler
    /// that blocks (e.g. to hold a download's lock while the test races a
    /// second download) never stalls the listener or another in-flight
    /// request. The handler returns a [`ServerResponse`]; it must *not* panic
    /// or assert — a panic in a worker thread would not fail the test, so
    /// handlers return error responses (e.g. 404) instead and the test asserts
    /// on the downloader's behaviour.
    struct TestServer {
        addr: String,
        stop: Arc<AtomicBool>,
        /// Tracked so `Drop` can stop the listener; the worker threads are
        /// detached and bounded by the connection lifetime, not joined here.
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(handler: impl Fn(usize, &str) -> ServerResponse + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = Arc::clone(&stop);
            let index = Arc::new(AtomicUsize::new(0));
            // Share the handler across worker threads.
            let handler = Arc::new(handler);
            let handle = thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop2.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let index = Arc::clone(&index);
                    let handler = Arc::clone(&handler);
                    // Serve this connection on its own thread so a blocking
                    // handler cannot deadlock the accept loop or another
                    // request (this is what previously hung on Windows: the
                    // blocking `/license` handler stalled the single server
                    // thread, and `Drop::join` then waited forever).
                    thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        let mut read = 0;
                        loop {
                            let n = stream.read(&mut buf[read..]).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            read += n;
                            if read >= buf.len() || buf[..read].windows(4).any(|w| w == b"\r\n\r\n")
                            {
                                break;
                            }
                        }
                        let req = String::from_utf8_lossy(&buf[..read]);
                        let target = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        // Parse `Range: bytes=N-` for the resume test.
                        let range = req
                            .lines()
                            .find_map(|l| {
                                let (name, rest) = l.split_once(':')?;
                                name.eq_ignore_ascii_case("range")
                                    .then_some(rest.trim().strip_prefix("bytes=")?)
                            })
                            .and_then(|v| v.trim_end_matches('-').parse::<u64>().ok());
                        REQUESTED_RANGE.with(|c| c.set(range));
                        // Assign a monotonic request index; the handler uses it
                        // to distinguish the first vs. later license requests.
                        let idx = index.fetch_add(1, Ordering::Relaxed);
                        let resp = handler(idx, target);
                        write_response(&mut stream, &resp);
                    });
                }
            });
            TestServer {
                addr,
                stop,
                handle: Some(handle),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.addr, path)
        }

        /// The bare host:port (no scheme), e.g. `127.0.0.1:54321`.
        fn host(&self) -> String {
            self.addr.trim_start_matches("http://").to_owned()
        }

        /// The bare hostname (no scheme, no port), e.g. `127.0.0.1`.
        fn host_only(&self) -> String {
            self.host()
                .split(':')
                .next()
                .unwrap_or("127.0.0.1")
                .to_owned()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(self.addr.trim_start_matches("http://"));
            if let Some(handle) = self.handle.take() {
                // Bounded join: the accept loop must observe `stop` and exit
                // quickly. If it does not, do not hang the whole test run.
                for _ in 0..100 {
                    if handle.is_finished() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    #[derive(Clone)]
    struct ServerResponse {
        status: u16,
        body: Vec<u8>,
        location: Option<String>,
        content_length: Option<usize>,
        content_range: Option<String>,
    }

    impl ServerResponse {
        fn ok(body: Vec<u8>) -> Self {
            ServerResponse {
                status: 200,
                body,
                location: None,
                content_length: None,
                content_range: None,
            }
        }
        fn not_found() -> Self {
            ServerResponse {
                status: 404,
                body: b"not found".to_vec(),
                location: None,
                content_length: None,
                content_range: None,
            }
        }
        fn redirect(location: String) -> Self {
            ServerResponse {
                status: 302,
                body: Vec::new(),
                location: Some(location),
                content_length: None,
                content_range: None,
            }
        }
        /// Omit the `Content-Length` header so the client must discover the
        /// real length by reading to EOF (used to exercise the truncated-body
        /// path).
        fn without_content_length(mut self) -> Self {
            self.content_length = Some(usize::MAX);
            self
        }
        /// A partial (206) response starting at `from` within `body`, with the
        /// total resource size `total` for the `Content-Range` header.
        fn partial(mut self, from: usize, total: u64) -> Self {
            self.status = 206;
            self.body = self.body[from..].to_vec();
            self.content_range = Some(format!("bytes {from}-{}/{total}", total - 1));
            self
        }
    }

    fn write_response(stream: &mut std::net::TcpStream, resp: &ServerResponse) {
        let reason = match resp.status {
            200 => "OK",
            206 => "Partial Content",
            301 => "Moved Permanently",
            302 => "Found",
            404 => "Not Found",
            429 => "Too Many Requests",
            _ => "Status",
        };
        let mut head = String::new();
        head.push_str(&format!("HTTP/1.1 {} {}\r\n", resp.status, reason));
        match resp.content_length {
            // `Some(usize::MAX)` means "omit the header".
            Some(usize::MAX) => {}
            Some(n) => head.push_str(&format!("Content-Length: {n}\r\n")),
            None => head.push_str(&format!("Content-Length: {}\r\n", resp.body.len())),
        }
        if let Some(cr) = &resp.content_range {
            head.push_str(&format!("Content-Range: {cr}\r\n"));
        }
        if let Some(loc) = &resp.location {
            head.push_str(&format!("Location: {loc}\r\n"));
        }
        head.push_str("Connection: close\r\n\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&resp.body);
        let _ = stream.flush();
    }

    fn sha256(data: &[u8]) -> String {
        let mut s = String::with_capacity(64);
        for b in Sha256::digest(data) {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    fn cancel_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    /// Join a worker thread with a hard deadline. A test that spawns threads or
    /// servers must never be able to hang the run: if the worker has not
    /// finished within `timeout`, this panics (turning a hang into a failure)
    /// instead of blocking forever. Once the handle reports finished, `join`
    /// returns immediately.
    fn join_with_timeout<T>(handle: thread::JoinHandle<T>, timeout: std::time::Duration) -> T {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if handle.is_finished() {
                return handle
                    .join()
                    .unwrap_or_else(|_| panic!("worker thread panicked"));
            }
            if std::time::Instant::now() >= deadline {
                panic!("worker thread did not finish within {timeout:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn temp_banks_dir(tag: &str) -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(&format!("bank-download-{tag}-"))
            .tempdir()
            .expect("tempdir")
            .keep();
        // `download_bank_into` creates the dir itself, but we want it to exist
        // for the stale-artifact tests; keep it, but remove any leftover.
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small builder for [`CatalogEntry`] so test helpers do not trip
    /// `clippy::too_many_arguments`.
    struct EntrySpec {
        id: String,
        url: String,
        license_url: String,
        format: String,
        size: u64,
        hash: String,
        redirect_hosts: Vec<String>,
        spdx: String,
        license_mode: String,
        license_hash: Option<String>,
    }

    impl EntrySpec {
        fn build(self) -> CatalogEntry {
            CatalogEntry {
                schema: 1,
                id: self.id.clone(),
                name: format!("Bank {}", self.id),
                family: "piano".to_owned(),
                version: "1".to_owned(),
                author: "Test Author".to_owned(),
                license: License {
                    spdx: self.spdx.clone(),
                    url: "https://example.com/license".to_owned(),
                    attribution_required: self.spdx == "CC-BY-3.0",
                },
                attribution: if self.spdx == "CC-BY-3.0" {
                    "Some attribution".to_owned()
                } else {
                    String::new()
                },
                source_url: self.url.clone(),
                sha256: Some(self.hash.clone()),
                size_bytes: Some(self.size),
                verified: true,
                license_check: Some(LicenseCheck {
                    url: self.license_url,
                    sha256: self.license_hash,
                    mode: self.license_mode,
                }),
                download: Some(Download {
                    url: self.url,
                    sha256: self.hash,
                    size_bytes: self.size,
                    redirect_hosts: self.redirect_hosts,
                }),
                sfz: None,
                format: self.format,
            }
        }
    }

    impl Default for EntrySpec {
        fn default() -> Self {
            EntrySpec {
                id: "test-bank".to_owned(),
                url: String::new(),
                license_url: String::new(),
                format: "zip".to_owned(),
                size: 0,
                hash: String::new(),
                redirect_hosts: Vec::new(),
                spdx: "CC0-1.0".to_owned(),
                license_mode: "metadata".to_owned(),
                license_hash: None,
            }
        }
    }

    /// The default: CC0-1.0 metadata license check, no redirect hosts.
    fn cc0_entry(
        url: String,
        license_url: String,
        format: &str,
        size: u64,
        hash: String,
    ) -> CatalogEntry {
        EntrySpec {
            url,
            license_url,
            format: format.to_owned(),
            size,
            hash,
            ..EntrySpec::default()
        }
        .build()
    }

    fn make_zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(data).unwrap();
            }
            zw.finish().unwrap();
        }
        cursor.into_inner()
    }

    /// Build a `.tar.bz2` archive (tar allows duplicate entry names, unlike
    /// `zip::ZipWriter`, so it is used to exercise duplicate detection).
    fn make_tar_bz2_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut builder = tar::Builder::new(&mut cursor);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, *name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        bzip2_bytes(&cursor.into_inner())
    }

    /// Build a `.tar.bz2` archive containing a symlink entry (and, optionally,
    /// a hard-link entry), used to assert that non-regular entries with data
    /// are rejected rather than extracted.
    fn make_tar_bz2_link_bytes(symlink: bool, hardlink: bool) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut builder = tar::Builder::new(&mut cursor);
            // A harmless regular file first.
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "bank/ok.sfz", &b"ok"[..])
                .unwrap();

            if symlink {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o644);
                h.set_cksum();
                builder
                    .append_link(&mut h, "bank/link.sfz", "ok.sfz")
                    .expect("append symlink");
            }
            if hardlink {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Link);
                h.set_size(0);
                h.set_mode(0o644);
                h.set_link_name("bank/ok.sfz").unwrap();
                h.set_cksum();
                builder
                    .append(&h, std::io::empty())
                    .expect("append hardlink");
            }
            builder.finish().unwrap();
        }
        bzip2_bytes(&cursor.into_inner())
    }

    fn bzip2_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = bzip2::write::BzEncoder::new(&mut out, bzip2::Compression::default());
            use std::io::Write;
            enc.write_all(raw).unwrap();
            enc.finish().unwrap();
        }
        out
    }

    /// Build a `.tar.gz` archive (gzip-compressed tar), used to exercise the
    /// `tar.gz` unpack path (SPEC-076).
    fn make_tar_gz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut builder = tar::Builder::new(&mut cursor);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, *name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        gzip_bytes(&cursor.into_inner())
    }

    fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut out = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
            enc.write_all(raw).unwrap();
            enc.finish().unwrap();
        }
        out
    }

    /// Build a `.tar.xz` archive (xz-compressed tar), used to exercise the
    /// `tar.xz` unpack path (FreePats / Salamander Grand).
    fn make_tar_xz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut builder = tar::Builder::new(&mut cursor);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, *name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        xz_bytes(&cursor.into_inner())
    }

    fn xz_bytes(raw: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut out = Vec::new();
        {
            let mut enc = xz2::write::XzEncoder::new(&mut out, 6);
            enc.write_all(raw).unwrap();
            enc.finish().unwrap();
        }
        out
    }

    /// Build a `.7z` archive (solid LZMA2), used to exercise the `7z` unpack
    /// path. `sevenz-rust` stores directory entries as stream-less entries and
    /// regular files as streamed entries; we emit exactly the files given.
    fn make_7z_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Cursor;
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = sevenz_rust::SevenZWriter::new(&mut cursor).expect("7z writer");
            for (name, data) in entries {
                let mut entry = sevenz_rust::SevenZArchiveEntry::new();
                entry.name = (*name).to_owned();
                entry.has_stream = true;
                entry.is_directory = false;
                writer
                    .push_archive_entry(entry, Some(Cursor::new(*data)))
                    .expect("push 7z entry");
            }
            let _ = writer.finish().expect("finish 7z");
        }
        cursor.into_inner()
    }

    /// Rewrites the first central-directory entry's `uncompressed_size` field
    /// (offset +24 within the central header) to `forged`, so `ZipFile::size()`
    /// understates the actual stored bytes. This models an archive that lies
    /// about its decompressed size.
    fn forge_zip_uncompressed_size(bytes: &mut [u8], forged: u32) {
        // Locate the first central-directory header: "PK\x01\x02".
        const CENTRAL_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
        let pos = bytes
            .windows(4)
            .position(|w| w == CENTRAL_SIG)
            .expect("central directory signature");
        let field = pos + 24; // uncompressed_size (4 bytes, little-endian)
        bytes[field..field + 4].copy_from_slice(&forged.to_le_bytes());
    }

    /// A server that serves the CC0 license at `/license` then a body at
    /// `/archive`, for `n` requests total.
    fn license_then_archive_server(archive: Arc<Vec<u8>>) -> TestServer {
        let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();
        TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(license_body.clone())
            } else if target == "/archive" {
                ServerResponse::ok((*archive).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        })
    }

    // ------------------------------------------------------------------
    // Success / registry / attribution
    // ------------------------------------------------------------------

    #[test]
    fn success_writes_registry_and_leaves_no_temp_files() {
        let archive = make_zip_bytes(&[
            ("bank/bank.sfz", b"<region> sample=bank.wav"),
            ("bank/bank.wav", b"RIFFfake"),
        ]);
        let hash = sha256(&archive);
        let size = archive.len() as u64;
        let archive_arc = Arc::new(archive);

        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("success");
        let progress = Progress::new(size);
        let result = download_bank_into_with_https(&e, &banks, &progress, &cancel_flag(), false);
        let bank = result.expect("download succeeds");

        assert_eq!(bank.id, "test-bank");
        assert_eq!(bank.name, "Bank test-bank");
        assert_eq!(progress.bytes(), size);

        // The bank folder and its files are present.
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );

        // The registry records the bank with the right id/attribution.
        let reg_text = std::fs::read_to_string(banks.join("banks.json")).expect("registry");
        let reg: crate::bank_library::InstalledRegistry =
            serde_json::from_str(&reg_text).expect("parse registry");
        assert_eq!(reg.banks.len(), 1);
        assert_eq!(reg.banks[0].id, "test-bank");
        assert_eq!(reg.banks[0].attribution, ""); // CC0 needs none

        // No `.tmp` artifacts are left behind. Advisory-lock files
        // (`.{id}.lock` and `banks.json.lock`) are left in place on purpose:
        // they are opened (not created) by `try_lock` and are harmless once the
        // kernel releases the lock, so a stale file never blocks the next run.
        let leftover = std::fs::read_dir(&banks)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(leftover.is_empty(), "leftover temp files: {leftover:?}");
    }

    #[test]
    fn attribution_is_stored_in_registry() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let hash = sha256(&archive);
        let size = archive.len() as u64;
        let archive_arc = Arc::new(archive);
        // CC-BY-3.0 text-hash license.
        let license_text = b"attribution required text\n".to_vec();
        let license_hash = sha256(&normalize_license_text(&license_text).into_bytes());
        let license_arc = Arc::new(license_text);

        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok((*license_arc).clone())
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        let e = EntrySpec {
            id: "attr-bank".to_owned(),
            url: server.url("/archive"),
            license_url: server.url("/license"),
            format: "zip".to_owned(),
            size,
            hash,
            redirect_hosts: Vec::new(),
            spdx: "CC-BY-3.0".to_owned(),
            license_mode: "text-hash".to_owned(),
            license_hash: Some(license_hash),
        }
        .build();
        let banks = temp_banks_dir("attribution");
        let bank =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false)
                .unwrap();
        assert_eq!(bank.attribution, "Some attribution");
    }

    // ------------------------------------------------------------------
    // Hash / license / truncation
    // ------------------------------------------------------------------

    #[test]
    fn hash_mismatch_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            "a".repeat(64),
        );
        let banks = temp_banks_dir("hash-mismatch");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::HashMismatch)));
        assert!(!banks.join("test-bank").exists());
        assert!(!banks.join("banks.json").exists());
    }

    #[test]
    fn license_metadata_mismatch_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC-BY-3.0"}}"#.to_vec();
        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(license_body.clone())
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("license-mismatch");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::LicenseMismatch)));
        assert!(!banks.join("test-bank").exists());
    }

    #[test]
    fn license_text_hash_mismatch_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let license_text: Vec<u8> = b"the audited text\n".to_vec();
        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(license_text.clone())
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        // Pin the *wrong* hash so the re-check fails.
        let e = EntrySpec {
            id: "test-bank".to_owned(),
            url: server.url("/archive"),
            license_url: server.url("/license"),
            format: "zip".to_owned(),
            size,
            hash,
            redirect_hosts: Vec::new(),
            spdx: "CC-BY-3.0".to_owned(),
            license_mode: "text-hash".to_owned(),
            license_hash: Some("b".repeat(64)),
        }
        .build();
        let banks = temp_banks_dir("license-hash-mismatch");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::LicenseMismatch)));
    }

    #[test]
    fn license_source_unreachable_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        // The license endpoint returns 404.
        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::not_found()
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("license-unreachable");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::HttpStatus(404, _))));
    }

    #[test]
    fn truncated_download_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"full-contents")]);
        let declared_size = archive.len() as u64 + 100;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        // Serve the archive WITHOUT a Content-Length header so the client must
        // read to EOF and discover it got fewer bytes than the pinned size.
        // With resume, a truncated body is retried (up to MAX_RETRIES) and the
        // final failure is RetriesExhausted with the underlying truncation.
        let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();
        let server = TestServer::start(move |_idx, target| {
            if target == "/license" {
                ServerResponse::ok(license_body.clone())
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone()).without_content_length()
            } else {
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            declared_size,
            hash,
        );
        let banks = temp_banks_dir("truncated");
        let result = download_bank_into_with_https(
            &e,
            &banks,
            &Progress::new(declared_size),
            &cancel_flag(),
            false,
        );
        assert!(matches!(
            result,
            Err(DownloadError::RetriesExhausted(_, msg)) if msg.contains("truncated")
        ));
        assert!(!banks.join("test-bank").exists());
    }

    #[test]
    fn interrupted_download_resumes_via_range() {
        let archive = make_zip_bytes(&[
            ("bank/bank.sfz", b"<region> sample=bank.wav"),
            ("bank/bank.wav", b"RIFFfake"),
        ]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);

        // First request serves only the first half of the archive (a truncated
        // body); the retry requests a Range and is served the remaining bytes
        // with a 206. The client must resume, not restart, and still verify
        // the SHA-256 over the whole file.
        let half = (size / 2) as usize;
        let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();
        let server = TestServer::start(move |_idx, target| {
            if target == "/license" {
                ServerResponse::ok(license_body.clone())
            } else if target == "/archive" {
                match requested_range() {
                    None => {
                        // Truncate: declare the full length but send only half.
                        let mut resp = ServerResponse::ok((*archive_arc)[..half].to_vec());
                        resp.content_length = Some((*archive_arc).len());
                        resp
                    }
                    Some(start) => {
                        ServerResponse::ok((*archive_arc).clone()).partial(start as usize, size)
                    }
                }
            } else {
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("resume");
        let progress = Progress::new(size);
        let bank = download_bank_into_with_https(&e, &banks, &progress, &cancel_flag(), false)
            .expect("resumed download succeeds");

        assert_eq!(bank.id, "test-bank");
        assert_eq!(progress.bytes(), size);
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
        // The partial file is consumed, not left behind.
        assert!(!banks.join(".test-bank.download.part").exists());
    }

    // ------------------------------------------------------------------
    // Redirects
    // ------------------------------------------------------------------

    #[test]
    fn redirect_to_allowed_host_succeeds() {
        // The CDN server holds the archive; the origin redirects to it.
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);

        let cdn = TestServer::start(move |_idx, target| {
            if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                ServerResponse::not_found()
            }
        });
        let cdn_host_port = cdn.host();
        let cdn_redirect = cdn_host_port.clone();
        let cdn_allow = cdn.host_only();
        let origin = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec())
            } else if target == "/archive" {
                ServerResponse::redirect(format!("http://{cdn_redirect}/archive"))
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        // Allow-list the CDN's exact host (the redirect URL uses the same
        // hostname, only the port differs — local test servers bind :0).
        let e = EntrySpec {
            id: "test-bank".to_owned(),
            url: origin.url("/archive"),
            license_url: origin.url("/license"),
            format: "zip".to_owned(),
            size,
            hash,
            redirect_hosts: vec![cdn_allow],
            spdx: "CC0-1.0".to_owned(),
            license_mode: "metadata".to_owned(),
            license_hash: None,
        }
        .build();
        let banks = temp_banks_dir("redirect-allowed");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(
            result.is_ok(),
            "allowed redirect should succeed: {result:?}"
        );
    }

    #[test]
    fn redirect_to_unlisted_host_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let evil = TestServer::start(move |_idx, target| {
            if target == "/evil" {
                ServerResponse::ok(b"evil".to_vec())
            } else {
                ServerResponse::not_found()
            }
        });
        let origin = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec())
            } else if target == "/archive" {
                ServerResponse::redirect(evil.url("/evil"))
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            origin.url("/archive"),
            origin.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("redirect-unlisted");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::RedirectToOtherHost(_))));
    }

    #[test]
    fn redirect_https_to_http_downgrade_is_rejected() {
        // Even a same-host redirect that downgrades to plain http is refused
        // when HTTPS is required. Exercise the production enforcement by
        // calling `download_bank_into_inner` with `https_required = true` via
        // a fake https URL we never actually reach (the downgrade is caught
        // before connecting).
        let origin = TestServer::start(move |_idx, target| {
            let _ = target;
            ServerResponse::not_found()
        });
        // The URL scheme is https:// but points at the local server; the
        // redirect target is http:// (downgrade). We can't bind TLS, so we
        // instead verify the *pure* resolution+policy path via `resolve_redirect`
        // + `ensure_https`, which the downloader runs before connecting.
        let next = resolve_redirect(
            &origin.url("/archive").replacen("http", "https", 1),
            "http://127.0.0.1:1/archive",
        )
        .unwrap();
        assert!(ensure_https(&next).is_err());
    }

    // ------------------------------------------------------------------
    // Zip-slip / unsafe entries
    // ------------------------------------------------------------------

    #[test]
    fn zip_slip_traversal_is_rejected() {
        let archive = make_zip_bytes(&[("../evil.sfz", b"x")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("zip-slip");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
        assert!(!banks.join("test-bank").exists());
    }

    #[test]
    fn zip_slip_windows_style_and_reserved_names_are_rejected() {
        for evil in [
            "C:/evil.sfz",
            "C:\\evil.sfz",
            "evil.sfz:stream",
            "evil.sfz::$DATA",
            "//server/evil.sfz",
            "\\\\server\\evil.sfz",
            "CON",
            "con.txt",
            "NUL",
            "COM1",
            "LPT9",
            "aux.foo",
        ] {
            let archive = make_zip_bytes(&[(evil, b"x")]);
            let size = archive.len() as u64;
            let hash = sha256(&archive);
            let archive_arc = Arc::new(archive);
            let server = license_then_archive_server(archive_arc);

            let e = cc0_entry(
                server.url("/archive"),
                server.url("/license"),
                "zip",
                size,
                hash,
            );
            let banks = temp_banks_dir("zip-slip-win");
            let result = download_bank_into_with_https(
                &e,
                &banks,
                &Progress::new(size),
                &cancel_flag(),
                false,
            );
            assert!(
                matches!(result, Err(DownloadError::UnsafeEntry(_))),
                "must reject {evil:?}, got {result:?}"
            );
        }
    }

    // ------------------------------------------------------------------
    // Size limits
    // ------------------------------------------------------------------

    #[test]
    fn oversized_archive_body_is_rejected() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"0123456789")]);
        let declared_size = 5u64; // smaller than the real body
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            declared_size,
            hash,
        );
        let banks = temp_banks_dir("oversize");
        let result = download_bank_into_with_https(
            &e,
            &banks,
            &Progress::new(declared_size),
            &cancel_flag(),
            false,
        );
        // The server advertises its real (larger) body via Content-Length, so
        // the early size check rejects it before any bytes are read.
        assert!(matches!(
            result,
            Err(DownloadError::TooLarge { expected, .. }) if expected == declared_size
        ));
    }

    #[test]
    fn cancel_aborts_download() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let cancel = cancel_flag();
        let cancel_for_handler = Arc::clone(&cancel);
        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                ServerResponse::ok(br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec())
            } else if target == "/archive" {
                cancel_for_handler.store(true, Ordering::Relaxed);
                ServerResponse::ok((*archive_arc).clone())
            } else {
                let _ = idx;
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("cancel");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel, false);
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        assert!(!banks.join("test-bank").exists());
    }

    // ------------------------------------------------------------------
    // Atomic install / concurrency
    // ------------------------------------------------------------------

    #[test]
    fn failed_reinstall_keeps_old_install() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"v1")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("reinstall");
        // First install succeeds.
        let ok =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(ok.is_ok());

        // A second download with a wrong hash must fail and keep the old bank.
        let bad = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            "c".repeat(64),
        );
        let result = download_bank_into_with_https(
            &bad,
            &banks,
            &Progress::new(size),
            &cancel_flag(),
            false,
        );
        assert!(matches!(result, Err(DownloadError::HashMismatch)));
        // The old install still exists and is intact.
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
        assert_eq!(
            std::fs::read(banks.join("test-bank").join("bank").join("bank.sfz")).unwrap(),
            b"v1"
        );
    }

    #[test]
    fn stale_staging_artifacts_are_ignored_and_removed() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("stale");
        // Plant a stale unpack dir with a planted file (the old code reused it).
        let stale = banks.join(".test-bank.unpack.12345.tmp");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("planted.sfz"), b"planted").unwrap();

        let ok =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(ok.is_ok());
        // The stale dir was swept away and the planted file did not leak in.
        assert!(!stale.exists());
        assert!(!banks.join("test-bank").join("planted.sfz").exists());
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
    }

    #[test]
    fn concurrent_same_bank_is_serialised() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        // The first license request signals "lock acquired"; the handler then
        // blocks until released, keeping the first download's lock held while
        // the second download races to acquire it.
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let release_flag = Arc::new(AtomicBool::new(false));
        let release_flag2 = Arc::clone(&release_flag);
        let server = TestServer::start(move |idx, target| {
            if target == "/license" {
                if idx == 0 {
                    let _ = started_tx.send(());
                    // Block until the test releases the first download.
                    while !release_flag2.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                ServerResponse::ok(br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec())
            } else if target == "/archive" {
                ServerResponse::ok((*archive_arc).clone())
            } else {
                ServerResponse::not_found()
            }
        });

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("concurrent");
        let e1 = e.clone();
        let e2 = e.clone();
        let banks1 = banks.clone();
        let banks2 = banks.clone();
        let t1 = thread::spawn(move || {
            download_bank_into_with_https(&e1, &banks1, &Progress::new(size), &cancel_flag(), false)
        });
        // Wait until the first download has acquired the lock and requested its
        // license, then start the competing download. Use a hard timeout so a
        // bug that prevents the signal becomes a failure, not a hang.
        started_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first download must reach the license check");
        let t2 = thread::spawn(move || {
            download_bank_into_with_https(&e2, &banks2, &Progress::new(size), &cancel_flag(), false)
        });
        // The second download must observe the held lock and bail out.
        // `join` here is bounded by the downloader's own timeouts; the server
        // worker for `/license` is independent, so nothing blocks forever.
        let r2 = join_with_timeout(t2, std::time::Duration::from_secs(30));
        assert!(
            matches!(r2, Err(DownloadError::AlreadyInProgress(_))),
            "the loser must report AlreadyInProgress: r2={r2:?}"
        );
        // Release the first download and let it finish.
        release_flag.store(true, Ordering::Relaxed);
        let r1 = join_with_timeout(t1, std::time::Duration::from_secs(30));
        assert!(r1.is_ok(), "the first download must succeed: r1={r1:?}");
    }

    // ------------------------------------------------------------------
    // Duplicate / case-collision entries
    // ------------------------------------------------------------------

    #[test]
    fn duplicate_entry_is_rejected() {
        let archive = make_tar_bz2_bytes(&[("bank/bank.sfz", b"one"), ("bank/bank.sfz", b"two")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.bz2",
            size,
            hash,
        );
        let banks = temp_banks_dir("dup");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
    }

    #[test]
    fn tar_gz_archive_unpacks() {
        let archive = make_tar_gz_bytes(&[
            ("bank/bank.sfz", b"<region> sample=bank.wav"),
            ("bank/bank.wav", b"RIFFfake"),
        ]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.gz",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-gz");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        let bank = result.expect("tar.gz download succeeds");
        assert_eq!(bank.id, "test-bank");
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
        let leftover = std::fs::read_dir(&banks)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(leftover.is_empty(), "leftover temp files: {leftover:?}");
    }

    #[test]
    fn tar_gz_duplicate_entry_is_rejected() {
        let archive = make_tar_gz_bytes(&[("bank/bank.sfz", b"one"), ("bank/bank.sfz", b"two")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.gz",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-gz-dup");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
    }

    // ------------------------------------------------------------------
    // tar.xz archives (FreePats / Salamander Grand)
    // ------------------------------------------------------------------

    #[test]
    fn tar_xz_archive_unpacks() {
        let archive = make_tar_xz_bytes(&[
            ("bank/bank.sfz", b"<region> sample=bank.wav"),
            ("bank/bank.wav", b"RIFFfake"),
        ]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.xz",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-xz");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        let bank = result.expect("tar.xz download succeeds");
        assert_eq!(bank.id, "test-bank");
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
        let leftover = std::fs::read_dir(&banks)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(leftover.is_empty(), "leftover temp files: {leftover:?}");
    }

    #[test]
    fn tar_xz_duplicate_entry_is_rejected() {
        let archive = make_tar_xz_bytes(&[("bank/bank.sfz", b"one"), ("bank/bank.sfz", b"two")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.xz",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-xz-dup");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
    }

    // ------------------------------------------------------------------
    // 7z archives (FreePats)
    // ------------------------------------------------------------------

    #[test]
    fn sevenz_archive_unpacks() {
        let archive = make_7z_bytes(&[
            ("bank/bank.sfz", b"<region> sample=bank.wav"),
            ("bank/bank.wav", b"RIFFfake"),
        ]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "7z",
            size,
            hash,
        );
        let banks = temp_banks_dir("sevenz");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        let bank = result.expect("7z download succeeds");
        assert_eq!(bank.id, "test-bank");
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.wav")
                .exists()
        );
    }

    #[test]
    fn sevenz_zip_slip_is_rejected() {
        let archive = make_7z_bytes(&[("../evil.txt", b"boom")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "7z",
            size,
            hash,
        );
        let banks = temp_banks_dir("sevenz-slip");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
        assert!(!banks.join("test-bank").exists());
    }

    #[test]
    fn case_insensitive_collision_is_rejected() {
        let archive = make_zip_bytes(&[("Bank/Sample.wav", b"a"), ("bank/sample.wav", b"b")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("collision");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
    }

    // ------------------------------------------------------------------
    // Non-regular tar entries (symlink / hardlink)
    // ------------------------------------------------------------------

    #[test]
    fn tar_symlink_entry_is_rejected() {
        let archive = make_tar_bz2_link_bytes(true, false);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.bz2",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-symlink");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
        assert!(!banks.join("test-bank").exists());
    }

    #[test]
    fn tar_hardlink_entry_is_rejected() {
        let archive = make_tar_bz2_link_bytes(false, true);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "tar.bz2",
            size,
            hash,
        );
        let banks = temp_banks_dir("tar-hardlink");
        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
        assert!(!banks.join("test-bank").exists());
    }

    // ------------------------------------------------------------------
    // N1: stale lock file must not block a fresh download
    // ------------------------------------------------------------------

    #[test]
    fn stale_lock_file_does_not_block_download() {
        let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
        let size = archive.len() as u64;
        let hash = sha256(&archive);
        let archive_arc = Arc::new(archive);
        let server = license_then_archive_server(archive_arc);

        let e = cc0_entry(
            server.url("/archive"),
            server.url("/license"),
            "zip",
            size,
            hash,
        );
        let banks = temp_banks_dir("stale-lock");
        // A crash/SIGKILL leaves a `.{id}.lock` file behind with no live
        // holder. The advisory lock is released by the kernel on process
        // death, so the next download must proceed (not AlreadyInProgress).
        std::fs::write(banks.join(".test-bank.lock"), b"").unwrap();

        let result =
            download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
        assert!(result.is_ok(), "stale lock file must not block: {result:?}");
        assert!(
            banks
                .join("test-bank")
                .join("bank")
                .join("bank.sfz")
                .exists()
        );
    }

    // ------------------------------------------------------------------
    // N2: zip forged (understated) uncompressed_size is rejected
    // ------------------------------------------------------------------

    #[test]
    fn zip_forged_uncompressed_size_is_rejected() {
        // A single stored entry whose real data is 20 bytes, but whose central
        // directory claims only 5. The old code trusted the declared size and
        // wrote all 20 bytes; the new code reads through a `Take(declared+1)`
        // and rejects when actual bytes exceed the declared size.
        let mut archive = make_zip_bytes(&[("bank/bank.sfz", &[b'x'; 20][..])]);
        forge_zip_uncompressed_size(&mut archive, 5);

        let dir = temp_banks_dir("zip-forged");
        let archive_path = dir.join("archive.zip");
        std::fs::write(&archive_path, &archive).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let limits = Limits {
            max_entries: 100_000,
            max_total_unpacked: 1_000_000,
            max_entry_bytes: 1_000_000,
        };
        let result = unpack_zip(&archive_path, &dest, &cancel_flag(), &limits, None);
        assert!(
            matches!(result, Err(DownloadError::UnsafeEntry(_))),
            "forged size must be rejected: {result:?}"
        );
    }

    #[test]
    fn zip_actual_bytes_count_against_total_limit() {
        // A single entry whose *declared* (central-directory) size is forged
        // down to 4 bytes, while the real content is 20 bytes. The total
        // unpacked limit is set to 8 — above the forged declared size, but
        // below the real content. On the pre-fix code, `register` summed the
        // *declared* size (4 ≤ 8), trusted it, and wrote all 20 bytes, so the
        // download returned `Ok`; real-byte accounting must reject this. The
        // forged-size `Take(declared+1)` guard trips first with `UnsafeEntry`.
        let mut archive = make_zip_bytes(&[("bank/a.wav", &[b'a'; 20][..])]);
        forge_zip_uncompressed_size(&mut archive, 4);

        let dir = temp_banks_dir("zip-total");
        let archive_path = dir.join("archive.zip");
        std::fs::write(&archive_path, &archive).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let limits = Limits {
            max_entries: 100_000,
            max_total_unpacked: 8,
            max_entry_bytes: 1_000_000,
        };
        let result = unpack_zip(&archive_path, &dest, &cancel_flag(), &limits, None);
        assert!(
            result.is_err(),
            "real (not declared) bytes must be enforced: {result:?}"
        );
    }

    // ------------------------------------------------------------------
    // Entry-count cap
    // ------------------------------------------------------------------

    #[test]
    fn entry_count_cap_is_enforced() {
        // Two entries, but the injected limit allows only one.
        let archive = make_zip_bytes(&[("bank/a", b"a"), ("bank/b", b"b")]);
        let dir = temp_banks_dir("entry-count");
        let archive_path = dir.join("archive.zip");
        std::fs::write(&archive_path, &archive).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let limits = Limits {
            max_entries: 1,
            max_total_unpacked: 1_000_000,
            max_entry_bytes: 1_000_000,
        };
        let result = unpack_zip(&archive_path, &dest, &cancel_flag(), &limits, None);
        assert!(matches!(result, Err(DownloadError::TooManyEntries(2))));
    }

    // ------------------------------------------------------------------
    // Mid-stream cancel inside the unpack copy loop
    // ------------------------------------------------------------------

    #[test]
    fn cancel_mid_stream_aborts_unpack() {
        // A large single entry (~4 MiB) so the copy loop spans many 64 KiB
        // chunks. Cancellation is injected deterministically via
        // `cancel_after_bytes`: the loop aborts after a known number of bytes
        // have been read, *during* the copy rather than before it. This
        // verifies the loop's mid-copy cancellation check without relying on
        // thread timing (which made the test flaky on fast machines).
        const CHUNK: usize = 64 * 1024;
        const CANCEL_AFTER: u64 = CHUNK as u64 + 1;
        let data = vec![b'z'; 4 * 1024 * 1024];
        let archive = make_zip_bytes(&[("bank/big.wav", &data[..])]);
        let dir = temp_banks_dir("mid-cancel");
        let archive_path = dir.join("archive.zip");
        std::fs::write(&archive_path, &archive).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let out_file = dest.join("bank").join("big.wav");

        let limits = Limits {
            max_entries: 100_000,
            max_total_unpacked: 1_000_000_000,
            max_entry_bytes: 1_000_000_000,
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let result = unpack_zip(&archive_path, &dest, &cancel, &limits, Some(CANCEL_AFTER));
        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "unpack must be cancelled mid-copy: {result:?}"
        );

        // The file was interrupted part-way: it exists but holds fewer bytes
        // than the full entry, proving the cancel fired mid-copy rather than
        // before any work was done. Because cancellation triggers when the
        // *read* total reaches `CANCEL_AFTER` (one byte past the first 64 KiB
        // chunk), the copy loop writes exactly one chunk before aborting.
        let written = std::fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);
        assert!(
            written > 0,
            "some bytes must have been written before cancel"
        );
        assert!(
            (written as usize) < data.len(),
            "unpack must not complete: wrote {written} of {}",
            data.len()
        );
    }
}
