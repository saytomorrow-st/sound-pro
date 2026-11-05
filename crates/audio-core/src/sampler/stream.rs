//! Disk streaming of long samples (RFC-002 stage 5): [`VoiceStream`].
//!
//! A streamed sample keeps only its first [`HEAD_FRAMES`] frames resident in a
//! [`SampleHead`]; the rest (the *tail*) arrives from a loader thread through a
//! per-voice pair of SPSC rings (`rtrb`): one carries interleaved `i16`
//! samples, the other a [`BlockHeader`] per block. The audio thread asks for
//! more data through a shared request ring ([`StreamRequests`]) carrying
//! [`LoadRequest`]s (RFC-002 §4).
//!
//! Protocol (RFC-002 §4):
//!
//! - [`VoiceStream::start`] bumps the voice *generation* and requests the first
//!   tail block (frames `head_frames..`) if the sample is longer than its head.
//! - The loader answers with [`StreamFeeder::push_block`], tagging each block
//!   with the generation it was asked for. Samples are pushed before the
//!   header, so a visible header implies its samples are visible too.
//! - The voice refills when at most half of the ring is outstanding: it always
//!   keeps `requested - consumed <= ring_frames`, so the loader never needs
//!   more ring space than the voice reserved.
//! - [`VoiceStream::cancel`] (note-off end, steal, sample end) bumps the
//!   generation; blocks of an older generation are popped and discarded.
//! - If the tail is not there in time (underrun) the voice fades its last
//!   frame out over [`STEAL_FADE_SAMPLES`] samples, increments
//!   [`StreamStats::underruns`] and finishes.
//!
//! Head/tail seam: while every interpolation neighbour of the read position
//! lies inside the head, the voice calls the resident readers
//! ([`read_hermite_i16`] and friends) on the head with the full sample length,
//! so the result is bit-identical to a resident sample. Past that point the
//! four Hermite neighbours are gathered from the head and a 4-frame window of
//! ring frames and fed to the same kernel with the same operation order, so
//! the seam produces exactly the samples a resident voice would (no
//! discontinuity in the interpolation).
//!
//! Realtime contract: [`VoiceStream::start`], [`VoiceStream::cancel`] and
//! [`VoiceStream::next_frame`] never allocate, lock, do I/O or panic; all rings
//! are allocated in the constructors. A `VoiceStream` holds no `Arc`; the
//! caller passes `&SampleHead`, so the audio thread never drops sample memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use super::voice::{
    Interpolation, STEAL_FADE_SAMPLES, read_hermite_i16, read_hermite_stereo, read_linear_i16,
    read_linear_stereo,
};

/// Default number of resident head frames per streamed sample (RFC-002 §3).
pub const HEAD_FRAMES: usize = 65_536;

/// Default per-voice ring size in frames (RFC-002 §3).
pub const RING_FRAMES: usize = 32_768;

/// Capacity of each voice's [`BlockHeader`] ring.
pub const HEADER_SLOTS: usize = 16;

/// Resident head of a streamed sample: the first frames plus the metadata the
/// voice needs to read past them.
#[derive(Debug, Clone)]
pub struct SampleHead {
    data: Arc<[i16]>,
    total_frames: u64,
    channels: u8,
    file_id: u32,
}

impl SampleHead {
    /// Wraps already-decoded head data (interleaved `i16`).
    ///
    /// # Panics
    ///
    /// Panics (constructor only, never on the audio path) if `channels` is not
    /// 1 or 2, if `data.len()` is not a multiple of `channels`, or if the head
    /// is longer than `total_frames`.
    #[must_use]
    pub fn new(data: Arc<[i16]>, total_frames: u64, channels: u8, file_id: u32) -> Self {
        assert!(channels == 1 || channels == 2, "channels must be 1 or 2");
        let ch = usize::from(channels);
        assert!(data.len() % ch == 0, "head length must be whole frames");
        assert!(
            (data.len() / ch) as u64 <= total_frames,
            "head longer than sample"
        );
        Self {
            data,
            total_frames,
            channels,
            file_id,
        }
    }

    /// Builds a head from a whole decoded sample, keeping its first
    /// `head_frames` frames (allocates; loader/constructor side only).
    ///
    /// # Panics
    ///
    /// Same conditions as [`SampleHead::new`].
    #[must_use]
    pub fn from_pcm(pcm: &[i16], channels: u8, file_id: u32, head_frames: usize) -> Self {
        let ch = usize::from(channels.max(1));
        let total = pcm.len() / ch;
        let keep = total.min(head_frames) * ch;
        Self::new(Arc::from(&pcm[..keep]), total as u64, channels, file_id)
    }

