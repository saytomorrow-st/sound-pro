//! A background queue downloader for catalog banks (SPEC-076).
//!
//! The «Установить» action (library manager) and the first-run «Загрузка
//! основных звуков» step both enqueue a set of catalog banks and drive them
//! through [`crate::bank_download::download_bank_into`] one at a time on a
//! worker thread. This module owns the shared state the GUI reads: the ordered
//! queue, the currently-downloading bank and its byte progress, the aggregate
//! byte progress over the whole queue, the transfer speed and ETA, and the
//! pause/cancel flags.
//!
//! The state is behind a mutex so the GUI can poll it cheaply on every frame
//! while the worker thread mutates it. All real I/O and networking stays on the
//! worker thread; nothing here is called from the audio callback.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bank_download::{self, Progress};
use crate::bank_library::CatalogEntry;

/// The overall lifecycle of a queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePhase {
    /// No worker is running (nothing queued, or already finished/stopped).
    Idle,
    /// A worker is downloading banks.
    Downloading,
    /// The worker is paused between banks (or mid-bank via the shared cancel
    /// flag being left set without the queue being torn down).
    Paused,
    /// The queue finished: every bank either succeeded or failed.
    Done,
    /// The queue was cancelled by the user.
    Cancelled,
}

/// A snapshot of the queue's current state, safe to read from the GUI.
#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    /// Lifecycle phase.
    pub phase: QueuePhase,
    /// Ids of banks still queued, in order.
    pub pending: Vec<String>,
    /// The bank currently downloading, if any.
    pub current: Option<String>,
    /// Bytes received for the current bank.
    pub current_bytes: u64,
    /// Total bytes of the current bank.
    pub current_total: u64,
    /// Aggregate bytes received across the whole queue (including the current
    /// bank and any already-completed banks).
    pub received: u64,
    /// Aggregate total bytes across the whole queue.
    pub total: u64,
    /// Successfully installed bank ids, in completion order.
    pub installed: Vec<String>,
    /// Failed banks: `(id, error message)`.
    pub failed: Vec<(String, String)>,
    /// Transfer speed in bytes/second (smoothed), `0` until enough time has
    /// elapsed.
    pub speed_bps: u64,
    /// Estimated seconds remaining, or `None` while speed is unknown/zero.
    pub eta_secs: Option<u64>,
}

impl QueueSnapshot {
    /// Aggregate progress fraction `0.0..=1.0` over the whole queue.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        crate::welcome::progress_fraction(self.received, self.total)
    }
}

/// The shared, thread-safe state behind a queue.
#[derive(Debug)]
struct Shared {
    phase: QueuePhase,
    pending: VecDeque<String>,
    current: Option<String>,
    current_bytes: u64,
    current_total: u64,
    received: u64,
    total: u64,
    installed: Vec<String>,
    failed: Vec<(String, String)>,
    speed_bps: u64,
    /// Timestamp (ms) of the last speed sample.
    last_sample_ms: u64,
    /// Bytes at the last speed sample.
    last_sample_bytes: u64,
    /// Smoothed speed (exponential moving average).
    smoothed_bps: f64,
    /// Pause flag; checked between banks and by the downloader's cancel flag.
    paused: Arc<AtomicBool>,
    /// Cancel flag; also handed to `download_bank_into` as its cancel signal.
    cancelled: Arc<AtomicBool>,
}

/// A handle to a queue downloader. Cloning shares the same underlying state so
/// the GUI and the worker both address one queue.
#[derive(Debug, Clone)]
pub struct DownloadQueue {
    shared: Arc<Mutex<Shared>>,
    /// The per-bank cancellation flag passed to `download_bank_into`.
    cancel: Arc<AtomicBool>,
}

