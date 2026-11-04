//! The sound bank library: catalog manifest, installed-bank registry, and
//! credits text (SPEC-066 / RFC-001 §6, PR 5).
//!
//! We host nothing. The catalog is a set of manifest files embedded in the
//! binary (`catalog/banks/*.json`, read with `include_str!`), each pointing at
//! an upstream URL with a pinned SHA-256. The installed-bank registry lives in
//! the user data directory and records local SFZ-folder installs. Downloading
//! is the next slice (SPEC-067), so every catalog entry here has a
//! placeholder (`null`) hash and is not downloadable yet.
//!
//! Everything in this module runs on the control thread only (I/O and
//! allocation are allowed); none of it is called from the audio callback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The catalog manifest schema version this build understands.
pub const CATALOG_SCHEMA: u32 = 1;

/// The installed-registry file schema version this build understands.
pub const REGISTRY_SCHEMA: u32 = 1;

/// Why a catalog entry or registry operation failed.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The manifest declares a schema this build does not know.
    #[error("unsupported catalog schema {0} (supported: {CATALOG_SCHEMA})")]
    UnsupportedSchema(u32),
    /// The manifest is not valid JSON.
    #[error("cannot parse catalog manifest: {0}")]
    Parse(String),
    /// A required field is missing or empty.
    #[error("missing or empty field {0:?} in catalog entry {1:?}")]
    MissingField(&'static str, String),
    /// The SPDX license identifier is not recognised or not allowed.
    #[error("license {0:?} is not allowed in the catalog")]
    ForbiddenLicense(String),
    /// A CC-BY entry is missing its mandatory attribution.
    #[error("CC-BY entry {0:?} requires attribution text")]
    MissingAttribution(String),
    /// The SHA-256 is neither `null` (placeholder) nor 64 lowercase hex.
    #[error("bad sha256 in catalog entry {0:?}: expected 64 lowercase hex chars or null")]
    BadHash(String),
    /// The `download` block is present but malformed or incomplete.
    #[error("bad download block in catalog entry {0:?}: {1}")]
    BadDownload(String, &'static str),
    /// The `license_check` block is present but malformed or incomplete.
    #[error("bad license_check block in catalog entry {0:?}: {1}")]
    BadLicenseCheck(String, &'static str),
    /// The archive `format` is not one this build can unpack.
    #[error("unsupported archive format {0:?} in catalog entry {1:?}")]
    BadFormat(String, String),
}

/// Why an installed-bank operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The user did not acknowledge a share-alike/GPL license before install.
    #[error("license acknowledgement is required to install {0:?}")]
    LicenseNotAcknowledged(String),
    /// The registry file could not be read or written.
    #[error("registry I/O error at {0}: {1}")]
    Io(PathBuf, String),
    /// The registry JSON is malformed.
    #[error("registry parse error: {0}")]
    Parse(String),
    /// The operation was cancelled while waiting for a contended lock.
    #[error("cancelled")]
    Cancelled,
}

/// A license as declared in a catalog manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    /// SPDX identifier, e.g. `CC0-1.0`, `CC-BY-3.0`, `MIT`.
    pub spdx: String,
    /// URL of the license text.
    pub url: String,
    /// Whether attribution is mandatory (CC-BY).
    #[serde(default)]
    pub attribution_required: bool,
}

/// A single catalog entry (one embedded `catalog/banks/*.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Manifest schema version.
    pub schema: u32,
    /// Stable, unique id, e.g. `salamander-grand-v3`.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Instrument family, e.g. `piano`.
    pub family: String,
    /// Upstream version string.
    pub version: String,
    /// Author or authors.
    pub author: String,
    /// License declaration.
    pub license: License,
    /// Attribution text shown in the Credits window.
    #[serde(default)]
    pub attribution: String,
    /// Upstream URL (original author's page or repository).
    pub source_url: String,
    /// Pinned SHA-256 of the download, or `None` while unverified (not
    /// downloadable until SPEC-067 fills it). Kept for SPEC-066 compatibility;
    /// the downloader reads `download` instead.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Download size in bytes, or `None` while unverified.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// Whether the hash/size were verified by the owner. Always `false` here.
    #[serde(default)]
    pub verified: bool,
    /// License re-check source (SPEC-067). `None` while unverified.
    #[serde(default)]
    pub license_check: Option<LicenseCheck>,
    /// The download archive (SPEC-067). `None` while unverified / not
    /// downloadable.
    #[serde(default)]
    pub download: Option<Download>,
    /// Path to the main `.sfz` inside the bank folder (`/` separators), or
    /// `None` to use a breadth-first search (loader fallback).
    #[serde(default)]
    pub sfz: Option<String>,
    /// Archive container: `"zip"` or `"tar.bz2"`.
    #[serde(default)]
    pub format: String,
}

/// How a catalog entry's license is re-checked before every download
/// (SPEC-067 / RFC-001 §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseCheck {
    /// URL of the license source (license text, page, or API metadata).
    pub url: String,
    /// SHA-256 of the normalized license text for `text-hash` mode; `None`
    /// for `metadata` mode (only the SPDX id is compared).
    #[serde(default)]
    pub sha256: Option<String>,
    /// Re-check mode: `text-hash` or `metadata`.
    pub mode: String,
}

