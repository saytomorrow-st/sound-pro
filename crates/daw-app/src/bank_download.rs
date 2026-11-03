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
//!   `..` component, or a symlink/hard-link is rejected before extraction and
//!   the whole download is discarded.
//! - **Zip bombs / size limits**: the declared (uncompressed) entry size must
//!   fit within [`MAX_UNPACKED_BYTES`]; the compressed archive is capped at the
//!   manifest `size_bytes` while streaming, so a lying `Content-Length` or a
//!   bomb that expands beyond the cap is caught.
//! - **Redirects to other hosts**: redirects are followed only while the host
//!   (scheme + host + port) stays identical to the pinned URL; a cross-host
//!   redirect aborts with [`DownloadError::RedirectToOtherHost`].
//! - **TLS**: data and license URLs must be `https`; `http` is rejected with
//!   [`DownloadError::InsecureUrl`]. TLS uses rustls (no OpenSSL).
//! - **Partial downloads / resume**: the archive is streamed to a temporary
//!   file and only renamed into place after hash verification; a truncated
//!   body fails the size/hash check and the temp file is deleted. There is no
//!   cross-session resume.
//! - **Disk space**: the temporary file and the unpacked tree are written and
//!   any I/O error aborts the download and removes the temp file.
//! - **Cancel**: the GUI flips a shared `AtomicBool`; every network read and
//!   the unpack loop check it and abort with [`DownloadError::Cancelled`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::bank_library::{CatalogEntry, Download, InstalledBank, LicenseCheck};
use sha2::{Digest, Sha256};

/// Maximum number of redirects followed before giving up.
const MAX_REDIRECTS: usize = 5;

/// Upper bound on the total unpacked size of a bank (2 GiB). Anything larger
/// is treated as a zip/tar bomb. This is far above both catalog banks.
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum size of a single unpacked archive entry (2 GiB).
const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Per-read timeout for network I/O (stalls mid-download).
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall connect/send timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a download failed.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The archive URL is not HTTPS.
    #[error("download URL is not HTTPS: {0}")]
    InsecureUrl(String),
    /// A redirect would leave the original host.
    #[error("the source redirected to a different host: {0}")]
    RedirectToOtherHost(String),
    /// Too many redirects.
    #[error("too many redirects while fetching {0}")]
    TooManyRedirects(String),
    /// An HTTP status that is not 2xx.
    #[error("HTTP {0} from {1}")]
    HttpStatus(u16, String),
    /// A network or body I/O failure.
    #[error("network error: {0}")]
    Network(String),
    /// The downloaded byte count differs from the manifest.
    #[error("download truncated: expected {expected} bytes, got {actual}")]
    Truncated {
        /// Bytes the manifest pinned.
        expected: u64,
        /// Bytes actually received.
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
    /// The archive is damaged or unreadable.
    #[error("cannot unpack archive: {0}")]
    Unpack(String),
    /// The user cancelled the download.
    #[error("download cancelled")]
    Cancelled,
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
/// The full pipeline: license re-check → stream the archive to a temp file
/// (progress + cancel) → verify size and SHA-256 → unpack into a temp dir
/// (safe) → atomically move into the banks directory → update the registry
/// with the attribution text. On any failure the temp artifacts are removed
/// and no registry entry is written.
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
    download_bank_into_with_https(entry, banks, progress, cancel, true)
}

/// [`download_bank_into`] with the HTTPS enforcement toggled. The production
/// path always requires HTTPS; the integration tests call this with `false` so
/// they can serve archives and license sources from a local `http://` server.
#[doc(hidden)]
pub fn download_bank_into_with_https(
    entry: &CatalogEntry,
    banks: &Path,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<InstalledBank, DownloadError> {
    let download = entry
        .download
        .as_ref()
        .ok_or_else(|| DownloadError::UnsupportedFormat(entry.format.clone()))?;
    let license_check = entry
        .license_check
        .as_ref()
        .ok_or_else(|| DownloadError::LicenseUnreachable("no license_check block".to_owned()))?;

    // 1. Re-check the license before downloading anything.
    recheck_license(entry, license_check, cancel, https_required)?;

    // 2. Stream the archive to a temp file, enforcing the size cap.
    std::fs::create_dir_all(banks)
        .map_err(|e| DownloadError::Io(banks.to_path_buf(), e.to_string()))?;

    let tmp_archive = banks.join(format!(".{}.download.tmp", entry.id));
    let result = download_archive(download, &tmp_archive, progress, cancel, https_required);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp_archive);
        return Err(e);
    }

    // 3. Unpack into a temp dir, then atomically install.
    let tmp_dir = banks.join(format!(".{}.unpack.tmp", entry.id));
    let unpack_result = (|| {
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| DownloadError::Io(tmp_dir.clone(), e.to_string()))?;
        unpack_archive(&tmp_archive, &entry.format, &tmp_dir, cancel)
    })();
    let _ = std::fs::remove_file(&tmp_archive);
    if let Err(e) = unpack_result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    let final_dir = banks.join(&entry.id);
    let install_result = atomic_install(&tmp_dir, &final_dir);
    if let Err(e) = install_result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    // 4. Record the install in the registry with attribution.
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
    crate::bank_library::record_installed_bank(&registry_file, &bank)
        .map_err(|e| DownloadError::Io(registry_file, e.to_string()))?;
    Ok(bank)
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
    let body = http_get_bytes(&check.url, cancel, https_required)?;
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
    let file = std::fs::File::create(out)
        .map_err(|e| DownloadError::Io(out.to_path_buf(), e.to_string()))?;
    let mut writer = std::io::BufWriter::new(file);

    let mut hasher = Sha256::new();
    let mut received: u64 = 0;

    let mut reader = http_open(&download.url, cancel, https_required)?;
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
        writer
            .write_all(&buf[..n])
            .map_err(|e| DownloadError::Io(out.to_path_buf(), e.to_string()))?;
        progress.received.store(received, Ordering::Relaxed);
    }
    writer
        .flush()
        .map_err(|e| DownloadError::Io(out.to_path_buf(), e.to_string()))?;

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
) -> Result<(), DownloadError> {
    match format {
        "zip" => unpack_zip(archive, dest, cancel),
        "tar.bz2" => unpack_tar_bz2(archive, dest, cancel),
        other => Err(DownloadError::UnsupportedFormat(other.to_owned())),
    }
}

