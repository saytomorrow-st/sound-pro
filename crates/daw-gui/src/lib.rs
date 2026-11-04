//! Sound Pro desktop editor (SPEC-015 to SPEC-018).
//!
//! [`Editor`] is the window's model: the open project, its file path, the
//! selection and the waveform cache. It knows nothing about drawing, so every
//! action the window offers is tested without a display. The window itself
//! lives in `main.rs` and only calls into this type.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dialog;
pub mod layout;
pub mod quantize;
pub mod timeline;
pub mod track_rows;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use audio_core::session::{
    AutoPoint, Clip, Lane, Note, Session, Track, TrackFolder, TrackFormat, TrackInstrument,
    TrackKind,
};
use audio_core::{
    Bus, BusSend, Effect, EffectKind, MAX_CLIPS_PER_TRACK, MAX_NAME_BYTES, MAX_TEMPO_POINTS,
    Marker, Project, ProjectError, SessionError, TempoPoint,
};

/// Sample rate of a new project.
pub const DEFAULT_RATE: u32 = 48_000;
/// Tempo of a new project.
pub const DEFAULT_TEMPO: f64 = 120.0;
/// Slowest tempo the window allows. The scalar tempo is the first tempo-map
/// point, so it shares the tempo-map range (SPEC-054); it was `20.0` before
/// the map existed.
pub const MIN_TEMPO: f64 = 30.0;
/// Fastest tempo the window allows. See [`MIN_TEMPO`].
pub const MAX_TEMPO: f64 = 300.0;
/// Frames summarised by one [`Peaks`] entry.
pub const PEAK_BLOCK: usize = 256;

/// What happens to a folder's children when the folder is deleted (SPEC-055).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteFolder {
    /// Keep the children, moving them back to the top level.
    KeepChildren,
    /// Delete the children along with the folder.
    DeleteChildren,
}

/// Errors from editor actions. The project is unchanged when one is returned.
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    /// A file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file could not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// File path.
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
        source: ProjectError,
    },
    /// A WAV file could not be decoded.
    #[error("{path}: {source}")]
    Wav {
        /// File path.
        path: PathBuf,
        /// Decoder error.
        source: audio_core::WavError,
    },
    /// A compressed file (MP3, FLAC or OGG Vorbis) could not be decoded.
    #[error("{path}: {message}")]
    Decode {
        /// File path.
        path: PathBuf,
        /// Decoder message.
        message: String,
    },
    /// The file holds no audio.
    #[error("{path}: the file has no audio")]
    EmptyFile {
        /// File path.
        path: PathBuf,
    },
    /// The path cannot be stored in a project document.
    #[error("{path}: the path is not valid UTF-8")]
    NonUtf8Path {
        /// File path.
        path: PathBuf,
    },
    /// The edit would break a session limit.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// There is nothing to export.
    #[error("the project is empty")]
    Empty,
    /// The recording holds no audio (SPEC-020).
    #[error("nothing was recorded")]
    EmptyRecording,
    /// The track or clip index does not exist.
    #[error("no such track or clip")]
    NoClip,
    /// A clip must be at least one sample long (SPEC-021).
    #[error("the clip is empty")]
    EmptyClip,
    /// The clip is not a MIDI clip, the note index is wrong, or the note
    /// breaks a session rule (SPEC-021).
    #[error("invalid note")]
    InvalidNote,
    /// The track or effect index does not exist (SPEC-022).
    #[error("no such track or effect")]
    NoEffect,
    /// The track or automation point index does not exist (SPEC-023).
    #[error("no such track or automation point")]
    NoPoint,
    /// The bus, track or bus effect index does not exist (SPEC-028).
    #[error("no such bus, track or bus effect")]
    NoBus,
    /// A cycle region must not be empty (SPEC-025).
    #[error("the cycle region is empty")]
    EmptyCycle,
    /// The time signature is not supported (SPEC-026).
    #[error("time signature is not supported")]
    InvalidTimeSignature,
    /// A split point must lie strictly inside the clip (SPEC-027).
    #[error("the split point is outside the clip")]
    SplitOutside,
    /// The clip has no next clip, or the two cannot form one region
    /// (SPEC-038).
    #[error("these clips cannot be joined")]
    CannotJoin,
    /// The clip has no neighbour that touches or overlaps it (SPEC-043).
    #[error("no touching clip to crossfade with")]
    CannotCrossfade,
    /// An audio region cannot go on an instrument track or a MIDI region on
    /// an audio track (SPEC-049).
    #[error("the region does not fit this track type")]
    WrongTrackKind,
    /// Nothing has been copied yet (SPEC-027).
    #[error("nothing to paste")]
    EmptyClipboard,
    /// A track name must not be blank (SPEC-029).
    #[error("the name is empty")]
    EmptyName,
    /// The marker index does not exist (SPEC-030).
    #[error("no such marker")]
    NoMarker,
    /// Another marker already sits at that position (SPEC-030).
    #[error("a marker is already there")]
    MarkerExists,
    /// The folder index does not exist (SPEC-055).
    #[error("no such folder")]
    NoFolder,
    /// A track is already inside a folder, or the folder operation would put
    /// a folder inside a folder (SPEC-055).
    #[error("track is already in a folder")]
    TrackInFolder,
    /// The folder has no children, or the track list is empty (SPEC-055).
    #[error("the folder is empty")]
    EmptyFolder,
    /// Loading clip files for the mixdown failed.
    #[error(transparent)]
    Load(#[from] daw_app::LoadError),
    /// Rendering the mixdown failed.
    #[error("render failed: {0}")]
    Render(String),
}

/// Minimum and maximum of each [`PEAK_BLOCK`]-frame block of a mono signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    /// Per-block minimum.
    pub min: Vec<f32>,
    /// Per-block maximum.
    pub max: Vec<f32>,
    /// Frames summarised.
    pub frames: u64,
}

impl Peaks {
    /// Zero placeholder peaks for `frames` so a clip is visible before
    /// decoded data arrives.
    #[must_use]
    pub fn empty(frames: u64) -> Self {
        let blocks = frames.div_ceil(PEAK_BLOCK as u64) as usize;
        Self {
            min: vec![0.0; blocks],
            max: vec![0.0; blocks],
            frames,
        }
    }

    /// Summarises `samples`; a shorter tail forms the last block.
    #[must_use]
    pub fn from_samples(samples: &[f32]) -> Self {
        let mut min = Vec::with_capacity(samples.len().div_ceil(PEAK_BLOCK));
        let mut max = Vec::with_capacity(min.capacity());
        for block in samples.chunks(PEAK_BLOCK) {
            min.push(block.iter().copied().fold(f32::INFINITY, f32::min));
            max.push(block.iter().copied().fold(f32::NEG_INFINITY, f32::max));
        }
        Self {
            min,
            max,
            frames: samples.len() as u64,
        }
    }

    /// Peaks of blocks `first..` of this track's frames from `mono`, the
    /// start of a signal at `from_rate`, mapped onto this track's rate.
    /// Only blocks `mono` fully covers are returned, as `(min, max)` pairs;
    /// call again with the returned count as `first` while decoding.
    #[must_use]
    pub fn blocks_from(
        &self,
        mono: &[f32],
        from_rate: u32,
        to_rate: u32,
        first: usize,
    ) -> Vec<(f32, f32)> {
        let (from_rate, to_rate) = (u128::from(from_rate), u128::from(to_rate.max(1)));
        let source_at = |frame: u64| (u128::from(frame) * from_rate / to_rate) as usize;
        let mut out = Vec::new();
        for block in first..self.min.len() {
            let start = source_at(block as u64 * PEAK_BLOCK as u64);
            let end = source_at(((block as u64 + 1) * PEAK_BLOCK as u64).min(self.frames));
            if end > mono.len() || start >= end {
                break;
            }
            let chunk = &mono[start..end];
            out.push((
                chunk.iter().copied().fold(f32::INFINITY, f32::min),
                chunk.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            ));
        }
        out
    }

    /// Stores `blocks` from block `first` on; extra blocks are ignored.
    pub fn set_blocks(&mut self, first: usize, blocks: &[(f32, f32)]) {
        for (i, &(lo, hi)) in blocks.iter().enumerate() {
            if let (Some(l), Some(h)) = (self.min.get_mut(first + i), self.max.get_mut(first + i)) {
                *l = lo;
                *h = hi;
            }
        }
    }

    /// `(min, max)` over source frames `from..to`, or `None` if the range
    /// holds no block.
    #[must_use]
    pub fn range(&self, from: u64, to: u64) -> Option<(f32, f32)> {
        let block = PEAK_BLOCK as u64;
        let first = usize::try_from(from / block).ok()?;
        let last = usize::try_from(to.div_ceil(block))
            .ok()?
            .min(self.min.len());
        if first >= last {
            return None;
        }
        let lo = self.min[first..last]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let hi = self.max[first..last]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        Some((lo, hi))
    }
}

/// How many edits [`Editor::undo`] can step back.
pub const UNDO_LIMIT: usize = 100;

/// Crossfade length for two clips that only touch, in samples (SPEC-043 §2).
pub const DEFAULT_XFADE: u64 = 960;

/// The window's model: one open project and what the user selected.
#[derive(Debug)]
pub struct Editor {
    project: Project,
    /// The session as it was before `pin_default_colors` filled in colours,
    /// so the next undo step restores it exactly (SPEC-060).
    pre_pin: Option<Session>,
    path: Option<PathBuf>,
    dirty: bool,
    waves: HashMap<String, Peaks>,
    /// Decoded mono files at the project rate, so playback starts without
    /// reading them again (SPEC-035).
    sounds: HashMap<String, Arc<Vec<f32>>>,
    /// Both channels of decoded stereo files (SPEC-050 §5).
    stereo: HashMap<String, daw_app::StereoSamples>,
    /// Waveforms of the left and right channels of stereo files.
    stereo_waves: HashMap<String, (Peaks, Peaks)>,
    /// On-disk decoded file cache.
    pcm_cache: Option<daw_app::pcm_cache::PcmCache>,
    /// Rendered MIDI clips of the last playback or export (SPEC-035).
    rendered: Mutex<RenderedNotes>,
    /// Sampler banks that fell back to the built-in piano in the last load.
    bank_warnings: Mutex<Vec<String>>,
    /// Banks preloaded in the background before switching a track's
    /// instrument, keyed by bank id (SPEC-066). Consulted by [`Self::load`] so
    /// switching to a large bank never blocks the UI on disk I/O.
    preloaded: Mutex<HashMap<String, Arc<daw_app::sample_bank::LoadedBank>>>,
    undo: Vec<Session>,
    redo: Vec<Session>,
    /// The session as last opened or saved.
    saved: Session,
    /// `Some(recorded)` while edits are grouped into one step.
    group: Option<bool>,
    /// Selected track, if any.
    pub selected_track: Option<usize>,
    /// Selected folder (track stack), if any; exclusive with track and clip
    /// selection (SPEC-055 §4).
    pub selected_folder: Option<usize>,
    /// Selected clip as `(track, clip)`, if any; the primary one of a
    /// multiple selection (SPEC-047).
    pub selected_clip: Option<(usize, usize)>,
    /// Clips selected besides [`Self::selected_clip`] (SPEC-047), sorted.
    more_clips: Vec<(usize, usize)>,
    /// Tracks selected besides [`Self::selected_track`] (SPEC-047), sorted.
    more_tracks: Vec<usize>,
    /// Folders selected besides [`Self::selected_folder`] (SPEC-057), sorted.
    more_folders: Vec<usize>,
    /// Cycle region `[start, end)` of the window; not saved (SPEC-025).
    cycle: Option<(u64, u64)>,
    /// Whether playback repeats [`Self::cycle`].
    pub cycle_enabled: bool,
    /// Whether playback clicks on every beat; not saved (SPEC-026).
    pub metronome: bool,
    /// Sampler interpolation used when loading for playback or export; a user
    /// audio setting, not saved in the project.
    pub interpolation: audio_core::Interpolation,
    /// Clip copied with [`Self::copy_clip`]; not saved (SPEC-027).
    clipboard: Option<Clip>,
    /// Tracks armed for recording, sorted and without repeats; not saved
    /// (SPEC-038, SPEC-046 §1).
    armed: Vec<usize>,
    /// Live levels of playback, shared by every graph it plays (SPEC-042).
    meters: daw_app::meters::Meters,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An empty, unsaved project.
    #[must_use]
    pub fn new() -> Self {
        let session = Session::new("untitled", DEFAULT_RATE, DEFAULT_TEMPO);
        Self {
            project: Project {
                name: "untitled".to_owned(),
                sample_rate: DEFAULT_RATE,
                tempo_bpm: DEFAULT_TEMPO,
                params: Vec::new(),
                session: session.clone(),
            },
            path: None,
            dirty: false,
            waves: HashMap::new(),
            sounds: HashMap::new(),
            stereo: HashMap::new(),
            stereo_waves: HashMap::new(),
            pcm_cache: daw_app::pcm_cache::PcmCache::default_location(),
            rendered: Mutex::default(),
            bank_warnings: Mutex::default(),
            preloaded: Mutex::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            saved: session,
            pre_pin: None,
            group: None,
            selected_track: None,
            selected_folder: None,
            selected_clip: None,
            more_clips: Vec::new(),
            more_tracks: Vec::new(),
            more_folders: Vec::new(),
            cycle: None,
            cycle_enabled: false,
            metronome: false,
            interpolation: audio_core::Interpolation::default(),
            armed: Vec::new(),
            clipboard: None,
            meters: daw_app::meters::Meters::new(),
        }
    }

