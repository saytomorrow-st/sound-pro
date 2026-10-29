//! Session model: tracks, clips and how they sit on the timeline (SPEC-010).
//!
//! This module is plain data plus its invariants. It knows nothing about the
//! graph, the audio thread, files or the clock: the shell builds nodes from a
//! [`Session`], and the project document (SPEC-008) stores one.

use std::fmt;

use crate::fx::{Effect, MAX_EFFECTS_PER_TRACK};
use crate::project::MAX_NAME_BYTES;

/// Most tracks a session may hold.
pub const MAX_TRACKS: usize = 32;

/// Most clips a single track may hold.
pub const MAX_CLIPS_PER_TRACK: usize = 128;

/// Longest accepted clip source path, in bytes.
pub const MAX_SOURCE_BYTES: usize = 4096;

/// Most notes a single MIDI clip may hold (SPEC-021).
pub const MAX_NOTES_PER_CLIP: usize = 4096;

/// Most points one automation lane may hold (SPEC-023).
pub const MAX_AUTOMATION_POINTS: usize = 1024;

/// Why a session (or one of its clips) is inconsistent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// `sample_rate` is zero.
    InvalidSampleRate,
    /// `tempo_bpm` is not finite or not positive.
    InvalidTempo,
    /// A session, track or clip name is longer than [`MAX_NAME_BYTES`].
    NameTooLong,
    /// The track list is longer than [`MAX_TRACKS`].
    TooManyTracks {
        /// Accepted maximum.
        max: usize,
    },
    /// A track holds more than [`MAX_CLIPS_PER_TRACK`] clips.
    TooManyClips {
        /// Index of the offending track.
        track: usize,
        /// Accepted maximum.
        max: usize,
    },
    /// A clip's source path is empty or longer than [`MAX_SOURCE_BYTES`].
    InvalidSource {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A clip has zero visible length.
    EmptyClip {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A clip's source has zero length.
    EmptySource {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// The clip reads past the end of its source.
    ClipOutOfSource {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// `start + length` does not fit in `u64`.
    TimelineOverflow {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A track gain is not finite or outside `[0, 1]`.
    InvalidGain {
        /// Index of the offending track.
        track: usize,
    },
    /// A track pan is not finite or outside `[-1, 1]`.
    InvalidPan {
        /// Index of the offending track.
        track: usize,
    },
    /// The master gain is not finite or outside `[0, 1]`.
    InvalidMasterGain,
    /// A clip gain is not finite or outside `[0, 1]`.
    InvalidClipGain {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A MIDI clip note is out of range, ends past the clip's content, or the
    /// clip has both notes and a file source (SPEC-021).
    InvalidNote {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
        /// Index of the offending note; [`MAX_NOTES_PER_CLIP`] when there are
        /// too many, `0` when the clip also has a `source`.
        note: usize,
    },
    /// An effect parameter is out of range, or the chain is longer than
    /// [`MAX_EFFECTS_PER_TRACK`] (SPEC-022).
    InvalidEffect {
        /// Index of the offending track.
        track: usize,
        /// Index of the offending effect; [`MAX_EFFECTS_PER_TRACK`] when there
        /// are too many.
        effect: usize,
    },
    /// An automation point is out of order or out of range, or the lane is
    /// longer than [`MAX_AUTOMATION_POINTS`] (SPEC-023).
    InvalidAutomation {
        /// Index of the offending track.
        track: usize,
        /// Which lane.
        lane: Lane,
        /// Index of the offending point; [`MAX_AUTOMATION_POINTS`] when there
        /// are too many.
        point: usize,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => write!(f, "sample rate must be non-zero"),
            Self::InvalidTempo => write!(f, "tempo must be finite and positive"),
            Self::NameTooLong => write!(f, "name longer than {MAX_NAME_BYTES} bytes"),
            Self::TooManyTracks { max } => write!(f, "more than {max} tracks"),
            Self::TooManyClips { track, max } => {
                write!(f, "track {track} holds more than {max} clips")
            }
            Self::InvalidSource { track, clip } => write!(
                f,
                "track {track} clip {clip}: source path is empty or longer than {MAX_SOURCE_BYTES} bytes"
            ),
            Self::EmptyClip { track, clip } => {
                write!(f, "clip {clip} of track {track} has zero length")
            }
            Self::EmptySource { track, clip } => {
                write!(f, "clip {clip} of track {track} has an empty source")
            }
            Self::ClipOutOfSource { track, clip } => write!(
                f,
                "clip {clip} of track {track} reads past the end of its source"
            ),
            Self::TimelineOverflow { track, clip } => {
                write!(f, "clip {clip} of track {track} ends past the timeline")
            }
            Self::InvalidGain { track } => {
                write!(f, "track {track} gain is not in [0, 1]")
            }
            Self::InvalidPan { track } => {
                write!(f, "track {track} pan is not in [-1, 1]")
            }
            Self::InvalidMasterGain => write!(f, "master gain is not in [0, 1]"),
            Self::InvalidClipGain { track, clip } => {
                write!(f, "clip {clip} of track {track} gain is not in [0, 1]")
            }
            Self::InvalidNote { track, clip, note } => {
                write!(f, "track {track} clip {clip}: note {note} is invalid")
            }
            Self::InvalidEffect { track, effect } => {
                write!(f, "track {track}: effect {effect} is invalid")
            }
            Self::InvalidAutomation { track, lane, point } => {
                write!(
                    f,
                    "track {track}: {lane} automation point {point} is invalid"
                )
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// One visible piece of content on a track's timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Where the clip starts on the timeline, in samples.
    pub start_samples: u64,
    /// Visible length, in samples; always `> 0`.
    pub length_samples: u64,
    /// How far into the content the clip starts (trimmed head).
    pub offset_samples: u64,
    /// Full length of the content, in samples.
    pub source_length: u64,
    /// Linear clip gain, `0.0..=1.0`.
    pub gain: f32,
    /// Optional label.
    pub name: String,
    /// Path to the clip's audio file, if the clip refers to one.
    ///
    /// The engine never opens it; the shell resolves it (SPEC-014).
    pub source: Option<String>,
    /// Notes of a MIDI clip (SPEC-021); `None` for an audio clip.
    pub notes: Option<Vec<Note>>,
}

/// One note of a MIDI clip, positioned inside the clip's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Note {
    /// MIDI pitch, `0..=127`; 60 is middle C.
    pub pitch: u8,
    /// Start, in samples from the beginning of the clip's content.
    pub start: u64,
    /// Duration in samples; always `> 0`.
    pub length: u64,
    /// Strength, `1..=127`.
    pub velocity: u8,
}

impl Note {
    /// Sample (exclusive) where the note is released, `None` on overflow.
    #[must_use]
    pub const fn end(&self) -> Option<u64> {
        self.start.checked_add(self.length)
    }

    /// Whether the note fits a clip whose content is `source_length` long.
    #[must_use]
    pub fn is_valid(&self, source_length: u64) -> bool {
        self.pitch <= 127
            && (1..=127).contains(&self.velocity)
            && self.length > 0
            && self.end().is_some_and(|end| end <= source_length)
    }
}

impl Clip {
    /// Clip that plays `source_length` samples from the very beginning.
    #[must_use]
    pub fn full(start_samples: u64, source_length: u64) -> Self {
        Self {
            start_samples,
            length_samples: source_length,
            offset_samples: 0,
            source_length,
            gain: 1.0,
            name: String::new(),
            source: None,
            notes: None,
        }
    }

    /// Empty MIDI clip of `length_samples`, playing from its beginning.
    #[must_use]
    pub fn midi(start_samples: u64, length_samples: u64) -> Self {
        Self {
            notes: Some(Vec::new()),
            ..Self::full(start_samples, length_samples)
        }
    }

    /// Whether this is a MIDI clip (it holds notes, not audio).
    #[must_use]
    pub const fn is_midi(&self) -> bool {
        self.notes.is_some()
    }

    /// Sample index on the timeline where the clip stops sounding (exclusive).
    ///
    /// Saturates instead of panicking; [`Session::validate`] rejects the
    /// overflowing case before any node sees it.
    #[must_use]
    pub fn end_samples(&self) -> u64 {
        self.start_samples.saturating_add(self.length_samples)
    }

    /// Whether timeline position `position` falls inside this clip.
    #[must_use]
    pub const fn covers(&self, position: u64) -> bool {
        position >= self.start_samples
            && position.wrapping_sub(self.start_samples) < self.length_samples
    }

    /// Index into the source for timeline position `position`.
    ///
    /// Only meaningful when [`Clip::covers`] is true.
    #[must_use]
    pub const fn source_index(&self, position: u64) -> u64 {
        self.offset_samples
            .wrapping_add(position.wrapping_sub(self.start_samples))
    }
}

/// Balance-law multipliers `(left, right)` for `pan` in `-1.0..=1.0`; both
/// are exactly `1.0` at centre (SPEC-013).
#[must_use]
pub fn pan_law(pan: f32) -> (f32, f32) {
    let left = if pan <= 0.0 { 1.0 } else { 1.0 - pan };
    let right = if pan >= 0.0 { 1.0 } else { 1.0 + pan };
    (left, right)
}

/// Which automated track parameter (SPEC-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Track gain, `0.0..=1.0`.
    Gain,
    /// Track pan, `-1.0..=1.0`.
    Pan,
}

impl Lane {
    /// Inclusive range of accepted values.
    #[must_use]
    pub const fn range(self) -> (f32, f32) {
        match self {
            Self::Gain => (0.0, 1.0),
            Self::Pan => (-1.0, 1.0),
        }
    }

    /// Whether `value` is finite and inside [`Lane::range`].
    #[must_use]
    pub fn accepts(self, value: f32) -> bool {
        let (low, high) = self.range();
        value.is_finite() && (low..=high).contains(&value)
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gain => "gain",
            Self::Pan => "pan",
        })
    }
}

/// One breakpoint of an automation lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoPoint {
    /// Timeline position in samples.
    pub time: u64,
    /// Parameter value at `time`.
    pub value: f32,
}

/// Breakpoint lanes for one track; points are strictly ordered by time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Automation {
    /// Gain lane.
    pub gain: Vec<AutoPoint>,
    /// Pan lane.
    pub pan: Vec<AutoPoint>,
}

impl Automation {
    /// `true` when neither lane has a point.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gain.is_empty() && self.pan.is_empty()
    }

    /// Points of one lane.
    #[must_use]
    pub fn lane(&self, lane: Lane) -> &[AutoPoint] {
        match lane {
            Lane::Gain => &self.gain,
            Lane::Pan => &self.pan,
        }
    }

    /// Mutable points of one lane.
    pub fn lane_mut(&mut self, lane: Lane) -> &mut Vec<AutoPoint> {
        match lane {
            Lane::Gain => &mut self.gain,
            Lane::Pan => &mut self.pan,
        }
    }

    /// Lane value at `time`: held before the first and after the last point,
    /// linear in between; `None` for an empty lane. Allocation-free.
    #[must_use]
    pub fn value_at(points: &[AutoPoint], time: u64) -> Option<f32> {
        let first = points.first()?;
        let next = points.partition_point(|point| point.time <= time);
        if next == 0 {
            return Some(first.value);
        }
        let before = points[next - 1];
        let Some(after) = points.get(next) else {
            return Some(before.value);
        };
        #[allow(clippy::cast_precision_loss)]
        let fraction = (time - before.time) as f64 / (after.time - before.time) as f64;
        #[allow(clippy::cast_possible_truncation)]
        let value =
            f64::from(before.value) + (f64::from(after.value) - f64::from(before.value)) * fraction;
        #[allow(clippy::cast_possible_truncation)]
        Some(value as f32)
    }
}

