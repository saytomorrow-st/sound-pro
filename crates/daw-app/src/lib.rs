//! Application shell.
//!
//! This crate wires a sound backend to [`audio_core::Renderer`]. The split
//! exists so the engine crate stays free of platform APIs: everything testable
//! lives in `audio-core`, and only the thin backend adapter here needs CI
//! hardware flags.
//!
//! Backend selection:
//!
//! * `offline` (default, always available): bounce to a WAV file. Used by CI,
//!   by agents, and by users without audio hardware.
//! * `realtime` (feature-gated `realtime`): CPAL → WASAPI on Windows,
//!   ALSA/PipeWire/JACK on Linux.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use audio_core::buffer::AudioBuffer;
use audio_core::graph::Graph;
use audio_core::offline::OfflineRenderer;
use audio_core::params::{ParamHandle, ParamId, ParamSpec};
use audio_core::render::Renderer;
use audio_core::schedule::Schedule;
use audio_core::synth::{Envelope, Synth};
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Transport, TransportCommand};
use audio_core::wav::{Recorder, WavSpec, write_mono_slice};

pub mod audio_file;
pub mod audio_settings;
pub mod meters;
pub mod nodes;
pub mod piano;

/// Errors from assembling a session graph (SPEC-010).
///
/// The session model itself is validated by
/// [`audio_core::Session::validate`]; these two variants describe the
/// mismatch between the model and the material the caller supplied for it,
/// which only the shell can see.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The job carried a different number of track sources than the session
    /// has tracks.
    #[error("session has {tracks} track(s), job carried {sources}")]
    TrackCountMismatch {
        /// Tracks in the session.
        tracks: usize,
        /// Entries in [`SessionJob::tracks`].
        sources: usize,
    },
    /// A track has a different number of clips and clip sources.
    #[error("track {track:?} has {clips} clip(s) but {sources} source(s)")]
    ClipCountMismatch {
        /// Track name, for the message.
        track: String,
        /// Clips on the track.
        clips: usize,
        /// Sources supplied for it.
        sources: usize,
    },
}

/// Render quantum used by the MIDI pass in [`render_midi`].
const BLOCK_FRAMES: usize = 256;

/// Options for an offline smoke bounce.
#[derive(Debug, Clone)]
pub struct OfflineJob {
    /// Destination WAV path.
    pub output: PathBuf,
    /// Duration in seconds.
    pub seconds: f64,
    /// Sample rate.
    pub sample_rate: u32,
    /// Render quantum used by the bounce, in frames.
    pub block_frames: usize,
    /// Split the output into parts of at most this many samples each.
    ///
    /// `None` (default) writes a single file. `Some(n)` writes
    /// `part-01.wav`, `part-02.wav`, ... as described by
    /// [`audio_core::wav::Recorder`].
    pub split_part_samples: Option<u64>,
}

impl Default for OfflineJob {
    fn default() -> Self {
        Self {
            output: PathBuf::from("bounce.wav"),
            seconds: 1.0,
            sample_rate: DEFAULT_SAMPLE_RATE,
            block_frames: 256,
            split_part_samples: None,
        }
    }
}

/// Result of an offline bounce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BounceReport {
    /// Total frames rendered, across all parts.
    pub frames: u64,
    /// Paths of the WAV files written, in order. A single file when
    /// [`OfflineJob::split_part_samples`] is `None`.
    pub parts: Vec<PathBuf>,
}