/// The download archive for a catalog entry (SPEC-067).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// Upstream URL of the archive (author's original host).
    pub url: String,
    /// Pinned SHA-256 of the archive, verified by the owner.
    pub sha256: String,
    /// Pinned size in bytes, verified by the owner.
    pub size_bytes: u64,
    /// Hosts the download may redirect to (HTTPS only). Each entry is either
    /// an exact host or a `.`-prefixed suffix (matching `host` and any
    /// subdomain). The pinned URL's own host is always allowed. Empty (the
    /// SPEC-066-compatible default) means redirects stay same-host only.
    ///
    /// Many real archives redirect to a CDN with a *different* host than the
    /// pinned URL (e.g. `github.com` → `release-assets.githubusercontent.com`,
    /// `archive.org` → `dn*.ca.archive.org`), so a pure same-host rule would
    /// block every bank. The SHA-256 still guarantees integrity even when the
    /// bytes come from a CDN.
    #[serde(default)]
    pub redirect_hosts: Vec<String>,
}

/// A bank the user installed from a local SFZ folder (SPEC-066).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledBank {
    /// Stable id chosen at install time (usually the catalog id, or a slug of
    /// the folder name for a purely-local bank).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Instrument family.
    pub family: String,
    /// Absolute path to the installed bank folder.
    pub path: String,
    /// SPDX identifier of the bank's license.
    pub license_spdx: String,
    /// Attribution text (may be empty for CC0/Unlicense/MIT/BSD).
    pub attribution: String,
    /// Whether a share-alike/GPL license was acknowledged at install time.
    #[serde(default)]
    pub license_acknowledged: bool,
    /// ISO-8601 install timestamp.
    pub installed_at: String,
}

/// The on-disk installed-bank registry (`banks.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRegistry {
    /// Registry schema version.
    pub schema: u32,
    /// Installed banks, in install order.
    #[serde(default)]
    pub banks: Vec<InstalledBank>,
}

impl InstalledRegistry {
    /// Parses the registry text. A malformed document is a
    /// [`LibraryError::Parse`].
    pub fn parse(text: &str) -> Result<Self, LibraryError> {
        let reg: Self =
            serde_json::from_str(text).map_err(|e| LibraryError::Parse(e.to_string()))?;
        Ok(reg)
    }

    /// Serialises the registry to JSON.
    pub fn to_text(&self) -> Result<String, LibraryError> {
        serde_json::to_string_pretty(self).map_err(|e| LibraryError::Parse(e.to_string()))
    }

    /// Reads the registry from `path`; a missing file gives an empty registry.
    pub fn load(path: &Path) -> Result<Self, LibraryError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                schema: REGISTRY_SCHEMA,
                banks: Vec::new(),
            }),
            Err(e) => Err(LibraryError::Io(path.to_path_buf(), e.to_string())),
        }
    }

    /// Writes the registry to `path`, creating its folder. The write is
    /// atomic: it goes to a temp file next to `path`, which is fsynced before
    /// being `rename`d over the destination, so a crash mid-write cannot leave
    /// a truncated registry and a crash after the rename cannot lose the new
    /// contents.
    pub fn save(&self, path: &Path) -> Result<(), LibraryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| LibraryError::Io(dir.to_path_buf(), e.to_string()))?;
        }
        let text = self.to_text()?;
        let tmp = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| LibraryError::Io(tmp.clone(), e.to_string()))?;
        use std::io::Write;
        file.write_all(text.as_bytes())
            .map_err(|e| LibraryError::Io(tmp.clone(), e.to_string()))?;
        // Persist the temp file's contents before it becomes the registry, so
        // the rename below cannot be reordered ahead of the data it points to.
        file.sync_all()
            .map_err(|e| LibraryError::Io(tmp.clone(), e.to_string()))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| LibraryError::Io(path.to_path_buf(), e.to_string()))
    }
}

/// SPDX identifiers that require mandatory attribution (CC-BY).
fn requires_attribution(spdx: &str) -> bool {
    spdx == "CC-BY-3.0" || spdx == "CC-BY-4.0"
}

/// SPDX identifiers that are share-alike or GPL-like: only allowed as a local
/// import with an acknowledgement, never in the catalog.
pub fn is_share_alike(spdx: &str) -> bool {
    spdx.starts_with("CC-BY-SA")
        || spdx.starts_with("GPL-")
        || spdx.starts_with("AGPL-")
        || spdx.starts_with("LGPL-")
}

/// SPDX identifiers that are outright forbidden everywhere.
fn is_forbidden(spdx: &str) -> bool {
    spdx.contains("NC") || spdx.contains("ND")
}

/// Validates and parses one embedded manifest's JSON text.
///
/// # Errors
///
/// [`CatalogError`] on a malformed or disallowed entry.
pub fn parse_catalog_entry(text: &str) -> Result<CatalogEntry, CatalogError> {
    let entry: CatalogEntry =
        serde_json::from_str(text).map_err(|e| CatalogError::Parse(e.to_string()))?;
    validate_catalog_entry(&entry)?;
    Ok(entry)
}

