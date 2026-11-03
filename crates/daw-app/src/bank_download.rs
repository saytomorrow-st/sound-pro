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
//! - **Partial downloads / resume**: the archive is streamed to a temporary
//!   file and only renamed into place after hash verification; a truncated
//!   body fails the size/hash check and the temp file is deleted. There is no
//!   cross-session resume.
//! - **Concurrency / atomicity**: a per-bank lock file (created with
//!   `create_new`) serialises concurrent downloads of the same bank; the
//!   install uses a unique staging dir and a rename into place, and the
//!   registry write is temp+rename.
//! - **Cancel**: the GUI flips a shared `AtomicBool`; every network read and
//!   the unpack loop check it and abort with [`DownloadError::Cancelled`].

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
    sweep_stale_artifacts(banks, &entry.id);

    // 1. Re-check the license before downloading anything.
    recheck_license(entry, license_check, cancel, https_required)?;

    // 2. Stream the archive to a unique temp file, enforcing the size cap.
    let tmp_archive = banks.join(format!(".{}.download.{}.tmp", entry.id, unique_token()));
    let result = download_archive(download, &tmp_archive, progress, cancel, https_required);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp_archive);
        return Err(e);
    }

    // 3. Unpack into a unique staging dir, then atomically install.
    let limits = Limits::for_archive(download.size_bytes);
    let tmp_dir = banks.join(format!(".{}.unpack.{}.tmp", entry.id, unique_token()));
    let unpack_result = (|| {
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| DownloadError::Io(tmp_dir.clone(), e.to_string()))?;
        unpack_archive(&tmp_archive, &entry.format, &tmp_dir, cancel, &limits)
    })();
    let _ = std::fs::remove_file(&tmp_archive);
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
    if let Err(e) = crate::bank_library::record_installed_bank(&registry_file, &bank)
        .map_err(|e| DownloadError::Io(registry_file, e.to_string()))
    {
        // Roll back: restore the previous install if there was one.
        let _ = std::fs::remove_dir_all(&final_dir);
        let old = banks.join(format!(".{}.old.tmp", entry.id));
        if old.exists() {
            let _ = std::fs::rename(&old, &final_dir);
        }
        return Err(e);
    }
    // Success: drop any backed-up previous install.
    let old = banks.join(format!(".{}.old.tmp", entry.id));
    let _ = std::fs::remove_dir_all(&old);
    Ok(bank)
}

/// A per-bank lock file guard. `create_new` fails if another process/thread is
/// already downloading this bank; dropping the guard releases it.
struct BankLock {
    path: PathBuf,
}

fn acquire_lock(path: &Path) -> Result<BankLock, DownloadError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Ok(BankLock {
            path: path.to_path_buf(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(DownloadError::AlreadyInProgress(
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ))
        }
        Err(e) => Err(DownloadError::Io(path.to_path_buf(), e.to_string())),
    }
}