/// Renders the demo project offline and writes it as mono WAV.
///
/// Missing parent directories of `job.output` are created. When
/// `job.split_part_samples` is set the audio is streamed into a
/// [`Recorder`] instead of being held in memory, and one file per part is
/// written.
///
/// # Errors
///
/// Returns a boxed error if the graph cannot be built, rendering fails, or the
/// file cannot be written.
pub fn run_offline(job: &OfflineJob) -> Result<BounceReport, Box<dyn std::error::Error>> {
    if let Some(parent) = job.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut graph = Graph::with_capacity(job.sample_rate, 1, 1024, 4, 1);
    let level = ParamHandle::new(demo_level_spec()?);
    graph.register_param(level.clone())?;
    graph.add_node("tone", nodes::Tone::new(440.0, level))?;

    let mut transport = Transport::new(job.sample_rate);
    transport.apply(TransportCommand::Play);

    let renderer = Renderer::new(graph, transport, 1024);
    let mut offline = OfflineRenderer::new(renderer, job.block_frames);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (job.seconds * f64::from(job.sample_rate)).round() as u64;

    let spec = WavSpec {
        sample_rate: job.sample_rate,
        channels: 1,
    };
    match job.split_part_samples {
        None => {
            let mono = offline.bounce_mono(frames)?;
            write_mono_slice(&job.output, job.sample_rate, &mono)?;
            Ok(BounceReport {
                frames,
                parts: vec![job.output.clone()],
            })
        }
        Some(max_part_samples) => {
            let mut recorder = Recorder::new(&job.output, spec, max_part_samples)?;
            // Stream in render quanta so a long bounce never holds the whole
            // take in memory.
            let mut done = 0u64;
            while done < frames {
                let n = (frames - done).min(job.block_frames as u64);
                #[allow(clippy::cast_possible_truncation)]
                let n = n as usize;
                let block = offline.bounce_mono(n as u64)?;
                recorder.write(&block)?;
                done += n as u64;
            }
            let parts = recorder.finish()?;
            Ok(BounceReport { frames, parts })
        }
    }
}

/// Converts a planar buffer into an interleaved output slice.
///
/// This is the last step of every realtime callback and is trivial enough to
/// unit-test without any audio device.
pub fn interleave(src: &AudioBuffer, frames: usize, out: &mut [f32]) {
    let channels = src.channels();
    let n = frames.min(src.frames());
    for frame in 0..n {
        for ch in 0..channels {
            out[frame * channels + ch] = src.channel(ch)[frame];
        }
    }
    for s in &mut out[n * channels..] {
        *s = 0.0;
    }
}

/// Realtime CPAL backend (SPEC-005). Always compiled for its pure helpers;
/// the device glue requires the `realtime` feature.
pub mod capture;
pub mod playback;
pub mod realtime;

/// Builds the demo graph used by `daw bounce`, `daw play`, and `selftest`: a
/// single 440 Hz sine at half amplitude on a mono graph.
///
/// # Errors
///
/// Returns a boxed error if the graph cannot be built.
pub fn build_demo_graph(sample_rate: u32) -> Result<Graph, Box<dyn std::error::Error>> {
    let mut graph = Graph::with_capacity(sample_rate, 1, 1024, 4, 1);
    let level = ParamHandle::new(demo_level_spec()?);
    graph.register_param(level.clone())?;
    graph.add_node("tone", nodes::Tone::new(440.0, level))?;
    Ok(graph)
}

/// Description of the demo tone's amplitude: 0..1, half by default, 10 ms ramp.
///
/// # Errors
///
/// Returns a boxed [`audio_core::params::ParamError`] if the description is
/// invalid; it is a constant, so this only trips if the constant is edited
/// wrongly.
pub fn demo_level_spec() -> Result<ParamSpec, Box<dyn std::error::Error>> {
    Ok(ParamSpec::new(ParamId(0), "tone.level", 0.0, 1.0, 0.5, 10)?)
}

/// Options for `daw render-midi`: a parsed schedule rendered to a WAV file.
#[derive(Debug, Clone)]
pub struct MidiRenderJob {
    /// Destination WAV path.
    pub output: PathBuf,
    /// Schedule built on the control thread from an SMF file.
    pub schedule: Schedule,
    /// MIDI notes in the source file, for the report.
    pub notes: u64,
    /// Sample rate.
    pub sample_rate: u32,
    /// Render quantum, in frames.
    pub block_frames: usize,
    /// Override the length: render this many frames instead of
    /// [`Schedule::length_samples`].
    pub frames: Option<u64>,
}

/// Result of `daw render-midi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiRenderReport {
    /// Frames rendered and written.
    pub frames: u64,
    /// How many of them the schedule covers (the rest is the trailing tail).
    pub scheduled_frames: u64,
    /// Voice starts counted by the instrument during this render (voice
    /// stealing included, so it can exceed the note count when the pool
    /// overflows).
    pub voices_started: u32,
}

