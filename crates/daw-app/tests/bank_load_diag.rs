//! One-off diagnostic: load the owner's real installed banks and report the
//! outcome. Ignored by default; run explicitly with
//! `cargo test -p daw-app --test bank_load_diag -- --ignored --nocapture`.

use std::path::PathBuf;

use daw_app::sample_bank::load_bank_with_hint;

fn banks_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME")).join(".local/share/sound-pro/banks")
}

fn probe(label: &str, dir: PathBuf, hint: &str) {
    if !dir.is_dir() {
        println!("[{label}] SKIP: {} not present", dir.display());
        return;
    }
    match load_bank_with_hint(
        &dir,
        daw_app::sample_bank::DEFAULT_RAM_BUDGET_MIB,
        Some(hint),
    ) {
        Ok(bank) => println!(
            "[{label}] OK: {} regions, {} MiB, {} decode failures",
            bank.regions.len(),
            bank.ram_bytes.div_ceil(1024 * 1024),
            bank.decode_failures.len()
        ),
        Err(e) => println!("[{label}] FAIL: {e}"),
    }
}

#[test]
#[ignore]
fn load_owner_banks() {
    probe(
        "osiris",
        banks_root().join("osiris-piano"),
        "Osiris Piano/Programs/01-natural.sfz",
    );
    probe(
        "salamander",
        banks_root().join("salamander-grand-v3"),
        "SalamanderGrandPianoV3_44.1khz16bit/SalamanderGrandPianoV3.sfz",
    );
}