/// Validates a parsed catalog entry against the catalog policy (SPEC-066 §1).
///
/// # Errors
///
/// [`CatalogError`].
pub fn validate_catalog_entry(entry: &CatalogEntry) -> Result<(), CatalogError> {
    if entry.schema != CATALOG_SCHEMA {
        return Err(CatalogError::UnsupportedSchema(entry.schema));
    }
    for (field, value) in [
        ("id", entry.id.as_str()),
        ("name", entry.name.as_str()),
        ("family", entry.family.as_str()),
        ("version", entry.version.as_str()),
        ("author", entry.author.as_str()),
        ("source_url", entry.source_url.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(CatalogError::MissingField(field, entry.id.clone()));
        }
    }
    let spdx = entry.license.spdx.trim();
    if spdx.is_empty() || entry.license.url.trim().is_empty() {
        return Err(CatalogError::MissingField(
            "license.spdx/url",
            entry.id.clone(),
        ));
    }
    if !is_valid_entry_id(&entry.id) {
        return Err(CatalogError::MissingField("id", entry.id.clone()));
    }
    if let Some(sfz) = &entry.sfz {
        if !is_valid_sfz_path(sfz) {
            return Err(CatalogError::MissingField("sfz", entry.id.clone()));
        }
    }
    if is_forbidden(spdx) || is_share_alike(spdx) {
        return Err(CatalogError::ForbiddenLicense(spdx.to_owned()));
    }
    if requires_attribution(spdx)
        && (!entry.license.attribution_required || entry.attribution.trim().is_empty())
    {
        return Err(CatalogError::MissingAttribution(entry.id.clone()));
    }
    if let Some(hash) = &entry.sha256 {
        if !is_sha256(hash) {
            return Err(CatalogError::BadHash(entry.id.clone()));
        }
    }
    validate_download(entry)?;
    validate_license_check(entry)?;
    Ok(())
}

/// Validates the optional `download` block of a catalog entry.
fn validate_download(entry: &CatalogEntry) -> Result<(), CatalogError> {
    let Some(dl) = &entry.download else {
        return Ok(());
    };
    if dl.url.trim().is_empty() {
        return Err(CatalogError::BadDownload(entry.id.clone(), "url is empty"));
    }
    if !is_sha256(&dl.sha256) {
        return Err(CatalogError::BadDownload(
            entry.id.clone(),
            "sha256 must be 64 lowercase hex chars",
        ));
    }
    if dl.size_bytes == 0 {
        return Err(CatalogError::BadDownload(
            entry.id.clone(),
            "size_bytes must be positive",
        ));
    }
    for host in &dl.redirect_hosts {
        if !is_valid_redirect_host(host) {
            return Err(CatalogError::BadDownload(
                entry.id.clone(),
                "redirect_hosts entries must be exact hosts or .-suffixed suffixes",
            ));
        }
    }
    match entry.format.as_str() {
        "zip" | "tar.bz2" => {}
        other => return Err(CatalogError::BadFormat(other.to_owned(), entry.id.clone())),
    }
    Ok(())
}

/// Whether a `redirect_hosts` entry is well-formed: a non-empty hostname
/// (letters, digits, `-`, `.`), optionally with a leading `.` to mean "this
/// host and any subdomain". No scheme, port, path, or whitespace is allowed.
#[must_use]
pub fn is_valid_redirect_host(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let body = s.strip_prefix('.').unwrap_or(s);
    if body.is_empty() {
        return false;
    }
    body.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && !body.starts_with('.')
        && !body.ends_with('.')
        && body
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
}

/// Validates the optional `license_check` block of a catalog entry.
fn validate_license_check(entry: &CatalogEntry) -> Result<(), CatalogError> {
    let Some(lc) = &entry.license_check else {
        return Ok(());
    };
    if lc.url.trim().is_empty() {
        return Err(CatalogError::BadLicenseCheck(
            entry.id.clone(),
            "url is empty",
        ));
    }
    match lc.mode.as_str() {
        "text-hash" => {
            if lc.sha256.as_deref().is_none_or(|h| !is_sha256(h)) {
                return Err(CatalogError::BadLicenseCheck(
                    entry.id.clone(),
                    "text-hash mode requires a 64-hex sha256",
                ));
            }
        }
        "metadata" => {}
        other => {
            return Err(CatalogError::BadLicenseCheck(
                entry.id.clone(),
                if other.is_empty() {
                    "mode is empty"
                } else {
                    "mode must be text-hash or metadata"
                },
            ));
        }
    }
    Ok(())
}

/// Whether `s` is a valid catalog entry id: `^[a-z0-9][a-z0-9._-]{0,63}$`,
/// and not a reserved name (e.g. the registry file `banks.json`).
#[must_use]
pub fn is_valid_entry_id(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if !chars
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
    {
        return false;
    }
    if s.len() > 64 {
        return false;
    }
    // Reserved names that would collide with files we manage next to the bank
    // directory (the registry, temp/lock artifacts).
    !matches!(s, "banks" | "banks.json" | "registry")
}