/// Renders `job.schedule` through a [`nodes::PolySynth`] and writes mono WAV.
///
/// The graph holds exactly one node, so the tail beyond the last event is
/// rendered as silence only because the schedule itself carries the release
/// tail ([`audio_core::ScheduleSpec::from_midi`]).
///
/// # Errors
///
/// Returns a boxed error if the graph cannot be built, rendering fails, or the
/// file cannot be written.
pub fn render_midi(job: &MidiRenderJob) -> Result<MidiRenderReport, Box<dyn std::error::Error>> {
    if let Some(parent) = job.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut graph = Graph::with_capacity(
        job.sample_rate,
        1,
        job.block_frames.next_power_of_two(),
        4,
        1,
    );
    let gain = ParamHandle::new(synth_gain_spec()?);
    graph.register_param(gain.clone())?;
    graph.add_node(
        "synth",
        nodes::PolySynth::new(
            job.schedule.clone(),
            job.sample_rate,
            Envelope::default(),
            gain,
        )?,
    )?;

    let mut transport = Transport::new(job.sample_rate);
    transport.apply(TransportCommand::Play);

    let scheduled = job.schedule.length_samples();
    let frames = job.frames.unwrap_or(scheduled);
    let renderer = Renderer::new(graph, transport, 1024);
    let mut offline = OfflineRenderer::new(renderer, job.block_frames);
    let mono = offline.bounce_mono(frames)?;
    write_mono_slice(&job.output, job.sample_rate, &mono)?;

    Ok(MidiRenderReport {
        frames,
        scheduled_frames: scheduled,
        voices_started: voices_started(&job.schedule, frames, job.sample_rate),
    })
}

/// Replays the schedule through a bare [`Synth`] to count voice starts.
///
/// The instrument inside the graph is not reachable from here, and adding a
/// counter to the audio path for a diagnostic would be worse: the replay is a
/// control-thread pass over the same events, in the same quanta, and the
/// `Synth` is deterministic, so the number is the one the render saw.
fn voices_started(schedule: &Schedule, frames: u64, sample_rate: u32) -> u32 {
    let Ok(mut synth) = Synth::new(sample_rate, Envelope::default()) else {
        return 0;
    };
    let mut cursor = schedule.cursor();
    let mut block = vec![0.0_f32; BLOCK_FRAMES];
    let mut position = 0_u64;
    while position < frames {
        let remaining = usize::try_from(frames - position).unwrap_or(usize::MAX);
        let n = remaining.min(BLOCK_FRAMES);
        synth.render(schedule, &mut cursor, position, n, &mut block[..n]);
        position += n as u64;
    }
    synth.voices_started()
}

/// Description of the instrument's output gain: 0..1, half by default, 10 ms
/// ramp (SPEC-009 §10).
///
/// # Errors
///
/// Returns a boxed [`audio_core::params::ParamError`] if the description is
/// invalid; it is a constant, so this only trips if the constant is edited
/// wrongly.
pub fn synth_gain_spec() -> Result<ParamSpec, Box<dyn std::error::Error>> {
    Ok(ParamSpec::new(ParamId(1), "synth.gain", 0.0, 1.0, 0.5, 10)?)
}

/// One track's material, ready to be mounted: the model plus one source per
/// clip, in the clip's order (SPEC-010 §4).
pub struct TrackSource {
    /// The track's model: names, gain, mute and clip windows.
    pub track: audio_core::session::Track,
    /// Content for each clip, `track.clips[i]` ↔ `clips[i]`.
    pub clips: Vec<Box<dyn nodes::ClipSource + Send>>,
}

impl std::fmt::Debug for TrackSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackSource")
            .field("track", &self.track.name)
            .field("sources", &self.clips.len())
            .finish()
    }
}

/// Everything `daw render-session` and the tests need to render a session.
#[derive(Debug)]
pub struct SessionJob {
    /// The session model; validated before anything is built.
    pub session: audio_core::session::Session,
    /// Material for each track, in the session's order.
    pub tracks: Vec<TrackSource>,
    /// Sample rate of the graph.
    pub sample_rate: u32,
    /// Render quantum, in frames.
    pub block_frames: usize,
    /// Render this many frames instead of [`audio_core::Session::length_samples`].
    pub frames: Option<u64>,
    /// Destination WAV path.
    pub output: PathBuf,
}

