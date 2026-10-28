//! WAV I/O helpers used by the golden tests.
//!
//! WAV is the first format because it is trivial, well specified, and gives
//! the engine a stable on-disk fixture format: every golden test stores its
//! expected output as a WAV, so a human can listen to a failing test.

use std::io::{Read, Seek};
use std::path::Path;

use hound::{SampleFormat, WavReader, WavSpec as HoundSpec, WavWriter};
use thiserror::Error;

use crate::buffer::{AudioBuffer, Sample};

/// Errors produced by WAV reading/writing.
#[derive(Debug, Error)]
pub enum WavError {
    /// Underlying hound/IO failure.
    #[error("wav io error: {0}")]
    Io(#[from] hound::Error),
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
}