    /// Opens a project document and loads the waveforms its clips refer to.
    ///
    /// Clips whose file is missing or unreadable open without a waveform.
    /// Relative `source` paths become absolute, resolved against the
    /// project's folder.
    ///
    /// # Errors
    ///
    /// [`EditorError::Read`] or [`EditorError::Project`]; `self` is unchanged.
    pub fn open(&mut self, path: &Path) -> Result<(), EditorError> {
        let path = std::path::absolute(path).map_err(|source| EditorError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let text = std::fs::read_to_string(&path).map_err(|source| EditorError::Read {
            path: path.clone(),
            source,
        })?;
        let mut project = Project::from_json(&text).map_err(|source| EditorError::Project {
            path: path.clone(),
            source,
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new(""));
        let mut waves = HashMap::new();
        let mut sounds = HashMap::new();
        let mut stereo = HashMap::new();
        let mut stereo_waves = HashMap::new();
        let rate = project.session.sample_rate;
        for track in &mut project.session.tracks {
            for clip in &mut track.clips {
                let Some(source) = clip.source.as_mut() else {
                    continue;
                };
                let resolved = resolve_source(base, source);
                if let Some(text) = resolved.to_str() {
                    *source = text.to_owned();
                }
                if !waves.contains_key(source.as_str()) {
                    if let Ok(channels) = read_source(&resolved, rate) {
                        let samples = channels.mono();
                        waves.insert(source.clone(), Peaks::from_samples(&samples));
                        sounds.insert(source.clone(), Arc::new(samples));
                        if let Some(right) = channels.right {
                            stereo_waves.insert(
                                source.clone(),
                                (
                                    Peaks::from_samples(&channels.left),
                                    Peaks::from_samples(&right),
                                ),
                            );
                            stereo
                                .insert(source.clone(), (Arc::new(channels.left), Arc::new(right)));
                        }
                    }
                }
            }
        }
        *self = Self {
            saved: project.session.clone(),
            project,
            path: Some(path),
            dirty: false,
            waves,
            sounds,
            stereo,
            stereo_waves,
            pcm_cache: self.pcm_cache.clone(),
            rendered: Mutex::default(),
            bank_warnings: Mutex::default(),
            preloaded: Mutex::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            pre_pin: None,
            group: None,
            selected_track: None,
            selected_folder: None,
            selected_clip: None,
            more_clips: Vec::new(),
            more_tracks: Vec::new(),
            more_folders: Vec::new(),
            cycle: None,
            cycle_enabled: false,
            metronome: self.metronome,
            interpolation: self.interpolation,
            clipboard: None,
            armed: Vec::new(),
            meters: self.meters.clone(),
        };
        Ok(())
    }

    /// Adds a WAV file as a new clip.
    ///
    /// With a selected track the clip goes after that track's last clip;
    /// otherwise a new track named after the file holds it at position 0.
    /// Returns `(track, clip)` of the new clip, which becomes selected.
    ///
    /// # Errors
    ///
    /// File and limit errors (see [`EditorError`]); `self` is unchanged.
    pub fn import_wav(&mut self, path: &Path) -> Result<(usize, usize), EditorError> {
        self.import_audio_at(path, self.selected_track, None)
    }

    /// Puts a WAV or MP3 file on `track` at `start` (SPEC-032 drag and drop).
    ///
    /// `track = None` (or past the last track) makes a new track named
    /// after the file; `start = None` places the clip after the track's last
    /// clip. One undo step; the new clip is selected.
    ///
    /// # Errors
    ///
    /// Same as [`Editor::import_wav`]; nothing changes on error.
    pub fn import_audio_at(
        &mut self,
        path: &Path,
        track: Option<usize>,
        start: Option<u64>,
    ) -> Result<(usize, usize), EditorError> {
        if let Some(channels) = self.cached_source(path) {
            return self.import_channels(path, channels, track, start);
        }
        let channels = read_source(path, self.project.session.sample_rate)?;
        self.import_channels(path, channels, track, start)
    }

    /// The decoded content of `path` if a clip already uses it, so a file
    /// is not decoded and resampled twice (SPEC-050 §6).
    #[must_use]
    pub fn cached_source(&self, path: &Path) -> Option<daw_app::audio_file::Channels> {
        let path = std::path::absolute(path).ok()?;
        let source = path.to_str()?;
        if let Some((left, right)) = self.stereo.get(source) {
            return Some(daw_app::audio_file::Channels {
                left: left.to_vec(),
                right: Some(right.to_vec()),
            });
        }
        self.sounds
            .get(source)
            .map(|mono| daw_app::audio_file::Channels {
                left: mono.to_vec(),
                right: None,
            })
    }

    /// The on-disk cache of decoded files, if the platform has one.
    #[must_use]
    pub fn pcm_cache(&self) -> Option<daw_app::pcm_cache::PcmCache> {
        self.pcm_cache.clone()
    }

    /// [`Editor::import_audio_at`] for a file already decoded with
    /// [`read_samples`] at the project rate, e.g. on a background thread
    /// (SPEC-035).
    ///
    /// # Errors
    ///
    /// As [`Editor::import_audio_at`]; nothing changes on error.
    pub fn import_decoded(
        &mut self,
        path: &Path,
        samples: Vec<f32>,
        track: Option<usize>,
        start: Option<u64>,
    ) -> Result<(usize, usize), EditorError> {
        let channels = daw_app::audio_file::Channels {
            left: samples,
            right: None,
        };
        self.import_channels(path, channels, track, start)
    }

    /// [`Editor::import_decoded`] keeping a stereo file's two channels
    /// (SPEC-050 §5). A new track made for a stereo file is stereo.
    ///
    /// # Errors
    ///
    /// As [`Editor::import_audio_at`]; nothing changes on error.
    pub fn import_channels(
        &mut self,
        path: &Path,
        channels: daw_app::audio_file::Channels,
        track: Option<usize>,
        start: Option<u64>,
    ) -> Result<(usize, usize), EditorError> {
        let samples = channels.mono();
        let path = std::path::absolute(path).map_err(|source| EditorError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let source = path
            .to_str()
            .ok_or_else(|| EditorError::NonUtf8Path { path: path.clone() })?
            .to_owned();
        let peaks = Peaks::from_samples(&samples);
        if peaks.frames == 0 {
            return Err(EditorError::EmptyFile { path });
        }
        let name = clip_name(&path);
        let mut session = self.project.session.clone();
        let track_index = match track.filter(|&t| {
            session
                .tracks
                .get(t)
                .is_some_and(|t| t.kind == TrackKind::Audio)
        }) {
            Some(t) => t,
            None => {
                let mut new = Track::new(&name);
                if channels.right.is_some() {
                    new.format = TrackFormat::Stereo;
                }
                session.tracks.push(new);
                session.tracks.len() - 1
            }
        };
        let track = &mut session.tracks[track_index];
        if track.clips.len() >= MAX_CLIPS_PER_TRACK {
            return Err(SessionError::TooManyClips {
                track: track_index,
                max: MAX_CLIPS_PER_TRACK,
            }
            .into());
        }
        let mut clip = Clip::full(start.unwrap_or(track.length_samples()), peaks.frames);
        clip.name = name;
        clip.source = Some(source.clone());
        track.clips.push(clip);
        let clip_index = track.clips.len() - 1;
        session.validate()?;
        self.record();
        self.project.session = session;
        self.waves.insert(source.clone(), peaks);
        if let Some(right) = channels.right {
            self.stereo_waves.insert(
                source.clone(),
                (
                    Peaks::from_samples(&channels.left),
                    Peaks::from_samples(&right),
                ),
            );
            self.stereo
                .insert(source.clone(), (Arc::new(channels.left), Arc::new(right)));
        }
        self.sounds.insert(source, Arc::new(samples));
        self.selected_track = Some(track_index);
        self.selected_clip = Some((track_index, clip_index));
        self.dirty = true;
        Ok((track_index, clip_index))
    }

    /// Drops audio files on the timeline (SPEC-032).
    ///
    /// Files that are not WAV or MP3 are skipped. The first audio file goes
    /// on `track` at `start` (a new track when `track` is `None`); the rest
    /// follow it on the same track. Returns the number of clips added and
    /// the first error, after which the remaining files are not imported.
    pub fn drop_audio_files(
        &mut self,
        paths: &[PathBuf],
        track: Option<usize>,
        start: u64,
    ) -> (usize, Option<EditorError>) {
        let mut added = 0;
        let mut target = track;
        for path in paths
            .iter()
            .filter(|p| daw_app::audio_file::is_audio_file(p))
        {
            let at = (added == 0).then_some(start);
            match self.import_audio_at(path, target, at) {
                Ok((t, _)) => {
                    target = Some(t);
                    added += 1;
                }
                Err(e) => return (added, Some(e)),
            }
        }
        (added, None)
    }

    /// Adds a microphone take as a clip at `start` (SPEC-020).
    ///
    /// The take is converted to the project rate and written to the first
    /// free `rec-NNN.wav` in `dir`. A clip of it goes on every armed track
    /// (SPEC-046 §1); with none armed, on the selected track, or a new
    /// "Запись" track. One undo step. Returns the first armed track's clip.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyRecording`] for no audio, [`EditorError::Session`]
    /// when a limit would break (nothing is written), [`EditorError::Write`]
    /// or [`EditorError::NonUtf8Path`] for the file.
    pub fn add_recording(
        &mut self,
        samples: &[f32],
        rate: u32,
        start: u64,
        dir: &Path,
    ) -> Result<(usize, usize), EditorError> {
        if samples.is_empty() {
            return Err(EditorError::EmptyRecording);
        }
        let project_rate = self.project.session.sample_rate;
        let audio = if rate == project_rate {
            samples.to_vec()
        } else {
            daw_app::audio_file::resample_parallel(samples, rate, project_rate)
                .map_err(|_| EditorError::EmptyRecording)?
        };
        if audio.is_empty() {
            return Err(EditorError::EmptyRecording);
        }
        let mut session = self.project.session.clone();
        let mut targets: Vec<usize> = self
            .armed_tracks()
            .filter(|&t| session.tracks[t].kind == TrackKind::Audio)
            .collect();
        if targets.is_empty() {
            targets.push(
                match self.selected_track.filter(|&t| {
                    session
                        .tracks
                        .get(t)
                        .is_some_and(|t| t.kind == TrackKind::Audio)
                }) {
                    Some(t) => t,
                    None => {
                        session.tracks.push(Track::new("Запись"));
                        session.tracks.len() - 1
                    }
                },
            );
        }
        let mut clip = Clip::full(start, audio.len() as u64);
        let placed: Vec<(usize, usize)> = targets
            .iter()
            .map(|&t| {
                session.tracks[t].clips.push(clip.clone());
                (t, session.tracks[t].clips.len() - 1)
            })
            .collect();
        let (track_index, clip_index) = placed[0];
        session.validate()?;

        let write_err = |path: &Path, source| EditorError::Write {
            path: path.to_path_buf(),
            source,
        };
        std::fs::create_dir_all(dir).map_err(|e| write_err(dir, e))?;
        let dir = std::path::absolute(dir).map_err(|e| write_err(dir, e))?;
        let path = (1..)
            .map(|n| dir.join(format!("rec-{n:03}.wav")))
            .find(|p| !p.exists())
            .unwrap_or_else(|| dir.join("rec.wav"));
        let source = path
            .to_str()
            .ok_or_else(|| EditorError::NonUtf8Path { path: path.clone() })?
            .to_owned();
        audio_core::wav::write_mono_slice(&path, project_rate, &audio).map_err(|e| {
            EditorError::Write {
                path: path.clone(),
                source: std::io::Error::other(e.to_string()),
            }
        })?;
        clip.name = clip_name(&path);
        clip.source = Some(source.clone());
        for &(t, c) in &placed {
            session.tracks[t].clips[c] = clip.clone();
        }

        self.record();
        self.project.session = session;
        self.waves
            .insert(source.clone(), Peaks::from_samples(&audio));
        self.sounds.insert(source, Arc::new(audio));
        self.selected_track = Some(track_index);
        self.selected_clip = Some((track_index, clip_index));
        self.dirty = true;
        Ok((track_index, clip_index))
    }

    /// Where new recordings go: `<project folder>/audio`, or a temp folder
    /// for an unsaved project.
    #[must_use]
    pub fn recordings_dir(&self) -> PathBuf {
        self.path.as_deref().and_then(Path::parent).map_or_else(
            || std::env::temp_dir().join("sound-pro-recordings"),
            |p| p.join("audio"),
        )
    }

    /// Writes the project to `path`; sources inside its folder become
    /// relative.
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] for an invalid session, [`EditorError::Write`]
    /// for I/O; nothing is written in the first case.
    pub fn save_as(&mut self, path: &Path) -> Result<(), EditorError> {
        let path = std::path::absolute(path).map_err(|source| EditorError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        self.project.session.validate()?;
        let base = path.parent().unwrap_or_else(|| Path::new(""));
        let mut document = self.project.clone();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            document.name = truncate(stem);
            document.session.name = document.name.clone();
        }
        for track in &mut document.session.tracks {
            for clip in &mut track.clips {
                if let Some(source) = clip.source.as_mut() {
                    *source = relative_source(base, source);
                }
            }
        }
        std::fs::write(&path, document.to_json()).map_err(|source| EditorError::Write {
            path: path.clone(),
            source,
        })?;
        self.project.name = document.name;
        self.project.session.name = document.session.name;
        self.saved = self.project.session.clone();
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    /// Mixes the project down to a stereo WAV at `out`.
    ///
    /// # Errors
    ///
    /// [`EditorError::Empty`] for a project with no audio, load or render
    /// errors otherwise.
    pub fn export(&self, out: &Path) -> Result<daw_app::SessionReport, EditorError> {
        if self.project.session.length_samples() == 0 {
            return Err(EditorError::Empty);
        }
        let job = self.load(out.to_path_buf())?;
        daw_app::render_session(job).map_err(|e| EditorError::Render(e.to_string()))
    }

    /// Mixes the project down to a stereo WAV at `out` using `spec` for the
    /// bit depth, target sample rate and optional peak normalisation
    /// (SPEC-052).
    ///
    /// # Errors
    ///
    /// [`EditorError::Empty`] for a project with no audio, load or render
    /// errors otherwise.
    pub fn export_with(
        &self,
        out: &Path,
        spec: &daw_app::ExportSpec,
    ) -> Result<daw_app::SessionReport, EditorError> {
        let job = self.prepare_export(out)?;
        daw_app::render_export(job, *spec).map_err(|e| EditorError::Render(e.to_string()))
    }

    /// Builds the render job for a mix export to `out` without rendering it,
    /// so the caller can run [`daw_app::render_export`] off the UI thread
    /// (SPEC-052).
    ///
    /// # Errors
    ///
    /// [`EditorError::Empty`] for a project with no audio, load errors
    /// otherwise.
    pub fn prepare_export(&self, out: &Path) -> Result<daw_app::SessionJob, EditorError> {
        if self.project.session.length_samples() == 0 {
            return Err(EditorError::Empty);
        }
        self.load(out.to_path_buf())
    }

    /// Loads the session for rendering from the decoded files and rendered
    /// notes this editor already holds (SPEC-035).
    fn load(&self, output: PathBuf) -> Result<daw_app::SessionJob, EditorError> {
        let mut rendered = self
            .rendered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut cache = EditorCache {
            sounds: &self.sounds,
            stereo: &self.stereo,
            rendered: &mut rendered,
            used: HashMap::new(),
        };
        let (job, warnings) = daw_app::load_session_reporting_with_preloaded(
            self.project.session.clone(),
            Path::new(""),
            output,
            &mut cache,
            self.interpolation,
            &self
                .preloaded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )?;
        let used = cache.used;
        rendered.0 = used;
        *self
            .bank_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = warnings;
        Ok(job)
    }

    /// Takes the warnings of the last load: one per sampler bank that could
    /// not be loaded and was replaced by the built-in piano.
    pub fn take_bank_warnings(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .bank_warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Stores a bank that was decoded in the background (SPEC-066), so the next
    /// [`Self::load`] builds its sampler instrument from memory instead of
    /// re-reading the SFZ and samples from disk.
    pub fn insert_preloaded_bank(
        &self,
        bank_id: String,
        bank: Arc<daw_app::sample_bank::LoadedBank>,
    ) {
        self.preloaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(bank_id, bank);
    }

    /// Builds a stopped playback pair for the current project, positioned
    /// at `start` — or at 0 when `start` is at or past the end (SPEC-016).
    ///
    /// # Errors
    ///
    /// [`EditorError::Empty`] for a project with no audio, load or graph
    /// errors otherwise.
    pub fn playback(
        &self,
        start: u64,
    ) -> Result<
        (
            daw_app::playback::PlaybackControl,
            daw_app::playback::PlaybackEngine,
        ),
        EditorError,
    > {
        let end = self.project.session.length_samples();
        if end == 0 {
            return Err(EditorError::Empty);
        }
        let (graph, rate) = self.playback_graph()?;
        let mut start = if start >= end { 0 } else { start };
        let cycle = self.active_cycle();
        if let Some((from, to)) = cycle
            && (start < from || start >= to)
        {
            start = from.min(end);
        }
        let (mut control, engine) = daw_app::playback::playback(graph, rate, start, end);
        let engine = engine.with_meters(self.meters.clone());
        control.set_cycle(cycle);
        control.set_metronome(self.metronome);
        let session = &self.project.session;
        control.set_meter(
            session.tempo_bpm,
            u32::from(session.time_signature.numerator),
        );
        Ok((control, engine))
    }

    /// The sound of the current project for a running player, to swap in
    /// with [`daw_app::playback::PlaybackControl::replace_graph`] after an
    /// edit (SPEC-035), and its sample rate.
    ///
    /// # Errors
    ///
    /// Load or graph errors.
    pub fn playback_graph(&self) -> Result<(audio_core::Graph, u32), EditorError> {
        let mut job = self.load(PathBuf::new())?;
        job.block_frames = daw_app::playback::PLAYBACK_BLOCK;
        let rate = job.sample_rate;
        let graph = daw_app::build_session_graph_metered(job, Some(&self.meters))
            .map_err(|e| EditorError::Render(e.to_string()))?;
        Ok((graph, rate))
    }

    /// Live levels of playback: track `i` of the session is meter `i`
    /// (SPEC-042).
    #[must_use]
    pub fn meters(&self) -> &daw_app::meters::Meters {
        &self.meters
    }

    /// The cycle region, even while it is turned off (SPEC-025).
    #[must_use]
    pub const fn cycle(&self) -> Option<(u64, u64)> {
        self.cycle
    }

    /// The cycle region if it is turned on.
    #[must_use]
    pub fn active_cycle(&self) -> Option<(u64, u64)> {
        self.cycle.filter(|_| self.cycle_enabled)
    }

    /// Sets the cycle region (ends in any order) and turns it on.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyCycle`] when `start == end`; the previous region
    /// is kept.
    pub fn set_cycle(&mut self, start: u64, end: u64) -> Result<(), EditorError> {
        if start == end {
            return Err(EditorError::EmptyCycle);
        }
        self.cycle = Some((start.min(end), start.max(end)));
        self.cycle_enabled = true;
        Ok(())
    }

    /// Removes the cycle region and turns the cycle off.
    pub const fn clear_cycle(&mut self) {
        self.cycle = None;
        self.cycle_enabled = false;
    }

    /// Removes one clip; out-of-range indices do nothing.
    pub fn delete_clip(&mut self, track: usize, clip: usize) {
        let Some(t) = self.project.session.tracks.get_mut(track) else {
            return;
        };
        if clip < t.clips.len() {
            self.record();
            self.project.session.tracks[track].clips.remove(clip);
            self.selected_clip = None;
            self.more_clips.clear();
            self.dirty = true;
        }
    }

    /// Places a clip at `start` on `to_track`. On the same track the clip
    /// keeps its index; on another one it goes to the end of its list.
    /// Returns the new `(track, clip)`, which becomes selected.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::Session`] for
    /// a full track or a timeline overflow; `self` is unchanged.
    pub fn move_clip(
        &mut self,
        track: usize,
        clip: usize,
        start: u64,
        to_track: usize,
    ) -> Result<(usize, usize), EditorError> {
        self.clip_ref(track, clip)?;
        if to_track >= self.project.session.tracks.len() {
            return Err(EditorError::NoClip);
        }
        let mut session = self.project.session.clone();
        let at = if to_track == track {
            session.tracks[track].clips[clip].start_samples = start;
            (track, clip)
        } else {
            let target = &session.tracks[to_track];
            if !target.kind.accepts(&session.tracks[track].clips[clip]) {
                return Err(EditorError::WrongTrackKind);
            }
            if target.clips.len() >= MAX_CLIPS_PER_TRACK {
                return Err(SessionError::TooManyClips {
                    track: to_track,
                    max: MAX_CLIPS_PER_TRACK,
                }
                .into());
            }
            let mut moved = session.tracks[track].clips.remove(clip);
            moved.start_samples = start;
            session.tracks[to_track].clips.push(moved);
            (to_track, session.tracks[to_track].clips.len() - 1)
        };
        self.commit(session, at)?;
        Ok(at)
    }

    /// Moves a clip's left edge to `start`, keeping its right edge. The edge
    /// stops at the start of the audio and one sample before the right edge.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; `self` is unchanged.
    pub fn trim_start(&mut self, track: usize, clip: usize, start: u64) -> Result<(), EditorError> {
        let c = self.clip_ref(track, clip)?;
        let right = c.start_samples + c.length_samples;
        let earliest = c.start_samples.saturating_sub(c.offset_samples);
        let start = start.clamp(earliest, right - 1);
        // `start >= earliest` keeps the new offset non-negative.
        let offset = c.offset_samples + start - c.start_samples;
        let mut session = self.project.session.clone();
        let edited = &mut session.tracks[track].clips[clip];
        edited.start_samples = start;
        edited.offset_samples = offset;
        edited.length_samples = right - start;
        fit_fades(edited);
        self.commit(session, (track, clip))
    }

    /// Moves a clip's right edge to `end`, keeping its left edge. The length
    /// stays between one sample and the rest of the audio.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; `self` is unchanged.
    pub fn trim_end(&mut self, track: usize, clip: usize, end: u64) -> Result<(), EditorError> {
        let c = self.clip_ref(track, clip)?;
        let longest = c.source_length - c.offset_samples;
        let length = end.saturating_sub(c.start_samples).clamp(1, longest);
        let mut session = self.project.session.clone();
        let edited = &mut session.tracks[track].clips[clip];
        edited.length_samples = length;
        fit_fades(edited);
        self.commit(session, (track, clip))
    }

    /// Cuts a clip at timeline position `at` (SPEC-027). The left part keeps
    /// its index; the right part goes right after it and becomes selected.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::SplitOutside`]
    /// unless `start < at < end`, [`EditorError::Session`] for a full track;
    /// `self` is unchanged.
    pub fn split_clip(
        &mut self,
        track: usize,
        clip: usize,
        at: u64,
    ) -> Result<(usize, usize), EditorError> {
        let c = self.clip_ref(track, clip)?;
        if at <= c.start_samples || at >= c.end_samples() {
            return Err(EditorError::SplitOutside);
        }
        let head = at - c.start_samples;
        let mut right = c.clone();
        right.start_samples = at;
        right.offset_samples += head;
        right.length_samples -= head;
        right.fade_in_samples = 0;
        right.fade_in_curve = 0.0;
        right.fade_out_samples = c.fade_out_samples.min(right.length_samples);
        let mut session = self.project.session.clone();
        let left = &mut session.tracks[track].clips[clip];
        left.length_samples = head;
        left.fade_in_samples = left.fade_in_samples.min(head);
        left.fade_out_samples = 0;
        left.fade_out_curve = 0.0;
        session.tracks[track].clips.insert(clip + 1, right);
        let at = (track, clip + 1);
        self.commit(session, at)?;
        Ok(at)
    }

    /// Sets a clip's fades, each limited so that both fit in the clip
    /// (SPEC-038 §6). One undo step; the clip becomes selected.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; `self` is unchanged.
    pub fn set_fades(
        &mut self,
        track: usize,
        clip: usize,
        fade_in: u64,
        fade_out: u64,
    ) -> Result<(), EditorError> {
        let c = self.clip_ref(track, clip)?;
        let length = c.length_samples;
        let fade_in = fade_in.min(length);
        let fade_out = fade_out.min(length - fade_in);
        if (c.fade_in_samples, c.fade_out_samples) == (fade_in, fade_out) {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        let c = &mut session.tracks[track].clips[clip];
        c.fade_in_samples = fade_in;
        c.fade_out_samples = fade_out;
        self.commit(session, (track, clip))
    }

    /// Sets the shapes of a clip's fades, each limited to `-1.0..=1.0`; NaN
    /// counts as linear (SPEC-045 §1). One undo step unless unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; `self` is unchanged.
    pub fn set_fade_curves(
        &mut self,
        track: usize,
        clip: usize,
        fade_in: f32,
        fade_out: f32,
    ) -> Result<(), EditorError> {
        let c = self.clip_ref(track, clip)?;
        let limit = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) };
        let (fade_in, fade_out) = (limit(fade_in), limit(fade_out));
        if c.fade_in_curve.to_bits() == fade_in.to_bits()
            && c.fade_out_curve.to_bits() == fade_out.to_bits()
        {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        let c = &mut session.tracks[track].clips[clip];
        c.fade_in_curve = fade_in;
        c.fade_out_curve = fade_out;
        self.commit(session, (track, clip))
    }

    /// Sets a clip's gain, limited to `0.0..=1.0`; NaN counts as 0
    /// (SPEC-039 §13). One undo step unless the value is unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; `self` is unchanged.
    pub fn set_clip_gain(
        &mut self,
        track: usize,
        clip: usize,
        gain: f32,
    ) -> Result<(), EditorError> {
        let c = self.clip_ref(track, clip)?;
        let gain = if gain.is_nan() {
            0.0
        } else {
            gain.clamp(0.0, 1.0)
        };
        if c.gain.to_bits() == gain.to_bits() {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.tracks[track].clips[clip].gain = gain;
        self.commit(session, (track, clip))
    }

    /// Sets the gain of several clips at once, limited like
    /// [`Self::set_clip_gain`] (SPEC-048 §4). One undo step; bad indices are
    /// skipped, the selection is kept.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] when no index is valid; `self` is unchanged.
    pub fn set_clips_gain(
        &mut self,
        clips: &[(usize, usize)],
        gain: f32,
    ) -> Result<(), EditorError> {
        let gain = if gain.is_nan() {
            0.0
        } else {
            gain.clamp(0.0, 1.0)
        };
        let mut session = self.project.session.clone();
        let mut found = false;
        let mut changed = false;
        for &(t, c) in clips {
            if let Some(clip) = session.tracks.get_mut(t).and_then(|tr| tr.clips.get_mut(c)) {
                found = true;
                if clip.gain.to_bits() != gain.to_bits() {
                    clip.gain = gain;
                    changed = true;
                }
            }
        }
        if !found {
            return Err(EditorError::NoClip);
        }
        if changed {
            self.commit_session(session)?;
        }
        Ok(())
    }

    /// Renames a clip; surrounding spaces are trimmed (SPEC-048 §5). One
    /// undo step unless the name is unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::EmptyName`]
    /// for a blank name, [`EditorError::Session`] for a too long one.
    pub fn rename_clip(
        &mut self,
        track: usize,
        clip: usize,
        name: &str,
    ) -> Result<(), EditorError> {
        let name = name.trim();
        let current = &self.clip_ref(track, clip)?.name;
        if name.is_empty() {
            return Err(EditorError::EmptyName);
        }
        if current == name {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        name.clone_into(&mut session.tracks[track].clips[clip].name);
        self.commit(session, (track, clip))
    }

    /// Crossfades a clip with the neighbour it touches or overlaps
    /// (SPEC-043 §2): the earlier clip fades out and the later one fades in
    /// across the overlap, or across [`DEFAULT_XFADE`] when they only touch.
    /// One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices,
    /// [`EditorError::CannotCrossfade`] when no neighbour touches the clip;
    /// `self` is unchanged.
    pub fn crossfade(&mut self, track: usize, clip: usize) -> Result<(), EditorError> {
        self.clip_ref(track, clip)?;
        let clips = &self.project.session.tracks[track].clips;
        let touches =
            |early: usize, late: usize| clips[late].start_samples <= clips[early].end_samples();
        let pair = self
            .next_clip(track, clip)
            .filter(|&next| touches(clip, next))
            .map(|next| (clip, next))
            .or_else(|| {
                (0..clips.len())
                    .find(|&i| i != clip && self.next_clip(track, i) == Some(clip))
                    .filter(|&prev| touches(prev, clip))
                    .map(|prev| (prev, clip))
            })
            .ok_or(EditorError::CannotCrossfade)?;
        let (early, late) = (&clips[pair.0], &clips[pair.1]);
        let overlap = early.end_samples() - late.start_samples;
        let length = if overlap == 0 { DEFAULT_XFADE } else { overlap }
            .min(early.length_samples)
            .min(late.length_samples);
        let mut session = self.project.session.clone();
        let early = &mut session.tracks[track].clips[pair.0];
        early.fade_out_samples = length;
        early.fade_in_samples = early.fade_in_samples.min(early.length_samples - length);
        let late = &mut session.tracks[track].clips[pair.1];
        late.fade_in_samples = length;
        late.fade_out_samples = late.fade_out_samples.min(late.length_samples - length);
        self.commit(session, (track, clip))
    }

    /// Moves every note start of a MIDI clip to the nearest `1/division`
    /// note of the project grid (SPEC-043 §3); `division` is 4, 8, 16 or 32.
    /// Notes stay inside the clip's data. One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or another division; `self` is unchanged.
    pub fn quantize_notes(
        &mut self,
        track: usize,
        clip: usize,
        division: u32,
    ) -> Result<(), EditorError> {
        if ![4, 8, 16, 32].contains(&division) {
            return Err(EditorError::InvalidNote);
        }
        let session = &self.project.session;
        let map = session
            .build_tempo_map(session.sample_rate)
            .unwrap_or_else(|_| {
                audio_core::TempoMap::single_or_default(session.tempo_bpm, session.sample_rate)
            });
        let step = (sixteenth_samples(&map) * 16 / u64::from(division)).max(1);
        let c = self.clip_ref(track, clip)?;
        let origin = i128::from(c.start_samples) - i128::from(c.offset_samples);
        let last = c.source_length.saturating_sub(1);
        let source_length = c.source_length;
        self.map_notes(track, clip, |n| {
            let at = origin + i128::from(n.start);
            let step = i128::from(step);
            let snapped = (at + step / 2).div_euclid(step) * step - origin;
            let start = u64::try_from(snapped.max(0)).unwrap_or(0).min(last);
            Note {
                start,
                length: n.length.min(source_length - start).max(1),
                ..n
            }
        })
    }

    /// Shifts every note of a MIDI clip by `semitones` (`-96..=96`), clamped
    /// to the MIDI range (SPEC-043 §3). One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or a shift out of range; `self` is unchanged.
    pub fn transpose_notes(
        &mut self,
        track: usize,
        clip: usize,
        semitones: i32,
    ) -> Result<(), EditorError> {
        if !(-96..=96).contains(&semitones) {
            return Err(EditorError::InvalidNote);
        }
        self.map_notes(track, clip, |n| Note {
            pitch: u8::try_from((i32::from(n.pitch) + semitones).clamp(0, 127)).unwrap_or(0),
            ..n
        })
    }

    /// Adds `delta` (`-99..=99`) to every note velocity of a MIDI clip,
    /// clamped to `1..=127` (SPEC-043 §3). One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or a delta out of range; `self` is unchanged.
    pub fn offset_velocity(
        &mut self,
        track: usize,
        clip: usize,
        delta: i32,
    ) -> Result<(), EditorError> {
        if !(-99..=99).contains(&delta) {
            return Err(EditorError::InvalidNote);
        }
        self.map_notes(track, clip, |n| Note {
            velocity: u8::try_from((i32::from(n.velocity) + delta).clamp(1, 127)).unwrap_or(1),
            ..n
        })
    }

    /// Quantizes the selected notes of a MIDI clip to a musical grid
    /// (SPEC-061 §1). `indices` lists which notes to move; when it is empty
    /// every note in the clip is quantized. One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or a bad note index; `self` is unchanged.
    pub fn quantize_selection(
        &mut self,
        track: usize,
        clip: usize,
        indices: &[usize],
        params: quantize::QuantizeParams,
    ) -> Result<(), EditorError> {
        let session = &self.project.session;
        let map = session
            .build_tempo_map(session.sample_rate)
            .unwrap_or_else(|_| {
                audio_core::TempoMap::single_or_default(session.tempo_bpm, session.sample_rate)
            });
        let c = self.clip_ref(track, clip)?;
        let source_length = c.source_length;
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        let all = indices.is_empty();
        for (i, note) in notes.iter_mut().enumerate() {
            if !all && !indices.contains(&i) {
                continue;
            }
            let (start, length) =
                quantize::quantize_note(note.start, note.length, source_length, &map, params);
            note.start = start;
            note.length = length;
        }
        notes.sort_by_key(|n| (n.start, n.pitch));
        if session == self.project.session {
            return Ok(());
        }
        self.commit_notes(session, (track, clip))
    }

    /// Sets one note's velocity to `velocity` (`1..=127`) (SPEC-061 §2).
    /// One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip, a bad note index or a velocity out of range; `self`
    /// is unchanged.
    pub fn set_note_velocity(
        &mut self,
        track: usize,
        clip: usize,
        index: usize,
        velocity: u8,
    ) -> Result<(), EditorError> {
        if !(1..=127).contains(&velocity) {
            return Err(EditorError::InvalidNote);
        }
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        if index >= notes.len() {
            return Err(EditorError::InvalidNote);
        }
        notes[index].velocity = velocity;
        self.commit_notes(session, (track, clip))
    }

    /// Sets the velocity of several notes in one undo step (SPEC-061 §2).
    /// `pairs` holds `(note_index, velocity)`; every velocity must be in
    /// `1..=127`. Notes not listed are left unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip, a bad note index or a velocity out of range; `self`
    /// is unchanged.
    pub fn set_notes_velocity(
        &mut self,
        track: usize,
        clip: usize,
        pairs: &[(usize, u8)],
    ) -> Result<(), EditorError> {
        if pairs.iter().any(|&(_, v)| !(1..=127).contains(&v)) {
            return Err(EditorError::InvalidNote);
        }
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        for &(index, velocity) in pairs {
            if index >= notes.len() {
                return Err(EditorError::InvalidNote);
            }
            notes[index].velocity = velocity;
        }
        if session == self.project.session {
            return Ok(());
        }
        self.commit_notes(session, (track, clip))
    }

    /// Scales every note length of a MIDI clip to `percent` (`25..=200`),
    /// at least one sample and no further than the clip's data (SPEC-043 §3).
    /// One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or a percentage out of range; `self` is unchanged.
    pub fn scale_gate(
        &mut self,
        track: usize,
        clip: usize,
        percent: u32,
    ) -> Result<(), EditorError> {
        if !(25..=200).contains(&percent) {
            return Err(EditorError::InvalidNote);
        }
        let source_length = self.clip_ref(track, clip)?.source_length;
        self.map_notes(track, clip, |n| Note {
            length: ((n.length * u64::from(percent) + 50) / 100)
                .min(source_length - n.start)
                .max(1),
            ..n
        })
    }

    fn map_notes(
        &mut self,
        track: usize,
        clip: usize,
        f: impl Fn(Note) -> Note,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        for note in notes.iter_mut() {
            *note = f(*note);
        }
        notes.sort_by_key(|n| (n.start, n.pitch));
        if session == self.project.session {
            return Ok(());
        }
        self.commit_notes(session, (track, clip))
    }

    /// Replaces a track's notes text (SPEC-043 §6). One undo step unless
    /// grouped or unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index, [`EditorError::Session`] when
    /// the text is longer than [`audio_core::MAX_NOTES_BYTES`].
    pub fn set_track_notes(&mut self, track: usize, text: &str) -> Result<(), EditorError> {
        let current = &self
            .project
            .session
            .tracks
            .get(track)
            .ok_or(EditorError::NoClip)?
            .notes;
        if current == text {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.tracks[track].notes = text.to_owned();
        self.commit_track(session, track)
    }

    /// Replaces the project notes text (SPEC-043 §6).
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] when the text is longer than
    /// [`audio_core::MAX_NOTES_BYTES`].
    pub fn set_project_notes(&mut self, text: &str) -> Result<(), EditorError> {
        if self.project.session.notes == text {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.notes = text.to_owned();
        session.validate()?;
        self.record();
        self.project.session = session;
        self.dirty = true;
        Ok(())
    }

    /// Index of the clip that starts next after `clip` on its track.
    fn next_clip(&self, track: usize, clip: usize) -> Option<usize> {
        let clips = &self.project.session.tracks.get(track)?.clips;
        let first = clips.get(clip)?;
        clips
            .iter()
            .enumerate()
            .filter(|&(i, c)| {
                i != clip
                    && (c.start_samples > first.start_samples
                        || (c.start_samples == first.start_samples && i > clip))
            })
            .min_by_key(|&(i, c)| (c.start_samples, i))
            .map(|(i, _)| i)
    }

    /// Joins a clip with the next clip on its track into one region
    /// (SPEC-038 §5). Returns the joined clip's `(track, clip)`, which
    /// becomes selected. One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::CannotJoin`]
    /// when there is no next clip or the two do not continue one another;
    /// `self` is unchanged.
    pub fn join_clips(&mut self, track: usize, clip: usize) -> Result<(usize, usize), EditorError> {
        let a = self.clip_ref(track, clip)?.clone();
        let next = self.next_clip(track, clip).ok_or(EditorError::CannotJoin)?;
        let b = self.project.session.tracks[track].clips[next].clone();
        let joined = match (&a.notes, &b.notes) {
            (Some(first), Some(second)) => {
                let length = b
                    .end_samples()
                    .max(a.end_samples())
                    .checked_sub(a.start_samples)
                    .ok_or(EditorError::CannotJoin)?;
                let mut notes = visible_notes(first, &a, 0);
                notes.extend(visible_notes(second, &b, b.start_samples - a.start_samples));
                notes.sort_by_key(|n| (n.start, n.pitch));
                Clip {
                    length_samples: length,
                    offset_samples: 0,
                    source_length: length,
                    notes: Some(notes),
                    fade_out_samples: b.fade_out_samples,
                    fade_out_curve: b.fade_out_curve,
                    ..a.clone()
                }
            }
            (None, None)
                if a.source.is_some()
                    && a.source == b.source
                    && a.gain.to_bits() == b.gain.to_bits()
                    && b.start_samples == a.end_samples()
                    && b.offset_samples == a.offset_samples + a.length_samples =>
            {
                Clip {
                    length_samples: a.length_samples + b.length_samples,
                    fade_out_samples: b.fade_out_samples,
                    fade_out_curve: b.fade_out_curve,
                    ..a.clone()
                }
            }
            _ => return Err(EditorError::CannotJoin),
        };
        let mut joined = joined;
        fit_fades(&mut joined);
        let mut session = self.project.session.clone();
        let clips = &mut session.tracks[track].clips;
        clips[clip] = joined;
        clips.remove(next);
        let at = (track, if next < clip { clip - 1 } else { clip });
        self.commit(session, at)?;
        Ok(at)
    }

    /// The first track armed for recording, if any (SPEC-038 §9).
    #[must_use]
    pub fn armed_track(&self) -> Option<usize> {
        self.armed_tracks().next()
    }

    /// Every track armed for recording, top to bottom (SPEC-046 §1).
    pub fn armed_tracks(&self) -> impl Iterator<Item = usize> + '_ {
        let tracks = self.project.session.tracks.len();
        self.armed.iter().copied().filter(move |&t| t < tracks)
    }

    /// Whether `track` is armed for recording.
    #[must_use]
    pub fn is_armed(&self, track: usize) -> bool {
        self.armed_tracks().any(|t| t == track)
    }

    /// Arms exactly one track for recording, or none; the project is
    /// untouched.
    pub fn arm_track(&mut self, track: Option<usize>) {
        self.armed.clear();
        if let Some(t) = track.filter(|&t| t < self.project.session.tracks.len()) {
            self.armed.push(t);
        }
    }

    /// Arms or disarms one track and leaves the others as they are, so
    /// several tracks can be armed at once, as in Logic (SPEC-046 §1).
    /// Bad indices do nothing; the project is untouched.
    pub fn set_armed(&mut self, track: usize, on: bool) {
        if track >= self.project.session.tracks.len() {
            return;
        }
        match (self.armed.binary_search(&track), on) {
            (Err(at), true) => self.armed.insert(at, track),
            (Ok(at), false) => {
                self.armed.remove(at);
            }
            _ => {}
        }
    }

    /// Every selected clip, primary included, sorted by `(track, clip)`
    /// (SPEC-047 §1).
    #[must_use]
    pub fn selected_clips(&self) -> Vec<(usize, usize)> {
        let mut all = self.more_clips.clone();
        if let Some(at) = self.selected_clip {
            if let Err(i) = all.binary_search(&at) {
                all.insert(i, at);
            }
        }
        let tracks = &self.project.session.tracks;
        all.retain(|&(t, c)| tracks.get(t).is_some_and(|tr| c < tr.clips.len()));
        all
    }

    /// Whether the clip at `(track, clip)` is part of the selection.
    #[must_use]
    pub fn is_clip_selected(&self, at: (usize, usize)) -> bool {
        self.selected_clip == Some(at) || self.more_clips.binary_search(&at).is_ok()
    }

    /// Selects one clip. Without `toggle` it replaces the selection (`None`
    /// clears it); with `toggle` (Shift- or Ctrl-click) it adds the clip or,
    /// if it was selected, removes it (SPEC-047 §1). Bad indices do nothing.
    pub fn select_clip(&mut self, at: Option<(usize, usize)>, toggle: bool) {
        let valid = |at: (usize, usize)| {
            self.project
                .session
                .tracks
                .get(at.0)
                .is_some_and(|t| at.1 < t.clips.len())
        };
        if at.is_some_and(|at| !valid(at)) {
            return;
        }
        if !toggle {
            self.more_clips.clear();
            self.more_tracks.clear();
            self.selected_clip = at;
            self.selected_folder = None;
            self.more_folders.clear();
            if let Some((t, _)) = at {
                self.selected_track = Some(t);
            }
            return;
        }
        let Some(at) = at else { return };
        if self.is_clip_selected(at) {
            if self.selected_clip == Some(at) {
                self.selected_clip =
                    (!self.more_clips.is_empty()).then(|| self.more_clips.remove(0));
            } else if let Ok(i) = self.more_clips.binary_search(&at) {
                self.more_clips.remove(i);
            }
            return;
        }
        if let Some(old) = self.selected_clip {
            if let Err(i) = self.more_clips.binary_search(&old) {
                self.more_clips.insert(i, old);
            }
        }
        self.selected_clip = Some(at);
        self.selected_track = Some(at.0);
    }

    /// Makes a selected clip the primary one, keeping the rest of the
    /// selection, so a drag on any clip of a group moves the group. Clips
    /// outside the selection do nothing.
    pub fn focus_clip(&mut self, at: (usize, usize)) {
        let Ok(i) = self.more_clips.binary_search(&at) else {
            return;
        };
        self.more_clips.remove(i);
        if let Some(old) = self.selected_clip.replace(at) {
            if let Err(i) = self.more_clips.binary_search(&old) {
                self.more_clips.insert(i, old);
            }
        }
        self.selected_track = Some(at.0);
    }

    /// Selects every clip on tracks `first..=last` that overlaps samples
    /// `from..to`: the rubber band of the arrange window (SPEC-047 §2). With
    /// `additive` the clips join the current selection. Returns how many
    /// clips the band touched.
    pub fn select_clips_in(
        &mut self,
        tracks: std::ops::RangeInclusive<usize>,
        from: u64,
        to: u64,
        additive: bool,
    ) -> usize {
        let mut indices = Vec::new();
        for t in tracks {
            indices.push(t);
        }
        self.select_clips_in_any(&indices, from, to, additive)
    }

    /// Selects every clip on the given tracks that overlaps samples
    /// `from..to`. The rubber band may cover a non-contiguous set of visible
    /// tracks once folders hide children (SPEC-055 §4), so this accepts an
    /// explicit index list rather than a range.
    pub fn select_clips_in_any(
        &mut self,
        tracks: &[usize],
        from: u64,
        to: u64,
        additive: bool,
    ) -> usize {
        let (from, to) = (from.min(to), from.max(to));
        let mut hits = Vec::new();
        for &t in tracks {
            if t >= self.project.session.tracks.len() {
                continue;
            }
            let track = &self.project.session.tracks[t];
            for (c, clip) in track.clips.iter().enumerate() {
                let end = clip.start_samples.saturating_add(clip.length_samples);
                if clip.start_samples < to.max(from + 1) && end > from {
                    hits.push((t, c));
                }
            }
        }
        if !additive {
            self.more_clips.clear();
            self.selected_clip = None;
        }
        let count = hits.len();
        for at in hits {
            if !self.is_clip_selected(at) {
                self.select_clip(Some(at), true);
            }
        }
        count
    }

    /// Selects every clip of the session (Ctrl+A, SPEC-047 §6).
    pub fn select_all_clips(&mut self) {
        let last = self.project.session.tracks.len().saturating_sub(1);
        self.select_clips_in(0..=last, 0, u64::MAX, false);
    }

    /// Removes every selected clip as one undo step and clears the
    /// selection (SPEC-047 §3). Returns how many clips were removed.
    pub fn delete_selected_clips(&mut self) -> usize {
        let all = self.selected_clips();
        if all.is_empty() {
            return 0;
        }
        self.record();
        // Back to front, so the remaining indices stay valid.
        for &(t, c) in all.iter().rev() {
            self.project.session.tracks[t].clips.remove(c);
        }
        self.selected_clip = None;
        self.more_clips.clear();
        self.dirty = true;
        all.len()
    }

    /// Moves every selected clip by `delta` samples and `delta_tracks` rows
    /// as one edit, keeping their relative places (SPEC-047 §4). The shift
    /// stops when the earliest clip reaches zero or a clip reaches the first
    /// or last track. Clips that change track go to the end of the new
    /// track's list; the selection follows them.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] when nothing is selected,
    /// [`EditorError::Session`] for a full track or a timeline overflow;
    /// `self` is unchanged.
    pub fn move_selected_clips(
        &mut self,
        delta: i64,
        delta_tracks: isize,
    ) -> Result<(), EditorError> {
        let all = self.selected_clips();
        if all.is_empty() {
            return Err(EditorError::NoClip);
        }
        let mut session = self.project.session.clone();
        let earliest = all
            .iter()
            .map(|&(t, c)| session.tracks[t].clips[c].start_samples)
            .min()
            .unwrap_or(0);
        let delta = delta.max(-i64::try_from(earliest).unwrap_or(i64::MAX));
        let count = session.tracks.len();
        let top = all.first().map_or(0, |a| a.0);
        let bottom = all.iter().map(|a| a.0).max().unwrap_or(0);
        let delta_tracks = delta_tracks
            .max(-isize::try_from(top).unwrap_or(isize::MAX))
            .min(isize::try_from(count - 1 - bottom).unwrap_or(0));
        if delta == 0 && delta_tracks == 0 {
            return Ok(());
        }
        let shift = |start: u64| start.checked_add_signed(delta).ok_or(EditorError::NoClip);
        let primary = self.selected_clip;
        let mut placed = Vec::with_capacity(all.len());
        let mut new_primary = None;
        if delta_tracks == 0 {
            for &(t, c) in &all {
                let clip = &mut session.tracks[t].clips[c];
                clip.start_samples = shift(clip.start_samples)?;
                placed.push((t, c));
            }
            new_primary = primary;
        } else {
            let mut moved = Vec::with_capacity(all.len());
            for &(t, c) in all.iter().rev() {
                let mut clip = session.tracks[t].clips.remove(c);
                clip.start_samples = shift(clip.start_samples)?;
                moved.push(((t, c), clip));
            }
            for ((t, c), clip) in moved.into_iter().rev() {
                let to = t.saturating_add_signed(delta_tracks);
                if !session.tracks[to].kind.accepts(&clip) {
                    return Err(EditorError::WrongTrackKind);
                }
                session.tracks[to].clips.push(clip);
                let at = (to, session.tracks[to].clips.len() - 1);
                if primary == Some((t, c)) {
                    new_primary = Some(at);
                }
                placed.push(at);
            }
        }
        session.validate()?;
        self.record();
        self.project.session = session;
        placed.sort_unstable();
        self.selected_clip = new_primary.or_else(|| placed.first().copied());
        if let Some((t, _)) = self.selected_clip {
            self.selected_track = Some(t);
        }
        placed.retain(|&at| Some(at) != self.selected_clip);
        self.more_clips = placed;
        self.dirty = true;
        Ok(())
    }

    /// Every selected track, primary included, top to bottom (SPEC-047 §5).
    #[must_use]
    pub fn selected_tracks(&self) -> Vec<usize> {
        let mut all = self.more_tracks.clone();
        if let Some(t) = self.selected_track {
            if let Err(i) = all.binary_search(&t) {
                all.insert(i, t);
            }
        }
        let count = self.project.session.tracks.len();
        all.retain(|&t| t < count);
        all
    }

    /// Whether `track` is part of the track selection.
    #[must_use]
    pub fn is_track_selected(&self, track: usize) -> bool {
        self.selected_track == Some(track) || self.more_tracks.binary_search(&track).is_ok()
    }

    /// Whether `folder` is the selected track stack (SPEC-055 §4).
    #[must_use]
    pub fn is_folder_selected(&self, folder: usize) -> bool {
        self.selected_folder == Some(folder) || self.more_folders.binary_search(&folder).is_ok()
    }

    /// Every selected folder, sorted (SPEC-057 §4).
    #[must_use]
    pub fn selected_folders(&self) -> Vec<usize> {
        let mut all = self.more_folders.clone();
        if let Some(f) = self.selected_folder {
            if let Err(i) = all.binary_search(&f) {
                all.insert(i, f);
            }
        }
        let count = self.project.session.folders.len();
        all.retain(|&f| f < count);
        all
    }

    /// Selects a folder (track stack), clearing any track or clip selection;
    /// `None` clears the folder selection (SPEC-055 §4). With `toggle` (Ctrl)
    /// the folder is added to or removed from the selection; with `range`
    /// (Shift) every folder up to the primary one is selected (SPEC-057 §4).
    /// Bad indices do nothing.
    pub fn select_folder(&mut self, folder: Option<usize>, toggle: bool, range: bool) {
        let Some(folder) = folder else {
            self.selected_folder = None;
            self.more_folders.clear();
            return;
        };
        if folder >= self.project.session.folders.len() {
            return;
        }
        if range {
            if let Some(anchor) = self.selected_folder {
                for f in anchor.min(folder)..=anchor.max(folder) {
                    if f != anchor {
                        if let Err(i) = self.more_folders.binary_search(&f) {
                            self.more_folders.insert(i, f);
                        }
                    }
                }
            } else {
                self.selected_folder = Some(folder);
            }
            self.selected_track = None;
            self.more_tracks.clear();
            self.selected_clip = None;
            self.more_clips.clear();
            return;
        }
        if toggle {
            if self.selected_folder == Some(folder) {
                self.selected_folder =
                    (!self.more_folders.is_empty()).then(|| self.more_folders.remove(0));
            } else if let Ok(i) = self.more_folders.binary_search(&folder) {
                self.more_folders.remove(i);
            } else {
                if let Some(old) = self.selected_folder {
                    if let Err(i) = self.more_folders.binary_search(&old) {
                        self.more_folders.insert(i, old);
                    }
                }
                self.selected_folder = Some(folder);
            }
            self.selected_track = None;
            self.more_tracks.clear();
            self.selected_clip = None;
            self.more_clips.clear();
            return;
        }
        self.more_folders.clear();
        self.selected_folder = Some(folder);
        self.selected_track = None;
        self.more_tracks.clear();
        self.selected_clip = None;
        self.more_clips.clear();
    }

    /// Selects a track: alone, with Ctrl (`toggle`) added or removed, or with
    /// Shift (`range`) together with every track up to the primary one
    /// (SPEC-047 §5). Bad indices do nothing. A plain (non-toggle) selection
    /// clears the folder selection.
    pub fn select_track(&mut self, track: usize, toggle: bool, range: bool) {
        if track >= self.project.session.tracks.len() {
            return;
        }
        if range {
            if let Some(anchor) = self.selected_track {
                for t in anchor.min(track)..=anchor.max(track) {
                    if let Err(i) = self.more_tracks.binary_search(&t) {
                        if t != anchor {
                            self.more_tracks.insert(i, t);
                        }
                    }
                }
                return;
            }
        }
        if toggle {
            if self.selected_track == Some(track) {
                self.selected_track =
                    (!self.more_tracks.is_empty()).then(|| self.more_tracks.remove(0));
            } else if let Ok(i) = self.more_tracks.binary_search(&track) {
                self.more_tracks.remove(i);
            } else {
                if let Some(old) = self.selected_track {
                    if let Err(i) = self.more_tracks.binary_search(&old) {
                        self.more_tracks.insert(i, old);
                    }
                }
                self.selected_track = Some(track);
            }
            return;
        }
        self.more_tracks.clear();
        self.selected_track = Some(track);
        self.selected_folder = None;
        self.more_folders.clear();
    }

    /// Copies a clip to the window's clipboard; the project is untouched.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices; the clipboard is unchanged.
    pub fn copy_clip(&mut self, track: usize, clip: usize) -> Result<(), EditorError> {
        self.clipboard = Some(self.clip_ref(track, clip)?.clone());
        Ok(())
    }

    /// Whether [`Self::paste_clip`] has something to paste.
    #[must_use]
    pub const fn has_clipboard(&self) -> bool {
        self.clipboard.is_some()
    }

    /// Appends the copied clip to `track`, starting at `at`. Returns the new
    /// `(track, clip)`, which becomes selected; the clipboard is kept.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyClipboard`], [`EditorError::NoClip`] for a bad
    /// track, [`EditorError::Session`] for a full track or a timeline
    /// overflow; `self` is unchanged.
    pub fn paste_clip(&mut self, track: usize, at: u64) -> Result<(usize, usize), EditorError> {
        let clip = self.clipboard.clone().ok_or(EditorError::EmptyClipboard)?;
        self.append_clip(track, clip, at)
    }

    /// Appends a copy of a clip to its track, starting where it ends.
    /// Returns the copy's `(track, clip)`, which becomes selected.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::Session`] for
    /// a full track or a timeline overflow; `self` is unchanged.
    pub fn duplicate_clip(
        &mut self,
        track: usize,
        clip: usize,
    ) -> Result<(usize, usize), EditorError> {
        let original = self.clip_ref(track, clip)?.clone();
        let at = original.end_samples();
        self.append_clip(track, original, at)
    }

    fn append_clip(
        &mut self,
        track: usize,
        mut clip: Clip,
        at: u64,
    ) -> Result<(usize, usize), EditorError> {
        if track >= self.project.session.tracks.len() {
            return Err(EditorError::NoClip);
        }
        if !self.project.session.tracks[track].kind.accepts(&clip) {
            return Err(EditorError::WrongTrackKind);
        }
        clip.start_samples = at;
        let mut session = self.project.session.clone();
        let clips = &mut session.tracks[track].clips;
        clips.push(clip);
        let at = (track, clips.len() - 1);
        self.commit(session, at)?;
        Ok(at)
    }

    /// Where a new clip goes for a playhead (SPEC-031): the start of the bar
    /// under it, or `0` once it is at or past the end of a non-empty project.
    #[must_use]
    pub fn new_clip_start(&self, playhead: u64) -> u64 {
        let session = &self.project.session;
        let end = session.length_samples();
        if end > 0 && playhead >= end {
            return 0;
        }
        let beats = u64::from(session.time_signature.numerator.max(1));
        let map = session
            .build_tempo_map(session.sample_rate)
            .unwrap_or_else(|_| {
                audio_core::TempoMap::single_or_default(session.tempo_bpm, session.sample_rate)
            });
        let bar = sixteenth_samples(&map) * 4 * beats;
        playhead - playhead % bar
    }

    /// Where and how long a new MIDI region is (SPEC-037): over the active
    /// cycle when one is on, so a region made while looping lands inside
    /// the loop; otherwise four bars from [`Self::new_clip_start`].
    #[must_use]
    pub fn new_midi_region(&self, playhead: u64) -> (u64, u64) {
        if let Some((from, to)) = self.active_cycle() {
            return (from, to - from);
        }
        let session = &self.project.session;
        let beats = u64::from(session.time_signature.numerator.max(1));
        let map = session
            .build_tempo_map(session.sample_rate)
            .unwrap_or_else(|_| {
                audio_core::TempoMap::single_or_default(session.tempo_bpm, session.sample_rate)
            });
        let bar = sixteenth_samples(&map) * 4 * beats;
        (self.new_clip_start(playhead), bar * 4)
    }

    /// Adds a track named "MIDI" holding one empty MIDI clip at `start`
    /// (SPEC-021). Returns `(track, clip)`, which becomes selected.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyClip`] for `length == 0`, [`EditorError::Session`]
    /// for the track limit or a timeline overflow; `self` is unchanged.
    pub fn add_midi_track(
        &mut self,
        start: u64,
        length: u64,
    ) -> Result<(usize, usize), EditorError> {
        if length == 0 {
            return Err(EditorError::EmptyClip);
        }
        let mut session = self.project.session.clone();
        let mut track = Track::instrument("MIDI");
        let mut clip = Clip::midi(start, length);
        clip.name = "MIDI".to_owned();
        track.clips.push(clip);
        session.tracks.push(track);
        let at = (session.tracks.len() - 1, 0);
        self.commit(session, at)?;
        Ok(at)
    }

    /// Adds `count` empty tracks of `kind` below the last track (SPEC-049).
    ///
    /// Names are `name` (or "Аудио"/"Инструмент" when blank) with " N"
    /// appended for N > 1. One undo step; the first new track is selected.
    /// Returns the new indices.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyClip`] for `count == 0`, [`EditorError::Session`]
    /// when the tracks would exceed [`audio_core::MAX_TRACKS`] or the name
    /// is too long; `self` is unchanged.
    pub fn add_tracks(
        &mut self,
        kind: TrackKind,
        count: usize,
        name: &str,
    ) -> Result<Vec<usize>, EditorError> {
        self.add_tracks_with_format(kind, count, name, TrackFormat::Mono)
    }

    /// [`Editor::add_tracks`] with the audio tracks made mono or stereo
    /// (SPEC-050 §5); instrument tracks ignore `format`.
    ///
    /// # Errors
    ///
    /// As [`Editor::add_tracks`].
    pub fn add_tracks_with_format(
        &mut self,
        kind: TrackKind,
        count: usize,
        name: &str,
        format: TrackFormat,
    ) -> Result<Vec<usize>, EditorError> {
        if count == 0 {
            return Err(EditorError::EmptyClip);
        }
        let base = match (name.trim(), kind) {
            ("", TrackKind::Audio) => "Аудио",
            ("", TrackKind::Instrument) => "Инструмент",
            (n, _) => n,
        };
        let mut session = self.project.session.clone();
        let first = session.tracks.len();
        if first + count > audio_core::MAX_TRACKS {
            return Err(SessionError::TooManyTracks {
                max: audio_core::MAX_TRACKS,
            }
            .into());
        }
        for n in 0..count {
            let name = if count == 1 {
                base.to_owned()
            } else {
                format!("{base} {}", n + 1)
            };
            session.tracks.push(match kind {
                TrackKind::Audio => {
                    let mut track = Track::new(&name);
                    track.format = format;
                    track
                }
                TrackKind::Instrument => Track::instrument(&name),
            });
        }
        self.commit_track(session, first)?;
        self.selected_clip = None;
        Ok((first..first + count).collect())
    }

    /// Adds an empty MIDI region to an instrument track (SPEC-049) and
    /// selects it.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad track, [`EditorError::WrongTrackKind`]
    /// for an audio track, [`EditorError::EmptyClip`] for `length == 0`,
    /// [`EditorError::Session`] for a full track; `self` is unchanged.
    pub fn add_midi_region(
        &mut self,
        track: usize,
        start: u64,
        length: u64,
    ) -> Result<(usize, usize), EditorError> {
        if length == 0 {
            return Err(EditorError::EmptyClip);
        }
        if track >= self.project.session.tracks.len() {
            return Err(EditorError::NoClip);
        }
        let mut clip = Clip::midi(start, length);
        clip.name = self.project.session.tracks[track].name.clone();
        self.append_clip(track, clip, start)
    }

    /// Adds `note` to a MIDI clip; notes stay sorted by `(start, pitch)`.
    /// Returns the new note's index.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad indices, [`EditorError::InvalidNote`]
    /// for an audio clip or a note the session rejects; `self` is unchanged.
    pub fn add_note(
        &mut self,
        track: usize,
        clip: usize,
        note: Note,
    ) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        let index = insert_note(midi_notes(&mut session, track, clip)?, note);
        self.commit_notes(session, (track, clip))?;
        Ok(index)
    }

    /// Removes one note from a MIDI clip.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for bad clip indices,
    /// [`EditorError::InvalidNote`] for an audio clip or a bad note index;
    /// `self` is unchanged.
    pub fn delete_note(
        &mut self,
        track: usize,
        clip: usize,
        index: usize,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        if index >= notes.len() {
            return Err(EditorError::InvalidNote);
        }
        notes.remove(index);
        self.commit_notes(session, (track, clip))
    }

    /// Moves one note to `start` and `pitch`, keeping its length and
    /// strength. Returns its new index.
    ///
    /// # Errors
    ///
    /// As [`Editor::delete_note`], plus [`EditorError::InvalidNote`] when the
    /// moved note leaves the clip or the key range; `self` is unchanged.
    pub fn move_note(
        &mut self,
        track: usize,
        clip: usize,
        index: usize,
        start: u64,
        pitch: u8,
    ) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        let notes = midi_notes(&mut session, track, clip)?;
        if index >= notes.len() {
            return Err(EditorError::InvalidNote);
        }
        let mut note = notes.remove(index);
        note.start = start;
        note.pitch = pitch;
        let index = insert_note(notes, note);
        self.commit_notes(session, (track, clip))?;
        Ok(index)
    }

    fn commit_notes(&mut self, session: Session, at: (usize, usize)) -> Result<(), EditorError> {
        match session.validate() {
            Err(SessionError::InvalidNote { .. }) => Err(EditorError::InvalidNote),
            Err(other) => Err(other.into()),
            Ok(()) => self.commit(session, at),
        }
    }

    fn clip_ref(&self, track: usize, clip: usize) -> Result<&Clip, EditorError> {
        self.project
            .session
            .tracks
            .get(track)
            .and_then(|t| t.clips.get(clip))
            .ok_or(EditorError::NoClip)
    }

    fn commit(&mut self, session: Session, selected: (usize, usize)) -> Result<(), EditorError> {
        session.validate()?;
        self.record();
        self.project.session = session;
        self.selected_track = Some(selected.0);
        self.selected_clip = Some(selected);
        self.more_clips.clear();
        self.more_tracks.clear();
        self.dirty = true;
        Ok(())
    }

    /// Appends an effect with default settings to a track's chain and
    /// returns its index (SPEC-022).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoEffect`] for a bad track index,
    /// [`EditorError::Session`] when the chain is full; `self` is unchanged.
    pub fn add_effect(&mut self, track: usize, kind: EffectKind) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        let effects = &mut session
            .tracks
            .get_mut(track)
            .ok_or(EditorError::NoEffect)?
            .effects;
        effects.push(Effect::new(kind));
        let index = effects.len() - 1;
        self.commit_track(session, track)?;
        Ok(index)
    }

    /// Removes one effect from a track's chain.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoEffect`] for bad indices; `self` is unchanged.
    pub fn remove_effect(&mut self, track: usize, index: usize) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let effects = effects_mut(&mut session, track, index)?;
        effects.remove(index);
        // SPEC-041: the removed effect's lanes go, the ones above shift down.
        let automation = &mut session.tracks[track].automation;
        automation
            .effects
            .retain(|lane| usize::from(lane.slot) != index);
        for lane in &mut automation.effects {
            if usize::from(lane.slot) > index {
                lane.slot -= 1;
            }
        }
        self.commit_track(session, track)
    }

    /// Replaces one effect's settings (bypass and parameters). Setting the
    /// value it already has records no undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoEffect`] for bad indices, [`EditorError::Session`]
    /// for an out-of-range value; `self` is unchanged.
    pub fn set_effect(
        &mut self,
        track: usize,
        index: usize,
        effect: Effect,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let slot = &mut effects_mut(&mut session, track, index)?[index];
        if *slot == effect {
            return Ok(());
        }
        *slot = effect;
        self.commit_track(session, track)
    }

    /// Moves the effect at `from` to position `to` in the chain.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoEffect`] when either index is outside the chain;
    /// `self` is unchanged.
    pub fn move_effect(&mut self, track: usize, from: usize, to: usize) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let effects = effects_mut(&mut session, track, from)?;
        if to >= effects.len() {
            return Err(EditorError::NoEffect);
        }
        if from == to {
            return Ok(());
        }
        let effect = effects.remove(from);
        effects.insert(to, effect);
        // SPEC-041: lanes follow their effect to its new slot.
        for lane in &mut session.tracks[track].automation.effects {
            lane.slot = moved_slot(lane.slot, from, to);
        }
        self.commit_track(session, track)
    }

    /// Adds an automation point, keeping the lane ordered by time; a point
    /// at the same time is replaced. Returns its index (SPEC-023).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoPoint`] for a bad track index,
    /// [`EditorError::Session`] for an out-of-range value or a full lane;
    /// `self` is unchanged.
    pub fn add_point(
        &mut self,
        track: usize,
        lane: Lane,
        point: AutoPoint,
    ) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        let points = lane_mut(&mut session, track, lane)?;
        let index = points.partition_point(|p| p.time < point.time);
        if points.get(index).is_some_and(|p| p.time == point.time) {
            if points[index] == point {
                return Ok(index);
            }
            points[index] = point;
        } else {
            points.insert(index, point);
        }
        self.commit_track(session, track)?;
        Ok(index)
    }

