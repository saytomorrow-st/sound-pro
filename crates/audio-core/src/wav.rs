//! WAV I/O helpers used by the golden tests.
//!
//! WAV is the first format because it is trivial, well specified, and gives
//! the engine a stable on-disk fixture format: every golden test stores its
//! expected output as a WAV, so a human can listen to a failing test.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec as HoundSpec, WavWriter};
use thiserror::Error;

use crate::buffer::{AudioBuffer, Sample};

/// Errors produced by WAV reading/writing.
#[derive(Debug, Error)]
pub enum WavError {
    /// Underlying hound/IO failure.
    #[error("wav io error: {0}")]
    Io(#[from] hound::Error),
    /// A raw filesystem operation failed (creating folders, probing paths).
    #[error("file system error at {path}: {source}")]
    FileSystem {
        /// Path the operation was attempted on.
        path: PathBuf,
        /// Underlying `std::io` error.
        source: std::io::Error,
    },
    /// Source has zero frames or channels.
    #[error("wav file is empty")]
    Empty,
    /// Bit depth / sample format this slice does not decode.
    #[error("unsupported layout: {channels} channels of {bits} bits {format:?}")]
    Unsupported {
        /// Channel count found in the file.
        channels: u16,
        /// Bit depth found in the file.
        bits: u16,
        /// Sample format found in the file.
        format: SampleFormat,
    },
    /// Recorder was asked to split at zero samples per part.
    #[error("max_part_samples must be greater than zero, got 0")]
    InvalidPartSize,
    /// Sample count is not a whole number of frames.
    #[error("sample count {samples} is not a multiple of {channels} channels")]
    NotFrameAligned {
        /// Number of samples passed to the recorder.
        samples: usize,
        /// Channel count of the target spec.
        channels: u16,
    },
    /// Recorder was driven after [`Recorder::finish`].
    #[error("recorder was already finished")]
    Finished,
}

/// Sample rate and channel layout of a WAV fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavSpec {
    /// Frames per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
}

impl Default for WavSpec {
    fn default() -> Self {
        Self {
            sample_rate: crate::transport::DEFAULT_SAMPLE_RATE,
            channels: 1,
        }
    }
}

impl From<WavSpec> for HoundSpec {
    fn from(spec: WavSpec) -> Self {
        Self {
            channels: spec.channels,
            sample_rate: spec.sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        }
    }
}

/// Writes a planar buffer as a 32-bit float WAV.
///
/// # Errors
///
/// Propagates any I/O or encoder error.
pub fn write_wav_mono<P: AsRef<Path>>(
    path: P,
    spec: WavSpec,
    buf: &AudioBuffer,
) -> Result<(), WavError> {
    let mut writer = WavWriter::create(path, spec.into())?;
    for frame in 0..buf.frames() {
        for ch in 0..buf.channels() {
            writer.write_sample(buf.channel(ch)[frame])?;
        }
    }
    writer.finalize()?;
    Ok(())
}

/// Reads a WAV into a planar buffer.
///
/// Supported inputs: 32-bit float and 16-bit integer PCM, any channel count.
/// Integer samples are scaled by `1/32768.0`, which is the convention used by
/// every DAW for symmetric full-scale conversion.
///
/// # Errors
///
/// Returns [`WavError::Unsupported`] for formats this slice does not cover.
pub fn read_wav_mono<P: AsRef<Path>>(path: P) -> Result<(WavSpec, AudioBuffer), WavError> {
    let reader = WavReader::open(path)?;
    let hound_spec = reader.spec();
    read_from_reader(reader, hound_spec)
}

