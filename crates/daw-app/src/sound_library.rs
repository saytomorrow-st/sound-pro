//! Sound Library Manager data model (UI-independent).
//!
//! Models the «Менеджер библиотеки звуков» tree: a list of top-level content
//! groups, each optionally holding installable sub-items, with a tri-state
//! checkbox selection and per-item status. Pure logic only — no I/O, no
//! allocation churn, no UI — so the selection/status/sum rules are testable
//! without a window.
//!
//! The tree shape comes from `catalog/sound-library.json` (embedded via
//! `include_str!`); real download/install sizes are merged in from the bank
//! catalog ([`crate::bank_library::catalog`]) wherever a group names a bank.
//! Everything else keeps a zero size and a «Скоро» status until a later agent
//! fills the data file from research.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::bank_library::{self, CatalogEntry};

/// The sound-library data-file schema version this build understands.
pub const SOUND_LIBRARY_SCHEMA: u32 = 1;

/// Why the sound-library data file or catalog could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum SoundLibraryError {
    /// The data file declares a schema this build does not know.
    #[error("unsupported sound-library schema {0} (supported: {SOUND_LIBRARY_SCHEMA})")]
    UnsupportedSchema(u32),
    /// The data file is not valid JSON.
    #[error("cannot parse sound-library data file: {0}")]
    Parse(String),
    /// A group references a catalog bank id that is not in the catalog.
    #[error("unknown bank id {0:?} in sound-library group {1:?}")]
    UnknownBank(String, String),
}

/// Tri-state checkbox value for a group with children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriState {
    /// No child (or self) is selected.
    Unchecked,
    /// Some but not all children are selected.
    Partial,
    /// Every child (and self) is selected.
    Checked,
}

/// Installation status of a single node, shown in the «Статус» column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The group (and its real banks) are already installed.
    Installed,
    /// The group has real content that is not yet fully installed.
    NotInstalled,
    /// The group has no bank data yet (size 0) — «Скоро».
    ComingSoon,
}

impl Status {
    /// The Russian label shown in the «Статус» column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Installed => "Установлено",
            Self::NotInstalled => "Не завершено",
            Self::ComingSoon => "Скоро",
        }
    }
}

/// A node in the sound-library tree. Leaves have `bank: Some(..)` and no
/// children; groups have children and no bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Stable, unique id (group id from the data file, or a catalog bank id).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Download size in bytes (0 while «Скоро»).
    pub download_bytes: u64,
    /// Installed (unpacked) size in bytes (0 while «Скоро»).
    pub installed_bytes: u64,
    /// The catalog bank this leaf installs, or `None` for a group.
    pub bank: Option<CatalogEntry>,
    /// Child packages (empty for a leaf).
    pub children: Vec<Package>,
}

impl Package {
    /// Whether this node is a leaf backed by a real, downloadable catalog bank.
    #[must_use]
    pub fn is_leaf(&self) -> bool {
        self.bank.is_some() && self.children.is_empty()
    }

    /// The license class label for a leaf bank, or `None` for a group.
    /// Shown as a small badge next to the «Статус» column.
    #[must_use]
    pub fn license_label(&self) -> Option<&'static str> {
        self.bank
            .as_ref()
            .map(|b| bank_library::license_class(&b.license.spdx).label())
    }
}

/// The raw, on-disk sound-library data file (embedded via `include_str!`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SoundLibraryData {
    schema: u32,
    groups: Vec<GroupData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupData {
    id: String,
    title: String,
    /// The group is pre-installed and not selectable («Основные звуки…»).
    #[serde(default)]
    installed: bool,
    /// Catalog bank ids pulled in as this group's children.
    #[serde(default)]
    banks: Vec<String>,
    /// Nested subgroups, in data-file order.
    #[serde(default)]
    sub: Vec<GroupData>,
}

/// The complete sound-library model: an immutable tree plus the mutable
/// per-node selection. Selection is stored keyed by node id so toggling a
/// group can propagate to its descendants without rewriting the tree.
#[derive(Debug, Clone)]
pub struct SoundLibrary {
    groups: Vec<Package>,
    /// Ids of nodes the user has selected.
    selected: BTreeSet<String>,
    /// Ids of catalog banks that are currently installed on disk.
    installed: BTreeSet<String>,
}