impl DownloadQueue {
    /// Creates a new, idle queue over `banks` (the full set to download, in
    /// order). Nothing is downloaded until [`DownloadQueue::start`].
    #[must_use]
    pub fn new(banks: Vec<CatalogEntry>) -> Self {
        let total: u64 = banks
            .iter()
            .filter_map(|e| e.download.as_ref())
            .map(|d| d.size_bytes)
            .sum();
        let pending: VecDeque<String> = banks.iter().map(|e| e.id.clone()).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Shared {
            phase: QueuePhase::Idle,
            pending,
            current: None,
            current_bytes: 0,
            current_total: 0,
            received: 0,
            total,
            installed: Vec::new(),
            failed: Vec::new(),
            speed_bps: 0,
            last_sample_ms: 0,
            last_sample_bytes: 0,
            smoothed_bps: 0.0,
            paused: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::clone(&cancel),
        };
        Self {
            shared: Arc::new(Mutex::new(shared)),
            cancel,
        }
    }

    /// A snapshot of the current state (cheap; locks briefly).
    #[must_use]
    pub fn snapshot(&self) -> QueueSnapshot {
        let s = self.shared.lock().unwrap();
        QueueSnapshot {
            phase: s.phase,
            pending: s.pending.iter().cloned().collect(),
            current: s.current.clone(),
            current_bytes: s.current_bytes,
            current_total: s.current_total,
            received: s.received,
            total: s.total,
            installed: s.installed.clone(),
            failed: s.failed.clone(),
            speed_bps: s.speed_bps,
            eta_secs: eta_secs(s.smoothed_bps, s.total.saturating_sub(s.received)),
        }
    }

    /// Starts (or resumes) the download on a background thread. A no-op if the
    /// queue is already running or has nothing left to do.
    pub fn start(&self) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.phase == QueuePhase::Downloading {
                return;
            }
            if s.pending.is_empty() && s.current.is_none() {
                // Nothing left; if we finished, stay Done, else idle.
                return;
            }
            s.phase = QueuePhase::Downloading;
            s.paused.store(false, Ordering::Relaxed);
        }
        let shared = Arc::clone(&self.shared);
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || run_worker(shared, cancel));
    }

    /// Pauses the queue: the current bank is cancelled (its partial file is
    /// kept for resume) and the worker stops before the next bank.
    pub fn pause(&self) {
        let mut s = self.shared.lock().unwrap();
        s.paused.store(true, Ordering::Relaxed);
        s.phase = QueuePhase::Paused;
        // Signal the in-flight download to stop.
        s.cancelled.store(true, Ordering::Relaxed);
    }

    /// Resumes a paused queue (re-runs the current bank from its partial file).
    pub fn resume(&self) {
        let s = self.shared.lock().unwrap();
        s.cancelled.store(false, Ordering::Relaxed);
        drop(s);
        self.start();
    }

    /// Cancels the whole queue: stops the current bank and discards the
    /// remaining pending banks.
    pub fn cancel(&self) {
        let mut s = self.shared.lock().unwrap();
        s.cancelled.store(true, Ordering::Relaxed);
        s.paused.store(false, Ordering::Relaxed);
        s.phase = QueuePhase::Cancelled;
        s.current = None;
        s.pending.clear();
        s.current_bytes = 0;
        s.current_total = 0;
    }

    /// The per-bank cancel flag (used to hand cancellation to the downloader).
    #[must_use]
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Whether the queue has any work left (pending or in-flight).
    #[must_use]
    pub fn is_active(&self) -> bool {
        let s = self.shared.lock().unwrap();
        matches!(s.phase, QueuePhase::Downloading | QueuePhase::Paused)
    }
}