/// Whether `s` is a valid archive-relative `sfz` path: non-empty, `/`-separated
/// relative `Normal` components only (no `..`, `.`, empty, `\`, drive letters,
/// or leading `/`).
#[must_use]
pub fn is_valid_sfz_path(s: &str) -> bool {
    if s.is_empty() || s.starts_with('/') || s.contains('\\') || s.contains(':') {
        return false;
    }
    s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Whether a catalog entry is downloadable now (SPEC-067): it has a verified
/// `download` block and a `license_check` block.
#[must_use]
pub fn is_downloadable(entry: &CatalogEntry) -> bool {
    entry.verified && entry.download.is_some() && entry.license_check.is_some()
}

/// Whether `s` is 64 lowercase hex characters (a SHA-256 digest).
#[must_use]
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// The embedded catalog: `catalog/banks/*.json` compiled into the binary.
///
/// A single unparseable manifest makes the whole catalog fail rather than
/// silently shipping an incomplete list.
///
/// # Errors
///
/// [`CatalogError`].
pub fn catalog() -> Result<Vec<CatalogEntry>, CatalogError> {
    const FILES: [&str; 2] = [
        include_str!("../../../catalog/banks/salamander-grand-v3.json"),
        include_str!("../../../catalog/banks/osiris-piano.json"),
    ];
    FILES.iter().map(|text| parse_catalog_entry(text)).collect()
}

/// Default banks directory: `$XDG_DATA_HOME/sound-pro/banks`,
/// `~/.local/share/sound-pro/banks`, or `%LOCALAPPDATA%\sound-pro\banks`.
#[must_use]
pub fn banks_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("sound-pro").join("banks"))
}

/// Default registry file path (`banks.json` next to the banks directory).
#[must_use]
pub fn registry_path() -> Option<PathBuf> {
    banks_dir().map(|d| d.join("banks.json"))
}

/// Loads the list of installed banks; a missing registry is empty.
///
/// # Errors
///
/// [`LibraryError`] on I/O or parse failure.
pub fn list_installed() -> Result<Vec<InstalledBank>, LibraryError> {
    let path = registry_path().ok_or_else(|| {
        LibraryError::Io(
            PathBuf::from("<no data dir>"),
            "no data directory".to_owned(),
        )
    })?;
    let registry = InstalledRegistry::load(&path)?;
    let Some(banks) = banks_dir() else {
        return Ok(registry.banks);
    };
    let catalog = catalog().unwrap_or_default();
    let (banks, adopted) = reconcile_installed(&banks, registry.banks, &catalog);
    for bank in &adopted {
        // Best effort: the in-memory list is already correct if this fails.
        let _ = record_installed_bank(&path, bank);
    }
    Ok(banks)
}

/// Reconciles the registry with the banks folder. Never touches the disk.
///
/// * A catalog bank whose folder has no loadable sfz is not installed (it is
///   dropped from the list, so the picker offers the download again).
/// * A catalog bank missing from the registry whose folder (named like the
///   catalog id) holds a loadable sfz is adopted: returned in the list and as
///   the second value, so the caller can persist it.
///
/// Banks that are not in the catalog are passed through unchanged.
#[must_use]
pub fn reconcile_installed(
    banks_root: &Path,
    installed: Vec<InstalledBank>,
    catalog: &[CatalogEntry],
) -> (Vec<InstalledBank>, Vec<InstalledBank>) {
    let hint_of = |id: &str| {
        catalog
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.sfz.clone())
    };
    let mut out: Vec<InstalledBank> = installed
        .into_iter()
        .filter(|b| {
            !catalog.iter().any(|e| e.id == b.id)
                || crate::sample_bank::has_loadable_sfz(
                    Path::new(&b.path),
                    hint_of(&b.id).as_deref(),
                )
        })
        .collect();
    let mut adopted = Vec::new();
    for entry in catalog {
        if out.iter().any(|b| b.id == entry.id) {
            continue;
        }
        let dir = banks_root.join(&entry.id);
        if dir.is_dir() && crate::sample_bank::has_loadable_sfz(&dir, entry.sfz.as_deref()) {
            let bank = InstalledBank {
                id: entry.id.clone(),
                name: entry.name.clone(),
                family: entry.family.clone(),
                path: dir.to_string_lossy().into_owned(),
                license_spdx: entry.license.spdx.clone(),
                attribution: entry.attribution.clone(),
                license_acknowledged: false,
                installed_at: timestamp(),
            };
            out.push(bank.clone());
            adopted.push(bank);
        }
    }
    (out, adopted)
}

/// One item in the track instrument picker list (SPEC-065 redesign): the
/// built-in piano, an installed bank, a not-yet-downloaded catalog bank, or a
/// selected bank whose folder is missing or corrupt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstrumentListItem {
    /// The built-in Salamander piano (always available).
    BuiltinPiano,
    /// An installed bank, ready to use.
    Installed {
        /// Registry id, also used as the project serialisation id.
        id: String,
        /// Human-readable bank name.
        name: String,
    },
    /// A catalog bank that has not been downloaded yet. Clicking it opens the
    /// library window (download is handled there, SPEC-067).
    Download {
        /// Catalog id.
        id: String,
        /// Human-readable bank name.
        name: String,
    },
    /// The track's currently selected bank is recorded in the registry but its
    /// folder is missing or unreadable, so it cannot be rendered.
    Broken {
        /// Registry id.
        id: String,
        /// Human-readable bank name.
        name: String,
    },
}

