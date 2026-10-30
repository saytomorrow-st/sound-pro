//! Sound Pro desktop editor (SPEC-015 to SPEC-018).
//!
//! [`Editor`] is the window's model: the open project, its file path, the
//! selection and the waveform cache. It knows nothing about drawing, so every
//! action the window offers is tested without a display. The window itself
//! lives in `main.rs` and only calls into this type.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod layout;
pub mod timeline;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use audio_core::session::{AutoPoint, Clip, Lane, Note, Session, Track};
use audio_core::{
    Bus, BusSend, Effect, EffectKind, MAX_CLIPS_PER_TRACK, MAX_NAME_BYTES, Marker, Project,
    ProjectError, SessionError,
};

/// Sample rate of a new project.
pub const DEFAULT_RATE: u32 = 48_000;
/// Tempo of a new project.
pub const DEFAULT_TEMPO: f64 = 120.0;
/// Slowest tempo the window allows (SPEC-026).
pub const MIN_TEMPO: f64 = 20.0;
/// Fastest tempo the window allows (SPEC-026).
pub const MAX_TEMPO: f64 = 999.0;
/// Frames summarised by one [`Peaks`] entry.
pub const PEAK_BLOCK: usize = 256;

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
    /// The MP3 file could not be decoded (SPEC-032).
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
    path: Option<PathBuf>,
    dirty: bool,
    waves: HashMap<String, Peaks>,
    /// Decoded mono files at the project rate, so playback starts without
    /// reading them again (SPEC-035).
    sounds: HashMap<String, Arc<Vec<f32>>>,
    /// Rendered MIDI clips of the last playback or export (SPEC-035).
    rendered: Mutex<RenderedNotes>,
    undo: Vec<Session>,
    redo: Vec<Session>,
    /// The session as last opened or saved.
    saved: Session,
    /// `Some(recorded)` while edits are grouped into one step.
    group: Option<bool>,
    /// Selected track, if any.
    pub selected_track: Option<usize>,
    /// Selected clip as `(track, clip)`, if any.
    pub selected_clip: Option<(usize, usize)>,
    /// Cycle region `[start, end)` of the window; not saved (SPEC-025).
    cycle: Option<(u64, u64)>,
    /// Whether playback repeats [`Self::cycle`].
    pub cycle_enabled: bool,
    /// Whether playback clicks on every beat; not saved (SPEC-026).
    pub metronome: bool,
    /// Clip copied with [`Self::copy_clip`]; not saved (SPEC-027).
    clipboard: Option<Clip>,
    /// Track armed for recording; not saved (SPEC-038).
    armed: Option<usize>,
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
            rendered: Mutex::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            saved: session,
            group: None,
            selected_track: None,
            selected_clip: None,
            cycle: None,
            cycle_enabled: false,
            metronome: false,
            armed: None,
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
                    if let Ok(samples) = read_samples(&resolved, rate) {
                        waves.insert(source.clone(), Peaks::from_samples(&samples));
                        sounds.insert(source.clone(), Arc::new(samples));
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
            rendered: Mutex::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            selected_track: None,
            selected_clip: None,
            cycle: None,
            cycle_enabled: false,
            metronome: self.metronome,
            clipboard: None,
            armed: None,
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
        let samples = read_samples(path, self.project.session.sample_rate)?;
        self.import_decoded(path, samples, track, start)
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
        let track_index = match track.filter(|&t| t < session.tracks.len()) {
            Some(t) => t,
            None => {
                session.tracks.push(Track::new(&name));
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
    /// free `rec-NNN.wav` in `dir`; the clip goes on the selected track, or a
    /// new "Запись" track. One undo step.
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
        let track_index = match self
            .armed_track()
            .or(self.selected_track)
            .filter(|&t| t < session.tracks.len())
        {
            Some(t) => t,
            None => {
                session.tracks.push(Track::new("Запись"));
                session.tracks.len() - 1
            }
        };
        let track = &mut session.tracks[track_index];
        let mut clip = Clip::full(start, audio.len() as u64);
        track.clips.push(clip.clone());
        let clip_index = track.clips.len() - 1;
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
        session.tracks[track_index].clips[clip_index] = clip;

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

    /// Loads the session for rendering from the decoded files and rendered
    /// notes this editor already holds (SPEC-035).
    fn load(&self, output: PathBuf) -> Result<daw_app::SessionJob, EditorError> {
        let mut rendered = self
            .rendered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut cache = EditorCache {
            sounds: &self.sounds,
            rendered: &mut rendered,
            used: HashMap::new(),
        };
        let job = daw_app::load_session_cached(
            self.project.session.clone(),
            Path::new(""),
            output,
            &mut cache,
        )?;
        let used = cache.used;
        rendered.0 = used;
        Ok(job)
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
        let step = (sixteenth_samples(session.sample_rate, session.tempo_bpm) * 16
            / u64::from(division))
        .max(1);
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

    /// Track armed for recording, if any (SPEC-038 §9).
    #[must_use]
    pub fn armed_track(&self) -> Option<usize> {
        self.armed
            .filter(|&t| t < self.project.session.tracks.len())
    }

    /// Arms one track for recording, or none; the project is untouched.
    pub fn arm_track(&mut self, track: Option<usize>) {
        self.armed = track.filter(|&t| t < self.project.session.tracks.len());
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
        let bar = sixteenth_samples(session.sample_rate, session.tempo_bpm) * 4 * beats;
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
        let bar = sixteenth_samples(session.sample_rate, session.tempo_bpm) * 4 * beats;
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
        let mut track = Track::new("MIDI");
        let mut clip = Clip::midi(start, length);
        clip.name = "MIDI".to_owned();
        track.clips.push(clip);
        session.tracks.push(track);
        let at = (session.tracks.len() - 1, 0);
        self.commit(session, at)?;
        Ok(at)
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

    /// Moves a track to position `to`; the others shift (SPEC-029).
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] when either index is out of range.
    pub fn move_track(&mut self, from: usize, to: usize) -> Result<(), EditorError> {
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
        self.commit_track(session, to)?;
        self.armed = self.armed.map(|a| {
            if a == from {
                to
            } else if from < a && a <= to {
                a - 1
            } else if to <= a && a < from {
                a + 1
            } else {
                a
            }
        });
        self.selected_clip = None;
        Ok(())
    }

    /// Inserts a copy of a track right below it and selects the copy
    /// (SPEC-029). Returns the copy's index.
    ///
    /// # Errors
    ///
    /// [`EditorError::NoClip`] for a bad index, [`EditorError::Session`] when
    /// the session already holds [`audio_core::MAX_TRACKS`] tracks.
    pub fn duplicate_track(&mut self, track: usize) -> Result<usize, EditorError> {
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
        self.commit_track(session, track + 1)?;
        self.armed = self.armed.map(|a| if a > track { a + 1 } else { a });
        self.selected_clip = None;
        Ok(track + 1)
    }

    /// Removes one track and its clips; out-of-range indices do nothing.
    pub fn delete_track(&mut self, track: usize) {
        if track < self.project.session.tracks.len() {
            self.record();
            self.project.session.tracks.remove(track);
            self.armed = match self.armed {
                Some(a) if a == track => None,
                Some(a) if a > track => Some(a - 1),
                other => other,
            };
            self.selected_track = None;
            self.selected_clip = None;
            self.dirty = true;
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

    /// Sets the tempo, clamped to `20..=999` BPM (SPEC-026). A value that is
    /// not finite, or the current tempo, changes nothing.
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
        match self.group {
            Some(true) => return,
            Some(false) => self.group = Some(true),
            None => {}
        }
        if self.undo.len() == UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(self.project.session.clone());
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
        self.selected_track = self.selected_track.filter(|&t| t < tracks.len());
        self.selected_clip = self
            .selected_clip
            .filter(|&(t, c)| tracks.get(t).is_some_and(|tr| c < tr.clips.len()));
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

/// The beat (4/4 grid) nearest to `samples`; halfway rounds up. A bad tempo
/// falls back to [`DEFAULT_TEMPO`].
#[must_use]
pub fn snap_to_beat(samples: u64, rate: u32, tempo_bpm: f64) -> u64 {
    let tempo = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
        tempo_bpm
    } else {
        DEFAULT_TEMPO
    };
    let beat = f64::from(rate.max(1)) * 60.0 / tempo;
    #[allow(clippy::cast_precision_loss)]
    let beats = (samples as f64 / beat + 0.5).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let snapped = (beats * beat).round() as u64;
    snapped
}

/// Length of a 1/16 note (a quarter beat) in samples, at least 1; the
/// piano roll grid (SPEC-021). A bad tempo falls back to [`DEFAULT_TEMPO`].
#[must_use]
pub fn sixteenth_samples(rate: u32, tempo_bpm: f64) -> u64 {
    let tempo = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
        tempo_bpm
    } else {
        DEFAULT_TEMPO
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step = (f64::from(rate) * 15.0 / tempo).round() as u64;
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
pub fn bars(samples: u64, rate: u32, tempo_bpm: f64) -> String {
    bars_in(samples, rate, tempo_bpm, 4)
}

/// Like [`bars`], with `beats_per_bar` beats in a bar (SPEC-026); zero
/// counts as 4.
#[must_use]
pub fn bars_in(samples: u64, rate: u32, tempo_bpm: f64, beats_per_bar: u32) -> String {
    let per_bar = u64::from(if beats_per_bar == 0 { 4 } else { beats_per_bar });
    let tempo = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
        tempo_bpm
    } else {
        DEFAULT_TEMPO
    };
    #[allow(clippy::cast_precision_loss)]
    let beats = samples as f64 / f64::from(rate.max(1)) * tempo / 60.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let beat = (beats + 1e-9).floor() as u64;
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
    use daw_app::audio_file::AudioFileError;
    let mono = daw_app::audio_file::read_mono_at(path, rate).map_err(|e| match e {
        AudioFileError::Empty | AudioFileError::Wav(audio_core::WavError::Empty) => {
            EditorError::EmptyFile {
                path: path.to_path_buf(),
            }
        }
        AudioFileError::Wav(source) => EditorError::Wav {
            path: path.to_path_buf(),
            source,
        },
        AudioFileError::Mp3(message) => EditorError::Decode {
            path: path.to_path_buf(),
            message,
        },
    })?;
    Ok(mono)
}

type NotesKey = (Vec<Note>, u32, u64);

/// MIDI clips rendered by the last load, keyed by notes, rate and length.
#[derive(Debug, Default)]
struct RenderedNotes(HashMap<NotesKey, Arc<Vec<f32>>>);

/// The editor's material as a [`daw_app::ClipCache`]; keeps only renders
/// the current session still uses.
struct EditorCache<'a> {
    sounds: &'a HashMap<String, Arc<Vec<f32>>>,
    rendered: &'a mut RenderedNotes,
    used: HashMap<NotesKey, Arc<Vec<f32>>>,
}

impl daw_app::ClipCache for EditorCache<'_> {
    fn audio(&mut self, source: &str) -> Option<Arc<Vec<f32>>> {
        self.sounds.get(source).cloned()
    }

    fn notes(&mut self, notes: &[Note], rate: u32, frames: u64) -> Option<Arc<Vec<f32>>> {
        let key = (notes.to_vec(), rate, frames);
        let hit = self
            .used
            .get(&key)
            .or_else(|| self.rendered.0.get(&key))
            .cloned()?;
        self.used.insert(key, Arc::clone(&hit));
        Some(hit)
    }

    fn store_notes(&mut self, notes: &[Note], rate: u32, frames: u64, samples: Arc<Vec<f32>>) {
        self.used.insert((notes.to_vec(), rate, frames), samples);
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
