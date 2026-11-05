//! Reading actual available memory so a sample bank that does not fit is
//! trimmed (fewer velocity layers / mono downmix) instead of being OOM-killed.
//!
//! Linux exposes `MemAvailable` in `/proc/meminfo`; it is the kernel's own
//! estimate of memory a process may allocate without pushing the system into
//! swap, which is exactly the number a bank loader should honour. This module
//! is control-thread only: it reads a file and must never be called from the
//! audio callback.

use std::path::Path;

/// Bytes the loader keeps free when sizing a bank against `MemAvailable`, to
/// leave headroom for the UI, the OS, and the rest of the process.
const SAFETY_MARGIN_MIB: u64 = 256;

/// Smallest budget we are willing to apply: below this the trimmed bank would
/// be too crippled to be useful, so the constant budget is left alone.
const FLOOR_MIB: u64 = 512;

/// `MemAvailable` value, in MiB, read from `/proc/meminfo` (`None` when the
/// file is absent — e.g. macOS/Windows — or unreadable).
///
/// `/proc/meminfo` reports `MemAvailable` in `kB`; the last digits are rarely
/// zero so we round up to avoid under-budgeting on a `XX999 kB` line.
#[must_use]
pub fn available_memory_mib() -> Option<u64> {
    available_memory_mib_from(Path::new("/proc/meminfo"))
}

/// [`available_memory_mib`] reading from `path`, so tests can feed a fixture.
#[must_use]
pub fn available_memory_mib_from(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next()? != "MemAvailable:" {
            continue;
        }
        let kb: u64 = parts.next()?.parse().ok()?;
        // kB → MiB, rounding up so a partial MiB is still counted.
        return Some(kb.div_ceil(1024));
    }
    None
}

/// The RAM budget a bank loader should use: the configured default, capped by
/// what is actually free on this machine.
///
/// On Linux with a readable `MemAvailable` the budget is
/// `min(default_mib, MemAvailable - SAFETY_MARGIN_MIB)`, clamped to be at
/// least [`FLOOR_MIB`] (a floor below which trimming would be pointless).
/// When `MemAvailable` is unreadable (non-Linux, containers hiding the file)
/// the default is returned unchanged, preserving the current behaviour.
#[must_use]
pub fn effective_budget_mib(default_mib: u64) -> u64 {
    effective_budget_mib_with(default_mib, available_memory_mib())
}

/// [`effective_budget_mib`] for an explicit `available` value, so the clamp
/// arithmetic is unit-testable without a real `/proc/meminfo`.
#[must_use]
pub fn effective_budget_mib_with(default_mib: u64, available: Option<u64>) -> u64 {
    let Some(available) = available else {
        return default_mib;
    };
    let available_after_margin = available.saturating_sub(SAFETY_MARGIN_MIB);
    let capped = default_mib.min(available_after_margin.max(FLOOR_MIB));
    // Respect an explicit default below the floor (caller asked for very
    // little); otherwise the cap is at least the floor.
    capped.max(default_mib.min(FLOOR_MIB))
}

/// The current process's resident set size (RSS) in MiB, read from
/// `/proc/self/statm`, or `None` when unavailable. Used only for diagnostics:
/// the loader logs it before and after decoding to make OOM regressions
/// visible in the log file.
#[must_use]
pub fn current_rss_mib() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/statm").ok()?;
    // statm: size resident shared text lib data dt; resident is field 2 (index 1).
    let resident_pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = page_size_bytes();
    Some(
        resident_pages
            .saturating_mul(page_size)
            .div_ceil(1024 * 1024),
    )
}

/// The system page size in bytes (Linux only; falls back to 4096).
fn page_size_bytes() -> u64 {
    // `sysconf(_SC_PAGESIZE)` has no safe stable std binding; the value is
    // 4096 on every Linux target Sound Pro ships for.
    4096
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_meminfo(tmp: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = tmp.path().join("meminfo");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_mem_available_in_kib() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_meminfo(
            &tmp,
            "MemTotal:        8123456 kB\nMemFree:         1000000 kB\nMemAvailable:    1360000 kB\n",
        );
        // 1_360_000 kB / 1024 = 1328.125 MiB → ceil 1329.
        assert_eq!(available_memory_mib_from(&path), Some(1329));
    }

    #[test]
    fn missing_or_unreadable_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(available_memory_mib_from(&tmp.path().join("nope")), None);
        let path = write_meminfo(&tmp, "MemTotal: 1000 kB\n");
        assert_eq!(available_memory_mib_from(&path), None);
    }

    #[test]
    fn budget_is_capped_by_available_memory() {
        // 800 MiB available → effective = 800 - 256 = 544 MiB (above the floor).
        assert_eq!(effective_budget_mib_with(3072, Some(800)), 544);
        // Plenty available → the default is kept.
        assert_eq!(effective_budget_mib_with(3072, Some(16_384)), 3072);
        // No reading → the default is kept unchanged.
        assert_eq!(effective_budget_mib_with(3072, None), 3072);
    }

    #[test]
    fn budget_never_drops_below_floor() {
        // 600 MiB available → margin leaves 344 MiB, below the 512 floor.
        assert_eq!(effective_budget_mib_with(3072, Some(600)), FLOOR_MIB);
        // An explicit tiny default is respected, never raised to the floor.
        assert_eq!(effective_budget_mib_with(128, Some(600)), 128);
        assert_eq!(effective_budget_mib_with(128, Some(16_384)), 128);
    }
}