/// Builds the instrument picker list: the built-in piano, every installed bank
/// (in install order), every catalog bank not yet installed (as a download
/// item), and — when the currently selected bank id is recorded but its folder
/// is missing — a `Broken` entry so the picker can flag it and fall back.
///
/// This is a pure function over already-loaded data; the caller supplies the
/// installed banks and catalog (loading them is I/O and stays with the caller).
#[must_use]
pub fn instrument_list(
    installed: &[InstalledBank],
    catalog: &[CatalogEntry],
    selected_id: Option<&str>,
) -> Vec<InstrumentListItem> {
    let mut items = Vec::with_capacity(1 + installed.len() + catalog.len() + 1);
    items.push(InstrumentListItem::BuiltinPiano);

    for bank in installed {
        let present = Path::new(&bank.path).is_dir();
        let is_selected = selected_id == Some(bank.id.as_str());
        if !present && is_selected {
            items.push(InstrumentListItem::Broken {
                id: bank.id.clone(),
                name: bank.name.clone(),
            });
        } else if present {
            items.push(InstrumentListItem::Installed {
                id: bank.id.clone(),
                name: bank.name.clone(),
            });
        }
        // A missing-but-not-selected bank is simply not offered; it is neither
        // installed nor (as a catalog id) downloadable, so it is skipped.
    }

    for entry in catalog {
        let already_installed = installed.iter().any(|b| b.id == entry.id);
        if !already_installed {
            items.push(InstrumentListItem::Download {
                id: entry.id.clone(),
                name: entry.name.clone(),
            });
        }
    }

    items
}

/// Resolves a sampler bank id to its installed folder, falling back to a legacy
/// absolute path for pre-redesign project files. Returns `None` when the bank
/// is not installed and the id is not an existing directory (a deleted or
/// corrupt bank).
///
/// Control thread only (reads the registry and the filesystem).
#[must_use]
pub fn resolve_bank_dir(bank_id: &str) -> Option<PathBuf> {
    if let Ok(installed) = list_installed() {
        if let Some(bank) = installed.iter().find(|b| b.id == bank_id) {
            let path = Path::new(&bank.path);
            if path.is_dir() {
                return Some(path.to_path_buf());
            }
            return None;
        }
    }
    // Legacy absolute folder path stored by pre-redesign project files.
    let legacy = Path::new(bank_id);
    if legacy.is_absolute() && legacy.is_dir() {
        return Some(legacy.to_path_buf());
    }
    None
}

/// Retrieves the catalog's `sfz` hint for a bank id, if one exists and is
/// valid. Used by the loader to prefer the catalog path before searching.
///
/// Control thread only.
#[must_use]
pub fn find_catalog_sfz(bank_id: &str) -> Option<String> {
    catalog().ok()?.into_iter().find(|e| e.id == bank_id)?.sfz
}

/// Installs a local SFZ folder as a bank and records it in the registry.
///
/// `source_dir` is the SFZ folder the user picked; `id` and `name` identify
/// the bank in the registry. For share-alike/GPL licenses (`is_share_alike`),
/// `license_acknowledged` must be `true` or the install is refused with
/// [`LibraryError::LicenseNotAcknowledged`].
///
/// # Errors
///
/// [`LibraryError`].
pub fn install_local_bank(
    source_dir: &Path,
    id: &str,
    name: &str,
    family: &str,
    license_spdx: &str,
    attribution: &str,
    license_acknowledged: bool,
) -> Result<InstalledBank, LibraryError> {
    if is_share_alike(license_spdx) && !license_acknowledged {
        return Err(LibraryError::LicenseNotAcknowledged(id.to_owned()));
    }

    let banks = banks_dir().ok_or_else(|| {
        LibraryError::Io(
            PathBuf::from("<no data dir>"),
            "no data directory".to_owned(),
        )
    })?;
    let target = banks.join(id);
    std::fs::create_dir_all(&banks).map_err(|e| LibraryError::Io(banks.clone(), e.to_string()))?;

    // Copy the folder into `banks/<id>/`. If the target already exists it is
    // replaced, matching a re-install.
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| LibraryError::Io(target.clone(), e.to_string()))?;
    }
    copy_dir(source_dir, &target)?;

    let bank = InstalledBank {
        id: id.to_owned(),
        name: name.to_owned(),
        family: family.to_owned(),
        path: target.to_string_lossy().into_owned(),
        license_spdx: license_spdx.to_owned(),
        attribution: attribution.to_owned(),
        license_acknowledged,
        installed_at: timestamp(),
    };

    let reg_path = registry_path().ok_or_else(|| {
        LibraryError::Io(
            PathBuf::from("<no data dir>"),
            "no data directory".to_owned(),
        )
    })?;
    record_installed_bank(&reg_path, &bank)?;
    Ok(bank)
}

/// A cross-process advisory lock on the registry file, held on an open handle
/// so the kernel releases it on process death (a leftover `banks.json.lock`
/// file is harmless).
struct RegistryLock {
    _file: std::fs::File,
}

