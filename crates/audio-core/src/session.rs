//! Session model: tracks, clips and how they sit on the timeline (SPEC-010).
//!
//! This module is plain data plus its invariants. It knows nothing about the
//! graph, the audio thread, files or the clock: the shell builds nodes from a
//! [`Session`], and the project document (SPEC-008) stores one.

use std::fmt;

use crate::project::MAX_NAME_BYTES;

/// Most tracks a session may hold.
pub const MAX_TRACKS: usize = 32;

/// Most clips a single track may hold.
pub const MAX_CLIPS_PER_TRACK: usize = 128;

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
    /// A clip gain is not finite or outside `[0, 1]`.
    InvalidClipGain {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
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
            Self::InvalidClipGain { track, clip } => {
                write!(f, "clip {clip} of track {track} gain is not in [0, 1]")
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
        }
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

/// A horizontal lane holding clips.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Display name.
    pub name: String,
    /// Linear track gain, `0.0..=1.0`.
    pub gain: f32,
    /// Silent track when `true`.
    pub mute: bool,
    /// Clips in any order; nothing depends on `Vec` order for rendering.
    pub clips: Vec<Clip>,
}

impl Track {
    /// Empty, unmuted track at unity gain.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            gain: 1.0,
            mute: false,
            clips: Vec::new(),
        }
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
            }
        }
        Ok(())
    }
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
        };
        assert!(!clip.covers(99));
        assert!(clip.covers(100));
        assert!(clip.covers(149));
        assert!(!clip.covers(150));
        assert_eq!(clip.source_index(100), 10);
        assert_eq!(clip.source_index(149), 59);
        assert_eq!(Clip::full(u64::MAX, 2).end_samples(), u64::MAX);
    }
}