    /// Moves one automation point; its time is clamped between its
    /// neighbours so the lane order never changes. Returns the stored point.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoPoint`] for bad indices, [`EditorError::Session`]
    /// for an out-of-range value; `self` is unchanged.
    pub fn move_point(
        &mut self,
        track: usize,
        lane: Lane,
        index: usize,
        point: AutoPoint,
    ) -> Result<AutoPoint, EditorError> {
        let mut session = self.project.session.clone();
        let points = lane_mut(&mut session, track, lane)?;
        if index >= points.len() {
            return Err(EditorError::NoPoint);
        }
        let low = if index == 0 {
            0
        } else {
            points[index - 1].time.saturating_add(1)
        };
        let high = points
            .get(index + 1)
            .map_or(u64::MAX, |next| next.time.saturating_sub(1));
        let point = AutoPoint {
            time: point.time.clamp(low, high.max(low)),
            value: point.value,
        };
        if points[index] == point {
            return Ok(point);
        }
        points[index] = point;
        self.commit_track(session, track)?;
        Ok(point)
    }

    /// Removes one automation point.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoPoint`] for bad indices; `self` is unchanged.
    pub fn remove_point(
        &mut self,
        track: usize,
        lane: Lane,
        index: usize,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let points = lane_mut(&mut session, track, lane)?;
        if index >= points.len() {
            return Err(EditorError::NoPoint);
        }
        points.remove(index);
        session.tracks[track].automation.prune();
        self.commit_track(session, track)
    }