/// Acquires the registry lock, optionally honouring a cancellation flag when
/// `Some`. A long contention retry loop honours the flag and aborts early with
/// [`LibraryError::Cancelled`]. Used by the downloader so a user cancel is not
/// delayed by up to the retry window.
fn acquire_registry_lock_with_cancel(
    registry_file: &Path,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<RegistryLock, LibraryError> {
    let lock_path = registry_file.with_extension("json.lock");
    if let Some(dir) = lock_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| LibraryError::Io(dir.to_path_buf(), e.to_string()))?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| LibraryError::Io(lock_path.clone(), e.to_string()))?;

    let contended = fs2::lock_contended_error().raw_os_error();
    let deadline = std::time::Instant::now() + LOCK_RETRY_WINDOW;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(RegistryLock { _file: file }),
            // Compare raw OS error codes, not `ErrorKind` (see `acquire_lock`
            // in bank_download.rs): Windows reports `ERROR_LOCK_VIOLATION`,
            // which is `ErrorKind::Other`, not `WouldBlock`.
            Err(e) if e.raw_os_error() == contended => {
                if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                    return Err(LibraryError::Cancelled);
                }
                if std::time::Instant::now() >= deadline {
                    return Err(LibraryError::Io(
                        lock_path,
                        "registry is locked by another process".to_owned(),
                    ));
                }
                std::thread::sleep(LOCK_RETRY_PAUSE);
            }
            Err(e) => return Err(LibraryError::Io(lock_path, e.to_string())),
        }
    }
}

/// How long to keep retrying a contended registry lock before giving up (the
/// critical section is a millisecond-scale read-modify-write, so a lock held
/// by another download finishing at the same moment is released almost
/// immediately).
const LOCK_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Per-attempt pause between registry-lock retries.
const LOCK_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// Appends (or replaces) a bank entry in the registry file at `registry_file`.
///
/// Used by both [`install_local_bank`] (the user data registry) and the
/// downloader (which writes `banks.json` next to the banks it installs, so a
/// download can target a non-default directory in tests).
///
/// The read-modify-write is serialised by an advisory lock on
/// `<registry_file>.lock`, so two concurrent downloads of *different* banks
/// cannot lose one another's registry entry (per-bank download locks do not
/// cover this).
///
/// # Errors
///
/// [`LibraryError`] on I/O or parse failure.
pub fn record_installed_bank(
    registry_file: &Path,
    bank: &InstalledBank,
) -> Result<(), LibraryError> {
    record_installed_bank_with_cancel(registry_file, bank, None)
}

/// [`record_installed_bank`] with an optional cancellation flag threaded to the
/// registry-lock acquisition, so the downloader can abort a contended lock
/// instead of waiting out the retry window after the user cancels.
pub(crate) fn record_installed_bank_with_cancel(
    registry_file: &Path,
    bank: &InstalledBank,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(), LibraryError> {
    // Serialise the read-modify-write so two concurrent downloads of different
    // banks cannot lose one another's registry entry.
    let _lock = acquire_registry_lock_with_cancel(registry_file, cancel)?;
    let mut reg = InstalledRegistry::load(registry_file)?;
    reg.banks.retain(|b| b.id != bank.id);
    reg.banks.push(bank.clone());
    reg.save(registry_file)
}

/// Removes an installed bank (folder and registry entry).
///
/// # Errors
///
/// [`LibraryError`] on I/O failure.
pub fn remove(bank_id: &str) -> Result<(), LibraryError> {
    let reg_path = registry_path().ok_or_else(|| {
        LibraryError::Io(
            PathBuf::from("<no data dir>"),
            "no data directory".to_owned(),
        )
    })?;
    let mut reg = InstalledRegistry::load(&reg_path)?;
    let before = reg.banks.len();
    reg.banks.retain(|b| b.id != bank_id);
    if reg.banks.len() != before {
        reg.save(&reg_path)?;
    }
    if let Some(banks) = banks_dir() {
        let target = banks.join(bank_id);
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| LibraryError::Io(target, e.to_string()))?;
        }
    }
    Ok(())
}

/// Copies `from` into `to` recursively.
fn copy_dir(from: &Path, to: &Path) -> Result<(), LibraryError> {
    std::fs::create_dir_all(to).map_err(|e| LibraryError::Io(to.to_path_buf(), e.to_string()))?;
    let entries =
        std::fs::read_dir(from).map_err(|e| LibraryError::Io(from.to_path_buf(), e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| LibraryError::Io(from.to_path_buf(), e.to_string()))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| LibraryError::Io(src, e.to_string()))?;
        }
    }
    Ok(())
}

/// Current UTC time as an RFC 3339 string, e.g. `2026-11-04T10:04:14Z`
/// (install timestamps).
#[must_use]
pub fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_utc(secs)
}