/// Reads a WAV file and normalises it to a *classic* (non-extensible) `PCM_16`
/// WAV at `path_out` on a best-effort basis. Useful for sharing fixtures with
/// tools that cannot open float or `WAVE_FORMAT_EXTENSIBLE` files.
///
/// # Errors
///
/// Propagates read and write errors.
pub fn convert_to_pcm16<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dst: Q,
    sample_rate: u32,
) -> Result<(), WavError> {
    let mut reader = WavReader::open(src)?;
    let spec = reader.spec();
    let pcm = hound::WavSpec {
        channels: spec.channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dst, pcm)?;
    match spec.sample_format {
        SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                let s = sample?;
                #[allow(clippy::cast_possible_truncation)]
                let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                writer.write_sample(v)?;
            }
        }
        SampleFormat::Int if spec.bits_per_sample == 16 => {
            for sample in reader.samples::<i16>() {
                writer.write_sample(sample?)?;
            }
        }
        other @ SampleFormat::Int => {
            return Err(WavError::Unsupported {
                channels: spec.channels,
                bits: spec.bits_per_sample,
                format: other,
            });
        }
    }
    writer.finalize()?;
    Ok(())
}

fn read_from_reader<R: Read + Seek>(
    mut reader: WavReader<R>,
    spec: HoundSpec,
) -> Result<(WavSpec, AudioBuffer), WavError> {
    let channels = usize::from(spec.channels);
    let frames = reader.len() as usize / channels;
    if channels == 0 || frames == 0 {
        return Err(WavError::Empty);
    }
    let mut buf = AudioBuffer::new(channels, frames);
    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => {
            for (i, sample) in reader.samples::<f32>().enumerate() {
                let frame = i / channels;
                let ch = i % channels;
                buf.channel_mut(ch)[frame] = sample?;
            }
        }
        (SampleFormat::Int, 16) => {
            for (i, sample) in reader.samples::<i16>().enumerate() {
                let frame = i / channels;
                let ch = i % channels;
                buf.channel_mut(ch)[frame] = Sample::from(sample?) / 32768.0;
            }
        }
        other => {
            return Err(WavError::Unsupported {
                channels: spec.channels,
                bits: spec.bits_per_sample,
                format: other.0,
            });
        }
    }
    Ok((
        WavSpec {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        },
        buf,
    ))
}

