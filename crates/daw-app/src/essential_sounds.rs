//! The «Основные звуки» (essential sounds) pack (SPEC-076).
//!
//! A fixed, curated set of catalog banks that ships as the default first-run
//! download — the analogue of Logic Pro's pre-installed content. The pack is
//! a hardcoded list of catalog ids (chosen over a separate manifest file for
//! simplicity: it is small, versioned with the binary, and trivially testable).
//! Every listed bank must exist in the catalog, be `verified`, and carry a
//! known download size; a bank that fails to resolve is skipped rather than
//! making the whole pack fail.
//!
//! The pack deliberately contains only CC0/CC-BY banks (no share-alike or
//! non-commercial licenses), so the first-run download never needs a license
//! acknowledgement dialog.

use crate::bank_library::{self, CatalogEntry};

/// The catalog ids of the «Основные звуки» pack, in install order.
///
/// The base set is the eight CC0/CC-BY banks named in the SPEC-076 brief:
/// Salamander Grand, Virtuosity Drums, Shinyguitar, Meatbass, Bigcat Cello,
/// Bear Sax, String Cyborgs, Merry Orks.
pub const ESSENTIAL_BANK_IDS: [&str; 8] = [
    "salamander-grand-v3",
    "virtuosity-drums",
    "shinyguitar",
    "meatbass",
    "bigcat-cello",
    "bear-sax",
    "string-cyborgs",
    "merry-orks",
];

/// Resolves the essential pack to its catalog entries, in [`ESSENTIAL_BANK_IDS`]
/// order. Banks that are missing from the catalog, unverified, or have no
/// download are silently dropped so the pack degrades gracefully.
#[must_use]
pub fn essential_banks() -> Vec<CatalogEntry> {
    let catalog = bank_library::catalog().unwrap_or_default();
    ESSENTIAL_BANK_IDS
        .iter()
        .filter_map(|id| {
            catalog
                .iter()
                .find(|e| &e.id == id)
                .filter(|e| bank_library::is_downloadable(e))
                .cloned()
        })
        .collect()
}

/// The catalog ids of the essential pack that are not yet installed on disk.
///
/// # Errors
///
/// [`bank_library::LibraryError`] if the installed registry cannot be read.
pub fn essential_banks_missing() -> Result<Vec<CatalogEntry>, bank_library::LibraryError> {
    let installed: std::collections::HashSet<String> = bank_library::list_installed()?
        .into_iter()
        .map(|b| b.id)
        .collect();
    Ok(essential_banks()
        .into_iter()
        .filter(|e| !installed.contains(&e.id))
        .collect())
}

/// Whether every essential bank is installed on disk.
///
/// # Errors
///
/// [`bank_library::LibraryError`] if the installed registry cannot be read.
pub fn essential_installed() -> Result<bool, bank_library::LibraryError> {
    Ok(essential_banks_missing()?.is_empty())
}

/// The total download size (bytes) of the essential pack.
#[must_use]
pub fn essential_download_bytes() -> u64 {
    essential_banks()
        .iter()
        .filter_map(|e| e.download.as_ref())
        .map(|d| d.size_bytes)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essential_pack_resolves_all_eight_banks() {
        let banks = essential_banks();
        assert_eq!(banks.len(), 8);
        let ids: Vec<&str> = banks.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ESSENTIAL_BANK_IDS);
        // All essential banks are downloadable and have a known size.
        assert!(banks.iter().all(bank_library::is_downloadable));
        assert!(
            banks
                .iter()
                .all(|e| e.download.as_ref().is_some_and(|d| d.size_bytes > 0))
        );
    }

    #[test]
    fn essential_pack_has_a_nonzero_total_size() {
        assert!(essential_download_bytes() > 0);
    }

    #[test]
    fn essential_pack_is_cc0_or_cc_by_only() {
        for bank in essential_banks() {
            assert!(
                matches!(bank.license.spdx.as_str(), "CC0-1.0" | "CC-BY-3.0"),
                "essential bank {} has unexpected license {}",
                bank.id,
                bank.license.spdx
            );
        }
    }

    #[test]
    fn essential_pack_contains_the_requested_base_set() {
        let ids: Vec<&str> = ESSENTIAL_BANK_IDS.to_vec();
        for expected in [
            "salamander-grand-v3",
            "virtuosity-drums",
            "shinyguitar",
            "meatbass",
            "bigcat-cello",
            "bear-sax",
            "string-cyborgs",
            "merry-orks",
        ] {
            assert!(ids.contains(&expected), "missing essential bank {expected}");
        }
    }
}