    /// Interleaved resident head samples.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        &self.data
    }

    /// Number of resident frames.
    #[must_use]
    pub fn head_frames(&self) -> usize {
        self.data.len() / usize::from(self.channels)
    }

    /// Total frames of the whole sample (head + tail).
    #[must_use]
    pub const fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Channel count (1 or 2).
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Loader-side file identifier.
    #[must_use]
    pub const fn file_id(&self) -> u32 {
        self.file_id
    }

    /// `true` if the sample has frames beyond the head.
    #[must_use]
    pub fn has_tail(&self) -> bool {
        (self.head_frames() as u64) < self.total_frames
    }
}

/// A request from the audio thread to the loader: "fill voice `voice`'s ring
/// with `frames` frames of sample `sample_index` starting at `from_frame`,
/// tagged with `generation`" (RFC-002 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadRequest {
    /// Voice slot index.
    pub voice: u16,
    /// Voice generation the data is for; stale generations are discarded.
    pub generation: u32,
    /// Sample (head) index chosen by the caller of [`VoiceStream::start`].
    pub sample_index: u32,
    /// First frame of the block (absolute frame index in the sample).
    pub from_frame: u64,
    /// Number of frames requested.
    pub frames: u32,
}

/// Header describing one block pushed into a voice's sample ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    /// Generation the block was loaded for.
    pub generation: u32,
    /// Absolute frame index of the block's first frame.
    pub from_frame: u64,
    /// Number of frames in the block.
    pub frames: u32,
}

/// Lock-free streaming counters shared between the audio thread and the UI.
#[derive(Debug, Default)]
pub struct StreamStats {
    /// Voices that ran out of tail data and were faded out.
    pub underruns: AtomicU32,
    /// Requests that did not fit into the request ring.
    pub dropped_requests: AtomicU32,
    /// Blocks discarded because their generation was stale.
    pub stale_blocks: AtomicU32,
}

impl StreamStats {
    fn bump(counter: &AtomicU32) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Current underrun count.
    #[must_use]
    pub fn underruns(&self) -> u32 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Current dropped-request count.
    #[must_use]
    pub fn dropped_requests(&self) -> u32 {
        self.dropped_requests.load(Ordering::Relaxed)
    }

    /// Current stale-block count.
    #[must_use]
    pub fn stale_blocks(&self) -> u32 {
        self.stale_blocks.load(Ordering::Relaxed)
    }
}

/// Audio-thread side of the shared request ring (one per engine; all voices
/// on the audio thread push through it).
#[derive(Debug)]
pub struct StreamRequests {
    producer: Producer<LoadRequest>,
    stats: Arc<StreamStats>,
}

impl StreamRequests {
    /// Pushes a request; on a full ring counts a dropped request and returns
    /// `false`. Never blocks or allocates.
    pub fn push(&mut self, request: LoadRequest) -> bool {
        if self.producer.push(request).is_ok() {
            true
        } else {
            StreamStats::bump(&self.stats.dropped_requests);
            false
        }
    }

    /// Shared counters.
    #[must_use]
    pub fn stats(&self) -> &StreamStats {
        &self.stats
    }
}

/// Loader-thread side of the shared request ring.
#[derive(Debug)]
pub struct RequestReceiver {
    consumer: Consumer<LoadRequest>,
}

impl RequestReceiver {
    /// Pops the next pending request, if any.
    pub fn pop(&mut self) -> Option<LoadRequest> {
        self.consumer.pop().ok()
    }

    /// Number of pending requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.consumer.slots()
    }

    /// `true` if no request is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }
}

/// Error returned by [`StreamFeeder::push_block`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedError {
    /// The block length is not `frames * channels` samples.
    BadLength,
    /// Not enough free ring space; retry later.
    Full,
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadLength => f.write_str("block length is not frames * channels"),
            Self::Full => f.write_str("stream ring is full"),
        }
    }
}

impl std::error::Error for FeedError {}

/// Loader-side producer of one voice's rings. Stage 6 drives it from the
/// loader thread; tests use it as a deterministic loader.
#[derive(Debug)]
pub struct StreamFeeder {
    samples: Producer<i16>,
    headers: Producer<BlockHeader>,
    generation: Arc<AtomicU32>,
    channels: usize,
}

impl StreamFeeder {
    /// `true` if `generation` is still the voice's current generation (lets
    /// the loader skip reading data that would be discarded anyway).
    #[must_use]
    pub fn is_current(&self, generation: u32) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    /// Pushes one block answering `request`. Samples go in before the header,
    /// so the voice never sees a header without its data.
    ///
    /// # Errors
    ///
    /// [`FeedError::BadLength`] if `pcm.len() != request.frames * channels`,
    /// [`FeedError::Full`] if either ring lacks space (nothing is pushed).
    pub fn push_block(&mut self, request: &LoadRequest, pcm: &[i16]) -> Result<(), FeedError> {
        let frames = request.frames as usize;
        if pcm.len() != frames * self.channels {
            return Err(FeedError::BadLength);
        }
        if self.samples.slots() < pcm.len() || self.headers.slots() == 0 {
            return Err(FeedError::Full);
        }
        for &s in pcm {
            // Space was checked above and this is the only producer.
            let _ = self.samples.push(s);
        }
        let _ = self.headers.push(BlockHeader {
            generation: request.generation,
            from_frame: request.from_frame,
            frames: request.frames,
        });
        Ok(())
    }

