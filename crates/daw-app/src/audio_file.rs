//! Reading audio files for clips: WAV through `audio_core::wav`, MP3 through
//! `symphonia` (SPEC-032). Never called on the audio path.

use std::path::Path;

use audio_core::buffer::AudioBuffer;
use audio_core::wav::WavSpec;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Extensions the importer accepts, lower case.
pub const AUDIO_EXTENSIONS: [&str; 2] = ["wav", "mp3"];

/// Why an audio file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum AudioFileError {
    /// The WAV reader failed.
    #[error(transparent)]
    Wav(#[from] audio_core::WavError),
    /// The MP3 file could not be opened or decoded.
    #[error("cannot decode mp3: {0}")]
    Mp3(String),
    /// The file decoded to zero frames.
    #[error("the file has no audio")]
    Empty,
}

/// True for a path whose extension is in [`AUDIO_EXTENSIONS`] (any case).
#[must_use]
pub fn is_audio_file(path: &Path) -> bool {
    extension(path).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Reads `path` as planar `f32`: MP3 by extension, everything else as WAV.
///
/// # Errors
///
/// See [`AudioFileError`].
pub fn read_audio(path: &Path) -> Result<(WavSpec, AudioBuffer), AudioFileError> {
    read_audio_progress(path, &mut |_, _| {})
}

/// Called while an MP3 decodes with its sample rate and the channels
/// decoded so far, after every packet.
pub type Progress<'a> = &'a mut dyn FnMut(u32, &[Vec<f32>]);

/// [`read_audio`] that reports a decoding MP3's content as it grows, so a
/// waveform can be drawn before the file is finished. WAV files are read in
/// one go and report nothing.
///
/// # Errors
///
/// See [`AudioFileError`].
pub fn read_audio_progress(
    path: &Path,
    progress: Progress<'_>,
) -> Result<(WavSpec, AudioBuffer), AudioFileError> {
    if extension(path).as_deref() == Some("mp3") {
        let file = std::fs::File::open(path).map_err(|e| AudioFileError::Mp3(e.to_string()))?;
        decode_mp3(Box::new(file), progress)
    } else {
        match audio_core::wav::read_wav_mono(path) {
            Err(audio_core::WavError::Empty) => Err(AudioFileError::Empty),
            other => Ok(other?),
        }
    }
}

/// Reads `path` and mixes its channels down to one at `rate`.
///
/// # Errors
///
/// See [`AudioFileError`]; a failed rate conversion is [`AudioFileError::Empty`].
pub fn read_mono_at(path: &Path, rate: u32) -> Result<Vec<f32>, AudioFileError> {
    let (spec, buffer) = read_audio(path)?;
    let mut mono = mix_down(&buffer);
    if spec.sample_rate != rate {
        mono =
            resample_parallel(&mono, spec.sample_rate, rate).map_err(|_| AudioFileError::Empty)?;
    }
    if mono.is_empty() {
        return Err(AudioFileError::Empty);
    }
    Ok(mono)
}

/// A file's content at the project rate: one channel, or left and right
/// (SPEC-050 §5). Files with more than two channels are mixed to one.
#[derive(Debug, Clone, PartialEq)]
pub struct Channels {
    /// The only channel, or the left one.
    pub left: Vec<f32>,
    /// The right channel of a stereo file, as long as `left`.
    pub right: Option<Vec<f32>>,
}

impl Channels {
    /// Frames per channel.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// The content as one channel: stereo is `(left + right) · 0.5`, the
    /// same mix [`read_mono_at`] makes.
    #[must_use]
    pub fn mono(&self) -> Vec<f32> {
        match &self.right {
            None => self.left.clone(),
            Some(right) => self
                .left
                .iter()
                .zip(right)
                .map(|(l, r)| (l + r) * 0.5)
                .collect(),
        }
    }
}

/// Reads `path` at `rate`, keeping a stereo file's two channels apart
/// (SPEC-050 §5). A file already at `rate` is not resampled.
///
/// # Errors
///
/// See [`AudioFileError`]; a failed rate conversion is [`AudioFileError::Empty`].
pub fn read_channels_at(path: &Path, rate: u32) -> Result<Channels, AudioFileError> {
    let (spec, buffer) = read_audio(path)?;
    channels_at(&spec, &buffer, rate)
}

/// [`read_channels_at`] for import: the fast resampler, and `progress`
/// called with the source rate and the channels decoded so far.
///
/// # Errors
///
/// As [`read_channels_at`].
pub fn read_channels_fast_at(
    path: &Path,
    rate: u32,
    progress: Progress<'_>,
) -> Result<Channels, AudioFileError> {
    let (spec, buffer) = read_audio_progress(path, progress)?;
    channels_with(&spec, &buffer, rate, true)
}

/// [`read_channels_at`] for content already decoded.
///
/// # Errors
///
/// As [`read_channels_at`].
pub fn channels_at(
    spec: &WavSpec,
    buffer: &AudioBuffer,
    rate: u32,
) -> Result<Channels, AudioFileError> {
    channels_with(spec, buffer, rate, false)
}

fn channels_with(
    spec: &WavSpec,
    buffer: &AudioBuffer,
    rate: u32,
    fast: bool,
) -> Result<Channels, AudioFileError> {
    let convert = |data: Vec<f32>| -> Result<Vec<f32>, AudioFileError> {
        if spec.sample_rate == rate {
            Ok(data)
        } else {
            resample_split(&data, spec.sample_rate, rate, fast).map_err(|_| AudioFileError::Empty)
        }
    };
    let channels = if buffer.channels() == 2 {
        Channels {
            left: convert(buffer.channel(0).to_vec())?,
            right: Some(convert(buffer.channel(1).to_vec())?),
        }
    } else {
        Channels {
            left: convert(mix_down(buffer))?,
            right: None,
        }
    };
    if channels.left.is_empty() {
        return Err(AudioFileError::Empty);
    }
    Ok(channels)
}

fn mix_down(buffer: &AudioBuffer) -> Vec<f32> {
    let channels = buffer.channels().max(1);
    let mut mono = buffer.channel(0).to_vec();
    if channels > 1 {
        for ch in 1..channels {
            for (out, sample) in mono.iter_mut().zip(buffer.channel(ch)) {
                *out += sample;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / channels as f32;
        for out in &mut mono {
            *out *= scale;
        }
    }
    mono
}

/// [`audio_core::resample`] split over the machine's cores (SPEC-035).
///
/// The result is bit for bit the same as the single-threaded call.
///
/// # Errors
///
/// [`audio_core::ResampleError::ZeroRate`] when either rate is zero.
pub fn resample_parallel(
    input: &[f32],
    from: u32,
    to: u32,
) -> Result<Vec<f32>, audio_core::ResampleError> {
    resample_split(input, from, to, false)
}

/// [`resample_parallel`] with the short kernel of
/// [`audio_core::resample_fast`], for importing files.
///
/// # Errors
///
/// [`audio_core::ResampleError::ZeroRate`] when either rate is zero.
pub fn resample_parallel_fast(
    input: &[f32],
    from: u32,
    to: u32,
) -> Result<Vec<f32>, audio_core::ResampleError> {
    resample_split(input, from, to, true)
}

fn resample_split(
    input: &[f32],
    from: u32,
    to: u32,
    fast: bool,
) -> Result<Vec<f32>, audio_core::ResampleError> {
    let one = |range: std::ops::Range<usize>| {
        if fast {
            audio_core::resample_range_fast(input, from, to, range)
        } else {
            audio_core::resample_range(input, from, to, range)
        }
    };
    const MIN_PIECE: usize = 1 << 16;
    let len = audio_core::resampled_len(input.len(), from, to)?;
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(len.div_ceil(MIN_PIECE))
        .max(1);
    if from == to {
        return audio_core::resample(input, from, to);
    }
    if threads == 1 {
        return one(0..len);
    }
    let piece = len.div_ceil(threads);
    let parts = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let range = i * piece..((i + 1) * piece).min(len);
                scope.spawn(move || one(range))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Err(audio_core::ResampleError::ZeroRate)))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(parts.concat())
}

/// Length of `path` at `rate` in frames, read from the header without
/// decoding the audio (SPEC-050 §2). MP3 files without a frame count in the
/// header are measured by walking their packets, still without decoding.
///
/// # Errors
///
/// See [`AudioFileError`]; a file with no audio is [`AudioFileError::Empty`].
pub fn probe_len_at(path: &Path, rate: u32) -> Result<u64, AudioFileError> {
    let (frames, from) = if extension(path).as_deref() == Some("mp3") {
        probe_mp3(path)?
    } else {
        let reader = hound::WavReader::open(path).map_err(audio_core::WavError::from)?;
        (u64::from(reader.duration()), reader.spec().sample_rate)
    };
    if frames == 0 || from == 0 || rate == 0 {
        return Err(AudioFileError::Empty);
    }
    Ok((u128::from(frames) * u128::from(rate)).div_ceil(u128::from(from)) as u64)
}

fn probe_mp3(path: &Path) -> Result<(u64, u32), AudioFileError> {
    let mp3 = |e: &dyn std::fmt::Display| AudioFileError::Mp3(e.to_string());
    let file = std::fs::File::open(path).map_err(|e| mp3(&e))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut format = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| mp3(&e))?
        .format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioFileError::Mp3("no audio track".into()))?;
    let rate = track.codec_params.sample_rate.unwrap_or(0);
    if let Some(frames) = track.codec_params.n_frames {
        return Ok((frames, rate));
    }
    let id = track.id;
    let mut frames = 0u64;
    loop {
        match format.next_packet() {
            Ok(packet) if packet.track_id() == id => frames += packet.dur,
            Ok(_) => {}
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(mp3(&e)),
        }
    }
    Ok((frames, rate))
}

