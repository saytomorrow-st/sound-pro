//! Registry/disk reconciliation of installed banks (picker + install detection).

use std::path::Path;

use daw_app::bank_library::{
    InstalledBank, InstrumentListItem, catalog, instrument_list, reconcile_installed,
};

fn registered(id: &str, path: &Path) -> InstalledBank {
    InstalledBank {
        id: id.to_owned(),
        name: id.to_owned(),
        family: "piano".to_owned(),
        path: path.to_string_lossy().into_owned(),
        license_spdx: "CC0-1.0".to_owned(),
        attribution: String::new(),
        license_acknowledged: false,
        installed_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn nested_sfz(root: &Path, id: &str) {
    let dir = root.join(id).join("Some Pack").join("Programs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("any.sfz"), "<region> sample=x.wav\n").unwrap();
}

#[test]
fn unregistered_folder_with_sfz_is_adopted_and_not_offered_for_download() {
    let root = tempfile::tempdir().unwrap();
    nested_sfz(root.path(), "osiris-piano");
    let catalog = catalog().unwrap();

    let (list, adopted) = reconcile_installed(root.path(), Vec::new(), &catalog);
    assert_eq!(adopted.len(), 1);
    assert_eq!(list[0].id, "osiris-piano");

    let items = instrument_list(&list, &catalog, None);
    assert!(
        items
            .iter()
            .any(|i| matches!(i, InstrumentListItem::Installed { id, .. } if id == "osiris-piano"))
    );
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, InstrumentListItem::Download { id, .. } if id == "osiris-piano"))
    );
    assert!(
        root.path().join("osiris-piano").is_dir(),
        "no data is deleted"
    );
}

#[test]
fn registry_entry_without_loadable_sfz_is_not_installed() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("osiris-piano");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("junk.txt"), "x").unwrap();
    let catalog = catalog().unwrap();

    let (list, adopted) = reconcile_installed(
        root.path(),
        vec![registered("osiris-piano", &dir)],
        &catalog,
    );
    assert!(list.is_empty() && adopted.is_empty());
    let items = instrument_list(&list, &catalog, None);
    assert!(
        items
            .iter()
            .any(|i| matches!(i, InstrumentListItem::Download { id, .. } if id == "osiris-piano"))
    );
    assert!(dir.join("junk.txt").exists(), "no data is deleted");
}

#[test]
fn registered_bank_with_nested_sfz_stays_installed() {
    let root = tempfile::tempdir().unwrap();
    nested_sfz(root.path(), "osiris-piano");
    let catalog = catalog().unwrap();
    let bank = registered("osiris-piano", &root.path().join("osiris-piano"));

    let (list, adopted) = reconcile_installed(root.path(), vec![bank.clone()], &catalog);
    assert_eq!(list, vec![bank]);
    assert!(adopted.is_empty());
}