    /// Free sample slots in the voice ring.
    #[must_use]
    pub fn free_samples(&self) -> usize {
        self.samples.slots()
    }
}

/// Playback state of a [`VoiceStream`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Idle,
    Playing,
    /// Underrun: fading `last` out; `done` samples of the fade elapsed.
    Fading {
        last: (f32, f32),
        done: u32,
    },
}

/// Number of ring frames kept for interpolation (Hermite needs `i-1..=i+2`).
const WINDOW: usize = 4;

/// Audio-thread side of one streamed voice (RFC-002 §3–§4).
///
/// Owns the consumer ends of the voice's sample and header rings plus a
/// four-frame window of tail frames used for interpolation across the
/// head/tail seam. All methods are allocation-, lock- and panic-free.
#[derive(Debug)]
pub struct VoiceStream {
    index: u16,
    channels: u8,
    ring_frames: usize,
    samples: Consumer<i16>,
    headers: Consumer<BlockHeader>,
    generation: Arc<AtomicU32>,
    stats: Arc<StreamStats>,
    state: State,
    sample_index: u32,
    total_frames: u64,
    head_frames: u64,
    pos: f64,
    /// Frames still to pop from the current (current-generation) block.
    block_left: u32,
    /// Absolute frame index one past the last frame popped from the ring.
    consumed_end: u64,
    /// Absolute frame index one past the last frame requested.
    requested_end: u64,
    /// Absolute frame index one past the last frame announced by a header.
    received_end: u64,
    /// Last popped tail frames; `window[WINDOW - 1]` is frame `consumed_end - 1`.
    window: [[i16; 2]; WINDOW],
}

impl VoiceStream {
    /// Voice slot index (as used in [`LoadRequest::voice`]).
    #[must_use]
    pub const fn index(&self) -> u16 {
        self.index
    }