/// Writes a mono WAV from a plain slice. Convenience for fixtures.
///
/// # Errors
///
/// Propagates any I/O or encoder error.
pub fn write_mono_slice<P: AsRef<Path>>(
    path: P,
    sample_rate: u32,
    samples: &[Sample],
) -> Result<(), WavError> {
    let mut writer = WavWriter::create(
        path,
        HoundSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    )?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Incremental multi-part WAV recorder.
///
/// Writes samples straight to disk instead of buffering a whole session in RAM,
/// which keeps long bounces (30+ minutes, many channels) memory-flat and
/// preserves finished parts if the process dies mid-session. When the current
/// part reaches `max_part_samples` frames it is finalized and a new part is
/// opened with the `-01`, `-02`, ... suffix (`part-01.wav`, `part-02.wav`, ...).
///
/// Each part carries its own valid RIFF header, so parts are individually
/// playable/readable; [`Recorder::finish`] finalizes the last open part.
///
/// # Realtime
///
/// This does file I/O on every [`Recorder::write`], so it **must not** be called
/// from [`crate::Graph::process`] / [`crate::Renderer::render`]. It belongs in
/// the fallback (offline/bounce) thread; in a realtime setup samples reach it
/// through an SPSC queue (see `docs/REALTIME.md`, SPEC-003).
pub struct Recorder {
    base: PathBuf,
    spec: WavSpec,
    max_part_samples: u64,
    part_index: u32,
    part_samples: u64,
    writer: Option<WavWriter<std::io::BufWriter<std::fs::File>>>,
    parts: Vec<PathBuf>,
    finished: bool,
}

impl Recorder {
    /// Creates the first part file immediately and returns a recorder.
    ///
    /// The output is split into parts of at most `max_part_samples` frames.
    /// Missing parent directories of `base` are created (as in the offline
    /// bounce). An existing file at a part path is overwritten, never appended.
    ///
    /// The part name is derived from `base`: `session.wav` yields
    /// `session-01.wav`, `session-02.wav`, ...; a `base` without an extension
    /// yields `base-01`, `base-02`, ....
    ///
    /// # Errors
    ///
    /// [`WavError::InvalidPartSize`] if `max_part_samples` is zero, or
    /// [`WavError::FileSystem`] / [`WavError::Io`] if the folder cannot be
    /// created or the first part cannot be opened.
    pub fn new<P: AsRef<Path>>(
        base: P,
        spec: WavSpec,
        max_part_samples: u64,
    ) -> Result<Self, WavError> {
        if max_part_samples == 0 {
            return Err(WavError::InvalidPartSize);
        }
        let base = base.as_ref().to_path_buf();
        if let Some(parent) = base.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| WavError::FileSystem {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let mut rec = Self {
            base,
            spec,
            max_part_samples,
            part_index: 0,
            part_samples: 0,
            writer: None,
            parts: Vec::new(),
            finished: false,
        };
        // Open the first (possibly empty) part eagerly, as the spec requires the
        // file to exist right after construction.
        rec.open_next_part()?;
        Ok(rec)
    }

    fn part_path(&self, index: u32) -> PathBuf {
        let stem = self
            .base
            .file_stem()
            .map_or_else(|| self.base.as_os_str(), |s| s);
        let mut name = std::ffi::OsString::from(stem);
        name.push(format!("-{index:02}"));
        if let Some(ext) = self.base.extension() {
            name.push(".");
            name.push(ext);
        }
        self.base.with_file_name(name)
    }

    fn open_next_part(&mut self) -> Result<(), WavError> {
        self.part_index += 1;
        let path = self.part_path(self.part_index);
        let writer = WavWriter::create(&path, self.spec.into())?;
        self.parts.push(path);
        self.part_samples = 0;
        self.writer = Some(writer);
        Ok(())
    }

    fn finalize_current(&mut self) -> Result<(), WavError> {
        if let Some(writer) = self.writer.take() {
            writer.finalize()?;
        }
        Ok(())
    }

    /// Appends interleaved samples (frame-major, `spec.channels` per frame).
    ///
    /// Parts are split on frame boundaries: a write never straddles the
    /// `max_part_samples` limit, so every part holds a whole number of frames.
    /// An empty slice is a no-op. A trailing partial frame (fewer than
    /// `spec.channels` samples) is an error.
    ///
    /// # Errors
    ///
    /// [`WavError::NotFrameAligned`] if the length is not a multiple of the
    /// channel count, [`WavError::Finished`] if called after
    /// [`Recorder::finish`], or [`WavError::Io`] on a disk error.
    pub fn write(&mut self, samples: &[Sample]) -> Result<(), WavError> {
        if self.finished {
            return Err(WavError::Finished);
        }
        if samples.is_empty() {
            return Ok(());
        }
        let channels = usize::from(self.spec.channels);
        if samples.len() % channels != 0 {
            return Err(WavError::NotFrameAligned {
                samples: samples.len(),
                channels: self.spec.channels,
            });
        }
        let mut chunk = samples;
        while !chunk.is_empty() {
            if self.writer.is_none() {
                self.open_next_part()?;
            }
            let remaining = self.max_part_samples - self.part_samples;
            let chunk_frames = chunk.len() / channels;
            #[allow(clippy::cast_possible_truncation)]
            let take_frames = remaining.min(chunk_frames as u64) as usize;
            let take_samples = take_frames * channels;
            {
                let Some(writer) = self.writer.as_mut() else {
                    unreachable!("writer is opened above when missing");
                };
                for &sample in &chunk[..take_samples] {
                    writer.write_sample(sample)?;
                }
            }
            self.part_samples += take_frames as u64;
            chunk = &chunk[take_samples..];
            if self.part_samples == self.max_part_samples && !chunk.is_empty() {
                self.finalize_current()?;
            }
        }
        Ok(())
    }

    /// Finalizes the last part and returns every written part path in order.
    ///
    /// Calling `finish` on a recorder with no samples still yields one valid
    /// zero-length WAV, because the first part is created in [`Recorder::new`].
    ///
    /// # Errors
    ///
    /// [`WavError::Finished`] if called twice, or [`WavError::Io`] if the final
    /// header cannot be written. Parts written so far remain on disk on error.
    pub fn finish(mut self) -> Result<Vec<PathBuf>, WavError> {
        if self.finished {
            return Err(WavError::Finished);
        }
        self.finalize_current()?;
        self.finished = true;
        Ok(self.parts)
    }
}

/// Fills a buffer with a unit test sine wave. Deterministic bit-for-bit, so it
/// is safe to compare across platforms.
#[must_use]
pub fn sine(freq_hz: f64, sample_rate: u32, offset: u64, frames: usize) -> Vec<Sample> {
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let sample_index = offset + i as u64;
        #[allow(clippy::cast_precision_loss)]
        let phase =
            std::f64::consts::TAU * freq_hz * (sample_index as f64) / f64::from(sample_rate);
        #[allow(clippy::cast_possible_truncation)]
        out.push(phase.sin() as Sample);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_float_wav() {
        let dir = std::env::temp_dir().join(format!("daw-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.wav");
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut buf = AudioBuffer::new(2, 64);
        buf.channel_mut(0)
            .copy_from_slice(&sine(440.0, 48_000, 0, 64));
        buf.channel_mut(1).fill(-0.25);
        write_wav_mono(&path, spec, &buf).unwrap();
        let (read_spec, read) = read_wav_mono(&path).unwrap();
        assert_eq!(read_spec, spec);
        assert_eq!(read, buf);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sine_is_deterministic() {
        assert_eq!(sine(440.0, 48_000, 0, 8), sine(440.0, 48_000, 0, 8));
        // Rendering from an offset is bit-identical to slicing a longer render,
        // which is what lets tests render overlapping quanta independently.
        let whole = sine(100.0, 8_000, 0, 16);
        let tail = sine(100.0, 8_000, 8, 8);
        let bits = |v: &[Sample]| v.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&whole[8..]), bits(&tail));
    }

    /// Owned temp directory removed on drop, so failing tests do not litter.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "daw-rec-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Exact values that are all distinct in `f32`, so a truncation or a
    /// reordering of samples would change the comparison below.
    #[allow(clippy::cast_precision_loss)] // fixtures stay far below 2^24
    fn distinct_samples(count: usize) -> Vec<Sample> {
        (0..count).map(|i| (i + 1) as Sample / 64.0).collect()
    }

    fn rec_spec() -> WavSpec {
        WavSpec {
            sample_rate: 48_000,
            channels: 1,
        }
    }

    #[test]
    fn recorder_splits_into_exact_parts() {
        let dir = TempDir::new("split");
        let base = dir.path("part.wav");
        let samples = distinct_samples(25);
        let mut rec = Recorder::new(&base, rec_spec(), 10).unwrap();
        rec.write(&samples).unwrap();
        let parts = rec.finish().unwrap();

        assert_eq!(parts.len(), 3, "25 samples at 10/part -> 10,10,5");
        assert_eq!(parts[0], dir.path("part-01.wav"));
        assert_eq!(parts[1], dir.path("part-02.wav"));
        assert_eq!(parts[2], dir.path("part-03.wav"));

        let lengths: Vec<usize> = parts
            .iter()
            .map(|p| read_wav_mono(p).unwrap().1.frames())
            .collect();
        assert_eq!(lengths, vec![10, 10, 5]);

        // Content is preserved exactly and in order.
        let mut rejoined = Vec::new();
        for p in &parts {
            rejoined.extend_from_slice(read_wav_mono(p).unwrap().1.channel(0));
        }
        assert_eq!(rejoined, samples);
    }

    #[test]
    fn recorder_split_boundary_lands_exactly() {
        // A write that exactly fills a part must not open an empty next part.
        let dir = TempDir::new("exact");
        let base = dir.path("exact.wav");
        let mut rec = Recorder::new(&base, rec_spec(), 8).unwrap();
        rec.write(&[0.5; 8]).unwrap();
        let parts = rec.finish().unwrap();
        assert_eq!(parts.len(), 1, "one exactly-full part, no trailing empty");
        assert_eq!(read_wav_mono(&parts[0]).unwrap().1.frames(), 8);
    }

    #[test]
    fn recorder_handles_writes_spanning_multiple_parts() {
        let dir = TempDir::new("span");
        let base = dir.path("span.wav");
        let mut rec = Recorder::new(&base, rec_spec(), 4).unwrap();
        // One call crosses three part boundaries.
        rec.write(&[1.0; 10]).unwrap();
        let parts = rec.finish().unwrap();
        let lengths: Vec<usize> = parts
            .iter()
            .map(|p| read_wav_mono(p).unwrap().1.frames())
            .collect();
        assert_eq!(lengths, vec![4, 4, 2]);
    }

    #[test]
    fn recorder_rejects_zero_part_size() {
        let dir = TempDir::new("zero");
        let err = Recorder::new(dir.path("z.wav"), rec_spec(), 0)
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, WavError::InvalidPartSize));
    }

    #[test]
    fn recorder_empty_write_is_noop() {
        let dir = TempDir::new("empty-write");
        let base = dir.path("e.wav");
        let mut rec = Recorder::new(&base, rec_spec(), 4).unwrap();
        rec.write(&[]).unwrap();
        let parts = rec.finish().unwrap();
        assert_eq!(parts.len(), 1);
        // read_wav_mono treats zero-frame files as Empty (its contract), so
        // verify the zero-length WAV through hound directly.
        let reader = WavReader::open(&parts[0]).unwrap();
        assert_eq!(reader.len(), 0);
        assert_eq!(reader.spec().sample_rate, 48_000);
    }

    #[test]
    fn recorder_finish_without_write_is_valid_zero_length() {
        let dir = TempDir::new("nowrite");
        let base = dir.path("n.wav");
        let rec = Recorder::new(&base, rec_spec(), 100).unwrap();
        let parts = rec.finish().unwrap();
        assert_eq!(parts.len(), 1);
        let reader = WavReader::open(&parts[0]).unwrap();
        assert_eq!(reader.len(), 0);
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 32);
    }

    #[test]
    fn recorder_overwrites_existing_file() {
        let dir = TempDir::new("overwrite");
        let base = dir.path("o-01.wav");
        // Pre-existing longer file must not be appended to.
        write_mono_slice(&base, 48_000, &[9.0; 100]).unwrap();
        let mut rec = Recorder::new(dir.path("o.wav"), rec_spec(), 8).unwrap();
        rec.write(&[0.25; 3]).unwrap();
        let parts = rec.finish().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], base);
        let buf = read_wav_mono(&base).unwrap().1;
        assert_eq!(buf.frames(), 3, "file truncated to new content");
        assert_eq!(buf.channel(0), &[0.25, 0.25, 0.25]);
    }

    #[test]
    fn recorder_creates_missing_parent_dirs() {
        let dir = TempDir::new("mkdirs");
        let nested = dir.path("a").join("b").join("c").join("deep.wav");
        let mut rec = Recorder::new(&nested, rec_spec(), 4).unwrap();
        rec.write(&[0.1; 2]).unwrap();
        let parts = rec.finish().unwrap();
        assert!(parts[0].exists());
        assert_eq!(parts[0], nested.with_file_name("deep-01.wav"));
    }

    #[test]
    fn recorder_rejects_partial_frame() {
        let dir = TempDir::new("partial");
        let base = dir.path("p.wav");
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut rec = Recorder::new(&base, spec, 8).unwrap();
        let err = rec.write(&[0.0; 3]).unwrap_err();
        assert!(matches!(
            err,
            WavError::NotFrameAligned {
                samples: 3,
                channels: 2
            }
        ));
    }
}