/// Result of [`render_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReport {
    /// Frames rendered and written.
    pub frames: u64,
    /// Frames the session itself covers; the rest of `frames` is silence.
    pub session_frames: u64,
    /// Track nodes mounted in the graph.
    pub tracks: usize,
}

/// Mounts one node per track into a graph built for `job.sample_rate`.
///
/// The graph sums the nodes itself, so track mixing is not reimplemented here.
/// Everything expensive — validation, source pairing, node construction —
/// happens before the transport starts, never inside `process`.
///
/// Node names are short labels derived from the track name, formatted once
/// here and leaked deliberately: a graph is built a handful of times per
/// session, and `Graph::add_node` wants a `&'static str`. Leaking a few dozen
/// short labels costs less than teaching the graph to own node names, and it
/// never happens on the audio thread.
///
/// # Errors
///
/// Returns [`audio_core::SessionError`] when the model is invalid, [`SessionError`]
/// when the job's tracks or clips do not line up, or a graph error.
pub fn build_session_graph(job: SessionJob) -> Result<Graph, Box<dyn std::error::Error>> {
    build_session_graph_metered(job, None)
}

/// [`build_session_graph`] whose tracks also report their post-fader levels
/// into `meters` (SPEC-042); the sound is the same with or without them.
///
/// # Errors
///
/// As [`build_session_graph`].
pub fn build_session_graph_metered(
    job: SessionJob,
    meters: Option<&meters::Meters>,
) -> Result<Graph, Box<dyn std::error::Error>> {
    job.session.validate()?;
    if job.tracks.len() != job.session.tracks.len() {
        return Err(Box::new(SessionError::TrackCountMismatch {
            tracks: job.session.tracks.len(),
            sources: job.tracks.len(),
        }));
    }

    let mut graph = Graph::with_capacity(
        job.sample_rate,
        2,
        job.block_frames.next_power_of_two(),
        4,
        job.session.tracks.len().max(1),
    );
    // Sources are moved into the graph one by one, so the job is consumed:
    // rendering the same session twice means building the job twice, which is
    // what the determinism test does.
    // SPEC-028: without buses every track stays its own node, exactly as
    // before; with buses one mixer node renders tracks, sends and returns.
    let with_buses = !job.session.buses.is_empty();
    let master = if with_buses {
        1.0
    } else {
        job.session.master_level()
    };
    let mut mixed = Vec::new();
    for (index, source) in job.tracks.into_iter().enumerate() {
        let node = nodes::Track::new(&source.track, source.clips)?
            .with_mix(job.session.is_audible(index), master)
            .with_effects(&source.track.effects, job.sample_rate)
            .with_automation(&source.track.automation);
        let node = match meters {
            Some(meters) => node.with_meter(meters, index),
            None => node,
        };
        if with_buses {
            mixed.push((node, source.track.sends.clone()));
        } else {
            let name = track_label(&source.track.name);
            graph.add_node(name, node)?;
        }
    }
    if with_buses {
        let mixer = nodes::Mixer::new(
            mixed,
            &job.session.buses,
            job.session.master_level(),
            job.sample_rate,
            job.block_frames.next_power_of_two(),
        );
        graph.add_node("mixer", mixer)?;
    }
    Ok(graph)
}

/// Errors from loading a project document and its clip files (SPEC-014).
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The project file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// Project path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The project document is invalid.
    #[error("{path}: {source}")]
    Project {
        /// Project path.
        path: PathBuf,
        /// Parse or validation error.
        source: audio_core::ProjectError,
    },
    /// A clip has no `source` path, so there is nothing to play.
    #[error("track {track} clip {clip} has no source file")]
    MissingSource {
        /// Track index.
        track: usize,
        /// Clip index inside the track.
        clip: usize,
    },
    /// A clip's WAV file could not be read.
    #[error("{path}: {source}")]
    Wav {
        /// Resolved file path.
        path: PathBuf,
        /// Underlying error.
        source: audio_core::WavError,
    },
    /// A clip's MP3 file could not be decoded (SPEC-032).
    #[error("{path}: {message}")]
    Decode {
        /// Resolved file path.
        path: PathBuf,
        /// Decoder message.
        message: String,
    },
    /// A MIDI clip's notes could not be rendered (SPEC-021).
    #[error("track {track} clip {clip}: {source}")]
    Synth {
        /// Track index.
        track: usize,
        /// Clip index inside the track.
        clip: usize,
        /// Synthesizer error.
        source: audio_core::SynthError,
    },
    /// A clip's WAV file holds fewer frames than its `source_length`.
    #[error("{path}: {file_frames} frame(s), clip needs source_length {source_length}")]
    SourceTooShort {
        /// Resolved file path.
        path: PathBuf,
        /// Frames in the file.
        file_frames: u64,
        /// Frames the clip declares.
        source_length: u64,
    },
}