    /// Current generation.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// `true` while the voice produces sound (playing or fading).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state != State::Idle
    }

    /// `true` while the voice fades out after an underrun.
    #[must_use]
    pub fn is_fading(&self) -> bool {
        matches!(self.state, State::Fading { .. })
    }

    /// Current read position in frames.
    #[must_use]
    pub const fn position(&self) -> f64 {
        self.pos
    }

    /// Starts playing `head` from frame 0, bumping the generation and
    /// requesting the first tail block if the sample has one. Returns `false`
    /// (and stays idle) if `head` has a different channel count than the
    /// voice rings.
    pub fn start(
        &mut self,
        head: &SampleHead,
        sample_index: u32,
        requests: &mut StreamRequests,
    ) -> bool {
        self.cancel();
        if head.channels() != self.channels || head.total_frames() == 0 {
            return false;
        }
        self.drain_stale();
        self.state = State::Playing;
        self.sample_index = sample_index;
        self.total_frames = head.total_frames();
        self.head_frames = head.head_frames() as u64;
        self.pos = 0.0;
        self.block_left = 0;
        self.consumed_end = self.head_frames;
        self.requested_end = self.head_frames;
        self.received_end = self.head_frames;
        self.window = [[0; 2]; WINDOW];
        self.maybe_request(requests);
        true
    }

    /// Loop wrap: jumps back to `pos`, which must lie inside the head
    /// (`pos < head_frames`). Bumps the generation (blocks in flight for the
    /// previous pass become stale) and re-requests the tail from the seam, so
    /// the next pass is streamed again. Returns `false` (and does nothing)
    /// unless the voice is playing and `pos` is inside the head.
    #[allow(clippy::cast_precision_loss)]
    pub fn rewind(&mut self, pos: f64, requests: &mut StreamRequests) -> bool {
        if self.state != State::Playing || pos.is_nan() || pos < 0.0 {
            return false;
        }
        if pos >= self.head_frames as f64 {
            return false;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        // The rest of the block being read is already behind its popped header.
        self.skip_samples(self.block_left as usize * usize::from(self.channels));
        self.drain_stale();
        self.pos = pos;
        self.block_left = 0;
        self.consumed_end = self.head_frames;
        self.requested_end = self.head_frames;
        self.received_end = self.head_frames;
        self.window = [[0; 2]; WINDOW];
        self.maybe_request(requests);
        true
    }

    /// Stops the voice immediately and invalidates any data in flight
    /// (note-off end, steal, sample end).
    pub fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.state = State::Idle;
        self.block_left = 0;
    }

    /// Pops and discards every block whose header is already visible. Called
    /// right after a generation bump, so all of them are stale.
    fn drain_stale(&mut self) {
        let ch = usize::from(self.channels);
        while let Ok(header) = self.headers.pop() {
            self.skip_samples(header.frames as usize * ch);
            StreamStats::bump(&self.stats.stale_blocks);
        }
    }

    fn skip_samples(&mut self, n: usize) {
        for _ in 0..n {
            if self.samples.pop().is_err() {
                break;
            }
        }
    }

    /// Requests the next tail block when at most half of the ring is
    /// outstanding and no request is in flight.
    fn maybe_request(&mut self, requests: &mut StreamRequests) {
        if self.state != State::Playing || self.requested_end >= self.total_frames {
            return;
        }
        if self.received_end < self.requested_end {
            return;
        }
        let outstanding = self.requested_end.saturating_sub(self.consumed_end);
        let ring = self.ring_frames as u64;
        if outstanding > ring / 2 {
            return;
        }
        let frames = (ring - outstanding)
            .min(self.total_frames - self.requested_end)
            .min(u64::from(u32::MAX));
        let frames = u32::try_from(frames).unwrap_or(u32::MAX);
        let request = LoadRequest {
            voice: self.index,
            generation: self.generation.load(Ordering::Acquire),
            sample_index: self.sample_index,
            from_frame: self.requested_end,
            frames,
        };
        if requests.push(request) {
            self.requested_end += u64::from(frames);
        }
    }

    /// Renders one frame at the current position and advances it by `step`
    /// frames. Returns `(left, right)` (mono voices return the same value
    /// twice) or `None` once the voice is idle (sample end, cancel, or the
    /// underrun fade finished). `head` must be the head passed to
    /// [`VoiceStream::start`].
    pub fn next_frame(
        &mut self,
        head: &SampleHead,
        step: f64,
        interpolation: Interpolation,
        requests: &mut StreamRequests,
    ) -> Option<(f32, f32)> {
        match self.state {
            State::Idle => None,
            State::Fading { last, done } => Some(self.fade(last, done)),
            State::Playing => self.play(head, step, interpolation, requests),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn fade(&mut self, last: (f32, f32), done: u32) -> (f32, f32) {
        let gain = 1.0 - done as f32 / STEAL_FADE_SAMPLES as f32;
        let done = done + 1;
        if done >= STEAL_FADE_SAMPLES {
            self.cancel();
        } else {
            self.state = State::Fading { last, done };
        }
        (last.0 * gain, last.1 * gain)
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn play(
        &mut self,
        head: &SampleHead,
        step: f64,
        interpolation: Interpolation,
        requests: &mut StreamRequests,
    ) -> Option<(f32, f32)> {
        if head.channels() != self.channels
            || self.pos.is_nan()
            || self.pos >= self.total_frames as f64
        {
            self.cancel();
            return None;
        }
        let last = (self.total_frames - 1) as f64;
        let clamped = self.pos.clamp(0.0, last);
        let i = clamped as u64;
        let reach = match interpolation {
            Interpolation::Linear => 1,
            Interpolation::Hermite4 => 2,
        };
        let need = (i + reach).min(self.total_frames - 1);
        let out = if need < self.head_frames {
            Self::read_head(head, self.pos, self.total_frames, interpolation)
        } else {
            if !self.fill_to(need + 1) {
                return Some(self.underrun(head));
            }
            self.read_seam(head, clamped, i, interpolation)
        };
        self.pos += step;
        self.maybe_request(requests);
        Some(out)
    }

    /// Resident path: every neighbour lies inside the head.
    #[allow(clippy::cast_possible_truncation)]
    fn read_head(
        head: &SampleHead,
        pos: f64,
        total: u64,
        interpolation: Interpolation,
    ) -> (f32, f32) {
        let len = total as usize;
        let data = head.data();
        match (head.channels(), interpolation) {
            (1, Interpolation::Linear) => {
                let v = read_linear_i16(data, pos, len);
                (v, v)
            }
            (1, Interpolation::Hermite4) => {
                let v = read_hermite_i16(data, pos, len);
                (v, v)
            }
            (_, Interpolation::Linear) => read_linear_stereo(data, pos, len),
            (_, Interpolation::Hermite4) => read_hermite_stereo(data, pos, len),
        }
    }

    /// Seam/tail path: neighbours come from the head and the ring window,
    /// combined with the resident readers' exact arithmetic.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn read_seam(
        &self,
        head: &SampleHead,
        clamped: f64,
        i: u64,
        interpolation: Interpolation,
    ) -> (f32, f32) {
        let last = self.total_frames - 1;
        let t = (clamped - i as f64) as f32;
        let x1 = self.frame(head, i);
        let x2 = self.frame(head, (i + 1).min(last));
        if self.total_frames == 1 {
            return x1;
        }
        match interpolation {
            Interpolation::Linear => (x1.0 + (x2.0 - x1.0) * t, x1.1 + (x2.1 - x1.1) * t),
            Interpolation::Hermite4 => {
                let x0 = self.frame(head, i.saturating_sub(1));
                let x3 = self.frame(head, (i + 2).min(last));
                (
                    hermite(x0.0, x1.0, x2.0, x3.0, t),
                    hermite(x0.1, x1.1, x2.1, x3.1, t),
                )
            }
        }
    }

    /// Frame `k` as `(left, right)` (mono duplicated) from the head or the
    /// ring window. Out-of-window indices read as silence (never panics).
    #[allow(clippy::cast_possible_truncation)]
    fn frame(&self, head: &SampleHead, k: u64) -> (f32, f32) {
        let ch = usize::from(self.channels);
        let raw = if k < self.head_frames {
            let base = k as usize * ch;
            let data = head.data();
            let l = data.get(base).copied().unwrap_or(0);
            let r = if ch == 2 {
                data.get(base + 1).copied().unwrap_or(0)
            } else {
                l
            };
            [l, r]
        } else {
            let back = self.consumed_end.saturating_sub(1).saturating_sub(k);
            if k >= self.consumed_end || back >= WINDOW as u64 {
                [0, 0]
            } else {
                self.window[WINDOW - 1 - back as usize]
            }
        };
        (f32::from(raw[0]), f32::from(raw[1]))
    }

    /// Pops ring frames until `consumed_end >= end`. Returns `false` on
    /// underrun (data not there yet).
    fn fill_to(&mut self, end: u64) -> bool {
        let ch = usize::from(self.channels);
        while self.consumed_end < end {
            if self.block_left == 0 && !self.next_block() {
                return false;
            }
            if self.samples.slots() < ch {
                return false;
            }
            let l = self.samples.pop().unwrap_or(0);
            let r = if ch == 2 {
                self.samples.pop().unwrap_or(0)
            } else {
                l
            };
            self.window.rotate_left(1);
            self.window[WINDOW - 1] = [l, r];
            self.consumed_end += 1;
            self.block_left -= 1;
        }
        true
    }

    /// Advances to the next current-generation block that continues the
    /// tail, discarding stale ones. Returns `false` if none is available.
    fn next_block(&mut self) -> bool {
        let ch = usize::from(self.channels);
        let current = self.generation.load(Ordering::Acquire);
        while let Ok(header) = self.headers.pop() {
            if header.generation == current && header.from_frame == self.consumed_end {
                self.block_left = header.frames;
                self.received_end = header.from_frame + u64::from(header.frames);
                if header.frames > 0 {
                    return true;
                }
            } else {
                self.skip_samples(header.frames as usize * ch);
                StreamStats::bump(&self.stats.stale_blocks);
            }
        }
        false
    }

    /// Starts the underrun fade from the last frame that is available.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn underrun(&mut self, head: &SampleHead) -> (f32, f32) {
        StreamStats::bump(&self.stats.underruns);
        let avail = self.consumed_end.saturating_sub(1);
        let at = (self.pos.max(0.0) as u64).min(avail);
        let last = self.frame(head, at);
        self.state = State::Fading { last, done: 0 };
        self.fade(last, 0)
    }
}