/// A horizontal lane holding clips.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Display name.
    pub name: String,
    /// Linear track gain, `0.0..=1.0`.
    pub gain: f32,
    /// Balance pan, `-1.0` (left) ..= `1.0` (right); `0.0` is centre.
    pub pan: f32,
    /// Silent track when `true`.
    pub mute: bool,
    /// When any track is soloed, only soloed, unmuted tracks sound.
    pub solo: bool,
    /// Clips in any order; nothing depends on `Vec` order for rendering.
    pub clips: Vec<Clip>,
    /// Insert effects, processed in order after the clips are summed and
    /// before gain and pan (SPEC-022).
    pub effects: Vec<Effect>,
    /// Gain and pan lanes; a non-empty lane overrides the slider (SPEC-023).
    pub automation: Automation,
}

impl Track {
    /// Empty, unmuted, centred track at unity gain.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            gain: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
            clips: Vec::new(),
            effects: Vec::new(),
            automation: Automation::default(),
        }
    }

    /// Balance-law multipliers `(left, right)`; both are exactly `1.0` at
    /// centre.
    #[must_use]
    pub fn pan_gains(&self) -> (f32, f32) {
        pan_law(self.pan)
    }

    /// Timeline position where the last clip of this track stops sounding.
    #[must_use]
    pub fn length_samples(&self) -> u64 {
        self.clips.iter().map(Clip::end_samples).max().unwrap_or(0)
    }
}

