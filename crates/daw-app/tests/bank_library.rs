//! SPEC-066: catalog manifest validation, installed-registry round-trip, and
//! credits text, all without a data directory or network access.

use std::path::PathBuf;

use daw_app::bank_library::{
    BUILTIN_PIANO_CREDIT, CatalogError, InstalledBank, InstalledRegistry, catalog, credits_text,
    is_downloadable, is_sha256, parse_catalog_entry, validate_catalog_entry,
};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("daw-library-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn valid_entry_json() -> String {
    serde_json::json!({
        "schema": 1,
        "id": "test-bank",
        "name": "Test Bank",
        "family": "piano",
        "version": "1.0",
        "author": "Test Author",
        "license": { "spdx": "CC0-1.0", "url": "https://example.com/license" },
        "attribution": "",
        "source_url": "https://example.com/bank",
        "sha256": null,
        "size_bytes": null,
        "verified": false
    })
    .to_string()
}

#[test]
fn catalog_has_exactly_two_known_entries() {
    let entries = catalog().expect("embedded catalog parses");
    assert_eq!(entries.len(), 2);
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"salamander-grand-v3"));
    assert!(ids.contains(&"osiris-piano"));
    // SPEC-067: both entries are now fully verified and downloadable.
    assert!(entries.iter().all(is_downloadable));
    assert!(entries.iter().all(|e| e.verified));
}

#[test]
fn catalog_rejects_forbidden_licenses() {
    for spdx in [
        "CC-BY-NC-4.0",
        "CC-BY-ND-4.0",
        "CC-BY-SA-4.0",
        "GPL-3.0-only",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&valid_entry_json()).unwrap();
        value["license"]["spdx"] = serde_json::Value::String(spdx.to_owned());
        let entry = parse_catalog_entry(&value.to_string());
        assert!(
            matches!(entry, Err(CatalogError::ForbiddenLicense(_))),
            "{spdx} must be rejected from the catalog"
        );
    }
}

#[test]
fn catalog_requires_attribution_for_cc_by() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_entry_json()).unwrap();
    value["license"]["spdx"] = serde_json::Value::String("CC-BY-4.0".to_owned());
    value["license"]["attribution_required"] = serde_json::Value::Bool(true);
    // No attribution text.
    assert!(matches!(
        parse_catalog_entry(&value.to_string()),
        Err(CatalogError::MissingAttribution(_))
    ));

    // With attribution it parses.
    value["attribution"] = serde_json::Value::String("Test Author".to_owned());
    let entry = parse_catalog_entry(&value.to_string()).unwrap();
    assert_eq!(entry.license.spdx, "CC-BY-4.0");
}

#[test]
fn catalog_rejects_bad_hash_and_accepts_good_hash() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_entry_json()).unwrap();
    value["sha256"] = serde_json::Value::String("not-a-hash".to_owned());
    assert!(matches!(
        parse_catalog_entry(&value.to_string()),
        Err(CatalogError::BadHash(_))
    ));

    let good = "a".repeat(64);
    value["sha256"] = serde_json::Value::String(good.clone());
    let entry = parse_catalog_entry(&value.to_string()).unwrap();
    assert_eq!(entry.sha256.as_deref(), Some(good.as_str()));
    assert!(is_sha256(&good));
}

#[test]
fn catalog_rejects_unknown_schema() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_entry_json()).unwrap();
    value["schema"] = serde_json::Value::Number(99.into());
    assert!(matches!(
        parse_catalog_entry(&value.to_string()),
        Err(CatalogError::UnsupportedSchema(99))
    ));
}

#[test]
fn registry_round_trips_through_disk() {
    let dir = temp_dir("registry");
    let path = dir.join("banks.json");
    let reg = InstalledRegistry {
        schema: 1,
        banks: vec![InstalledBank {
            id: "osiris-piano".to_owned(),
            name: "Osiris Piano".to_owned(),
            family: "piano".to_owned(),
            path: "/nonexistent/osiris-piano".to_owned(),
            license_spdx: "CC0-1.0".to_owned(),
            attribution: "Osiris Piano by Versilian Studios and Karoryfer Samples".to_owned(),
            license_acknowledged: false,
            installed_at: "1T00:00:00Z".to_owned(),
        }],
    };
    reg.save(&path).expect("save registry");
    let back = InstalledRegistry::load(&path).expect("load registry");
    assert_eq!(reg, back);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_registry_is_empty() {
    let dir = temp_dir("missing");
    let reg = InstalledRegistry::load(&dir.join("banks.json")).expect("load missing registry");
    assert_eq!(reg.schema, 1);
    assert!(reg.banks.is_empty());
}

#[test]
fn credits_text_starts_with_builtin_piano_and_lists_banks() {
    let installed = vec![InstalledBank {
        id: "salamander".to_owned(),
        name: "Salamander Grand Piano V3".to_owned(),
        family: "piano".to_owned(),
        path: "/nonexistent/salamander".to_owned(),
        license_spdx: "CC-BY-3.0".to_owned(),
        attribution: "Salamander Grand Piano by Alexander Holm".to_owned(),
        license_acknowledged: false,
        installed_at: "1T00:00:00Z".to_owned(),
    }];
    let text = credits_text(&installed);
    assert!(text.starts_with(BUILTIN_PIANO_CREDIT));
    assert!(text.contains("Salamander Grand Piano V3"));
    assert!(text.contains("CC-BY-3.0"));
    assert!(text.contains("(attribution required)"));
}

#[test]
fn credits_text_handles_empty_bank_list() {
    let text = credits_text(&[]);
    assert_eq!(text.trim_end(), BUILTIN_PIANO_CREDIT.trim_end());
}

#[test]
fn validation_checks_required_fields() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_entry_json()).unwrap();
    value["name"] = serde_json::Value::String(String::new());
    assert!(matches!(
        parse_catalog_entry(&value.to_string()),
        Err(CatalogError::MissingField("name", _))
    ));
}

#[test]
fn validate_accepts_a_valid_entry() {
    let entry = parse_catalog_entry(&valid_entry_json()).unwrap();
    assert!(validate_catalog_entry(&entry).is_ok());
}