/// Loads a project document and every clip's WAV file into a [`SessionJob`].
///
/// Relative `source` paths are resolved against the project file's folder.
/// Multi-channel files are averaged to mono; frames past `source_length`
/// are dropped. All I/O happens here, before any rendering starts.
///
/// # Errors
///
/// See [`LoadError`]; the first failing clip, in session order, is reported.
pub fn load_session_project(
    path: &std::path::Path,
    output: PathBuf,
) -> Result<SessionJob, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let project = audio_core::Project::from_json(&text).map_err(|source| LoadError::Project {
        path: path.to_path_buf(),
        source,
    })?;
    let base = path.parent().unwrap_or_else(|| std::path::Path::new(""));
    load_session(project.session().clone(), base, output)
}

/// Loads every clip's WAV file of an in-memory `session` into a [`SessionJob`].
///
/// MIDI clips need no file: their notes are rendered by the built-in synth
/// here, before playback (SPEC-021).
///
/// Relative `source` paths are resolved against `base`; this is what
/// [`load_session_project`] does after parsing the document, and what the
/// editor calls for a project that has not been saved yet.
///
/// # Errors
///
/// See [`LoadError`]; the first failing clip, in session order, is reported.
pub fn load_session(
    session: audio_core::Session,
    base: &std::path::Path,
    output: PathBuf,
) -> Result<SessionJob, LoadError> {
    load_session_cached(session, base, output, &mut NoCache)
}

/// Shared left and right samples of one stereo file.
pub type StereoSamples = (std::sync::Arc<Vec<f32>>, std::sync::Arc<Vec<f32>>);

/// Decoded material a caller already holds, so loading a session does not
/// read files or render notes again (SPEC-035).
pub trait ClipCache {
    /// Mono samples of file `source` at the session rate, if cached.
    fn audio(&mut self, source: &str) -> Option<std::sync::Arc<Vec<f32>>>;
    /// Left and right samples of stereo file `source` at the session rate,
    /// if cached (SPEC-050 §5). Asked first for every audio clip: a mono
    /// track plays them as `(L + R) · 0.5`. A miss falls back to
    /// [`ClipCache::audio`] and then to disk.
    fn audio_stereo(&mut self, _source: &str) -> Option<StereoSamples> {
        None
    }
    /// Rendered `notes` of `frames` frames at `rate`, if cached.
    fn notes(
        &mut self,
        notes: &[audio_core::session::Note],
        rate: u32,
        frames: u64,
    ) -> Option<std::sync::Arc<Vec<f32>>>;
    /// Called after `notes` were rendered on a miss.
    fn store_notes(
        &mut self,
        notes: &[audio_core::session::Note],
        rate: u32,
        frames: u64,
        samples: std::sync::Arc<Vec<f32>>,
    );
}

/// A cache that never hits: everything comes from disk or the synth.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCache;

impl ClipCache for NoCache {
    fn audio(&mut self, _source: &str) -> Option<std::sync::Arc<Vec<f32>>> {
        None
    }
    fn notes(
        &mut self,
        _notes: &[audio_core::session::Note],
        _rate: u32,
        _frames: u64,
    ) -> Option<std::sync::Arc<Vec<f32>>> {
        None
    }
    fn store_notes(
        &mut self,
        _notes: &[audio_core::session::Note],
        _rate: u32,
        _frames: u64,
        _samples: std::sync::Arc<Vec<f32>>,
    ) {
    }
}