fn unpack_zip(archive: &Path, dest: &Path, cancel: &Arc<AtomicBool>) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| DownloadError::Unpack(e.to_string()))?;

    let mut total: u64 = 0;
    for i in 0..zip.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let mut entry = zip
            .by_index(i)
            .map_err(|e| DownloadError::Unpack(e.to_string()))?;
        let name = entry.name().to_owned();
        let out_path = safe_join(dest, &name)?;

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| DownloadError::Io(out_path, e.to_string()))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(DownloadError::UnsafeEntry(format!(
                "symlink entry {name:?}"
            )));
        }
        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            return Err(DownloadError::UnsafeEntry(format!(
                "entry {name:?} is too large ({size} bytes)"
            )));
        }
        total += size;
        if total > MAX_UNPACKED_BYTES {
            return Err(DownloadError::UnsafeEntry(
                "unpacked size exceeds limit".to_owned(),
            ));
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DownloadError::Io(parent.to_path_buf(), e.to_string()))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| DownloadError::Io(out_path.clone(), e.to_string()))?;
        let mut remaining = size;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let want = remaining.min(buf.len() as u64) as usize;
            let n = entry
                .read(&mut buf[..want])
                .map_err(|e| DownloadError::Unpack(e.to_string()))?;
            if n == 0 {
                return Err(DownloadError::Unpack(format!("truncated entry {name:?}")));
            }
            out.write_all(&buf[..n])
                .map_err(|e| DownloadError::Io(out_path.clone(), e.to_string()))?;
            remaining -= n as u64;
        }
    }
    Ok(())
}

fn unpack_tar_bz2(
    archive: &Path,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.to_path_buf(), e.to_string()))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    let mut total: u64 = 0;
    for entry in tar
        .entries()
        .map_err(|e| DownloadError::Unpack(e.to_string()))?
    {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
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
        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            return Err(DownloadError::UnsafeEntry(format!(
                "entry {name:?} is too large ({size} bytes)"
            )));
        }
        total += size;
        if total > MAX_UNPACKED_BYTES {
            return Err(DownloadError::UnsafeEntry(
                "unpacked size exceeds limit".to_owned(),
            ));
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DownloadError::Io(parent.to_path_buf(), e.to_string()))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| DownloadError::Io(out_path, e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| DownloadError::Unpack(e.to_string()))?;
    }
    Ok(())
}

/// Resolves an archive entry name against `dest`, rejecting absolute paths,
/// parent-directory traversal, and (for zip) names that would escape `dest`.
fn safe_join(dest: &Path, name: &str) -> Result<PathBuf, DownloadError> {
    // Normalize `\` to `/` (Salamander uses backslashes in the SFZ only; the
    // tar paths are `/`-separated, but be defensive).
    let normalized = name.replace('\\', "/");
    let p = Path::new(&normalized);
    if p.is_absolute() {
        return Err(DownloadError::UnsafeEntry(format!(
            "absolute path {name:?}"
        )));
    }
    let joined = dest.join(p);
    // Reject any `..` component that escapes `dest`.
    let canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    if let Ok(Some(parent)) = joined.parent().map(Path::canonicalize).transpose() {
        if !parent.starts_with(&canonical_dest) {
            return Err(DownloadError::UnsafeEntry(format!(
                "path escapes destination: {name:?}"
            )));
        }
    }
    if normalized.split('/').any(|c| c == "..") {
        return Err(DownloadError::UnsafeEntry(format!(
            "parent traversal in {name:?}"
        )));
    }
    Ok(joined)
}