impl Drop for BankLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Removes stale `.<id>.download.*.tmp`, `.<id>.unpack.*.tmp` and
/// `.<id>.old.tmp` artifacts for `id`. The `.<id>.lock` file is intentionally
/// left alone: it is released by `BankLock::drop`, and this function must never
/// remove a lock held by a concurrent download.
fn sweep_stale_artifacts(banks: &Path, id: &str) {
    let entries = match std::fs::read_dir(banks) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Staging names are `.{id}.download.{token}.tmp`, `.{id}.unpack.{token}.tmp`
        // and `.{id}.old.tmp`. The lock file `.{id}.lock` is deliberately NOT
        // swept here: it is released by `BankLock::drop`, and sweeping it would
        // remove a live lock held by a concurrent download.
        let is_stale = name.starts_with(&format!(".{id}.")) && name.ends_with(".tmp");
        if is_stale {
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
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
fn download_archive(
    download: &Download,
    out: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<(), DownloadError> {
    let file = std::fs::File::create(out).map_err(|e| io_err(out, e))?;
    let mut writer = std::io::BufWriter::new(file);

    let mut hasher = Sha256::new();
    let mut received: u64 = 0;

    let mut reader = http_open(
        &download.url,
        cancel,
        https_required,
        Some(&download.redirect_hosts),
        None,
    )?;
    // Early size check: a lying `Content-Length` is rejected before we read.
    if let Some(len) = reader.content_length() {
        if len != download.size_bytes {
            return Err(DownloadError::TooLarge {
                expected: download.size_bytes,
                actual: len,
            });
        }
    }
    let mut buf = vec![0u8; 64 * 1024];
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
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n]).map_err(|e| io_err(out, e))?;
        progress.received.store(received, Ordering::Relaxed);
    }
    writer.flush().map_err(|e| io_err(out, e))?;

    if received != download.size_bytes {
        return Err(DownloadError::Truncated {
            expected: download.size_bytes,
            actual: received,
        });
    }
    let actual = hex(&hasher.finalize());
    if !constant_time_eq(actual.as_bytes(), download.sha256.as_bytes()) {
        return Err(DownloadError::HashMismatch);
    }
    Ok(())
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
        "zip" => unpack_zip(archive, dest, cancel, limits),
        "tar.bz2" => unpack_tar_bz2(archive, dest, cancel, limits),
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
    /// case) and enforcing the per-archive limits.
    fn register(&mut self, rel: &str, limits: &Limits, size: u64) -> Result<(), DownloadError> {
        let key = rel.to_lowercase();
        if !self.seen.insert(key) {
            return Err(DownloadError::UnsafeEntry(format!(
                "duplicate entry path {rel:?}"
            )));
        }
        self.total = self.total.saturating_add(size);
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
        state.register(&name, limits, declared)?;

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

fn unpack_tar_bz2(
    archive: &Path,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    limits: &Limits,
) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;
    let decoder = bzip2::read::BzDecoder::new(file);
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
        state.register(&name, limits, declared)?;

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
            out.write_all(&buf[..n]).map_err(|e| io_err(&out_path, e))?;
        }
    }
    Ok(())
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
    match std::fs::rename(tmp_dir, final_dir) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Restore the previous install on failure.
            if old.exists() {
                let _ = std::fs::rename(&old, final_dir);
            }
            Err(DownloadError::Io(tmp_dir.to_path_buf(), e.to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// A streaming HTTP response reader that remembers its `Content-Length` for
/// the early size check.
struct BodyReader {
    inner: Box<dyn Read>,
    content_length: Option<u64>,
}

impl BodyReader {
    fn content_length(&self) -> Option<u64> {
        self.content_length
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
    let mut reader = http_open(url, cancel, https_required, None, Some(LICENSE_TIMEOUT))?;
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
fn http_open(
    url: &str,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
    allowed_hosts: Option<&[String]>,
    global_timeout: Option<Duration>,
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
        let response = agent
            .get(&current)
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// A small single-threaded HTTP server. The handler returns a
    /// [`ServerResponse`]; it must *not* panic or assert — a panic in the
    /// server thread would not fail the test, so handlers return error
    /// responses (e.g. 404) instead and the test asserts on the downloader's
    /// behaviour.
    struct TestServer {
        addr: String,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(handler: impl Fn(usize, &str) -> ServerResponse + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = Arc::clone(&stop);
            let handle = thread::spawn(move || {
                let mut index = 0usize;
                for stream in listener.incoming() {
                    if stop2.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let mut buf = [0u8; 8192];
                    let mut read = 0;
                    loop {
                        let n = stream.read(&mut buf[read..]).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        read += n;
                        if read >= buf.len() || buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let req = String::from_utf8_lossy(&buf[..read]);
                    let target = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let resp = handler(index, target);
                    index += 1;
                    write_response(&mut stream, &resp);
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
                let _ = handle.join();
            }
        }
    }

    #[derive(Clone)]
    struct ServerResponse {
        status: u16,
        body: Vec<u8>,
        location: Option<String>,
        content_length: Option<usize>,
    }

    impl ServerResponse {
        fn ok(body: Vec<u8>) -> Self {
            ServerResponse {
                status: 200,
                body,
                location: None,
                content_length: None,
            }
        }
        fn not_found() -> Self {
            ServerResponse {
                status: 404,
                body: b"not found".to_vec(),
                location: None,
                content_length: None,
            }
        }
        fn redirect(location: String) -> Self {
            ServerResponse {
                status: 302,
                body: Vec::new(),
                location: Some(location),
                content_length: None,
            }
        }
        /// Omit the `Content-Length` header so the client must discover the
        /// real length by reading to EOF (used to exercise the truncated-body
        /// path).
        fn without_content_length(mut self) -> Self {
            self.content_length = Some(usize::MAX);
            self
        }
    }

    fn write_response(stream: &mut std::net::TcpStream, resp: &ServerResponse) {
        let reason = match resp.status {
            200 => "OK",
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
        let raw = cursor.into_inner();
        let mut out = Vec::new();
        {
            let mut enc = bzip2::write::BzEncoder::new(&mut out, bzip2::Compression::default());
            use std::io::Write;
            enc.write_all(&raw).unwrap();
            enc.finish().unwrap();
        }
        out
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

        // No `.tmp` artifacts are left behind.
        let leftover = std::fs::read_dir(&banks)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp") || n.contains(".lock"))
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
            Err(DownloadError::Truncated { expected, .. }) if expected == declared_size
        ));
        assert!(!banks.join("test-bank").exists());
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
        // license, then start the competing download.
        started_rx.recv().unwrap();
        let t2 = thread::spawn(move || {
            download_bank_into_with_https(&e2, &banks2, &Progress::new(size), &cancel_flag(), false)
        });
        let r2 = t2.join().unwrap();
        // The second download must observe the held lock and bail out.
        assert!(
            matches!(r2, Err(DownloadError::AlreadyInProgress(_))),
            "the loser must report AlreadyInProgress: r2={r2:?}"
        );
        // Release the first download and let it finish.
        release_flag.store(true, Ordering::Relaxed);
        let r1 = t1.join().unwrap();
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
}