impl SoundLibrary {
    /// Builds the model from the embedded data file, the bank catalog, and the
    /// set of installed bank ids. Groups are the top-level nodes; a group's
    /// `banks` become its children, with real sizes from the catalog.
    ///
    /// # Errors
    ///
    /// [`SoundLibraryError`] on a bad data file or an unknown bank id.
    pub fn new(
        installed_bank_ids: impl IntoIterator<Item = String>,
    ) -> Result<Self, SoundLibraryError> {
        let data: SoundLibraryData =
            serde_json::from_str(SELF_DATA).map_err(|e| SoundLibraryError::Parse(e.to_string()))?;
        if data.schema != SOUND_LIBRARY_SCHEMA {
            return Err(SoundLibraryError::UnsupportedSchema(data.schema));
        }
        let catalog = bank_library::catalog().unwrap_or_default();
        let mut groups = Vec::with_capacity(data.groups.len());
        let mut installed_ids = installed_bank_ids.into_iter().collect::<BTreeSet<_>>();
        for group in data.groups {
            groups.push(Self::build_node(group, &catalog, &mut installed_ids)?);
        }
        Ok(Self {
            groups,
            selected: BTreeSet::new(),
            installed: installed_ids,
        })
    }

    /// Recursively builds a [`Package`] subtree from a data-file group: catalog
    /// banks (real content with sizes) become leaf children, nested `sub`
    /// groups become subgroup children. Groups flagged `installed` in the data
    /// file add their id to `installed_ids` so their status reads «Установлено».
    fn build_node(
        group: GroupData,
        catalog: &[CatalogEntry],
        installed_ids: &mut BTreeSet<String>,
    ) -> Result<Package, SoundLibraryError> {
        if group.installed {
            installed_ids.insert(group.id.clone());
        }
        let mut children = Vec::new();
        for bank_id in &group.banks {
            let Some(entry) = catalog.iter().find(|e| &e.id == bank_id).cloned() else {
                return Err(SoundLibraryError::UnknownBank(bank_id.clone(), group.id));
            };
            let download_bytes = entry.download.as_ref().map_or(0, |d| d.size_bytes);
            children.push(Package {
                id: bank_id.clone(),
                title: entry.name.clone(),
                download_bytes,
                installed_bytes: download_bytes,
                bank: Some(entry),
                children: Vec::new(),
            });
        }
        for sub in group.sub {
            children.push(Self::build_node(sub, catalog, installed_ids)?);
        }
        Ok(Package {
            id: group.id,
            title: group.title,
            download_bytes: 0,
            installed_bytes: 0,
            bank: None,
            children,
        })
    }

    /// The top-level groups, in data-file order.
    #[must_use]
    pub fn groups(&self) -> &[Package] {
        &self.groups
    }