/// Atomically moves the freshly-unpacked tree into its final location,
/// replacing any previous install.
fn atomic_install(tmp_dir: &Path, final_dir: &Path) -> Result<(), DownloadError> {
    if final_dir.exists() {
        std::fs::remove_dir_all(final_dir)
            .map_err(|e| DownloadError::Io(final_dir.to_path_buf(), e.to_string()))?;
    }
    std::fs::rename(tmp_dir, final_dir)
        .map_err(|e| DownloadError::Io(tmp_dir.to_path_buf(), e.to_string()))
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Fetches a URL body into memory, with manual same-host redirect handling and
/// cancellation. When `https_required` is true, non-HTTPS URLs are rejected.
fn http_get_bytes(
    url: &str,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<Vec<u8>, DownloadError> {
    let mut reader = http_open(url, cancel, https_required)?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| DownloadError::Network(e.to_string()))?;
    Ok(out)
}

/// Opens a streaming reader for `url`, following same-host redirects (up to
/// [`MAX_REDIRECTS`]) and enforcing HTTPS when `https_required`.
fn http_open(
    url: &str,
    cancel: &Arc<AtomicBool>,
    https_required: bool,
) -> Result<Box<dyn Read>, DownloadError> {
    if https_required {
        ensure_https(url)?;
    }
    let agent = build_agent();
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
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| DownloadError::TooManyRedirects(current.clone()))?;
            let next = resolve_redirect(&current, location)?;
            if https_required {
                ensure_https(&next)?;
            }
            if !same_host(&current, &next) {
                return Err(DownloadError::RedirectToOtherHost(next));
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(DownloadError::HttpStatus(status, current));
        }
        let reader = response.into_body().into_reader();
        return Ok(Box::new(reader));
    }
    Err(DownloadError::TooManyRedirects(current))
}

fn build_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
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

/// Resolves a `Location` header (absolute or relative) against the current URL.
fn resolve_redirect(current: &str, location: &str) -> Result<String, DownloadError> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_owned());
    }
    // Relative resolution: split off the path/query from the origin.
    let base = current.trim_end_matches('/');
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let rest = &base[scheme_end..];
    let slash = rest.find('/');
    let origin = match slash {
        Some(i) => &base[..scheme_end + i],
        None => base,
    };
    if location.starts_with('/') {
        Ok(format!("{origin}{location}"))
    } else {
        let dir = origin.trim_end_matches('/');
        Ok(format!("{dir}/{location}"))
    }
}

/// Whether two URLs share the same scheme, host and (implicit) port.
fn same_host(a: &str, b: &str) -> bool {
    let a = host_port(a);
    let b = host_port(b);
    a == b
}

/// Extracts `scheme://host[:port]` from a URL.
fn host_port(url: &str) -> String {
    let scheme_end = match url.find("://") {
        Some(i) => i + 3,
        None => return url.to_owned(),
    };
    let rest = &url[scheme_end..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    url[..scheme_end + end].to_owned()
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

/// Extracts the SPDX id from a license metadata JSON document (the
/// `license.spdx_id` field of the GitHub license API response).
fn license_spdx_from_metadata(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("license")?
        .get("spdx_id")?
        .as_str()
        .map(str::to_owned)
}

/// Convenience: one-off SHA-256 of a file (used by tests and audit tooling).
#[allow(dead_code)]
fn sha256_file(path: &Path) -> Result<String, DownloadError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| DownloadError::Io(path.to_path_buf(), e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| DownloadError::Io(path.to_path_buf(), e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
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
    fn host_port_extraction() {
        assert_eq!(
            host_port("https://example.com/a/b?c=d#e"),
            "https://example.com"
        );
        assert_eq!(
            host_port("https://example.com:8443/x"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn same_host_compares_scheme_host_port() {
        assert!(same_host("https://example.com/a", "https://example.com/b"));
        assert!(!same_host("https://example.com/a", "https://evil.com/a"));
        assert!(!same_host("https://example.com/a", "http://example.com/a"));
    }

    #[test]
    fn resolve_redirect_handles_relative_and_absolute() {
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "/c").unwrap(),
            "https://example.com/c"
        );
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "c").unwrap(),
            "https://example.com/c"
        );
        assert_eq!(
            resolve_redirect("https://example.com/a", "https://other.com/x").unwrap(),
            "https://other.com/x"
        );
    }

    #[test]
    fn ensure_https_rejects_http() {
        assert!(ensure_https("http://example.com").is_err());
        assert!(ensure_https("https://example.com").is_ok());
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let dest = Path::new("/tmp/dest");
        assert!(safe_join(dest, "../evil").is_err());
        assert!(safe_join(dest, "/abs/path").is_err());
        assert!(safe_join(dest, "a/../../evil").is_err());
        assert!(safe_join(dest, "ok/file.sfz").is_ok());
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