/// Everything a session renders: tracks, clips, rate and tempo.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Display name.
    pub name: String,
    /// Sample rate; clip content is expected to be at this rate already.
    pub sample_rate: u32,
    /// Tempo in beats per minute.
    pub tempo_bpm: f64,
    /// Linear master gain, `0.0..=1.0`, applied to the whole mix.
    pub master_gain: f32,
    /// Tracks in session order.
    pub tracks: Vec<Track>,
}

impl Session {
    /// Empty session with no tracks.
    #[must_use]
    pub fn new(name: &str, sample_rate: u32, tempo_bpm: f64) -> Self {
        Self {
            name: name.to_owned(),
            sample_rate,
            tempo_bpm,
            master_gain: 1.0,
            tracks: Vec::new(),
        }
    }

    /// The session [`crate::Project::capture`] stores: named after the project,
    /// with no tracks.
    #[must_use]
    pub fn new_untitled(sample_rate: u32, tempo_bpm: f64) -> Self {
        Self::new("untitled", sample_rate, tempo_bpm)
    }

    /// Timeline length: where the last clip ends, `0` for an empty session.
    #[must_use]
    pub fn length_samples(&self) -> u64 {
        self.tracks
            .iter()
            .map(Track::length_samples)
            .max()
            .unwrap_or(0)
    }