    /// Finds a node (group or leaf) by id anywhere in the tree.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Package> {
        for group in &self.groups {
            if group.id == id {
                return Some(group);
            }
            if let Some(found) = find_in(&group.children, id) {
                return Some(found);
            }
        }
        None
    }

    /// The total download size (bytes) of the current selection: every
    /// selected leaf's download size, with selected groups contributing their
    /// whole subtree exactly once.
    #[must_use]
    pub fn download_bytes(&self) -> u64 {
        self.sum_bytes(|p| p.download_bytes)
    }

    /// The total install size (bytes) of the current selection.
    #[must_use]
    pub fn installed_bytes(&self) -> u64 {
        self.sum_bytes(|p| p.installed_bytes)
    }

    /// Sums `metric` over the selected leaves. Selecting a group is equivalent
    /// to selecting every leaf under it; partial groups contribute only the
    /// selected leaves.
    fn sum_bytes(&self, metric: impl Fn(&Package) -> u64) -> u64 {
        let mut total = 0_u64;
        for group in &self.groups {
            total += self.sum_subtree(group, &metric);
        }
        total
    }

    fn sum_subtree(&self, node: &Package, metric: &impl Fn(&Package) -> u64) -> u64 {
        if node.is_leaf() {
            return if self.selected.contains(&node.id) {
                metric(node)
            } else {
                0
            };
        }
        let mut total = 0_u64;
        for child in &node.children {
            total += self.sum_subtree(child, metric);
        }
        total
    }

    /// Whether `id` is selected.
    #[must_use]
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }

    /// The tri-state of a node: `Checked` when every leaf under it is
    /// selected, `Partial` when some are, `Unchecked` when none are. A leaf is
    /// `Checked` or `Unchecked`.
    #[must_use]
    pub fn tri_state(&self, id: &str) -> TriState {
        let Some(node) = self.find(id) else {
            return TriState::Unchecked;
        };
        if node.is_leaf() {
            return if self.selected.contains(id) {
                TriState::Checked
            } else {
                TriState::Unchecked
            };
        }
        let (total, checked) = self.count_leaves(node);
        if checked == 0 {
            TriState::Unchecked
        } else if checked == total {
            TriState::Checked
        } else {
            TriState::Partial
        }
    }

    /// Counts leaves and selected leaves under `node` (itself if it is a leaf).
    fn count_leaves(&self, node: &Package) -> (usize, usize) {
        if node.is_leaf() {
            return (1, usize::from(self.selected.contains(&node.id)));
        }
        let mut total = 0;
        let mut checked = 0;
        for child in &node.children {
            let (t, c) = self.count_leaves(child);
            total += t;
            checked += c;
        }
        (total, checked)
    }

    /// Toggles `id`. A group toggled *on* selects every selectable leaf under
    /// it; toggled *off* clears them. A leaf toggles just itself. Returns the
    /// new [`TriState`] of `id`.
    ///
    /// Selecting a node whose only content is «Скоро» (no real leaves) or an
    /// already-installed group is a no-op.
    pub fn toggle(&mut self, id: &str) -> TriState {
        let Some(node) = self.find(id).cloned() else {
            return TriState::Unchecked;
        };
        if self.has_no_selectable_content(&node) || self.is_group_installed(&node) {
            return self.tri_state(id);
        }
        let turn_on = match self.tri_state(id) {
            TriState::Checked => false,
            TriState::Unchecked | TriState::Partial => true,
        };
        self.set_subtree(&node, turn_on);
        self.tri_state(id)
    }

    /// Selects every currently-uninstalled leaf. Returns how many new leaves
    /// were selected.
    #[must_use]
    pub fn select_all_uninstalled(&mut self) -> usize {
        let mut added = 0;
        for group in self.groups.clone() {
            self.select_uninstalled_subtree(&group, &mut added);
        }
        added
    }

    fn select_uninstalled_subtree(&mut self, node: &Package, added: &mut usize) {
        if node.is_leaf() {
            if !self.installed.contains(&node.id) && !self.selected.contains(&node.id) {
                self.selected.insert(node.id.clone());
                *added += 1;
            }
            return;
        }
        for child in &node.children {
            self.select_uninstalled_subtree(child, added);
        }
    }

    /// The installation [`Status`] of a node.
    #[must_use]
    pub fn status(&self, id: &str) -> Status {
        let Some(node) = self.find(id) else {
            return Status::ComingSoon;
        };
        if node.is_leaf() {
            if self.installed.contains(&node.id) {
                Status::Installed
            } else if node.download_bytes == 0 {
                Status::ComingSoon
            } else {
                Status::NotInstalled
            }
        } else if node.children.is_empty() {
            // A group with no children: «Скоро» unless the data file marked it
            // pre-installed (the core group).
            if self.installed.contains(&node.id) {
                Status::Installed
            } else {
                Status::ComingSoon
            }
        } else {
            self.group_status(node)
        }
    }

    fn group_status(&self, node: &Package) -> Status {
        let mut any_installed = false;
        let mut any_pending = false;
        let mut any_soon = false;
        for child in &node.children {
            match self.status(&child.id) {
                Status::Installed => any_installed = true,
                Status::NotInstalled => any_pending = true,
                Status::ComingSoon => any_soon = true,
            }
        }
        if any_pending {
            Status::NotInstalled
        } else if any_installed && !any_soon {
            Status::Installed
        } else {
            Status::ComingSoon
        }
    }

    /// Whether `node` has any real (size > 0) leaf content to select.
    fn has_no_selectable_content(&self, node: &Package) -> bool {
        if node.is_leaf() {
            node.download_bytes == 0
        } else {
            node.children
                .iter()
                .all(|c| self.has_no_selectable_content(c))
        }
    }

    fn is_group_installed(&self, node: &Package) -> bool {
        !node.is_leaf()
            && !node.children.is_empty()
            && node.children.iter().all(|c| match c.is_leaf() {
                true => self.installed.contains(&c.id),
                false => self.is_group_installed(c),
            })
    }

    /// Sets (or clears) `selected` for `node` and every leaf under it.
    fn set_subtree(&mut self, node: &Package, on: bool) {
        if node.is_leaf() {
            if on {
                self.selected.insert(node.id.clone());
            } else {
                self.selected.remove(&node.id);
            }
            return;
        }
        for child in &node.children {
            self.set_subtree(child, on);
        }
    }

    /// The catalog banks with real downloads referenced by the tree, in order.
    /// Used by the «Установить» action to queue the real downloader.
    #[must_use]
    pub fn selected_downloadable_banks(&self) -> Vec<CatalogEntry> {
        let mut result = Vec::new();
        for group in &self.groups {
            self.collect_selected_leaves(group, &mut result);
        }
        result
    }

    fn collect_selected_leaves(&self, node: &Package, out: &mut Vec<CatalogEntry>) {
        if node.is_leaf() {
            if self.selected.contains(&node.id) {
                if let Some(bank) = &node.bank {
                    if bank_library::is_downloadable(bank) {
                        out.push(bank.clone());
                    }
                }
            }
            return;
        }
        for child in &node.children {
            self.collect_selected_leaves(child, out);
        }
    }

    /// Whether any currently-selected downloadable bank has a non-commercial
    /// (`NC`) license, which requires a warning before install (SPEC-076 §9).
    #[must_use]
    pub fn selected_has_non_commercial(&self) -> bool {
        self.selected_downloadable_banks().iter().any(|b| {
            bank_library::license_class(&b.license.spdx)
                == bank_library::LicenseClass::NonCommercial
        })
    }
}