/// Formats Unix seconds as RFC 3339 UTC (civil-from-days, proleptic Gregorian).
#[must_use]
pub fn rfc3339_utc(secs: u64) -> String {
    let rem = secs % 86_400;
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// The built-in piano credit (the single source is `piano::PIANO_CREDIT`).
///
/// Kept here so the Credits window can assemble the full text from installed
/// banks plus the built-in piano without reading `piano::PIANO_CREDIT`
/// directly in the GUI.
pub const BUILTIN_PIANO_CREDIT: &str = crate::piano::PIANO_CREDIT;

/// Generates the multi-line credits text from the installed banks plus the
/// built-in piano (SPEC-066 §4).
///
/// A bank with mandatory attribution is marked; a bank whose attribution text
/// is empty contributes only name + author + license.
#[must_use]
pub fn credits_text(installed: &[InstalledBank]) -> String {
    let mut text = String::new();
    text.push_str(BUILTIN_PIANO_CREDIT);
    text.push('\n');
    for bank in installed {
        text.push_str(&bank.name);
        if bank.family.trim().is_empty() {
            text.push('\n');
        } else {
            text.push_str(" (");
            text.push_str(&bank.family);
            text.push_str(")\n");
        }
        if !bank.attribution.trim().is_empty() {
            text.push_str(&bank.attribution);
            text.push('\n');
        }
        text.push_str("License: ");
        text.push_str(&bank.license_spdx);
        if requires_attribution(&bank.license_spdx) {
            text.push_str(" (attribution required)");
        }
        text.push('\n');
    }
    // Collapse a trailing blank line to a single newline.
    while text.ends_with("\n\n") {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_is_valid() {
        let entries = catalog().expect("catalog must parse");
        assert_eq!(entries.len(), 2);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"salamander-grand-v3"));
        assert!(ids.contains(&"osiris-piano"));
        // SPEC-067 filled both entries with verified upstream hashes/sizes.
        assert!(entries.iter().all(|e| e.verified));
        assert!(entries.iter().all(is_downloadable));
    }

    #[test]
    fn salamander_is_cc_by_with_attribution() {
        let entries = catalog().unwrap();
        let s = entries
            .iter()
            .find(|e| e.id == "salamander-grand-v3")
            .unwrap();
        assert_eq!(s.license.spdx, "CC-BY-3.0");
        assert!(s.license.attribution_required);
        assert!(!s.attribution.is_empty());
        assert_eq!(s.format, "tar.bz2");
        let lc = s.license_check.as_ref().unwrap();
        assert_eq!(lc.mode, "text-hash");
        assert!(lc.sha256.as_deref().is_some_and(is_sha256));
        let dl = s.download.as_ref().unwrap();
        assert!(is_sha256(&dl.sha256));
        assert!(dl.size_bytes > 0);
    }

    #[test]
    fn osiris_is_cc0_without_attribution_requirement() {
        let entries = catalog().unwrap();
        let o = entries.iter().find(|e| e.id == "osiris-piano").unwrap();
        assert_eq!(o.license.spdx, "CC0-1.0");
        assert!(!o.license.attribution_required);
        assert_eq!(o.format, "zip");
        let lc = o.license_check.as_ref().unwrap();
        assert_eq!(lc.mode, "metadata");
        assert!(lc.sha256.is_none());
        assert!(is_sha256(&o.download.as_ref().unwrap().sha256));
    }

    #[test]
    fn forbidden_licenses_are_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.license.spdx = "CC-BY-NC-4.0".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::ForbiddenLicense(_))
        ));
        e.license.spdx = "CC-BY-ND-4.0".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::ForbiddenLicense(_))
        ));
    }

    #[test]
    fn share_alike_is_forbidden_in_catalog() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.license.spdx = "CC-BY-SA-4.0".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::ForbiddenLicense(_))
        ));
    }

    #[test]
    fn cc_by_without_attribution_is_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.license.spdx = "CC-BY-3.0".to_owned();
        e.license.attribution_required = false;
        e.attribution = String::new();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::MissingAttribution(_))
        ));
    }

    #[test]
    fn bad_hash_is_rejected_and_good_hash_accepted() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.sha256 = Some("abc".to_owned());
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadHash(_))
        ));

        e.sha256 = Some("a".repeat(64));
        assert!(validate_catalog_entry(&e).is_ok());

        e.sha256 = Some("A".repeat(64));
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadHash(_))
        ));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.schema = 999;
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::UnsupportedSchema(999))
        ));
    }

    #[test]
    fn is_sha256_checks_format() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256("a"));
        assert!(!is_sha256(&"A".repeat(64)));
        assert!(!is_sha256(&"g".repeat(64)));
    }

    #[test]
    fn bad_download_is_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.download.as_mut().unwrap().sha256 = "abc".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadDownload(_, _))
        ));

        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.download.as_mut().unwrap().size_bytes = 0;
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadDownload(_, _))
        ));
    }

    #[test]
    fn bad_license_check_is_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.license_check.as_mut().unwrap().mode = "bogus".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadLicenseCheck(_, _))
        ));
    }

    #[test]
    fn text_hash_mode_requires_sha256() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.license_check.as_mut().unwrap().mode = "text-hash".to_owned();
        e.license_check.as_mut().unwrap().sha256 = None;
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadLicenseCheck(_, _))
        ));
    }

    #[test]
    fn bad_format_is_rejected() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.format = "rar".to_owned();
        assert!(matches!(
            validate_catalog_entry(&e),
            Err(CatalogError::BadFormat(_, _))
        ));
    }

    #[test]
    fn entry_without_download_is_not_downloadable() {
        let mut e = catalog().unwrap().into_iter().next().unwrap();
        e.download = None;
        e.verified = false;
        assert!(!is_downloadable(&e));
        assert!(validate_catalog_entry(&e).is_ok());
    }

    #[test]
    fn registry_round_trips() {
        let reg = InstalledRegistry {
            schema: REGISTRY_SCHEMA,
            banks: vec![InstalledBank {
                id: "osiris-piano".to_owned(),
                name: "Osiris Piano".to_owned(),
                family: "piano".to_owned(),
                path: "/tmp/banks/osiris-piano".to_owned(),
                license_spdx: "CC0-1.0".to_owned(),
                attribution: "Osiris Piano by Versilian Studios and Karoryfer Samples".to_owned(),
                license_acknowledged: false,
                installed_at: "12345T00:00:00Z".to_owned(),
            }],
        };
        let text = reg.to_text().unwrap();
        let back = InstalledRegistry::parse(&text).unwrap();
        assert_eq!(reg, back);
    }

    #[test]
    fn registry_round_trips_share_alike_with_acknowledgement() {
        let reg = InstalledRegistry {
            schema: REGISTRY_SCHEMA,
            banks: vec![InstalledBank {
                id: "local-sa".to_owned(),
                name: "Local SA bank".to_owned(),
                family: "piano".to_owned(),
                path: "/tmp/banks/local-sa".to_owned(),
                license_spdx: "CC-BY-SA-4.0".to_owned(),
                attribution: "Someone's bank".to_owned(),
                license_acknowledged: true,
                installed_at: "0T00:00:00Z".to_owned(),
            }],
        };
        let text = reg.to_text().unwrap();
        let back = InstalledRegistry::parse(&text).unwrap();
        assert_eq!(reg, back);
    }

    #[test]
    fn credits_text_includes_builtin_and_installed() {
        let installed = vec![InstalledBank {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
            family: "piano".to_owned(),
            path: "/tmp/banks/osiris-piano".to_owned(),
            license_spdx: "CC0-1.0".to_owned(),
            attribution: "Osiris Piano by Versilian Studios and Karoryfer Samples".to_owned(),
            license_acknowledged: false,
            installed_at: "0T00:00:00Z".to_owned(),
        }];
        let text = credits_text(&installed);
        assert!(text.contains(BUILTIN_PIANO_CREDIT));
        assert!(text.contains("Osiris Piano"));
        assert!(text.contains("CC0-1.0"));
    }

    #[test]
    fn credits_text_marks_required_attribution() {
        let installed = vec![InstalledBank {
            id: "salamander".to_owned(),
            name: "Salamander Grand Piano V3".to_owned(),
            family: "piano".to_owned(),
            path: "/tmp/banks/salamander".to_owned(),
            license_spdx: "CC-BY-3.0".to_owned(),
            attribution: "Salamander Grand Piano by Alexander Holm".to_owned(),
            license_acknowledged: false,
            installed_at: "0T00:00:00Z".to_owned(),
        }];
        let text = credits_text(&installed);
        assert!(text.contains("(attribution required)"));
    }

    #[test]
    fn instrument_list_builtin_installed_download_and_broken() {
        let dir = tempfile::tempdir().unwrap();
        let osiris = dir.path().join("osiris-piano");
        std::fs::create_dir(&osiris).unwrap();

        let installed = vec![InstalledBank {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
            family: "piano".to_owned(),
            path: osiris.to_string_lossy().into_owned(),
            license_spdx: "CC0-1.0".to_owned(),
            attribution: String::new(),
            license_acknowledged: false,
            installed_at: "0T00:00:00Z".to_owned(),
        }];
        // The catalog has salamander-grand-v3 and osiris-piano. Osiris is
        // installed; salamander is not, so it appears as a Download item.
        let catalog = catalog().unwrap();

        let items = instrument_list(&installed, &catalog, None);
        assert!(items.contains(&InstrumentListItem::BuiltinPiano));
        assert!(items.contains(&InstrumentListItem::Installed {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
        }));
        assert!(items.contains(&InstrumentListItem::Download {
            id: "salamander-grand-v3".to_owned(),
            name: "Salamander Grand Piano V3".to_owned(),
        }));
        // No broken item when nothing is selected.
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, InstrumentListItem::Broken { .. }))
        );
    }

    #[test]
    fn instrument_list_marks_selected_broken_bank() {
        let installed = vec![InstalledBank {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
            family: "piano".to_owned(),
            // The folder no longer exists.
            path: "/tmp/does/not/exist".to_owned(),
            license_spdx: "CC0-1.0".to_owned(),
            attribution: String::new(),
            license_acknowledged: false,
            installed_at: "0T00:00:00Z".to_owned(),
        }];
        let catalog = catalog().unwrap();

        // Selected and missing → Broken.
        let items = instrument_list(&installed, &catalog, Some("osiris-piano"));
        assert!(items.contains(&InstrumentListItem::Broken {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
        }));
        assert!(!items.iter().any(
            |i| matches!(i, InstrumentListItem::Installed { id, .. } if id == "osiris-piano")
        ));
    }

    #[test]
    fn resolve_bank_dir_uses_legacy_absolute_path_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // A legacy absolute folder path resolves directly, even without a
        // registry entry.
        let resolved = resolve_bank_dir(&path.to_string_lossy()).unwrap();
        assert_eq!(resolved, path);

        // A missing folder yields `None` (deleted bank → piano fallback).
        let missing = dir.path().join("nope");
        assert!(resolve_bank_dir(&missing.to_string_lossy()).is_none());
    }
}