/// Catmull-Rom kernel with the exact operation order of
/// [`read_hermite_i16`], so seam output is bit-identical.
#[inline]
fn hermite(x0: f32, x1: f32, x2: f32, x3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x2 - x0);
    let c2 = x0 - 2.5 * x1 + 2.0 * x2 - 0.5 * x3;
    let c3 = 0.5 * (x3 - x0) + 1.5 * (x1 - x2);
    ((c3 * t + c2) * t + c1) * t + x1
}

/// Preallocated set of streamed voices plus the loader-side ends
/// (RFC-002 §3). Built off the audio thread; the public fields are moved
/// to their owning threads by destructuring.
#[derive(Debug)]
pub struct StreamPool {
    /// Audio-thread voices, indexed by slot.
    pub voices: Vec<VoiceStream>,
    /// Audio-thread request producer.
    pub requests: StreamRequests,
    /// Loader-side feeders, one per voice (same index as `voices`).
    pub feeders: Vec<StreamFeeder>,
    /// Loader-side request consumer.
    pub receiver: RequestReceiver,
    /// Shared counters.
    pub stats: Arc<StreamStats>,
}

impl StreamPool {
    /// Allocates `max_voices` voices with [`RING_FRAMES`]-frame rings.
    ///
    /// # Panics
    ///
    /// See [`StreamPool::with_ring_frames`].
    #[must_use]
    pub fn new(max_voices: usize, channels: u8) -> Self {
        Self::with_ring_frames(max_voices, channels, RING_FRAMES)
    }

