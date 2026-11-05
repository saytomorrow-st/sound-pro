//! Disk loader for streamed samples (RFC-002 §3–4, stage 6; SPEC-073).
//!
//! The audio thread asks for tail blocks through the engine's request ring
//! ([`audio_core::LoadRequest`]); this module answers them by reading the
//! sample's `.pcm` cache file and pushing the frames into the voice's SPSC
//! ring ([`audio_core::StreamFeeder`]). It never runs on the audio thread, so
//! it may allocate, block and do I/O.
//!
//! Two drivers share one [`StreamLoader`]:
//!
//! * [`StreamLoader::pump`] — synchronous, used by the offline pre-render
//!   ([`crate::instrument::SamplerInstrument`]) and tests: deterministic;
//! * [`StreamLoader::spawn`] — a background "disk thread" for realtime
//!   playback, stopped and joined when the [`LoaderThread`] is dropped.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use audio_core::{FeedError, LoadRequest, RequestReceiver, StreamFeeder, StreamLoaderEnds};

use crate::bank_cache::PCM_HEADER_LEN;

/// Idle sleep of the background thread when no request is pending.
const IDLE_SLEEP: Duration = Duration::from_millis(1);

/// Open file handles kept per loader (one per streamed sample in use).
const MAX_OPEN_FILES: usize = 256;

/// Loader-side counters (failed reads), complementing
/// [`audio_core::StreamStats`].
#[derive(Debug, Default)]
pub struct LoaderStats {
    /// Blocks served into a voice ring.
    pub blocks: AtomicU64,
    /// Requests skipped because the voice generation had moved on.
    pub skipped_stale: AtomicU64,
    /// Requests that failed (missing file, short read, unknown sample);
    /// the voice underruns and fades out.
    pub read_errors: AtomicU64,
}

impl LoaderStats {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Blocks served so far.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    /// Stale requests skipped so far.
    #[must_use]
    pub fn skipped_stale(&self) -> u64 {
        self.skipped_stale.load(Ordering::Relaxed)
    }

    /// Failed requests so far.
    #[must_use]
    pub fn read_errors(&self) -> u64 {
        self.read_errors.load(Ordering::Relaxed)
    }
}

/// Answers [`LoadRequest`]s by reading `.pcm` files into voice rings.
pub struct StreamLoader {
    receiver: RequestReceiver,
    feeders: Vec<StreamFeeder>,
    files: Arc<[Option<PathBuf>]>,
    open: HashMap<u32, File>,
    pending: VecDeque<LoadRequest>,
    bytes: Vec<u8>,
    pcm: Vec<i16>,
    stats: Arc<LoaderStats>,
}

impl std::fmt::Debug for StreamLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamLoader")
            .field("voices", &self.feeders.len())
            .field("files", &self.files.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl StreamLoader {
    /// Builds a loader from the engine's loader ends
    /// ([`audio_core::SamplerEngine::enable_streaming`]) and the per-sample
    /// `.pcm` paths ([`crate::sample_bank::LoadedBank::stream_files`],
    /// indexed by `LoadRequest::sample_index`).
    #[must_use]
    pub fn new(ends: StreamLoaderEnds, files: Arc<[Option<PathBuf>]>) -> Self {
        Self {
            receiver: ends.receiver,
            feeders: ends.feeders,
            files,
            open: HashMap::new(),
            pending: VecDeque::new(),
            bytes: Vec::new(),
            pcm: Vec::new(),
            stats: Arc::new(LoaderStats::default()),
        }
    }

    /// Loader counters (shared with [`LoaderThread::stats`]).
    #[must_use]
    pub fn stats(&self) -> Arc<LoaderStats> {
        Arc::clone(&self.stats)
    }

    /// Serves every pending request whose voice ring has room; requests that
    /// do not fit yet are kept and retried on the next call. Returns the
    /// number of requests resolved (served, skipped or failed).
    pub fn pump(&mut self) -> usize {
        while let Some(request) = self.receiver.pop() {
            self.pending.push_back(request);
        }
        let mut resolved = 0;
        for _ in 0..self.pending.len() {
            let Some(request) = self.pending.pop_front() else {
                break;
            };
            if self.serve(&request) {
                resolved += 1;
            } else {
                self.pending.push_back(request);
            }
        }
        resolved
    }

    /// `true` once the request is resolved; `false` to retry (ring full).
    fn serve(&mut self, request: &LoadRequest) -> bool {
        let voice = usize::from(request.voice);
        let Some(feeder) = self.feeders.get(voice) else {
            LoaderStats::bump(&self.stats.read_errors);
            return true;
        };
        if !feeder.is_current(request.generation) {
            LoaderStats::bump(&self.stats.skipped_stale);
            return true;
        }
        let channels = 2_usize;
        let samples = request.frames as usize * channels;
        if feeder.free_samples() < samples {
            return false;
        }
        if let Err(e) = self.read_block(request, channels) {
            crate::log_warn!("stream loader: sample {}: {e}", request.sample_index);
            LoaderStats::bump(&self.stats.read_errors);
            return true;
        }
        let feeder = &mut self.feeders[voice];
        if !feeder.is_current(request.generation) {
            LoaderStats::bump(&self.stats.skipped_stale);
            return true;
        }
        match feeder.push_block(request, &self.pcm) {
            Ok(()) => {
                LoaderStats::bump(&self.stats.blocks);
                true
            }
            Err(FeedError::Full) => false,
            Err(FeedError::BadLength) => {
                LoaderStats::bump(&self.stats.read_errors);
                true
            }
        }
    }

    /// Reads the request's frames into `self.pcm`.
    fn read_block(&mut self, request: &LoadRequest, channels: usize) -> Result<(), String> {
        let index = request.sample_index;
        let path = self
            .files
            .get(index as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| "no stream file".to_string())?;
        if !self.open.contains_key(&index) {
            if self.open.len() >= MAX_OPEN_FILES {
                self.open.clear();
            }
            let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
            self.open.insert(index, file);
        }
        let file = &self.open[&index];
        let frame_bytes = (channels * 2) as u64;
        let offset = PCM_HEADER_LEN as u64 + request.from_frame.saturating_mul(frame_bytes);
        let len = request.frames as usize * channels * 2;
        self.bytes.resize(len, 0);
        read_exact_at(file, &mut self.bytes, offset)
            .map_err(|e| format!("read {} @{offset}: {e}", path.display()))?;
        self.pcm.clear();
        self.pcm.extend(
            self.bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]])),
        );
        Ok(())
    }

    /// Moves the loader onto a background disk thread.
    ///
    /// # Errors
    ///
    /// The OS error if the thread cannot be spawned.
    pub fn spawn(mut self) -> std::io::Result<LoaderThread> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = self.stats();
        let flag = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("sp-stream-loader".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    if self.pump() == 0 {
                        std::thread::sleep(IDLE_SLEEP);
                    }
                }
            })?;
        Ok(LoaderThread {
            stop,
            stats,
            join: Some(join),
        })
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A running background loader; stops and joins its thread on drop.
#[derive(Debug)]
pub struct LoaderThread {
    stop: Arc<AtomicBool>,
    stats: Arc<LoaderStats>,
    join: Option<JoinHandle<()>>,
}

impl LoaderThread {
    /// Loader counters.
    #[must_use]
    pub fn stats(&self) -> &LoaderStats {
        &self.stats
    }
}

impl Drop for LoaderThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