/// Runs the download loop on a worker thread until the queue is drained,
/// paused, or cancelled.
fn run_worker(shared: Arc<Mutex<Shared>>, cancel: Arc<AtomicBool>) {
    // Resolve bank id -> entry once from the catalog.
    let catalog = crate::bank_library::catalog().unwrap_or_default();
    loop {
        // Reset the per-bank cancel flag at the start of each bank (it is also
        // used as the pause signal, which is cleared on resume).
        cancel.store(false, Ordering::Relaxed);

        let Some(banks_dir) = crate::bank_library::banks_dir() else {
            let mut s = shared.lock().unwrap();
            s.failed
                .push(("<no data dir>".to_owned(), "no data directory".to_owned()));
            s.phase = QueuePhase::Done;
            return;
        };

        // Pop the next bank.
        let (id, entry) = {
            let mut s = shared.lock().unwrap();
            if s.paused.load(Ordering::Relaxed) {
                s.phase = QueuePhase::Paused;
                return;
            }
            match s.pending.pop_front() {
                Some(id) => {
                    let entry = catalog.iter().find(|e| e.id == id).cloned();
                    match entry {
                        Some(e) => {
                            s.current = Some(id.clone());
                            s.current_bytes = 0;
                            s.current_total = e.download.as_ref().map_or(0, |d| d.size_bytes);
                            (id, e)
                        }
                        None => {
                            // Unknown id (shouldn't happen) — record and continue.
                            s.failed.push((id.clone(), "unknown bank id".to_owned()));
                            continue;
                        }
                    }
                }
                None => {
                    s.phase = QueuePhase::Done;
                    s.current = None;
                    return;
                }
            }
        };

        let progress = Progress::new(entry.download.as_ref().map_or(0, |d| d.size_bytes));
        let start_ms = now_ms();

        // Drive the per-bank download, polling the progress into the shared
        // state so the GUI sees byte-level movement.
        let result = {
            // A separate thread would be cleaner, but the downloader already
            // exposes a `Progress` we can poll while it runs on this thread.
            // We run it directly and sample progress via a helper thread that
            // mirrors `progress.bytes()` into the shared state.
            let shared2 = Arc::clone(&shared);
            let progress2 = progress.clone();
            let sampler = std::thread::spawn(move || {
                while !progress2.bytes() >= progress2.total() {
                    let bytes = progress2.bytes();
                    let mut s = shared2.lock().unwrap();
                    s.current_bytes = bytes;
                    sample_speed(&mut s, bytes, now_ms());
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });
            let r = bank_download::download_bank_into(&entry, &banks_dir, &progress, &cancel);
            let _ = sampler.join();
            r
        };

        let elapsed_ms = now_ms() - start_ms;
        let received = progress.bytes();

        let mut s = shared.lock().unwrap();
        // Account for this bank's bytes in the aggregate (the `received` we
        // just read is for this bank only).
        s.received += received;
        s.current_bytes = received;
        s.current = None;
        s.current_total = 0;
        let aggregate = s.received;
        sample_speed(&mut s, aggregate, now_ms());

        match result {
            Ok(bank) => {
                s.installed.push(bank.id);
            }
            Err(bank_download::DownloadError::Cancelled) => {
                // Cancelled mid-bank: if paused, keep the partial and stop; if
                // cancelled, drop the rest.
                if s.paused.load(Ordering::Relaxed) {
                    s.phase = QueuePhase::Paused;
                    return;
                }
                s.phase = QueuePhase::Cancelled;
                s.pending.clear();
                return;
            }
            Err(e) => {
                s.failed.push((id, e.to_string()));
            }
        }
        let _ = elapsed_ms;
    }
}

/// Updates the speed estimate in `s` from a new byte total.
fn sample_speed(s: &mut Shared, bytes: u64, now_ms: u64) {
    if s.last_sample_ms == 0 {
        s.last_sample_ms = now_ms;
        s.last_sample_bytes = bytes;
        return;
    }
    let dt_ms = now_ms.saturating_sub(s.last_sample_ms);
    if dt_ms < 250 {
        // Too soon for a meaningful sample; keep the last estimate.
        return;
    }
    let db = bytes.saturating_sub(s.last_sample_bytes);
    let bps = if dt_ms == 0 {
        0.0
    } else {
        db as f64 * 1000.0 / dt_ms as f64
    };
    // Exponential moving average, 20% weight on the new sample.
    if s.smoothed_bps == 0.0 {
        s.smoothed_bps = bps;
    } else {
        s.smoothed_bps = s.smoothed_bps * 0.8 + bps * 0.2;
    }
    s.speed_bps = s.smoothed_bps.round() as u64;
    s.last_sample_ms = now_ms;
    s.last_sample_bytes = bytes;
}

/// ETA in seconds for `remaining` bytes at `bps`, or `None` when speed is 0.
fn eta_secs(bps: f64, remaining: u64) -> Option<u64> {
    if bps <= 0.0 || remaining == 0 {
        return None;
    }
    Some((remaining as f64 / bps).ceil() as u64)
}

/// Milliseconds since the Unix epoch (monotonic would be ideal, but wall time
/// is fine for speed/ETA display).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Formats an ETA duration (seconds) as `Xм Yс` or `Yс`, in Russian.
#[must_use]
pub fn format_eta(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}ч {}м", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}м {}с", secs / 60, secs % 60)
    } else {
        format!("{secs}с")
    }
}