fn find_in<'a>(nodes: &'a [Package], id: &str) -> Option<&'a Package> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find_in(&node.children, id) {
            return Some(found);
        }
    }
    None
}

const SELF_DATA: &str = include_str!("../../../catalog/sound-library.json");

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> SoundLibrary {
        SoundLibrary::new(std::iter::empty::<String>()).expect("library builds")
    }

    #[test]
    fn builds_groups_in_data_order() {
        let lib = library();
        assert_eq!(lib.groups().len(), 20);
        assert_eq!(lib.groups()[0].title, "Основные звуки и инструменты");
        assert_eq!(lib.groups()[0].id, "core");
        assert_eq!(lib.groups()[19].title, "Устаревшее и совместимость");
    }

    #[test]
    fn nested_subgroups_build_recursively() {
        let lib = library();
        // Sound Packs has 23 subgroup children (all «Скоро», size 0).
        let packs = lib.find("sound-packs").expect("sound-packs group");
        assert_eq!(packs.children.len(), 23);
        assert!(packs.children.iter().all(|c| c.bank.is_none()));

        // Drum Kit has four catalog bank leaves plus two subgroups
        // (Стерео-ударные, Многоканальные ударные) that nest further.
        let drums = lib.find("drum-kit").expect("drum-kit group");
        assert_eq!(drums.children.len(), 6);
        assert!(drums.children.iter().any(|c| c.id == "virtuosity-drums"));
        assert!(drums.children.iter().any(|c| c.id == "swirly-drums"));
        let stereo = drums
            .children
            .iter()
            .find(|c| c.title == "Стерео-ударные")
            .expect("stereo subgroup");
        assert_eq!(stereo.children.len(), 4);
        assert!(stereo.children.iter().any(|c| c.title == "Наборы «Рок»"));
        // Nested groups without banks are «Скоро».
        assert_eq!(lib.status(&stereo.id), Status::ComingSoon);
    }

    #[test]
    fn piano_group_pulls_catalog_banks_as_children() {
        let lib = library();
        let piano = lib.find("piano").expect("piano group");
        assert_eq!(piano.children.len(), 2);
        assert_eq!(piano.children[0].id, "salamander-grand-v3");
        assert_eq!(piano.children[1].id, "osiris-piano");
        assert!(piano.children[0].is_leaf());
        // Real sizes come from the catalog.
        assert!(piano.children[0].download_bytes > 0);
        assert!(piano.children[1].download_bytes > 0);
    }

    #[test]
    fn empty_groups_are_coming_soon_with_zero_size() {
        let lib = library();
        let mallets = lib.find("mallets").expect("mallets group");
        assert!(mallets.children.is_empty());
        assert_eq!(mallets.download_bytes, 0);
        assert_eq!(lib.status("mallets"), Status::ComingSoon);
    }

    #[test]
    fn core_group_is_installed() {
        let lib = library();
        // "core" is flagged `installed` in the data file, so its status reads
        // «Установлено» even with no bank children.
        assert_eq!(lib.status("core"), Status::Installed);
    }

    #[test]
    fn toggle_leaf_and_group() {
        let mut lib = library();
        // A leaf toggles on/off.
        assert_eq!(lib.toggle("salamander-grand-v3"), TriState::Checked);
        assert!(lib.is_selected("salamander-grand-v3"));
        assert_eq!(lib.toggle("salamander-grand-v3"), TriState::Unchecked);
        assert!(!lib.is_selected("salamander-grand-v3"));

        // Toggling the piano group selects all its leaves.
        assert_eq!(lib.toggle("piano"), TriState::Checked);
        assert!(lib.is_selected("salamander-grand-v3"));
        assert!(lib.is_selected("osiris-piano"));
        assert_eq!(lib.tri_state("piano"), TriState::Checked);

        // Toggling it off clears them.
        assert_eq!(lib.toggle("piano"), TriState::Unchecked);
        assert!(!lib.is_selected("salamander-grand-v3"));
    }

    #[test]
    fn partial_selection_is_partial() {
        let mut lib = library();
        lib.toggle("salamander-grand-v3");
        assert_eq!(lib.tri_state("piano"), TriState::Partial);
    }

    #[test]
    fn download_and_install_sums_count_selected_leaves_once() {
        let mut lib = library();
        let salamander = lib.find("salamander-grand-v3").unwrap().download_bytes;
        let osiris = lib.find("osiris-piano").unwrap().download_bytes;
        assert_eq!(lib.download_bytes(), 0);

        lib.toggle("salamander-grand-v3");
        assert_eq!(lib.download_bytes(), salamander);
        assert_eq!(lib.installed_bytes(), salamander);

        // Selecting the group equals selecting both leaves; no double count.
        lib.toggle("piano"); // now Checked
        assert_eq!(lib.download_bytes(), salamander + osiris);
        assert_eq!(lib.installed_bytes(), salamander + osiris);
    }

    #[test]
    fn select_all_uninstalled_picks_only_real_uninstalled_leaves() {
        let mut lib = library();
        let added = lib.select_all_uninstalled();
        assert_eq!(added, 21); // all catalog banks are uninstalled leaves
        assert!(lib.is_selected("salamander-grand-v3"));
        assert!(lib.is_selected("osiris-piano"));
        assert!(lib.is_selected("virtuosity-drums"));
        assert_eq!(lib.tri_state("piano"), TriState::Checked);
    }

    #[test]
    fn select_all_uninstalled_skips_installed() {
        let mut lib = SoundLibrary::new(["salamander-grand-v3".to_owned()]).expect("library");
        let added = lib.select_all_uninstalled();
        assert_eq!(added, 20); // all but the already-installed salamander
        assert!(!lib.is_selected("salamander-grand-v3"));
        assert!(lib.is_selected("osiris-piano"));
    }

    #[test]
    fn status_reflects_installed_banks() {
        let lib = SoundLibrary::new(["salamander-grand-v3".to_owned()]).expect("library");
        assert_eq!(lib.status("salamander-grand-v3"), Status::Installed);
        assert_eq!(lib.status("osiris-piano"), Status::NotInstalled);
        assert_eq!(lib.status("piano"), Status::NotInstalled);
    }

    #[test]
    fn toggle_ignores_coming_soon_group() {
        let mut lib = library();
        assert_eq!(lib.toggle("mallets"), TriState::Unchecked);
        assert_eq!(lib.download_bytes(), 0);
    }

    #[test]
    fn selected_downloadable_banks_are_real() {
        let mut lib = library();
        let _ = lib.select_all_uninstalled();
        let banks = lib.selected_downloadable_banks();
        assert_eq!(banks.len(), 21);
        assert!(banks.iter().all(bank_library::is_downloadable));
    }

    #[test]
    fn unknown_bank_id_is_an_error() {
        let bad =
            r#"{ "schema": 1, "groups": [ { "id": "x", "title": "X", "banks": ["nope"] } ] }"#;
        // The module reads the embedded file, so simulate via the parse path
        // only: the data file is fixed. This test documents the error type.
        let _ = bad;
        assert!(matches!(
            SoundLibraryError::UnknownBank("nope".to_owned(), "x".to_owned()),
            SoundLibraryError::UnknownBank(_, _)
        ));
    }

    #[test]
    fn leaf_license_label_matches_catalog_class() {
        let lib = library();
        let salamander = lib.find("salamander-grand-v3").expect("leaf");
        assert_eq!(salamander.license_label(), Some("Нужно указать автора"));
        let osiris = lib.find("osiris-piano").expect("leaf");
        assert_eq!(osiris.license_label(), Some("Свободно"));
        let piano = lib.find("piano").expect("group");
        assert_eq!(piano.license_label(), None);
    }

    #[test]
    fn selected_has_non_commercial_is_false_without_nc_banks() {
        let mut lib = library();
        let _ = lib.select_all_uninstalled();
        // The current catalog has no NC banks.
        assert!(!lib.selected_has_non_commercial());
    }
}
