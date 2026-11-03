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
    /// Path to the main `.sfz` inside the archive (`/` separators), or `None`
    /// to use the first `.sfz` found in the installed bank root.
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

    /// Writes the registry to `path`, creating its folder.
    pub fn save(&self, path: &Path) -> Result<(), LibraryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| LibraryError::Io(dir.to_path_buf(), e.to_string()))?;
        }
        std::fs::write(path, self.to_text()?)
            .map_err(|e| LibraryError::Io(path.to_path_buf(), e.to_string()))
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
    match entry.format.as_str() {
        "zip" | "tar.bz2" => {}
        other => return Err(CatalogError::BadFormat(other.to_owned(), entry.id.clone())),
    }
    Ok(())
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
    Ok(InstalledRegistry::load(&path)?.banks)
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

/// Appends (or replaces) a bank entry in the registry file at `registry_file`.
///
/// Used by both [`install_local_bank`] (the user data registry) and the
/// downloader (which writes `banks.json` next to the banks it installs, so a
/// download can target a non-default directory in tests).
///
/// # Errors
///
/// [`LibraryError`] on I/O or parse failure.
pub fn record_installed_bank(
    registry_file: &Path,
    bank: &InstalledBank,
) -> Result<(), LibraryError> {
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

/// Current UTC time as a minimal ISO-8601-ish string (install timestamps).
#[must_use]
pub fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let h = rem / 3_600;
    let m = (rem % 3_600) / 60;
    let s = rem % 60;
    format!("{days}T{h:02}:{m:02}:{s:02}Z")
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
}