    /// Allocates `max_voices` voices whose sample rings hold `ring_frames`
    /// frames; the request ring holds `2 * max_voices` requests.
    ///
    /// # Panics
    ///
    /// Panics (constructor only) if `channels` is not 1 or 2, `ring_frames`
    /// is zero, or `max_voices` exceeds `u16::MAX + 1`.
    #[must_use]
    pub fn with_ring_frames(max_voices: usize, channels: u8, ring_frames: usize) -> Self {
        assert!(channels == 1 || channels == 2, "channels must be 1 or 2");
        assert!(ring_frames > 0, "ring_frames must be > 0");
        assert!(max_voices <= usize::from(u16::MAX) + 1, "too many voices");
        let stats = Arc::new(StreamStats::default());
        let (req_tx, req_rx) = RingBuffer::new((2 * max_voices).max(1));
        let ch = usize::from(channels);
        let mut voices = Vec::with_capacity(max_voices);
        let mut feeders = Vec::with_capacity(max_voices);
        for index in 0..max_voices {
            let (s_tx, s_rx) = RingBuffer::new(ring_frames * ch);
            let (h_tx, h_rx) = RingBuffer::new(HEADER_SLOTS);
            let generation = Arc::new(AtomicU32::new(0));
            voices.push(VoiceStream {
                index: u16::try_from(index).unwrap_or(u16::MAX),
                channels,
                ring_frames,
                samples: s_rx,
                headers: h_rx,
                generation: Arc::clone(&generation),
                stats: Arc::clone(&stats),
                state: State::Idle,
                sample_index: 0,
                total_frames: 0,
                head_frames: 0,
                pos: 0.0,
                block_left: 0,
                consumed_end: 0,
                requested_end: 0,
                received_end: 0,
                window: [[0; 2]; WINDOW],
            });
            feeders.push(StreamFeeder {
                samples: s_tx,
                headers: h_tx,
                generation,
                channels: ch,
            });
        }
        Self {
            voices,
            requests: StreamRequests {
                producer: req_tx,
                stats: Arc::clone(&stats),
            },
            feeders,
            receiver: RequestReceiver { consumer: req_rx },
            stats,
        }
    }
}

/// Memory budget of streaming in bytes (RFC-002 §7): all resident heads plus
/// `voices` rings of `ring_frames` frames of `channels` `i16` samples.
#[must_use]
pub fn stream_budget_bytes(
    heads: &[SampleHead],
    voices: usize,
    channels: u8,
    ring_frames: usize,
) -> usize {
    let sample = std::mem::size_of::<i16>();
    let heads_bytes: usize = heads.iter().map(|h| std::mem::size_of_val(h.data())).sum();
    heads_bytes + voices * ring_frames * usize::from(channels) * sample
}

#[cfg(test)]
// Test fixtures cast small, bounded indices and synthetic PCM values.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)]
mod tests {
    use super::*;

    fn pcm(frames: usize, channels: usize) -> Vec<i16> {
        (0..frames * channels)
            .map(|k| {
                let x = k as f64 * 0.37;
                (x.sin() * 12_000.0 + (x * 3.1).cos() * 5_000.0) as i16
            })
            .collect()
    }

    /// Deterministic loader: answers every pending request from `pcm`.
    fn serve(pool: &mut StreamPool, pcm: &[i16], ch: usize) {
        while let Some(req) = pool.receiver.pop() {
            let feeder = &mut pool.feeders[usize::from(req.voice)];
            if !feeder.is_current(req.generation) {
                continue;
            }
            let from = req.from_frame as usize * ch;
            let to = from + req.frames as usize * ch;
            feeder.push_block(&req, &pcm[from..to]).expect("ring space");
        }
    }

    fn resident(
        data: &[i16],
        ch: u8,
        pos: f64,
        frames: usize,
        interp: Interpolation,
    ) -> (f32, f32) {
        match (ch, interp) {
            (1, Interpolation::Linear) => {
                let v = read_linear_i16(data, pos, frames);
                (v, v)
            }
            (1, Interpolation::Hermite4) => {
                let v = read_hermite_i16(data, pos, frames);
                (v, v)
            }
            (_, Interpolation::Linear) => read_linear_stereo(data, pos, frames),
            (_, Interpolation::Hermite4) => read_hermite_stereo(data, pos, frames),
        }
    }