/// Formats a speed (bytes/second) as a human string with a decimal comma.
#[must_use]
pub fn format_speed(bps: u64) -> String {
    if bps >= 1_000_000 {
        crate::welcome::format_megabytes(bps)
            .trim_end_matches(" МБ")
            .to_owned()
            + " МБ/с"
    } else if bps >= 1_000 {
        let kb = bps as f64 / 1000.0;
        let s = format!("{kb:.0}");
        format!("{s} КБ/с")
    } else {
        format!("{bps} Б/с")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_formatting_is_russian() {
        assert_eq!(format_eta(0), "0с");
        assert_eq!(format_eta(59), "59с");
        assert_eq!(format_eta(60), "1м 0с");
        assert_eq!(format_eta(65), "1м 5с");
        assert_eq!(format_eta(3600), "1ч 0м");
        assert_eq!(format_eta(3665), "1ч 1м");
    }

    #[test]
    fn speed_formatting() {
        assert_eq!(format_speed(0), "0 Б/с");
        assert_eq!(format_speed(999), "999 Б/с");
        assert_eq!(format_speed(1000), "1 КБ/с");
        assert_eq!(format_speed(1_000_000), "1,0 МБ/с");
    }

    #[test]
    fn eta_is_none_when_speed_unknown_or_done() {
        assert_eq!(eta_secs(0.0, 100), None);
        assert_eq!(eta_secs(100.0, 0), None);
        assert_eq!(eta_secs(10.0, 100), Some(10));
        assert_eq!(eta_secs(10.0, 101), Some(11));
    }

    #[test]
    fn new_queue_has_zero_progress_and_idle_phase() {
        let q = DownloadQueue::new(Vec::new());
        let s = q.snapshot();
        assert_eq!(s.phase, QueuePhase::Idle);
        assert_eq!(s.total, 0);
        assert_eq!(s.received, 0);
    }

    #[test]
    fn snapshot_fraction_is_bounded() {
        let q = DownloadQueue::new(Vec::new());
        let s = q.snapshot();
        assert_eq!(s.fraction(), 0.0);
    }

    #[test]
    fn sample_speed_estimates_bps() {
        let mut s = Shared {
            phase: QueuePhase::Idle,
            pending: VecDeque::new(),
            current: None,
            current_bytes: 0,
            current_total: 0,
            received: 0,
            total: 0,
            installed: Vec::new(),
            failed: Vec::new(),
            speed_bps: 0,
            last_sample_ms: 0,
            last_sample_bytes: 0,
            smoothed_bps: 0.0,
            paused: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        // First sample establishes the baseline.
        sample_speed(&mut s, 0, 1000);
        // 1000 bytes over 1000ms = 1000 B/s.
        sample_speed(&mut s, 1000, 2000);
        assert_eq!(s.speed_bps, 1000);
    }
}