/// [`load_session`] that asks `cache` first and reads files or renders
/// notes only on a miss (SPEC-035). Cached audio shorter than the clip's
/// `source_length` is [`LoadError::SourceTooShort`], as from disk.
///
/// # Errors
///
/// As [`load_session`].
pub fn load_session_cached(
    session: audio_core::Session,
    base: &std::path::Path,
    output: PathBuf,
    cache: &mut dyn ClipCache,
) -> Result<SessionJob, LoadError> {
    let rate = session.sample_rate;
    let mut tracks = Vec::with_capacity(session.tracks.len());
    for (track_index, track) in session.tracks.iter().enumerate() {
        let mut clips: Vec<Box<dyn nodes::ClipSource + Send>> =
            Vec::with_capacity(track.clips.len());
        for (clip_index, clip) in track.clips.iter().enumerate() {
            let length = clip.source_length;
            if let Some(notes) = &clip.notes {
                let samples = match cache.notes(notes, rate, length) {
                    Some(hit) => hit,
                    None => {
                        let fresh =
                            std::sync::Arc::new(piano::render_piano(notes, rate, length).map_err(
                                |source| LoadError::Synth {
                                    track: track_index,
                                    clip: clip_index,
                                    source,
                                },
                            )?);
                        cache.store_notes(notes, rate, length, std::sync::Arc::clone(&fresh));
                        fresh
                    }
                };
                clips.push(Box::new(nodes::SharedClip::whole(samples)));
                continue;
            }
            let source = clip.source.as_deref().ok_or(LoadError::MissingSource {
                track: track_index,
                clip: clip_index,
            })?;
            let file = base.join(source);
            let stereo = track.format == audio_core::TrackFormat::Stereo;
            if let Some((left, right)) = cache.audio_stereo(source) {
                let file_frames = left.len().min(right.len()) as u64;
                let shared = nodes::SharedClip::stereo(left, right, length).ok_or(
                    LoadError::SourceTooShort {
                        path: file,
                        file_frames,
                        source_length: length,
                    },
                )?;
                clips.push(Box::new(shared));
                continue;
            }
            if stereo && cache.audio(source).is_none() {
                clips.push(Box::new(load_clip_stereo(&file, rate, length)?));
                continue;
            }
            if let Some(samples) = cache.audio(source) {
                let file_frames = samples.len() as u64;
                let shared =
                    nodes::SharedClip::new(samples, length).ok_or(LoadError::SourceTooShort {
                        path: file,
                        file_frames,
                        source_length: length,
                    })?;
                clips.push(Box::new(shared));
                continue;
            }
            clips.push(Box::new(load_clip_file(&file, rate, length)?));
        }
        tracks.push(TrackSource {
            track: track.clone(),
            clips,
        });
    }
    Ok(SessionJob {
        sample_rate: session.sample_rate,
        session,
        tracks,
        block_frames: BLOCK_FRAMES,
        frames: None,
        output,
    })
}

fn audio_file_error(path: &std::path::Path, e: audio_file::AudioFileError) -> LoadError {
    let wav_error = |source| LoadError::Wav {
        path: path.to_path_buf(),
        source,
    };
    match e {
        audio_file::AudioFileError::Wav(source) => wav_error(source),
        audio_file::AudioFileError::Empty => wav_error(audio_core::WavError::Empty),
        audio_file::AudioFileError::Mp3(message) => LoadError::Decode {
            path: path.to_path_buf(),
            message,
        },
    }
}

/// Reads `path` for a stereo track: a stereo file keeps its two channels,
/// a mono one is played on both (SPEC-050 §5).
fn load_clip_stereo(
    path: &std::path::Path,
    sample_rate: u32,
    source_length: u64,
) -> Result<nodes::SharedClip, LoadError> {
    let channels =
        audio_file::read_channels_at(path, sample_rate).map_err(|e| audio_file_error(path, e))?;
    let file_frames = channels.frames() as u64;
    let too_short = || LoadError::SourceTooShort {
        path: path.to_path_buf(),
        file_frames,
        source_length,
    };
    let left = std::sync::Arc::new(channels.left);
    match channels.right {
        Some(right) => nodes::SharedClip::stereo(left, std::sync::Arc::new(right), source_length)
            .ok_or_else(too_short),
        None => nodes::SharedClip::new(left, source_length).ok_or_else(too_short),
    }
}