/// Decodes MP3 data held in memory, e.g. samples built into the program.
///
/// # Errors
///
/// [`AudioFileError::Mp3`] for undecodable data, [`AudioFileError::Empty`]
/// when it holds no audio.
pub fn decode_mp3_bytes(bytes: &'static [u8]) -> Result<(WavSpec, AudioBuffer), AudioFileError> {
    decode_mp3(Box::new(std::io::Cursor::new(bytes)), &mut |_, _| {})
}

fn decode_mp3(
    source: Box<dyn symphonia::core::io::MediaSource>,
    progress: Progress<'_>,
) -> Result<(WavSpec, AudioBuffer), AudioFileError> {
    let mp3 = |e: &dyn std::fmt::Display| AudioFileError::Mp3(e.to_string());
    let stream = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| mp3(&e))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioFileError::Mp3("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| mp3(&e))?;

    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut samples: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(mp3(&e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged frame is skipped, like any player does.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(mp3(&e)),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let count = spec.channels.count().max(1);
        if channels.is_empty() {
            channels = vec![Vec::new(); count];
        }
        let needed = decoded.capacity() as u64;
        let buffer = match samples.as_mut() {
            Some(buffer) if buffer.capacity() as u64 >= needed * count as u64 => buffer,
            _ => samples.insert(SampleBuffer::new(needed, spec)),
        };
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks_exact(count) {
            for (ch, out) in channels.iter_mut().enumerate() {
                out.push(frame.get(ch).copied().unwrap_or(0.0));
            }
        }
        progress(rate, &channels);
    }

    let frames = channels.first().map_or(0, Vec::len);
    if frames == 0 || rate == 0 {
        return Err(AudioFileError::Empty);
    }
    let mut buffer = AudioBuffer::new(channels.len(), frames);
    for (ch, data) in channels.iter().enumerate() {
        buffer.channel_mut(ch).copy_from_slice(data);
    }
    let spec = WavSpec {
        sample_rate: rate,
        channels: u16::try_from(channels.len()).unwrap_or(u16::MAX),
    };
    Ok((spec, buffer))
}