    /// Length of one track, `0` for an out-of-range index.
    #[must_use]
    pub fn track_length_samples(&self, track: usize) -> u64 {
        self.tracks.get(track).map_or(0, Track::length_samples)
    }

    /// Whether `track` sounds: not muted, and either nothing is soloed or it
    /// is. Mute beats solo; an out-of-range index is `false`.
    #[must_use]
    pub fn is_audible(&self, track: usize) -> bool {
        let Some(t) = self.tracks.get(track) else {
            return false;
        };
        !t.mute && (t.solo || !self.tracks.iter().any(|other| other.solo))
    }

    /// Checks every invariant the renderer relies on.
    ///
    /// The constructor for the model is the caller's literal, so this is the
    /// single gate: the project loader and the graph builder both call it and
    /// nothing downstream re-checks.
    ///
    /// # Errors
    ///
    /// Returns the first [`SessionError`] found, in track order and then clip
    /// order.
    pub fn validate(&self) -> Result<(), SessionError> {
        if self.name.len() > MAX_NAME_BYTES {
            return Err(SessionError::NameTooLong);
        }
        if self.sample_rate == 0 {
            return Err(SessionError::InvalidSampleRate);
        }
        if !self.tempo_bpm.is_finite() || self.tempo_bpm <= 0.0 {
            return Err(SessionError::InvalidTempo);
        }
        if !valid_gain(self.master_gain) {
            return Err(SessionError::InvalidMasterGain);
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(SessionError::TooManyTracks { max: MAX_TRACKS });
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            if track.name.len() > MAX_NAME_BYTES {
                return Err(SessionError::NameTooLong);
            }
            if !valid_gain(track.gain) {
                return Err(SessionError::InvalidGain { track: track_index });
            }
            if !(track.pan.is_finite() && (-1.0..=1.0).contains(&track.pan)) {
                return Err(SessionError::InvalidPan { track: track_index });
            }
            if track.clips.len() > MAX_CLIPS_PER_TRACK {
                return Err(SessionError::TooManyClips {
                    track: track_index,
                    max: MAX_CLIPS_PER_TRACK,
                });
            }
            for (clip_index, clip) in track.clips.iter().enumerate() {
                if clip.name.len() > MAX_NAME_BYTES {
                    return Err(SessionError::NameTooLong);
                }
                if clip
                    .source
                    .as_ref()
                    .is_some_and(|path| path.is_empty() || path.len() > MAX_SOURCE_BYTES)
                {
                    return Err(SessionError::InvalidSource {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if !valid_gain(clip.gain) {
                    return Err(SessionError::InvalidClipGain {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if clip.length_samples == 0 {
                    return Err(SessionError::EmptyClip {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if clip.source_length == 0 {
                    return Err(SessionError::EmptySource {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if clip
                    .start_samples
                    .checked_add(clip.length_samples)
                    .is_none()
                {
                    return Err(SessionError::TimelineOverflow {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if clip
                    .offset_samples
                    .checked_add(clip.length_samples)
                    .is_none_or(|end| end > clip.source_length)
                {
                    return Err(SessionError::ClipOutOfSource {
                        track: track_index,
                        clip: clip_index,
                    });
                }
                if let Some(notes) = &clip.notes {
                    validate_notes(notes, clip, track_index, clip_index)?;
                }
            }
            validate_effects(&track.effects, track_index)?;
            validate_automation(&track.automation, track_index)?;
        }
        Ok(())
    }
}

/// SPEC-021 checks for one MIDI clip.
fn validate_notes(
    notes: &[Note],
    clip: &Clip,
    track: usize,
    clip_index: usize,
) -> Result<(), SessionError> {
    let error = |note| SessionError::InvalidNote {
        track,
        clip: clip_index,
        note,
    };
    if clip.source.is_some() {
        return Err(error(0));
    }
    if notes.len() > MAX_NOTES_PER_CLIP {
        return Err(error(MAX_NOTES_PER_CLIP));
    }
    match notes
        .iter()
        .position(|note| !note.is_valid(clip.source_length))
    {
        Some(index) => Err(error(index)),
        None => Ok(()),
    }
}

/// SPEC-022 checks for one track's effect chain.
fn validate_effects(effects: &[Effect], track: usize) -> Result<(), SessionError> {
    if effects.len() > MAX_EFFECTS_PER_TRACK {
        return Err(SessionError::InvalidEffect {
            track,
            effect: MAX_EFFECTS_PER_TRACK,
        });
    }
    match effects.iter().position(|effect| !effect.kind.is_valid()) {
        Some(effect) => Err(SessionError::InvalidEffect { track, effect }),
        None => Ok(()),
    }
}

/// SPEC-023 checks for one track's automation lanes.
fn validate_automation(automation: &Automation, track: usize) -> Result<(), SessionError> {
    for lane in [Lane::Gain, Lane::Pan] {
        let points = automation.lane(lane);
        let error = |point| SessionError::InvalidAutomation { track, lane, point };
        if points.len() > MAX_AUTOMATION_POINTS {
            return Err(error(MAX_AUTOMATION_POINTS));
        }
        for (index, point) in points.iter().enumerate() {
            let ordered = index == 0 || points[index - 1].time < point.time;
            if !ordered || !lane.accepts(point.value) {
                return Err(error(index));
            }
        }
    }
    Ok(())
}

/// Finite and inside `[0, 1]`.
fn valid_gain(gain: f32) -> bool {
    gain.is_finite() && (0.0..=1.0).contains(&gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut session = Session::new("demo", 48_000, 120.0);
        let mut track = Track::new("lead");
        track.clips.push(Clip::full(1000, 500));
        session.tracks.push(track);
        session
    }

    #[test]
    fn a_valid_session_passes() {
        assert_eq!(session().validate(), Ok(()));
    }

    #[test]
    fn length_is_the_farthest_clip_end() {
        let mut s = session();
        s.tracks[0].clips.push(Clip::full(10, 20));
        assert_eq!(s.tracks[0].clips[1].end_samples(), 30);
        assert_eq!(s.length_samples(), 1500);
        assert_eq!(s.track_length_samples(0), 1500);
        assert_eq!(s.track_length_samples(7), 0);
    }

    #[test]
    fn an_empty_session_has_zero_length() {
        let s = Session::new_untitled(48_000, 120.0);
        assert_eq!(s.length_samples(), 0);
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn sample_rate_tempo_and_names_are_checked() {
        let mut s = session();
        s.sample_rate = 0;
        assert_eq!(s.validate(), Err(SessionError::InvalidSampleRate));
        let mut s = session();
        s.tempo_bpm = f64::NAN;
        assert_eq!(s.validate(), Err(SessionError::InvalidTempo));
        let mut s = session();
        s.name = "x".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(s.validate(), Err(SessionError::NameTooLong));
    }

    #[test]
    fn clip_geometry_is_checked() {
        let mut s = session();
        s.tracks[0].clips[0].length_samples = 0;
        assert_eq!(
            s.validate(),
            Err(SessionError::EmptyClip { track: 0, clip: 0 })
        );
        let mut s = session();
        s.tracks[0].clips[0].source_length = 0;
        assert_eq!(
            s.validate(),
            Err(SessionError::EmptySource { track: 0, clip: 0 })
        );
        let mut s = session();
        s.tracks[0].clips[0].offset_samples = 1;
        assert_eq!(
            s.validate(),
            Err(SessionError::ClipOutOfSource { track: 0, clip: 0 })
        );
        let mut s = session();
        s.tracks[0].clips[0].start_samples = u64::MAX;
        assert_eq!(
            s.validate(),
            Err(SessionError::TimelineOverflow { track: 0, clip: 0 })
        );
    }

    #[test]
    fn gains_are_checked_for_tracks_and_clips() {
        let mut s = session();
        s.tracks[0].gain = 1.5;
        assert_eq!(s.validate(), Err(SessionError::InvalidGain { track: 0 }));
        let mut s = session();
        s.tracks[0].clips[0].gain = -0.1;
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidClipGain { track: 0, clip: 0 })
        );
    }

    #[test]
    fn mixer_fields_default_to_neutral() {
        let s = session();
        assert_eq!(s.master_gain, 1.0);
        assert_eq!(s.tracks[0].pan, 0.0);
        assert!(!s.tracks[0].solo);
        assert_eq!(s.tracks[0].pan_gains(), (1.0, 1.0));
    }

    #[test]
    fn pan_and_master_are_checked() {
        for bad in [1.5, -1.01, f32::NAN, f32::INFINITY] {
            let mut s = session();
            s.tracks[0].pan = bad;
            assert_eq!(s.validate(), Err(SessionError::InvalidPan { track: 0 }));
        }
        for bad in [2.0, -0.1, f32::NAN] {
            let mut s = session();
            s.master_gain = bad;
            assert_eq!(s.validate(), Err(SessionError::InvalidMasterGain));
        }
        let mut s = session();
        s.tracks[0].pan = -1.0;
        s.master_gain = 0.0;
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn balance_law_keeps_the_near_side_at_unity() {
        let mut t = Track::new("t");
        t.pan = -1.0;
        assert_eq!(t.pan_gains(), (1.0, 0.0));
        t.pan = 1.0;
        assert_eq!(t.pan_gains(), (0.0, 1.0));
        t.pan = 0.5;
        assert_eq!(t.pan_gains(), (0.5, 1.0));
        t.pan = -0.25;
        assert_eq!(t.pan_gains(), (1.0, 0.75));
    }

    #[test]
    fn solo_and_mute_decide_audibility() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.tracks = (0..3).map(|_| Track::new("t")).collect();
        assert!((0..3).all(|i| s.is_audible(i)));
        assert!(!s.is_audible(3));
        s.tracks[1].solo = true;
        assert_eq!(
            (0..3).map(|i| s.is_audible(i)).collect::<Vec<_>>(),
            [false, true, false]
        );
        s.tracks[2].solo = true;
        assert_eq!(
            (0..3).map(|i| s.is_audible(i)).collect::<Vec<_>>(),
            [false, true, true]
        );
        s.tracks[2].solo = false;
        s.tracks[1].mute = true;
        assert!((0..3).all(|i| !s.is_audible(i)), "mute beats solo");
    }

    #[test]
    fn too_many_tracks_or_clips_are_named() {
        let mut s = session();
        s.tracks = (0..=MAX_TRACKS).map(|_| Track::new("t")).collect();
        assert_eq!(
            s.validate(),
            Err(SessionError::TooManyTracks { max: MAX_TRACKS })
        );
        let mut s = session();
        s.tracks[0].clips = (0..=MAX_CLIPS_PER_TRACK)
            .map(|_| Clip::full(0, 1))
            .collect();
        assert_eq!(
            s.validate(),
            Err(SessionError::TooManyClips {
                track: 0,
                max: MAX_CLIPS_PER_TRACK
            })
        );
    }

    #[test]
    fn cover_and_source_index_follow_the_clip_window() {
        let clip = Clip {
            start_samples: 100,
            length_samples: 50,
            offset_samples: 10,
            source_length: 200,
            gain: 1.0,
            name: String::new(),
            source: None,
            notes: None,
        };
        assert!(!clip.covers(99));
        assert!(clip.covers(100));
        assert!(clip.covers(149));
        assert!(!clip.covers(150));
        assert_eq!(clip.source_index(100), 10);
        assert_eq!(clip.source_index(149), 59);
        assert_eq!(Clip::full(u64::MAX, 2).end_samples(), u64::MAX);
    }

    #[test]
    fn new_tracks_have_no_effects() {
        assert_eq!(Track::new("t").effects, Vec::new());
    }

    #[test]
    fn a_full_valid_effect_chain_passes() {
        let mut s = session();
        s.tracks[0].effects = (0..MAX_EFFECTS_PER_TRACK)
            .map(|_| Effect::new(crate::fx::EffectKind::reverb()))
            .collect();
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn too_many_effects_are_rejected() {
        let mut s = session();
        s.tracks[0].effects = (0..=MAX_EFFECTS_PER_TRACK)
            .map(|_| Effect::new(crate::fx::EffectKind::eq()))
            .collect();
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidEffect {
                track: 0,
                effect: MAX_EFFECTS_PER_TRACK
            })
        );
    }

    #[test]
    fn an_out_of_range_effect_is_rejected_by_index() {
        let mut s = session();
        s.tracks[0].effects = vec![
            Effect::new(crate::fx::EffectKind::eq()),
            Effect::new(crate::fx::EffectKind::Compressor {
                threshold_db: -18.0,
                ratio: 0.5,
                attack_ms: 10.0,
                release_ms: 100.0,
                makeup_db: 0.0,
            }),
        ];
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidEffect {
                track: 0,
                effect: 1
            })
        );
        // Bypass does not excuse an invalid value.
        s.tracks[0].effects[1].bypass = true;
        assert!(s.validate().is_err());
    }
}