/// Reads `path` as mono content of exactly `source_length` frames at
/// `sample_rate`, converting the rate first when the file differs (SPEC-019).
fn load_clip_file(
    path: &std::path::Path,
    sample_rate: u32,
    source_length: u64,
) -> Result<Vec<f32>, LoadError> {
    let mono =
        audio_file::read_mono_at(path, sample_rate).map_err(|e| audio_file_error(path, e))?;
    let file_frames = mono.len() as u64;
    if file_frames < source_length {
        return Err(LoadError::SourceTooShort {
            path: path.to_path_buf(),
            file_frames,
            source_length,
        });
    }
    let mut mono = mono;
    // `source_length <= mono.len()`, so it fits in usize.
    #[allow(clippy::cast_possible_truncation)]
    mono.truncate(source_length as usize);
    Ok(mono)
}

/// A `&'static str` label for the track node, truncated to keep it short.
fn track_label(name: &str) -> &'static str {
    let mut label = String::from("track:");
    for ch in name.chars().take(48) {
        label.push(ch);
    }
    Box::leak(label.into_boxed_str())
}

/// Renders `job` to a stereo float32 WAV and reports what was written.
///
/// The session is rendered from frame 0 with the transport playing; the
/// default length is the session's own length, so a shorter `frames` trims
/// and a longer one pads with silence.
///
/// # Errors
///
/// Returns what [`build_session_graph`] returns, plus renderer and file
/// errors.
pub fn render_session(job: SessionJob) -> Result<SessionReport, Box<dyn std::error::Error>> {
    if let Some(parent) = job.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let frames = job.frames.unwrap_or_else(|| job.session.length_samples());
    let session_frames = job.session.length_samples();
    let tracks = job.session.tracks.len();
    let sample_rate = job.sample_rate;
    let block_frames = job.block_frames;
    let output = job.output.clone();
    let graph = build_session_graph(job)?;
    let mut transport = Transport::new(sample_rate);
    transport.apply(TransportCommand::Play);
    let renderer = Renderer::new(graph, transport, 1024);
    let mut offline = OfflineRenderer::new(renderer, block_frames);
    let mix = offline.bounce(frames)?;
    audio_core::wav::write_wav_mono(
        &output,
        audio_core::wav::WavSpec {
            sample_rate,
            channels: 2,
        },
        &mix,
    )?;
    Ok(SessionReport {
        frames,
        session_frames,
        tracks,
    })
}

/// A two-track session built in memory for `daw render-session`: a 440 Hz
/// sine for half a second at the start, and a 660 Hz sine for a quarter
/// second starting a quarter of a second in (SPEC-010 §6).
///
/// The sines are generated once, here, on the control thread; the render path
/// only reads them.
///
/// # Errors
///
/// Returns the boxed error of [`audio_core::session::SessionError`] if the
/// built session did not validate — a constant shape, so this only trips if
/// the constants below are edited wrongly.
pub fn build_demo_session(sample_rate: u32) -> Result<SessionJob, Box<dyn std::error::Error>> {
    let half = u64::from(sample_rate) / 2;
    let quarter = u64::from(sample_rate) / 4;

    let low = nodes::MonoClip::sine(440.0, sample_rate, half);
    let high = nodes::MonoClip::sine(660.0, sample_rate, quarter);

    let mut low_track = audio_core::SessionTrack::new("low");
    low_track.pan = -0.5;
    low_track.clips.push(audio_core::Clip::full(0, half));

    let mut high_track = audio_core::SessionTrack::new("high");
    high_track.pan = 0.5;
    high_track
        .clips
        .push(audio_core::Clip::full(quarter, quarter));

    let mut session = audio_core::Session::new("demo", sample_rate, 120.0);
    session.tracks.push(low_track.clone());
    session.tracks.push(high_track.clone());
    session.validate()?;

    Ok(SessionJob {
        tracks: vec![
            TrackSource {
                track: low_track,
                clips: vec![Box::new(low)],
            },
            TrackSource {
                track: high_track,
                clips: vec![Box::new(high)],
            },
        ],
        session,
        sample_rate,
        block_frames: BLOCK_FRAMES,
        frames: None,
        output: PathBuf::from("session.wav"),
    })
}