    /// Removes every point of one lane; an empty lane records no undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoPoint`] for a bad track index.
    pub fn clear_lane(&mut self, track: usize, lane: Lane) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let points = lane_mut(&mut session, track, lane)?;
        if points.is_empty() {
            return Ok(());
        }
        points.clear();
        session.tracks[track].automation.prune();
        self.commit_track(session, track)
    }

    /// Appends a bus named `name` with unity gain and returns its index
    /// (SPEC-028).
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] when the bus list is full or the name is
    /// invalid; `self` is unchanged.
    pub fn add_bus(&mut self, name: &str) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        session.buses.push(Bus::new(name));
        let index = session.buses.len() - 1;
        self.commit_session(session)?;
        Ok(index)
    }

    /// Removes a bus, every send to it, and shifts sends to later buses down
    /// by one.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad index; `self` is unchanged.
    pub fn remove_bus(&mut self, bus: usize) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        if bus >= session.buses.len() {
            return Err(EditorError::NoBus);
        }
        session.buses.remove(bus);
        for track in &mut session.tracks {
            track.sends.retain(|send| send.bus != bus);
            for send in &mut track.sends {
                if send.bus > bus {
                    send.bus -= 1;
                }
            }
        }
        // SPEC-051: bus sends also target buses, so they must be cleaned up
        // and re-indexed the same way track sends are.
        for sender in &mut session.buses {
            sender.sends.retain(|send| send.bus != bus);
            for send in &mut sender.sends {
                if send.bus > bus {
                    send.bus -= 1;
                }
            }
        }
        self.commit_session(session)
    }

    /// Sets a bus's return gain. Setting the current value records no undo
    /// step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad index, [`EditorError::Session`] for
    /// a gain outside `0..=1`; `self` is unchanged.
    pub fn set_bus_gain(&mut self, bus: usize, gain: f32) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let slot = &mut session.buses.get_mut(bus).ok_or(EditorError::NoBus)?.gain;
        if slot.to_bits() == gain.to_bits() {
            return Ok(());
        }
        *slot = gain;
        self.commit_session(session)
    }

    /// Sets the level of bus `bus`'s send to bus `target`; `0` removes the
    /// send (SPEC-051). A self-send or a send that would form a cycle is
    /// rejected by validation, leaving `self` unchanged.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad bus or target index,
    /// [`EditorError::Session`] for a level outside `0..=1`, a self-send, or
    /// a cycle; `self` is unchanged.
    pub fn set_bus_send(
        &mut self,
        bus: usize,
        target: usize,
        level: f32,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        if bus >= session.buses.len() || target >= session.buses.len() {
            return Err(EditorError::NoBus);
        }
        let sends = &mut session.buses[bus].sends;
        let current = sends.iter().position(|send| send.bus == target);
        match current {
            Some(index) if level == 0.0 => {
                sends.remove(index);
            }
            Some(index) => {
                if sends[index].level.to_bits() == level.to_bits() {
                    return Ok(());
                }
                sends[index].level = level;
            }
            None if level == 0.0 => return Ok(()),
            None => sends.push(BusSend { bus: target, level }),
        }
        self.commit_session(session)
    }

    /// Toggles a bus's mute (SPEC-051). Setting the current value records no
    /// undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad index; `self` is unchanged.
    pub fn set_bus_mute(&mut self, bus: usize, mute: bool) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let slot = &mut session.buses.get_mut(bus).ok_or(EditorError::NoBus)?.mute;
        if *slot == mute {
            return Ok(());
        }
        *slot = mute;
        self.commit_session(session)
    }

    /// Toggles a bus's solo (SPEC-051). Setting the current value records no
    /// undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad index; `self` is unchanged.
    pub fn set_bus_solo(&mut self, bus: usize, solo: bool) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let slot = &mut session.buses.get_mut(bus).ok_or(EditorError::NoBus)?.solo;
        if *slot == solo {
            return Ok(());
        }
        *slot = solo;
        self.commit_session(session)
    }

    /// Sets the level of `track`'s send to `bus`; `0` removes the send.
    /// Setting the current level records no undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad track or bus index,
    /// [`EditorError::Session`] for a level outside `0..=1`; `self` is
    /// unchanged.
    pub fn set_send(&mut self, track: usize, bus: usize, level: f32) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        if bus >= session.buses.len() {
            return Err(EditorError::NoBus);
        }
        let sends = &mut session
            .tracks
            .get_mut(track)
            .ok_or(EditorError::NoBus)?
            .sends;
        let current = sends.iter().position(|send| send.bus == bus);
        match current {
            Some(index) if level == 0.0 => {
                sends.remove(index);
            }
            Some(index) => {
                if sends[index].level.to_bits() == level.to_bits() {
                    return Ok(());
                }
                sends[index].level = level;
            }
            None if level == 0.0 => return Ok(()),
            None => sends.push(BusSend { bus, level }),
        }
        self.commit_track(session, track)
    }

    /// Appends an effect with default settings to a bus and returns its
    /// index.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for a bad bus index, [`EditorError::Session`]
    /// when the chain is full; `self` is unchanged.
    pub fn add_bus_effect(&mut self, bus: usize, kind: EffectKind) -> Result<usize, EditorError> {
        let mut session = self.project.session.clone();
        let effects = &mut session
            .buses
            .get_mut(bus)
            .ok_or(EditorError::NoBus)?
            .effects;
        effects.push(Effect::new(kind));
        let index = effects.len() - 1;
        self.commit_session(session)?;
        Ok(index)
    }

    /// Removes one effect from a bus.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for bad indices; `self` is unchanged.
    pub fn remove_bus_effect(&mut self, bus: usize, index: usize) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        bus_effects_mut(&mut session, bus, index)?.remove(index);
        self.commit_session(session)
    }

    /// Replaces one bus effect's settings. Setting the current value records
    /// no undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoBus`] for bad indices, [`EditorError::Session`] for
    /// an out-of-range value; `self` is unchanged.
    pub fn set_bus_effect(
        &mut self,
        bus: usize,
        index: usize,
        effect: Effect,
    ) -> Result<(), EditorError> {
        let mut session = self.project.session.clone();
        let slot = &mut bus_effects_mut(&mut session, bus, index)?[index];
        if *slot == effect {
            return Ok(());
        }
        *slot = effect;
        self.commit_session(session)
    }

    fn commit_session(&mut self, session: Session) -> Result<(), EditorError> {
        session.validate()?;
        self.record();
        self.project.session = session;
        self.dirty = true;
        Ok(())
    }

    fn commit_track(&mut self, session: Session, track: usize) -> Result<(), EditorError> {
        session.validate()?;
        self.record();
        self.project.session = session;
        self.selected_track = Some(track);
        self.more_tracks.clear();
        self.more_clips.clear();
        self.dirty = true;
        Ok(())
    }

    /// Adds "Маркер N" at `position` in order and returns its index
    /// (SPEC-030); N is the lowest number not used by another marker.
    ///
    /// # Errors
    ///
    /// [`EditorError::MarkerExists`] when the position is taken,
    /// [`EditorError::Session`] past [`audio_core::MAX_MARKERS`].
    pub fn add_marker(&mut self, position: u64) -> Result<usize, EditorError> {
        let markers = &self.project.session.markers;
        let index = markers.partition_point(|m| m.position < position);
        if markers.get(index).is_some_and(|m| m.position == position) {
            return Err(EditorError::MarkerExists);
        }
        let number = (1..)
            .find(|n| !markers.iter().any(|m| m.name == format!("Маркер {n}")))
            .unwrap_or(1);
        let mut session = self.project.session.clone();
        session.markers.insert(
            index,
            Marker {
                name: format!("Маркер {number}"),
                position,
            },
        );
        self.commit_session(session)?;
        Ok(index)
    }

    /// Renames a marker; surrounding spaces are trimmed (SPEC-030).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoMarker`], [`EditorError::EmptyName`] or
    /// [`EditorError::Session`] for a too long name.
    pub fn rename_marker(&mut self, index: usize, name: &str) -> Result<(), EditorError> {
        let name = name.trim();
        let current = &self
            .project
            .session
            .markers
            .get(index)
            .ok_or(EditorError::NoMarker)?
            .name;
        if name.is_empty() {
            return Err(EditorError::EmptyName);
        }
        if current == name {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        name.clone_into(&mut session.markers[index].name);
        self.commit_session(session)
    }

    /// Moves a marker and keeps the list sorted; returns its new index
    /// (SPEC-030).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoMarker`], or [`EditorError::MarkerExists`] when
    /// another marker sits at `position`.
    pub fn move_marker(&mut self, index: usize, position: u64) -> Result<usize, EditorError> {
        let markers = &self.project.session.markers;
        let marker = markers.get(index).ok_or(EditorError::NoMarker)?;
        if marker.position == position {
            return Ok(index);
        }
        if markers.iter().any(|m| m.position == position) {
            return Err(EditorError::MarkerExists);
        }
        let mut session = self.project.session.clone();
        let mut moved = session.markers.remove(index);
        moved.position = position;
        let to = session.markers.partition_point(|m| m.position < position);
        session.markers.insert(to, moved);
        self.commit_session(session)?;
        Ok(to)
    }

    /// Removes a marker (SPEC-030).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoMarker`] for a bad index.
    pub fn delete_marker(&mut self, index: usize) -> Result<(), EditorError> {
        if index >= self.project.session.markers.len() {
            return Err(EditorError::NoMarker);
        }
        let mut session = self.project.session.clone();
        session.markers.remove(index);
        self.commit_session(session)
    }

    /// Position of the first marker strictly after `position` (SPEC-030).
    #[must_use]
    pub fn next_marker(&self, position: u64) -> Option<u64> {
        self.project
            .session
            .markers
            .iter()
            .map(|m| m.position)
            .find(|&p| p > position)
    }

    /// Position of the last marker strictly before `position` (SPEC-030).
    #[must_use]
    pub fn previous_marker(&self, position: u64) -> Option<u64> {
        self.project
            .session
            .markers
            .iter()
            .rev()
            .map(|m| m.position)
            .find(|&p| p < position)
    }

    /// Renames a track; surrounding spaces are trimmed (SPEC-029).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index, [`EditorError::EmptyName`]
    /// for a blank name, [`EditorError::Session`] for a too long one.
    pub fn rename_track(&mut self, track: usize, name: &str) -> Result<(), EditorError> {
        let name = name.trim();
        let current = &self
            .project
            .session
            .tracks
            .get(track)
            .ok_or(EditorError::NoClip)?
            .name;
        if name.is_empty() {
            return Err(EditorError::EmptyName);
        }
        if current == name {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        name.clone_into(&mut session.tracks[track].name);
        self.commit_session(session)
    }

    /// Sets or clears a track's palette colour (SPEC-029).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index, [`EditorError::Session`] for
    /// a colour outside the palette.
    pub fn set_track_color(&mut self, track: usize, color: Option<u8>) -> Result<(), EditorError> {
        let current = self
            .project
            .session
            .tracks
            .get(track)
            .ok_or(EditorError::NoClip)?
            .color;
        if current == color {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.tracks[track].color = color;
        self.commit_session(session)
    }

    /// Makes an audio track mono or stereo (SPEC-050 §5). One undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index or an instrument track.
    pub fn set_track_format(
        &mut self,
        track: usize,
        format: TrackFormat,
    ) -> Result<(), EditorError> {
        let current = self
            .project
            .session
            .tracks
            .get(track)
            .filter(|t| t.kind == TrackKind::Audio)
            .ok_or(EditorError::NoClip)?
            .format;
        if current == format {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.tracks[track].format = format;
        self.commit_session(session)
    }

    /// Sets a software-instrument track's instrument (SPEC-065). One undo
    /// step; ignored on audio tracks.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index or an audio track.
    pub fn set_track_instrument(
        &mut self,
        track: usize,
        instrument: TrackInstrument,
    ) -> Result<(), EditorError> {
        let current = self
            .project
            .session
            .tracks
            .get(track)
            .filter(|t| t.kind == TrackKind::Instrument)
            .ok_or(EditorError::NoClip)?
            .instrument
            .clone();
        if current == instrument {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.tracks[track].instrument = instrument;
        self.commit_session(session)
    }

    /// Moves a track to position `to`; the others shift (SPEC-029).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] when either index is out of range.
    pub fn move_track(&mut self, from: usize, to: usize) -> Result<(), EditorError> {
        self.pin_default_colors();
        let count = self.project.session.tracks.len();
        if from >= count || to >= count {
            return Err(EditorError::NoClip);
        }
        if from == to {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        let moved = session.tracks.remove(from);
        session.tracks.insert(to, moved);
        remap_folder_children(&mut session.folder_children, |c| {
            if c == from {
                to
            } else if from < to && from < c && c <= to {
                c - 1
            } else if to < from && to <= c && c < from {
                c + 1
            } else {
                c
            }
        });
        self.commit_track(session, to)?;
        for a in &mut self.armed {
            *a = if *a == from {
                to
            } else if from < *a && *a <= to {
                *a - 1
            } else if to <= *a && *a < from {
                *a + 1
            } else {
                *a
            };
        }
        self.armed.sort_unstable();
        self.selected_clip = None;
        Ok(())
    }

    /// Moves a group of tracks so they form one contiguous block at position
    /// `to`, preserving their relative order (SPEC-057 §1). One undo step;
    /// folder child indices and armed tracks are remapped. Empty or repeated
    /// selections are collapsed; bad indices are rejected.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] when an index is out of range.
    pub fn move_tracks(&mut self, tracks: &[usize], to: usize) -> Result<(), EditorError> {
        self.pin_default_colors();
        let count = self.project.session.tracks.len();
        let mut moved: Vec<usize> = tracks.to_vec();
        moved.sort_unstable();
        moved.dedup();
        if moved.is_empty() {
            return Ok(());
        }
        if moved.iter().any(|&t| t >= count) || to > count {
            return Err(EditorError::NoClip);
        }
        let mut session = self.project.session.clone();

        // Remove the selected tracks, in order, collecting them.
        let mut selected = Vec::with_capacity(moved.len());
        for (offset, &t) in moved.iter().enumerate() {
            selected.push(session.tracks.remove(t - offset));
        }

        // The block lands at `to`, clamped to the now-shorter list.
        let insert_at = to.min(session.tracks.len());
        session.tracks.splice(insert_at..insert_at, selected);

        // Recompute every folder child index through the old->new mapping.
        let mut old_to_new = vec![0usize; count];
        let mut non_moved = 0usize;
        for (t, slot) in old_to_new.iter_mut().enumerate() {
            if moved.binary_search(&t).is_ok() {
                continue;
            }
            *slot = non_moved;
            non_moved += 1;
        }
        for (order, &t) in moved.iter().enumerate() {
            old_to_new[t] = insert_at + order;
        }
        for (t, slot) in old_to_new.iter_mut().enumerate() {
            if moved.binary_search(&t).is_err() && *slot >= insert_at {
                *slot += moved.len();
            }
        }
        remap_folder_children(&mut session.folder_children, |c| old_to_new[c]);

        self.commit_session(session)?;
        self.armed = self
            .armed
            .iter()
            .map(|&a| old_to_new[a])
            .collect::<Vec<_>>();
        self.armed.sort_unstable();
        self.selected_track = self
            .selected_track
            .and_then(|t| (t < old_to_new.len()).then(|| old_to_new[t]));
        self.more_tracks = self
            .more_tracks
            .iter()
            .filter(|&&t| t < old_to_new.len())
            .map(|&t| old_to_new[t])
            .collect();
        self.more_tracks.sort_unstable();
        self.selected_clip = None;
        Ok(())
    }

    /// Moves `tracks` into `folder`, first removing them from any folder they
    /// already belong to (SPEC-057 §2). One undo step. Bad indices are
    /// rejected; repeated tracks are collapsed.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`], [`EditorError::NoClip`] for a bad track
    /// index.
    pub fn move_tracks_to_folder(
        &mut self,
        folder: usize,
        tracks: &[usize],
    ) -> Result<(), EditorError> {
        self.pin_default_colors();
        if folder >= self.project.session.folders.len() {
            return Err(EditorError::NoFolder);
        }
        let count = self.project.session.tracks.len();
        let mut sorted: Vec<usize> = tracks.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() {
            return Ok(());
        }
        if sorted.iter().any(|&t| t >= count) {
            return Err(EditorError::NoClip);
        }
        let mut session = self.project.session.clone();
        for list in &mut session.folder_children {
            list.retain(|c| !sorted.contains(c));
        }
        session.folder_children[folder].extend_from_slice(&sorted);
        session.folder_children[folder].sort_unstable();
        self.commit_session(session)
    }

    /// Inserts a copy of a track right below it and selects the copy
    /// (SPEC-029). Returns the copy's index.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index, [`EditorError::Session`] when
    /// the session already holds [`audio_core::MAX_TRACKS`] tracks.
    pub fn duplicate_track(&mut self, track: usize) -> Result<usize, EditorError> {
        self.pin_default_colors();
        let mut session = self.project.session.clone();
        let mut copy = session
            .tracks
            .get(track)
            .ok_or(EditorError::NoClip)?
            .clone();
        copy.name.push_str(" копия");
        if copy.name.len() > audio_core::MAX_NAME_BYTES {
            copy.name.clone_from(&session.tracks[track].name);
        }
        session.tracks.insert(track + 1, copy);
        remap_folder_children(&mut session.folder_children, |c| {
            if c > track { c + 1 } else { c }
        });
        self.commit_track(session, track + 1)?;
        for a in &mut self.armed {
            if *a > track {
                *a += 1;
            }
        }
        self.selected_clip = None;
        Ok(track + 1)
    }

    /// Removes one track and its clips; out-of-range indices do nothing.
    pub fn delete_track(&mut self, track: usize) {
        self.pin_default_colors();
        if track < self.project.session.tracks.len() {
            self.record();
            self.project.session.tracks.remove(track);
            // Remove the track from its folder (if any) and shift the other
            // child indices (SPEC-055).
            for children in &mut self.project.session.folder_children {
                children.retain(|&c| c != track);
                for c in children {
                    if *c > track {
                        *c -= 1;
                    }
                }
            }
            self.armed.retain(|&a| a != track);
            for a in &mut self.armed {
                if *a > track {
                    *a -= 1;
                }
            }
            self.selected_track = None;
            self.selected_clip = None;
            self.more_tracks.clear();
            self.more_clips.clear();
            self.dirty = true;
        }
    }

    /// Creates a track folder holding `tracks` (SPEC-055 §3). Children keep
    /// their session order; the folder is inserted at the top level. Returns
    /// the new folder's index.
    ///
    /// # Errors
    ///
    /// [`EditorError::EmptyFolder`] when `tracks` is empty,
    /// [`EditorError::NoClip`] when an index is out of range,
    /// [`EditorError::TrackInFolder`] when a track is already in a folder.
    pub fn create_folder(&mut self, tracks: &[usize]) -> Result<usize, EditorError> {
        self.pin_default_colors();
        if tracks.is_empty() {
            return Err(EditorError::EmptyFolder);
        }
        let count = self.project.session.tracks.len();
        let mut sorted: Vec<usize> = tracks.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut seen = vec![false; count];
        for children in &self.project.session.folder_children {
            for &c in children {
                seen[c] = true;
            }
        }
        for &t in &sorted {
            if t >= count {
                return Err(EditorError::NoClip);
            }
            if seen[t] {
                return Err(EditorError::TrackInFolder);
            }
            seen[t] = true;
        }
        let number = (1..)
            .find(|n| {
                !self
                    .project
                    .session
                    .folders
                    .iter()
                    .any(|f| f.name == format!("Папка {n}"))
            })
            .unwrap_or(1);
        let mut session = self.project.session.clone();
        session
            .folders
            .push(TrackFolder::new(&format!("Папка {number}")));
        session.folder_children.push(sorted);
        self.commit_session(session)?;
        Ok(self.project.session.folders.len() - 1)
    }

    /// Creates an empty folder at the top level and returns its index
    /// (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] when the session is full.
    pub fn create_empty_folder(&mut self) -> Result<usize, EditorError> {
        let number = (1..)
            .find(|n| {
                !self
                    .project
                    .session
                    .folders
                    .iter()
                    .any(|f| f.name == format!("Папка {n}"))
            })
            .unwrap_or(1);
        let mut session = self.project.session.clone();
        session
            .folders
            .push(TrackFolder::new(&format!("Папка {number}")));
        session.folder_children.push(Vec::new());
        self.commit_session(session)?;
        Ok(self.project.session.folders.len() - 1)
    }

    /// Deletes a folder; its children either stay on the top level or are
    /// deleted with it (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`] for a bad index.
    pub fn delete_folder(&mut self, folder: usize, mode: DeleteFolder) -> Result<(), EditorError> {
        self.pin_default_colors();
        if folder >= self.project.session.folders.len() {
            return Err(EditorError::NoFolder);
        }
        let mut session = self.project.session.clone();
        let children = session.folder_children[folder].clone();
        match mode {
            DeleteFolder::KeepChildren => {
                session.folders.remove(folder);
                session.folder_children.remove(folder);
            }
            DeleteFolder::DeleteChildren => {
                // Delete child tracks (highest index first) and reindex all
                // remaining folder child lists.
                for &child in children.iter().rev() {
                    session.tracks.remove(child);
                }
                for list in &mut session.folder_children {
                    list.retain(|&c| !children.contains(&c));
                }
                session.folders.remove(folder);
                session.folder_children.remove(folder);
                for list in &mut session.folder_children {
                    for c in list {
                        *c -= children.iter().filter(|&&child| child < *c).count();
                    }
                }
            }
        }
        self.commit_session(session)?;
        self.selected_track = None;
        self.selected_folder = None;
        self.more_folders.clear();
        Ok(())
    }

    /// Renames a folder (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`], [`EditorError::EmptyName`] or
    /// [`EditorError::Session`] for a too long name.
    pub fn rename_folder(&mut self, folder: usize, name: &str) -> Result<(), EditorError> {
        let name = name.trim();
        let current = &self
            .project
            .session
            .folders
            .get(folder)
            .ok_or(EditorError::NoFolder)?
            .name;
        if name.is_empty() {
            return Err(EditorError::EmptyName);
        }
        if current == name {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        name.clone_into(&mut session.folders[folder].name);
        self.commit_session(session)
    }

    /// Sets or clears a folder's colour (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`], [`EditorError::Session`] for a colour
    /// outside the palette.
    pub fn set_folder_color(
        &mut self,
        folder: usize,
        color: Option<u8>,
    ) -> Result<(), EditorError> {
        let current = self
            .project
            .session
            .folders
            .get(folder)
            .ok_or(EditorError::NoFolder)?
            .color;
        if current == color {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        session.folders[folder].color = color;
        self.commit_session(session)
    }

    /// Collapses or expands a folder; display-only, never touches data or
    /// sound (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`].
    pub fn toggle_folder(&mut self, folder: usize) -> Result<(), EditorError> {
        if folder >= self.project.session.folders.len() {
            return Err(EditorError::NoFolder);
        }
        let mut session = self.project.session.clone();
        session.folders[folder].collapsed = !session.folders[folder].collapsed;
        self.commit_session(session)
    }

    /// Moves a folder together with all its children to track position `to`
    /// (SPEC-055 §3). The children are gathered into one contiguous block that
    /// starts at `to`, in their original relative order; every other track
    /// shifts to make room. Child indices in every folder are recomputed, so
    /// the move is a single undo step.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`] for a bad folder index.
    pub fn move_folder(&mut self, folder: usize, to: usize) -> Result<(), EditorError> {
        self.pin_default_colors();
        if folder >= self.project.session.folders.len() {
            return Err(EditorError::NoFolder);
        }
        let old_count = self.project.session.tracks.len();
        let mut children = self.project.session.folder_children[folder].clone();
        children.sort_unstable();
        // Split the tracks into children (in order) and the rest.
        let mut child_tracks = Vec::with_capacity(children.len());
        let mut others = Vec::with_capacity(old_count - children.len());
        let mut old_to_new = vec![0usize; old_count];
        let mut non_child_pos = 0usize;
        for (i, track) in self.project.session.tracks.iter().enumerate() {
            if children.contains(&i) {
                // A child's final position is `insert_at + its order`.
                child_tracks.push(track.clone());
            } else {
                others.push(track.clone());
                old_to_new[i] = non_child_pos;
                non_child_pos += 1;
            }
        }
        let insert_at = to.min(others.len());
        // Non-children after the insertion point shift by the child count.
        for (i, _track) in self.project.session.tracks.iter().enumerate() {
            if children.contains(&i) {
                let order = children.iter().position(|&c| c == i).unwrap();
                old_to_new[i] = insert_at + order;
            } else if old_to_new[i] >= insert_at {
                old_to_new[i] += children.len();
            }
        }
        let mut session = self.project.session.clone();
        let mut new_tracks = others;
        new_tracks.splice(insert_at..insert_at, child_tracks);
        session.tracks = new_tracks;
        remap_folder_children(&mut session.folder_children, |c| old_to_new[c]);
        // Keep the same selected track (following the physical move).
        let selected = self.selected_track.and_then(|t| old_to_new.get(t).copied());
        self.commit_session(session)?;
        self.selected_track = selected;
        self.selected_clip = None;
        Ok(())
    }

    /// Adds `tracks` to `folder` (SPEC-055 §3).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoFolder`], [`EditorError::NoClip`] for a bad track
    /// index, [`EditorError::TrackInFolder`] when a track is already inside
    /// a folder.
    pub fn add_tracks_to_folder(
        &mut self,
        folder: usize,
        tracks: &[usize],
    ) -> Result<(), EditorError> {
        self.pin_default_colors();
        if folder >= self.project.session.folders.len() {
            return Err(EditorError::NoFolder);
        }
        let count = self.project.session.tracks.len();
        let mut seen = vec![false; count];
        for children in &self.project.session.folder_children {
            for &c in children {
                seen[c] = true;
            }
        }
        for &t in tracks {
            if t >= count {
                return Err(EditorError::NoClip);
            }
            if seen[t] {
                return Err(EditorError::TrackInFolder);
            }
            seen[t] = true;
        }
        let mut session = self.project.session.clone();
        session.folder_children[folder].extend_from_slice(tracks);
        session.folder_children[folder].sort_unstable();
        self.commit_session(session)
    }

    /// Removes `tracks` from whichever folder holds them (SPEC-055 §3).
    /// Tracks that are not in a folder are ignored.
    pub fn remove_tracks_from_folder(&mut self, tracks: &[usize]) -> Result<(), EditorError> {
        self.pin_default_colors();
        if tracks.is_empty() {
            return Ok(());
        }
        let mut session = self.project.session.clone();
        for list in &mut session.folder_children {
            list.retain(|c| !tracks.contains(c));
        }
        self.commit_session(session)
    }

    /// Writes the palette colour a track shows into `Track::color` when it
    /// has none, so the colour belongs to the track and not to its position
    /// (SPEC-060). Folders get the same treatment. Visually a no-op, so it
    /// adds no undo step.
    fn pin_default_colors(&mut self) {
        let needs_pin = self
            .project
            .session
            .tracks
            .iter()
            .any(|t| t.color.is_none())
            || self
                .project
                .session
                .folders
                .iter()
                .any(|f| f.color.is_none());
        if !needs_pin {
            return;
        }
        if self.pre_pin.is_none() {
            self.pre_pin = Some(self.project.session.clone());
        }
        for (i, t) in self.project.session.tracks.iter_mut().enumerate() {
            if t.color.is_none() {
                t.color = Some(layout::display_color(None, i));
            }
        }
        for (i, f) in self.project.session.folders.iter_mut().enumerate() {
            if f.color.is_none() {
                f.color = Some(layout::display_color(None, i));
            }
        }
    }

    /// Mutable access to one track's mixer settings; marks the project
    /// changed.
    pub fn track_mut(&mut self, track: usize) -> Option<&mut Track> {
        if track >= self.project.session.tracks.len() {
            return None;
        }
        self.record();
        self.dirty = true;
        self.project.session.tracks.get_mut(track)
    }

    /// Sets the master gain, clamped to `0.0..=1.0`.
    pub fn set_master_gain(&mut self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if gain.to_bits() == self.project.session.master_gain.to_bits() {
            return;
        }
        self.record();
        self.project.session.master_gain = gain;
        self.dirty = true;
    }

    /// Mutes or unmutes Stereo Out, undoably (SPEC-044 §1).
    pub fn set_master_mute(&mut self, mute: bool) {
        if mute != self.project.session.master_mute {
            self.record();
            self.project.session.master_mute = mute;
            self.dirty = true;
        }
    }

    /// Dims Stereo Out by 20 dB or restores it, undoably (SPEC-044 §1).
    pub fn set_master_dim(&mut self, dim: bool) {
        if dim != self.project.session.master_dim {
            self.record();
            self.project.session.master_dim = dim;
            self.dirty = true;
        }
    }

    /// Sets the tempo, clamped to `30..=300` BPM (SPEC-054; the scalar tempo
    /// is the first tempo-map point). A value that is not finite, or the
    /// current tempo, changes nothing.
    pub fn set_tempo(&mut self, tempo_bpm: f64) {
        if !tempo_bpm.is_finite() {
            return;
        }
        let tempo = tempo_bpm.clamp(MIN_TEMPO, MAX_TEMPO);
        if tempo.to_bits() == self.project.session.tempo_bpm.to_bits() {
            return;
        }
        self.record();
        self.project.session.tempo_bpm = tempo;
        self.dirty = true;
    }

    /// Number of tempo points, including the implied first point at tick 0
    /// (SPEC-054).
    #[must_use]
    pub fn tempo_point_count(&self) -> usize {
        self.project.session.tempo_map.len() + 1
    }

    /// Tempo point `index` in the full ordered list, where index 0 is the
    /// implied first point (`tempo_bpm` at tick 0). Returns `None` when out of
    /// range.
    #[must_use]
    pub fn tempo_point(&self, index: usize) -> Option<TempoPoint> {
        if index == 0 {
            return Some(TempoPoint {
                position_ticks: 0,
                bpm: self.project.session.tempo_bpm,
            });
        }
        self.project.session.tempo_map.get(index - 1).copied()
    }

    /// Adds a tempo point at `position_ticks` (in ticks) with `bpm`, or
    /// updates the first point when `position_ticks == 0`. Points are kept
    /// ordered by position; a point at an existing position replaces it.
    /// Returns the full-list index of the point, or `None` if the inputs are
    /// out of range or the point limit is reached.
    pub fn add_tempo_point(&mut self, position_ticks: u64, bpm: f64) -> Option<usize> {
        if !bpm.is_finite() || !(MIN_TEMPO..=MAX_TEMPO).contains(&bpm) {
            return None;
        }
        if position_ticks == 0 {
            self.set_tempo(bpm);
            return Some(0);
        }
        if self.project.session.tempo_map.len() >= MAX_TEMPO_POINTS - 1 {
            return None;
        }
        self.record();
        let map = &mut self.project.session.tempo_map;
        if let Some(existing) = map.iter().position(|p| p.position_ticks == position_ticks) {
            map[existing].bpm = bpm;
            self.dirty = true;
            return Some(existing + 1);
        }
        let point = TempoPoint {
            position_ticks,
            bpm,
        };
        let idx = map.partition_point(|p| p.position_ticks < position_ticks);
        map.insert(idx, point);
        self.dirty = true;
        Some(idx + 1)
    }

    /// Moves tempo point `index` (full list) to `position_ticks`, reordering
    /// as needed. The first point cannot move. Returns `true` when it moved.
    pub fn move_tempo_point(&mut self, index: usize, position_ticks: u64) -> bool {
        if index == 0 || index > self.project.session.tempo_map.len() || position_ticks == 0 {
            return false;
        }
        let map = &self.project.session.tempo_map;
        if map[index - 1].position_ticks == position_ticks {
            return false;
        }
        // A collision collapses into one point (the surviving one keeps its
        // BPM), so positions stay unique.
        if let Some(other) = map.iter().position(|p| p.position_ticks == position_ticks)
            && other != index - 1
        {
            // Drop one of the two colliding points, then set the position.
            let keep = index - 1;
            let drop = other;
            self.record();
            self.project.session.tempo_map.remove(drop);
            let keep = if drop < keep { keep - 1 } else { keep };
            self.project.session.tempo_map[keep].position_ticks = position_ticks;
            self.project
                .session
                .tempo_map
                .sort_by_key(|p| p.position_ticks);
            self.dirty = true;
            return true;
        }
        self.record();
        self.project.session.tempo_map[index - 1].position_ticks = position_ticks;
        self.project
            .session
            .tempo_map
            .sort_by_key(|p| p.position_ticks);
        self.dirty = true;
        true
    }

    /// Removes tempo point `index` (full list). The first point cannot be
    /// removed. Returns `true` when a point was removed.
    pub fn remove_tempo_point(&mut self, index: usize) -> bool {
        if index == 0 || index > self.project.session.tempo_map.len() {
            return false;
        }
        self.record();
        self.project.session.tempo_map.remove(index - 1);
        self.dirty = true;
        true
    }

    /// Sets the BPM of tempo point `index` (full list), clamped to 30–300.
    /// Index 0 routes to [`Self::set_tempo`]. Returns `true` when changed.
    pub fn set_tempo_point_bpm(&mut self, index: usize, bpm: f64) -> bool {
        if !bpm.is_finite() || !(MIN_TEMPO..=MAX_TEMPO).contains(&bpm) {
            return false;
        }
        if index == 0 {
            let old = self.project.session.tempo_bpm;
            self.set_tempo(bpm);
            return self.project.session.tempo_bpm.to_bits() != old.to_bits();
        }
        let Some(&point) = self.project.session.tempo_map.get(index - 1) else {
            return false;
        };
        if point.bpm.to_bits() == bpm.to_bits() {
            return false;
        }
        self.record();
        self.project.session.tempo_map[index - 1].bpm = bpm;
        self.dirty = true;
        true
    }

    /// Builds the full tempo map from the session (SPEC-054). Control-side
    /// only; the result is moved to the audio thread, never built there.
    ///
    /// # Errors
    ///
    /// [`crate::audio_core::TempoMapError`] if the combined points are invalid.
    pub fn tempo_map(
        &self,
        sample_rate: u32,
    ) -> Result<audio_core::TempoMap, audio_core::TempoMapError> {
        self.project.session.build_tempo_map(sample_rate)
    }

    /// Sets the time signature (SPEC-026).
    ///
    /// # Errors
    ///
    /// [`EditorError::InvalidTimeSignature`] for an unsupported signature;
    /// `self` is unchanged.
    pub fn set_time_signature(
        &mut self,
        numerator: u8,
        denominator: u8,
    ) -> Result<(), EditorError> {
        let signature = audio_core::TimeSignature {
            numerator,
            denominator,
        };
        if !audio_core::valid_time_signature(signature) {
            return Err(EditorError::InvalidTimeSignature);
        }
        if signature == self.project.session.time_signature {
            return Ok(());
        }
        self.record();
        self.project.session.time_signature = signature;
        self.dirty = true;
        Ok(())
    }

    /// Remembers the session before an edit (once per group).
    fn record(&mut self) {
        let pre_pin = self.pre_pin.take();
        match self.group {
            Some(true) => return,
            Some(false) => self.group = Some(true),
            None => {}
        }
        if self.undo.len() == UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo
            .push(pre_pin.unwrap_or_else(|| self.project.session.clone()));
        self.redo.clear();
    }

    /// Starts grouping edits into one undo step; does nothing if a group is
    /// already open.
    pub fn begin_group(&mut self) {
        self.group.get_or_insert(false);
    }

    /// Ends the open group, if any.
    pub fn end_group(&mut self) {
        self.group = None;
    }

    /// Whether [`Self::undo`] has a step to go back to.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`Self::redo`] has an undone step to repeat.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Returns to the session before the last edit; `false` if there is none.
    pub fn undo(&mut self) -> bool {
        let Some(session) = self.undo.pop() else {
            return false;
        };
        let current = self.restore(session);
        self.redo.push(current);
        true
    }

    /// Repeats the last undone edit; `false` if there is none.
    pub fn redo(&mut self) -> bool {
        let Some(session) = self.redo.pop() else {
            return false;
        };
        let current = self.restore(session);
        self.undo.push(current);
        true
    }

    /// Puts `session` in place, keeping the project name; returns the
    /// replaced session.
    fn restore(&mut self, mut session: Session) -> Session {
        self.group = None;
        session.name.clone_from(&self.project.session.name);
        let current = std::mem::replace(&mut self.project.session, session);
        let tracks = &self.project.session.tracks;
        let folders = &self.project.session.folders;
        self.selected_track = self.selected_track.filter(|&t| t < tracks.len());
        self.selected_folder = self.selected_folder.filter(|&f| f < folders.len());
        self.more_folders.retain(|&f| f < folders.len());
        self.selected_clip = self
            .selected_clip
            .filter(|&(t, c)| tracks.get(t).is_some_and(|tr| c < tr.clips.len()));
        self.more_clips
            .retain(|&(t, c)| tracks.get(t).is_some_and(|tr| c < tr.clips.len()));
        self.more_tracks.retain(|&t| t < tracks.len());
        let mut saved = self.saved.clone();
        saved.name.clone_from(&self.project.session.name);
        self.dirty = self.project.session != saved;
        current
    }

    /// The open session.
    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.project.session
    }

    /// Where the project was opened from or last saved to.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether there are unsaved changes.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Waveform of a clip's source file, if it could be loaded.
    #[must_use]
    pub fn peaks(&self, clip: &Clip) -> Option<&Peaks> {
        self.waves.get(clip.source.as_deref()?)
    }

    /// Left and right waveforms of a clip whose source is a stereo file
    /// (SPEC-050 §5).
    #[must_use]
    pub fn stereo_peaks(&self, clip: &Clip) -> Option<&(Peaks, Peaks)> {
        self.stereo_waves.get(clip.source.as_deref()?)
    }

    /// Window title: file name and `*` for unsaved changes.
    #[must_use]
    pub fn title(&self) -> String {
        let name = self
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| "untitled".into(), |n| n.to_string_lossy());
        let mark = if self.dirty { " *" } else { "" };
        format!("Sound Pro — {name}{mark}")
    }
}

fn midi_notes(
    session: &mut Session,
    track: usize,
    clip: usize,
) -> Result<&mut Vec<Note>, EditorError> {
    session
        .tracks
        .get_mut(track)
        .and_then(|t| t.clips.get_mut(clip))
        .ok_or(EditorError::NoClip)?
        .notes
        .as_mut()
        .ok_or(EditorError::InvalidNote)
}

/// Shortens a clip's fades so both fit in its length (SPEC-038).
fn fit_fades(clip: &mut Clip) {
    clip.fade_in_samples = clip.fade_in_samples.min(clip.length_samples);
    clip.fade_out_samples = clip
        .fade_out_samples
        .min(clip.length_samples - clip.fade_in_samples);
}

/// Notes of `clip` heard on the timeline, moved so that the clip's visible
/// start sits at `shift` and cut at the clip's end (SPEC-038 §5).
fn visible_notes(notes: &[Note], clip: &Clip, shift: u64) -> Vec<Note> {
    let from = clip.offset_samples;
    let to = from + clip.length_samples;
    notes
        .iter()
        .filter(|n| n.start >= from && n.start < to)
        .map(|n| Note {
            start: n.start - from + shift,
            length: n.length.min(to - n.start),
            ..*n
        })
        .collect()
}

fn insert_note(notes: &mut Vec<Note>, note: Note) -> usize {
    let index = notes.partition_point(|n| (n.start, n.pitch) <= (note.start, note.pitch));
    notes.insert(index, note);
    index
}

/// Remaps every folder child index through `f` (SPEC-055). Used when a track
/// moves, is duplicated or is deleted so the child lists keep addressing the
/// right [`Track`]s.
fn remap_folder_children(children: &mut [Vec<usize>], f: impl Fn(usize) -> usize) {
    for list in children {
        for child in list {
            *child = f(*child);
        }
    }
}

/// A track's effect chain, provided `index` is inside it.
/// Where chain slot `slot` lands when the effect at `from` moves to `to`.
fn moved_slot(slot: u8, from: usize, to: usize) -> u8 {
    let index = usize::from(slot);
    let moved = if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    };
    u8::try_from(moved).unwrap_or(slot)
}

fn lane_mut(
    session: &mut Session,
    track: usize,
    lane: Lane,
) -> Result<&mut Vec<AutoPoint>, EditorError> {
    session
        .tracks
        .get_mut(track)
        .map(|track| track.automation.lane_mut(lane))
        .ok_or(EditorError::NoPoint)
}

fn bus_effects_mut(
    session: &mut Session,
    bus: usize,
    index: usize,
) -> Result<&mut Vec<Effect>, EditorError> {
    session
        .buses
        .get_mut(bus)
        .map(|b| &mut b.effects)
        .filter(|effects| index < effects.len())
        .ok_or(EditorError::NoBus)
}

fn effects_mut(
    session: &mut Session,
    track: usize,
    index: usize,
) -> Result<&mut Vec<Effect>, EditorError> {
    session
        .tracks
        .get_mut(track)
        .map(|t| &mut t.effects)
        .filter(|effects| index < effects.len())
        .ok_or(EditorError::NoEffect)
}

/// The stopped-playhead target for a user seek: the raw sample position,
/// unclamped. The playhead may sit anywhere on the timeline, including past
/// the last region or on an empty project (SPEC-040). A running transport
/// clamps itself to its own end, so this value is only ever stored or
/// forwarded — never used to index audio.
#[must_use]
pub const fn playhead_seek_target(samples: u64) -> u64 {
    samples
}

/// Horizontal offset of `samples` at `zoom` pixels per second.
#[must_use]
pub fn samples_to_x(samples: u64, rate: u32, zoom: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let seconds = samples as f64 / f64::from(rate.max(1));
    #[allow(clippy::cast_possible_truncation)]
    let x = (seconds * f64::from(zoom)) as f32;
    x
}

/// Sample position under a horizontal offset `x` at `zoom` pixels per
/// second; negative offsets give 0.
#[must_use]
pub fn x_to_samples(x: f32, rate: u32, zoom: f32) -> u64 {
    if !x.is_finite() || x <= 0.0 || !zoom.is_finite() || zoom <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (f64::from(x) / f64::from(zoom) * f64::from(rate)).round() as u64;
    samples
}

/// Where a file dropped at offset `x` starts: under the pointer, snapped to
/// the beat like regions when `snap` is on (SPEC-050 §2). The preview frame
/// and the drop use the same value.
#[must_use]
pub fn drop_start(x: f32, rate: u32, zoom: f32, tempo_bpm: f64, snap: bool) -> u64 {
    let samples = x_to_samples(x, rate, zoom);
    if snap {
        let map = audio_core::TempoMap::single_or_default(tempo_bpm, rate);
        snap_to_beat(samples, &map)
    } else {
        samples
    }
}

/// Converts a pointer position reported in native (physical) pixels — the
/// space the OS and the XDND drag protocol use — into an offset from the
/// timeline area's top-left corner, in egui points.
///
/// This is the single place the OS scale factor (`pixels_per_point`, DPI) and
/// the window origin are accounted for, so a dropped file lands under the
/// cursor regardless of screen size or where the window sits on the desktop
/// (SPEC-050 §2). A non-finite or non-positive scale falls back to `1.0`.
#[must_use]
pub fn native_pointer_to_timeline(
    pointer_native: (f32, f32),
    window_origin_native: (f32, f32),
    pixels_per_point: f32,
    timeline_origin_points: (f32, f32),
) -> (f32, f32) {
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    (
        (pointer_native.0 - window_origin_native.0) / scale - timeline_origin_points.0,
        (pointer_native.1 - window_origin_native.1) / scale - timeline_origin_points.1,
    )
}

/// Where a dropped audio file lands (SPEC-050 §2, §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropTarget {
    /// Existing audio track under the pointer; `None` for a new track.
    pub track: Option<usize>,
    /// Region start in samples.
    pub start: u64,
}

/// Picks the track and start for a file released at (`x`, `y`), measured from
/// the top-left of the timeline area. A row of an audio track takes the file;
/// the ruler, the empty area below the last track, an instrument track or no
/// known pointer (`None`) mean a new track. The preview frame and the drop
/// call this with the same arguments, so they agree.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn drop_target(
    pointer: Option<(f32, f32)>,
    ruler_height: f32,
    row_height: f32,
    audio_rows: &[bool],
    rate: u32,
    zoom: f32,
    tempo_bpm: f64,
    snap: bool,
) -> DropTarget {
    let Some((x, y)) = pointer.filter(|(x, y)| x.is_finite() && y.is_finite()) else {
        return DropTarget {
            track: None,
            start: 0,
        };
    };
    let row = ((y - ruler_height) / row_height.max(1.0)).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let track = (row >= 0.0)
        .then_some(row as usize)
        .filter(|&r| audio_rows.get(r).copied().unwrap_or(false));
    DropTarget {
        track,
        start: drop_start(x.max(0.0), rate, zoom, tempo_bpm, snap),
    }
}

/// Remembers where the preview frame was last drawn so the drop lands there
/// (SPEC-050 §2).
///
/// While a file is dragged over the window the platform may stop reporting
/// the pointer (X11/XWayland send no pointer motion during a drag), so the
/// position seen on the frame of the drop can be missing or stale. The frame
/// the user watched is the one that counts.
#[derive(Debug, Default, Clone, Copy)]
pub struct DropTracker {
    hovered: Option<DropTarget>,
}

impl DropTracker {
    /// Records the target drawn for the current hover frame; `None` (no
    /// pointer known) keeps the previous one.
    pub fn hover(&mut self, target: Option<DropTarget>) {
        if target.is_some() {
            self.hovered = target;
        }
    }

    /// Forgets the hover, e.g. when the drag left the window without a drop.
    pub fn cancel(&mut self) {
        self.hovered = None;
    }

    /// The target for a file released now. The last hover target wins over
    /// `at_release`, the target computed from the pointer on the drop frame;
    /// without either the file gets a new track at 0. Clears the hover.
    pub fn release(&mut self, at_release: Option<DropTarget>) -> DropTarget {
        self.hovered.take().or(at_release).unwrap_or(DropTarget {
            track: None,
            start: 0,
        })
    }
}

/// The beat (4/4 grid) nearest to `samples`; halfway rounds up. A bad tempo
/// falls back to [`DEFAULT_TEMPO`]. Routes through the tempo map so a
/// multi-point map snaps against its own grid.
#[must_use]
pub fn snap_to_beat(samples: u64, map: &audio_core::TempoMap) -> u64 {
    let beat = map.samples_per_beat_at(samples);
    #[allow(clippy::cast_precision_loss)]
    let beats = (samples as f64 / beat + 0.5).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let snapped = (beats * beat).round() as u64;
    snapped
}

/// Length of a 1/16 note (a quarter beat) in samples, at least 1; the
/// piano roll grid (SPEC-021). Routes through the tempo map.
#[must_use]
pub fn sixteenth_samples(map: &audio_core::TempoMap) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step = (map.samples_per_beat_at(0) / 4.0).round() as u64;
    step.max(1)
}

/// Clock time `m:ss.mmm` of a sample position.
#[must_use]
pub fn clock(samples: u64, rate: u32) -> String {
    let rate = u64::from(rate.max(1));
    let millis = samples * 1000 / rate;
    format!(
        "{}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

/// Musical position `bar.beat` in 4/4, both counted from 1.
#[must_use]
pub fn bars(samples: u64, map: &audio_core::TempoMap) -> String {
    bars_in(samples, map, 4)
}

/// Like [`bars`], with `beats_per_bar` beats in a bar (SPEC-026); zero
/// counts as 4.
#[must_use]
pub fn bars_in(samples: u64, map: &audio_core::TempoMap, beats_per_bar: u32) -> String {
    let per_bar = u64::from(if beats_per_bar == 0 { 4 } else { beats_per_bar });
    let beat = map.samples_to_beats(samples);
    format!("{}.{}", beat / per_bar + 1, beat % per_bar + 1)
}

/// Reads a WAV or MP3 file as its mono average at `rate` (SPEC-019,
/// SPEC-032). Slow for long files; the window calls it off its thread and
/// hands the result to [`Editor::import_decoded`] (SPEC-035).
///
/// # Errors
///
/// [`EditorError::EmptyFile`], [`EditorError::Wav`] or
/// [`EditorError::Decode`].
pub fn read_samples(path: &Path, rate: u32) -> Result<Vec<f32>, EditorError> {
    read_source(path, rate).map(|c| c.mono())
}

/// Reads a WAV or MP3 file at `rate`, keeping a stereo file's two channels
/// (SPEC-050 §5). Slow for long files; call it off the window's thread.
///
/// # Errors
///
/// As [`read_samples`].
pub fn read_source(path: &Path, rate: u32) -> Result<daw_app::audio_file::Channels, EditorError> {
    daw_app::audio_file::read_channels_at(path, rate).map_err(|e| file_error(path, e))
}

/// [`read_source`] for import: looks in `cache` first (path, mtime, size and
/// rate must match), uses the fast resampler, calls `progress` with the
/// source rate and the channels decoded so far, and stores the result.
///
/// # Errors
///
/// As [`read_samples`].
pub fn read_source_cached(
    path: &Path,
    rate: u32,
    cache: Option<&daw_app::pcm_cache::PcmCache>,
    progress: daw_app::audio_file::Progress<'_>,
) -> Result<daw_app::audio_file::Channels, EditorError> {
    if let Some(hit) = cache.and_then(|c| c.load(path, rate)) {
        return Ok(hit);
    }
    let stamp = daw_app::pcm_cache::Stamp::of(path);
    let channels = daw_app::audio_file::read_channels_fast_at(path, rate, progress)
        .map_err(|e| file_error(path, e))?;
    if let (Some(cache), Some(stamp)) = (cache, stamp) {
        cache.store(&stamp, rate, &channels);
    }
    Ok(channels)
}

/// Length of a WAV or MP3 file in frames at `rate`, read from its header
/// without decoding (SPEC-050 §2).
///
/// # Errors
///
/// As [`read_samples`].
pub fn probe_len(path: &Path, rate: u32) -> Result<u64, EditorError> {
    daw_app::audio_file::probe_len_at(path, rate).map_err(|e| file_error(path, e))
}

fn file_error(path: &Path, e: daw_app::audio_file::AudioFileError) -> EditorError {
    use daw_app::audio_file::AudioFileError;
    match e {
        AudioFileError::Empty | AudioFileError::Wav(audio_core::WavError::Empty) => {
            EditorError::EmptyFile {
                path: path.to_path_buf(),
            }
        }
        AudioFileError::Wav(source) => EditorError::Wav {
            path: path.to_path_buf(),
            source,
        },
        AudioFileError::Decode(message) => EditorError::Decode {
            path: path.to_path_buf(),
            message,
        },
    }
}

type NotesKey = (String, Vec<Note>, u32, u64);

/// MIDI clips rendered by the last load, keyed by instrument id, notes, rate
/// and length (SPEC-065).
#[derive(Debug, Default)]
struct RenderedNotes(HashMap<NotesKey, Arc<Vec<f32>>>);

/// The editor's material as a [`daw_app::ClipCache`]; keeps only renders
/// the current session still uses.
struct EditorCache<'a> {
    sounds: &'a HashMap<String, Arc<Vec<f32>>>,
    stereo: &'a HashMap<String, daw_app::StereoSamples>,
    rendered: &'a mut RenderedNotes,
    used: HashMap<NotesKey, Arc<Vec<f32>>>,
}

impl daw_app::ClipCache for EditorCache<'_> {
    fn audio(&mut self, source: &str) -> Option<Arc<Vec<f32>>> {
        self.sounds.get(source).cloned()
    }

    fn audio_stereo(&mut self, source: &str) -> Option<daw_app::StereoSamples> {
        self.stereo.get(source).cloned()
    }

    fn notes(
        &mut self,
        instrument: &str,
        notes: &[Note],
        rate: u32,
        frames: u64,
    ) -> Option<Arc<Vec<f32>>> {
        let key = (instrument.to_owned(), notes.to_vec(), rate, frames);
        let hit = self
            .used
            .get(&key)
            .or_else(|| self.rendered.0.get(&key))
            .cloned()?;
        self.used.insert(key, Arc::clone(&hit));
        Some(hit)
    }

    fn store_notes(
        &mut self,
        instrument: &str,
        notes: &[Note],
        rate: u32,
        frames: u64,
        samples: Arc<Vec<f32>>,
    ) {
        self.used.insert(
            (instrument.to_owned(), notes.to_vec(), rate, frames),
            samples,
        );
    }
}

/// File stem, cut to [`MAX_NAME_BYTES`] on a character boundary.
fn clip_name(path: &Path) -> String {
    truncate(
        &path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
    )
}

fn truncate(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if out.len() + ch.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        out.push(ch);
    }
    out
}

/// Saved relative sources always use `/`; rebuild them with native separators.
fn resolve_source(base: &Path, source: &str) -> PathBuf {
    if Path::new(source).is_absolute() {
        return PathBuf::from(source);
    }
    let mut path = base.to_path_buf();
    path.extend(source.split('/').filter(|part| !part.is_empty()));
    path
}

/// `source` relative to `base` with `/` separators, or unchanged when it
/// lies outside `base`.
fn relative_source(base: &Path, source: &str) -> String {
    let Ok(rest) = Path::new(source).strip_prefix(base) else {
        return source.to_owned();
    };
    let parts: Option<Vec<&str>> = rest.components().map(|c| c.as_os_str().to_str()).collect();
    match parts {
        Some(parts) if !parts.is_empty() => parts.join("/"),
        _ => source.to_owned(),
    }
}
