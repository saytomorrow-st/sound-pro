//! Sound Pro desktop editor (SPEC-015 to SPEC-018).
//!
//! [`Editor`] is the window's model: the open project, its file path, the
//! selection and the waveform cache. It knows nothing about drawing, so every
//! action the window offers is tested without a display. The window itself
//! lives in `main.rs` and only calls into this type.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use audio_core::session::{AutoPoint, Clip, Lane, Note, Session, Track};
use audio_core::{
    Effect, EffectKind, MAX_CLIPS_PER_TRACK, MAX_NAME_BYTES, Project, ProjectError, SessionError,
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
    /// The WAV file holds no audio.
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
    /// A cycle region must not be empty (SPEC-025).
    #[error("the cycle region is empty")]
    EmptyCycle,
    /// The time signature is not supported (SPEC-026).
    #[error("time signature is not supported")]
    InvalidTimeSignature,
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

/// The window's model: one open project and what the user selected.
#[derive(Debug)]
pub struct Editor {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    waves: HashMap<String, Peaks>,
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
            undo: Vec::new(),
            redo: Vec::new(),
            saved: session,
            group: None,
            selected_track: None,
            selected_clip: None,
            cycle: None,
            cycle_enabled: false,
            metronome: false,
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
                    if let Ok(peaks) = read_mono(&resolved, rate) {
                        waves.insert(source.clone(), peaks);
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
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            selected_track: None,
            selected_clip: None,
            cycle: None,
            cycle_enabled: false,
            metronome: self.metronome,
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
        let path = std::path::absolute(path).map_err(|source| EditorError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let source = path
            .to_str()
            .ok_or_else(|| EditorError::NonUtf8Path { path: path.clone() })?
            .to_owned();
        let peaks = read_mono(&path, self.project.session.sample_rate)?;
        if peaks.frames == 0 {
            return Err(EditorError::EmptyFile { path });
        }
        let name = clip_name(&path);
        let mut session = self.project.session.clone();
        let track_index = match self.selected_track.filter(|&t| t < session.tracks.len()) {
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
        let mut clip = Clip::full(track.length_samples(), peaks.frames);
        clip.name = name;
        clip.source = Some(source.clone());
        track.clips.push(clip);
        let clip_index = track.clips.len() - 1;
        session.validate()?;
        self.record();
        self.project.session = session;
        self.waves.insert(source, peaks);
        self.selected_track = Some(track_index);
        self.selected_clip = Some((track_index, clip_index));
        self.dirty = true;
        Ok((track_index, clip_index))
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
            audio_core::resample(samples, rate, project_rate)
                .map_err(|_| EditorError::EmptyRecording)?
        };
        if audio.is_empty() {
            return Err(EditorError::EmptyRecording);
        }
        let mut session = self.project.session.clone();
        let track_index = match self.selected_track.filter(|&t| t < session.tracks.len()) {
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
        self.waves.insert(source, Peaks::from_samples(&audio));
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
        let job = daw_app::load_session(
            self.project.session.clone(),
            Path::new(""),
            out.to_path_buf(),
        )?;
        daw_app::render_session(job).map_err(|e| EditorError::Render(e.to_string()))
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
        let mut job =
            daw_app::load_session(self.project.session.clone(), Path::new(""), PathBuf::new())?;
        job.block_frames = daw_app::playback::PLAYBACK_BLOCK;
        let rate = job.sample_rate;
        let graph =
            daw_app::build_session_graph(job).map_err(|e| EditorError::Render(e.to_string()))?;
        let mut start = if start >= end { 0 } else { start };
        let cycle = self.active_cycle();
        if let Some((from, to)) = cycle
            && (start < from || start >= to)
        {
            start = from.min(end);
        }
        let (mut control, engine) = daw_app::playback::playback(graph, rate, start, end);
        control.set_cycle(cycle);
        control.set_metronome(self.metronome);
        let session = &self.project.session;
        control.set_meter(
            session.tempo_bpm,
            u32::from(session.time_signature.numerator),
        );
        Ok((control, engine))
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
        session.tracks[track].clips[clip].length_samples = length;
        self.commit(session, (track, clip))
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
        self.commit_track(session, track)
    }

    fn commit_track(&mut self, session: Session, track: usize) -> Result<(), EditorError> {
        session.validate()?;
        self.record();
        self.project.session = session;
        self.selected_track = Some(track);
        self.dirty = true;
        Ok(())
    }

    /// Removes one track and its clips; out-of-range indices do nothing.
    pub fn delete_track(&mut self, track: usize) {
        if track < self.project.session.tracks.len() {
            self.record();
            self.project.session.tracks.remove(track);
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

fn insert_note(notes: &mut Vec<Note>, note: Note) -> usize {
    let index = notes.partition_point(|n| (n.start, n.pitch) <= (note.start, note.pitch));
    notes.insert(index, note);
    index
}

/// A track's effect chain, provided `index` is inside it.
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

/// Reads a WAV file as peaks of its mono average at `rate` (SPEC-019).
fn read_mono(path: &Path, rate: u32) -> Result<Peaks, EditorError> {
    let (spec, buffer) = audio_core::wav::read_wav_mono(path).map_err(|source| match source {
        audio_core::WavError::Empty => EditorError::EmptyFile {
            path: path.to_path_buf(),
        },
        source => EditorError::Wav {
            path: path.to_path_buf(),
            source,
        },
    })?;
    let channels = buffer.channels().max(1);
    let mut mono = buffer.channel(0).to_vec();
    for ch in 1..channels {
        for (out, sample) in mono.iter_mut().zip(buffer.channel(ch)) {
            *out += sample;
        }
    }
    if channels > 1 {
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / channels as f32;
        for out in &mut mono {
            *out *= scale;
        }
    }
    if spec.sample_rate != rate {
        mono = audio_core::resample(&mono, spec.sample_rate, rate).map_err(|_| {
            EditorError::EmptyFile {
                path: path.to_path_buf(),
            }
        })?;
    }
    Ok(Peaks::from_samples(&mono))
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