    fn check_seam(ch: u8, interp: Interpolation, step: f64) {
        let chu = usize::from(ch);
        let total = 300;
        let data = pcm(total, chu);
        let head = SampleHead::from_pcm(&data, ch, 0, 17);
        let mut pool = StreamPool::with_ring_frames(1, ch, 32);
        let mut requests = std::mem::replace(
            &mut pool.requests,
            StreamPool::with_ring_frames(0, 1, 1).requests,
        );
        assert!(pool.voices[0].start(&head, 0, &mut requests));
        let mut n = 0;
        loop {
            serve(&mut pool, &data, chu);
            let pos = pool.voices[0].position();
            let Some(got) = pool.voices[0].next_frame(&head, step, interp, &mut requests) else {
                break;
            };
            let want = resident(&data, ch, pos, total, interp);
            assert_eq!(
                got.0.to_bits(),
                want.0.to_bits(),
                "L ch={ch} {interp:?} pos={pos}"
            );
            assert_eq!(
                got.1.to_bits(),
                want.1.to_bits(),
                "R ch={ch} {interp:?} pos={pos}"
            );
            n += 1;
        }
        assert!(n as f64 >= (total as f64 / step).floor());
        assert_eq!(pool.stats.underruns(), 0);
        assert!(!pool.voices[0].is_active());
    }

    #[test]
    fn seam_is_bit_identical_to_resident() {
        for ch in [1, 2] {
            for interp in [Interpolation::Linear, Interpolation::Hermite4] {
                for step in [1.0, 0.73, 1.61, 3.9] {
                    check_seam(ch, interp, step);
                }
            }
        }
    }

    #[test]
    fn short_sample_sends_no_request() {
        let data = pcm(10, 1);
        let head = SampleHead::from_pcm(&data, 1, 0, 64);
        assert!(!head.has_tail());
        let mut pool = StreamPool::with_ring_frames(1, 1, 32);
        let StreamPool {
            voices,
            requests,
            receiver,
            ..
        } = &mut pool;
        assert!(voices[0].start(&head, 0, requests));
        assert!(receiver.is_empty());
        let mut n = 0;
        while voices[0]
            .next_frame(&head, 1.0, Interpolation::Hermite4, requests)
            .is_some()
        {
            n += 1;
        }
        assert_eq!(n, 10);
        assert!(receiver.is_empty());
    }

    #[test]
    fn start_requests_first_tail_block() {
        let data = pcm(1000, 2);
        let head = SampleHead::from_pcm(&data, 2, 3, 8);
        let mut pool = StreamPool::with_ring_frames(2, 2, 32);
        let StreamPool {
            voices,
            requests,
            receiver,
            ..
        } = &mut pool;
        assert!(voices[1].start(&head, 5, requests));
        let req = receiver.pop().expect("request");
        assert_eq!(
            req,
            LoadRequest {
                voice: 1,
                generation: voices[1].generation(),
                sample_index: 5,
                from_frame: 8,
                frames: 32,
            }
        );
        assert!(receiver.is_empty());
    }

    #[test]
    fn refill_fires_at_half_ring() {
        let data = pcm(1000, 1);
        let head = SampleHead::from_pcm(&data, 1, 0, 4);
        let mut pool = StreamPool::with_ring_frames(1, 1, 32);
        let mut requests = std::mem::replace(
            &mut pool.requests,
            StreamPool::with_ring_frames(0, 1, 1).requests,
        );
        assert!(pool.voices[0].start(&head, 0, &mut requests));
        serve(&mut pool, &data, 1);
        let mut frames = 0;
        let second = loop {
            pool.voices[0]
                .next_frame(&head, 1.0, Interpolation::Linear, &mut requests)
                .expect("playing");
            frames += 1;
            if let Some(req) = pool.receiver.pop() {
                break req;
            }
            assert!(frames < 100, "no refill request");
        };
        // Outstanding (36 - consumed) drops to ring/2 = 16 when frame 19 is
        // popped; linear reads i+1, so that is position 18, the 19th frame.
        assert_eq!(frames, 19);
        assert_eq!(second.from_frame, 36);
        assert_eq!(second.frames, 16);
    }

