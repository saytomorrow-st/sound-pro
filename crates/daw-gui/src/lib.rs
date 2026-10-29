//! Sound Pro desktop editor (SPEC-015).
//!
//! [`Editor`] is the window's model: the open project, its file path, the
//! selection and the waveform cache. It knows nothing about drawing, so every
//! action the window offers is tested without a display. The window itself
//! lives in `main.rs` and only calls into this type.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use audio_core::session::{Clip, Session, Track};
use audio_core::{MAX_CLIPS_PER_TRACK, MAX_NAME_BYTES, Project, ProjectError, SessionError};

/// Sample rate of a new project.
pub const DEFAULT_RATE: u32 = 48_000;
/// Tempo of a new project.
pub const DEFAULT_TEMPO: f64 = 120.0;
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
    /// The WAV file runs at another rate than the project.
    #[error("{path}: sample rate {file} Hz, project runs at {project} Hz")]
    SampleRate {
        /// File path.
        path: PathBuf,
        /// Rate stored in the file.
        file: u32,
        /// Project rate.
        project: u32,
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

/// The window's model: one open project and what the user selected.
#[derive(Debug)]
pub struct Editor {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    waves: HashMap<String, Peaks>,
    /// Selected track, if any.
    pub selected_track: Option<usize>,
    /// Selected clip as `(track, clip)`, if any.
    pub selected_clip: Option<(usize, usize)>,
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
        Self {
            project: Project {
                name: "untitled".to_owned(),
                sample_rate: DEFAULT_RATE,
                tempo_bpm: DEFAULT_TEMPO,
                params: Vec::new(),
                session: Session::new("untitled", DEFAULT_RATE, DEFAULT_TEMPO),
            },
            path: None,
            dirty: false,
            waves: HashMap::new(),
            selected_track: None,
            selected_clip: None,
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
                    if let Ok((_, peaks)) = read_mono(&resolved) {
                        waves.insert(source.clone(), peaks);
                    }
                }
            }
        }
        *self = Self {
            project,
            path: Some(path),
            dirty: false,
            waves,
            selected_track: None,
            selected_clip: None,
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
    /// File, rate and limit errors (see [`EditorError`]); `self` is unchanged.
    pub fn import_wav(&mut self, path: &Path) -> Result<(usize, usize), EditorError> {
        let path = std::path::absolute(path).map_err(|source| EditorError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let source = path
            .to_str()
            .ok_or_else(|| EditorError::NonUtf8Path { path: path.clone() })?
            .to_owned();
        let (rate, peaks) = read_mono(&path)?;
        if rate != self.project.sample_rate {
            return Err(EditorError::SampleRate {
                path,
                file: rate,
                project: self.project.sample_rate,
            });
        }
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
        self.project.session = session;
        self.waves.insert(source, peaks);
        self.selected_track = Some(track_index);
        self.selected_clip = Some((track_index, clip_index));
        self.dirty = true;
        Ok((track_index, clip_index))
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

    /// Removes one clip; out-of-range indices do nothing.
    pub fn delete_clip(&mut self, track: usize, clip: usize) {
        let Some(t) = self.project.session.tracks.get_mut(track) else {
            return;
        };
        if clip < t.clips.len() {
            t.clips.remove(clip);
            self.selected_clip = None;
            self.dirty = true;
        }
    }

    /// Removes one track and its clips; out-of-range indices do nothing.
    pub fn delete_track(&mut self, track: usize) {
        if track < self.project.session.tracks.len() {
            self.project.session.tracks.remove(track);
            self.selected_track = None;
            self.selected_clip = None;
            self.dirty = true;
        }
    }

    /// Mutable access to one track's mixer settings; marks the project
    /// changed.
    pub fn track_mut(&mut self, track: usize) -> Option<&mut Track> {
        let t = self.project.session.tracks.get_mut(track)?;
        self.dirty = true;
        Some(t)
    }

    /// Sets the master gain, clamped to `0.0..=1.0`.
    pub fn set_master_gain(&mut self, gain: f32) {
        self.project.session.master_gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.dirty = true;
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

/// Reads a WAV file as `(rate, peaks of its mono average)`.
fn read_mono(path: &Path) -> Result<(u32, Peaks), EditorError> {
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
    Ok((spec.sample_rate, Peaks::from_samples(&mono)))
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

/// `source` relative to `base` with `/` separators, or unchanged when it
/// lies outside `base`.
/// Saved relative sources always use `/`; rebuild them with native separators.
fn resolve_source(base: &Path, source: &str) -> PathBuf {
    if Path::new(source).is_absolute() {
        return PathBuf::from(source);
    }
    let mut path = base.to_path_buf();
    path.extend(source.split('/').filter(|part| !part.is_empty()));
    path
}

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