    #[test]
    fn stale_generation_blocks_are_discarded() {
        let total = 20;
        let data = pcm(total, 1);
        let head = SampleHead::from_pcm(&data, 1, 0, 4);
        let mut pool = StreamPool::with_ring_frames(1, 1, 64);
        let mut requests = std::mem::replace(
            &mut pool.requests,
            StreamPool::with_ring_frames(0, 1, 1).requests,
        );
        assert!(pool.voices[0].start(&head, 0, &mut requests));
        let old = pool.receiver.pop().expect("first request");
        assert!(pool.voices[0].start(&head, 0, &mut requests));
        let new = pool.receiver.pop().expect("second request");
        assert_ne!(old.generation, new.generation);
        assert!(!pool.feeders[0].is_current(old.generation));
        // A slow loader delivers the stale block (with wrong data) first.
        let junk = vec![i16::MAX; old.frames as usize];
        pool.feeders[0].push_block(&old, &junk).expect("space");
        let from = new.from_frame as usize;
        pool.feeders[0]
            .push_block(&new, &data[from..from + new.frames as usize])
            .expect("space");
        let mut n = 0;
        loop {
            let pos = pool.voices[0].position();
            let Some(got) =
                pool.voices[0].next_frame(&head, 1.0, Interpolation::Hermite4, &mut requests)
            else {
                break;
            };
            let want = read_hermite_i16(&data, pos, total);
            assert_eq!(got.0.to_bits(), want.to_bits(), "pos={pos}");
            n += 1;
        }
        assert_eq!(n, total);
        assert_eq!(pool.stats.stale_blocks(), 1);
        assert_eq!(pool.stats.underruns(), 0);
    }

    #[test]
    fn underrun_fades_out_and_counts() {
        let data = pcm(100, 1);
        let head = SampleHead::from_pcm(&data, 1, 0, 8);
        let mut pool = StreamPool::with_ring_frames(1, 1, 32);
        let StreamPool {
            voices,
            requests,
            stats,
            ..
        } = &mut pool;
        assert!(voices[0].start(&head, 0, requests));
        let mut out = Vec::new();
        while let Some((l, _)) = voices[0].next_frame(&head, 1.0, Interpolation::Linear, requests) {
            out.push(l);
            assert!(out.len() < 1000);
        }
        // Linear reads frame i+1: frames 0..=6 are resident, then the fade.
        let resident = 7;
        assert_eq!(out.len(), resident + STEAL_FADE_SAMPLES as usize);
        assert_eq!(stats.underruns(), 1);
        let last = f32::from(data[7]);
        assert_eq!(out[resident].to_bits(), last.to_bits());
        for w in out[resident..].windows(2) {
            assert!(w[1].abs() <= w[0].abs());
        }
        assert!(out.last().unwrap().abs() <= last.abs() / STEAL_FADE_SAMPLES as f32 + 1e-3);
        assert!(!voices[0].is_active());
        assert!(
            voices[0]
                .next_frame(&head, 1.0, Interpolation::Linear, requests)
                .is_none()
        );
    }

    #[test]
    fn full_request_queue_counts_drop() {
        let data = pcm(1000, 1);
        let head = SampleHead::from_pcm(&data, 1, 0, 4);
        let mut pool = StreamPool::with_ring_frames(1, 1, 32);
        let StreamPool {
            voices,
            requests,
            receiver,
            stats,
            ..
        } = &mut pool;
        for _ in 0..3 {
            assert!(voices[0].start(&head, 0, requests));
        }
        assert_eq!(receiver.len(), 2);
        assert_eq!(stats.dropped_requests(), 1);
    }

    #[test]
    fn channel_mismatch_is_rejected() {
        let data = pcm(100, 2);
        let head = SampleHead::from_pcm(&data, 2, 0, 8);
        let mut pool = StreamPool::with_ring_frames(1, 1, 32);
        let StreamPool {
            voices,
            requests,
            receiver,
            ..
        } = &mut pool;
        assert!(!voices[0].start(&head, 0, requests));
        assert!(!voices[0].is_active());
        assert!(receiver.is_empty());
    }

    #[test]
    fn feeder_rejects_bad_length_and_full() {
        let mut pool = StreamPool::with_ring_frames(1, 2, 4);
        let req = LoadRequest {
            voice: 0,
            generation: 0,
            sample_index: 0,
            from_frame: 0,
            frames: 3,
        };
        assert_eq!(
            pool.feeders[0].push_block(&req, &[0; 5]),
            Err(FeedError::BadLength)
        );
        assert_eq!(pool.feeders[0].push_block(&req, &[0; 6]), Ok(()));
        assert_eq!(
            pool.feeders[0].push_block(&req, &[0; 6]),
            Err(FeedError::Full)
        );
    }

    #[test]
    fn budget_formula() {
        let a = SampleHead::from_pcm(&pcm(100, 2), 2, 0, 10);
        let b = SampleHead::from_pcm(&pcm(5, 1), 1, 1, 10);
        assert_eq!(
            stream_budget_bytes(&[a, b], 4, 2, 8),
            (20 + 5) * 2 + 4 * 8 * 2 * 2
        );
        let full = 64 * HEAD_FRAMES * 2 * 2 + 64 * RING_FRAMES * 2 * 2;
        let heads: Vec<SampleHead> = (0..64)
            .map(|i| SampleHead::new(Arc::from(vec![0_i16; HEAD_FRAMES * 2]), 1 << 20, 2, i))
            .collect();
        assert_eq!(stream_budget_bytes(&heads, 64, 2, RING_FRAMES), full);
    }
}
