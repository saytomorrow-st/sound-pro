//! Session model: tracks, clips and how they sit on the timeline (SPEC-010).
//!
//! This module is plain data plus its invariants. It knows nothing about the
//! graph, the audio thread, files or the clock: the shell builds nodes from a
//! [`Session`], and the project document (SPEC-008) stores one.

use std::fmt;

use crate::fx::{Effect, EffectParam, MAX_EFFECTS_PER_TRACK};
use crate::project::MAX_NAME_BYTES;
use crate::transport::TimeSignature;

/// Most tracks a session may hold.
pub const MAX_TRACKS: usize = 32;

/// Most markers a session may hold (SPEC-030).
pub const MAX_MARKERS: usize = 256;

/// Size of the track colour palette; [`Track::color`] indexes it (SPEC-029).
pub const TRACK_COLORS: u8 = 8;

/// Whether `signature` is one the editor supports: `1..=16` beats of a
/// half, quarter, eighth or sixteenth note (SPEC-026).
#[must_use]
pub const fn valid_time_signature(signature: TimeSignature) -> bool {
    matches!(signature.numerator, 1..=16) && matches!(signature.denominator, 2 | 4 | 8 | 16)
}

/// Most clips a single track may hold.
pub const MAX_CLIPS_PER_TRACK: usize = 128;

/// Longest accepted clip source path, in bytes.
pub const MAX_SOURCE_BYTES: usize = 4096;

/// Most notes a single MIDI clip may hold (SPEC-021).
pub const MAX_NOTES_PER_CLIP: usize = 4096;

/// Most points one automation lane may hold (SPEC-023).
pub const MAX_AUTOMATION_POINTS: usize = 1024;

/// Longest track or project notes text, in bytes (SPEC-043).
pub const MAX_NOTES_BYTES: usize = 64 * 1024;

/// Most buses a session may hold (SPEC-028).
pub const MAX_BUSES: usize = 4;

/// Why a session (or one of its clips) is inconsistent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// `sample_rate` is zero.
    InvalidSampleRate,
    /// `tempo_bpm` is not finite or not positive.
    InvalidTempo,
    /// A session, track or clip name is longer than [`MAX_NAME_BYTES`].
    NameTooLong,
    /// Track or project notes are longer than [`MAX_NOTES_BYTES`].
    NotesTooLong,
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
    /// The time signature numerator is outside `1..=16` or the denominator
    /// is not 2, 4, 8 or 16 (SPEC-026).
    InvalidTimeSignature,
    /// A clip gain is not finite or outside `[0, 1]`.
    InvalidClipGain {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A clip's fades are longer than the clip together (SPEC-038).
    InvalidFade {
        /// Index of the track holding the clip.
        track: usize,
        /// Index of the clip inside the track.
        clip: usize,
    },
    /// A fade curve is not finite or outside `[-1, 1]` (SPEC-045 §1).
    InvalidFadeCurve {
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
    /// A bus gain is out of range, its name is too long, one of its effects
    /// is invalid, or there are more than [`MAX_BUSES`] buses (SPEC-028).
    InvalidBus {
        /// Index of the offending bus; [`MAX_BUSES`] when there are too many.
        bus: usize,
    },
    /// A send points at a missing bus, repeats a bus, has a level outside
    /// `[0, 1]`, or the track has more than [`MAX_BUSES`] sends (SPEC-028).
    InvalidSend {
        /// Index of the track holding the send.
        track: usize,
        /// Index of the offending send; [`MAX_BUSES`] when there are too many.
        send: usize,
    },
    /// A track colour is outside the palette (SPEC-029).
    InvalidColor {
        /// Index of the offending track.
        track: usize,
    },
    /// A marker name is too long, markers are out of order or share a
    /// position, or there are more than [`MAX_MARKERS`] (SPEC-030).
    InvalidMarker {
        /// Index of the offending marker; [`MAX_MARKERS`] when there are too
        /// many.
        marker: usize,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => write!(f, "sample rate must be non-zero"),
            Self::InvalidTempo => write!(f, "tempo must be finite and positive"),
            Self::NameTooLong => write!(f, "name longer than {MAX_NAME_BYTES} bytes"),
            Self::NotesTooLong => write!(f, "notes longer than {MAX_NOTES_BYTES} bytes"),
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
            Self::InvalidTimeSignature => write!(f, "time signature is not supported"),
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
            Self::InvalidBus { bus } => write!(f, "bus {bus} is invalid"),
            Self::InvalidSend { track, send } => {
                write!(f, "track {track}: send {send} is invalid")
            }
            Self::InvalidColor { track } => write!(f, "track {track}: colour is invalid"),
            Self::InvalidMarker { marker } => write!(f, "marker {marker} is invalid"),
            Self::InvalidFade { track, clip } => {
                write!(
                    f,
                    "track {track}, clip {clip}: fades are longer than the clip"
                )
            }
            Self::InvalidFadeCurve { track, clip } => {
                write!(f, "track {track}, clip {clip}: fade curve is invalid")
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
    /// Linear fade-in from the clip start, in samples (SPEC-038).
    pub fade_in_samples: u64,
    /// Linear fade-out to the clip end, in samples (SPEC-038).
    pub fade_out_samples: u64,
    /// Shape of the fade-in, `-1.0..=1.0`; `0.0` is linear (SPEC-045 §1,
    /// see [`shape_fade`]).
    pub fade_in_curve: f32,
    /// Shape of the fade-out, `-1.0..=1.0`; `0.0` is linear (SPEC-045 §1).
    pub fade_out_curve: f32,
}

/// Shaped fade multiplier for progress `x` in `0.0..=1.0` (SPEC-045 §1).
///
/// A power curve `x^(2^(-2 * curve))`, the exponential-family fade of
/// Roads, *The Computer Music Tutorial* (1996), ch. 2: `curve > 0` rises
/// fast (convex, exponent down to `1/4`), `curve < 0` rises slowly
/// (concave, exponent up to `4`). `curve == 0.0` returns `x` unchanged, so
/// a linear fade is bit-identical to SPEC-038.
#[must_use]
pub fn shape_fade(x: f32, curve: f32) -> f32 {
    if curve == 0.0 {
        x
    } else {
        x.powf((-2.0 * curve).exp2())
    }
}

/// Longest edge ramp of a clip lying inside another: 10 ms at 48 kHz
/// (SPEC-044 §2).
pub const CROSSFADE_SAMPLES: u64 = 480;

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
            fade_in_samples: 0,
            fade_out_samples: 0,
            fade_in_curve: 0.0,
            fade_out_curve: 0.0,
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

    /// Multiplier of the clip's own fades at `position` (SPEC-038 §1).
    ///
    /// Linear ramps: `t / fade_in` over the first `fade_in` samples, where
    /// `t = position - start`, and `r / fade_out` over the last `fade_out`,
    /// where `r = end - 1 - position`; the smaller one wins where they meet.
    /// Exactly `1.0` outside both zones, so a clip without fades sounds
    /// bit-identical. Only meaningful when [`Clip::covers`] is true.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "fade lengths are far below 2^24 samples in practice; the ramp only needs f32 precision"
    )]
    pub fn fade_gain(&self, position: u64) -> f32 {
        let t = position.wrapping_sub(self.start_samples);
        let r = self.length_samples.saturating_sub(t).saturating_sub(1);
        let mut gain = 1.0_f32;
        if t < self.fade_in_samples {
            gain = gain.min(shape_fade(
                t as f32 / self.fade_in_samples as f32,
                self.fade_in_curve,
            ));
        }
        if r < self.fade_out_samples {
            gain = gain.min(shape_fade(
                r as f32 / self.fade_out_samples as f32,
                self.fade_out_curve,
            ));
        }
        gain
    }

    /// Weight of this clip in a mix where it lies above `lower` (or above
    /// nothing) at `position` (SPEC-045 §2).
    ///
    /// Without `lower` this is [`Clip::fade_gain`]. Over `lower`, an edge
    /// with an explicit fade uses that fade instead of the automatic ramp
    /// of [`Clip::overlap_weight`]; the two never multiply, so an explicit
    /// crossfade does not dip. Without fades this equals `overlap_weight`.
    ///
    /// `above` is the clip sounding over this one, if any: when it starts
    /// inside this clip and runs past its end, this clip's own fade-out is
    /// ignored, because `above` already fades this clip out.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "an overlap longer than 2^24 samples only loses ramp precision"
    )]
    pub fn layer_weight(&self, above: Option<&Self>, lower: Option<&Self>, position: u64) -> f32 {
        let crossed = above.is_some_and(|a| {
            a.start_samples > self.start_samples && a.end_samples() >= self.end_samples()
        });
        let Some(lower) = lower else {
            if crossed {
                let t = position.wrapping_sub(self.start_samples);
                return if t < self.fade_in_samples {
                    shape_fade(t as f32 / self.fade_in_samples as f32, self.fade_in_curve)
                } else {
                    1.0
                };
            }
            return self.fade_gain(position);
        };
        let start = self.start_samples;
        let end = self.end_samples();
        let lower_end = lower.end_samples();
        let t = position.wrapping_sub(start);
        let r = end.saturating_sub(position).saturating_sub(1);
        let start_inside = start > lower.start_samples;
        let runs_past = start_inside && end >= lower_end;
        let n = CROSSFADE_SAMPLES.min(self.length_samples / 2).max(1);
        let head = if self.fade_in_samples > 0 {
            if t < self.fade_in_samples {
                shape_fade(t as f32 / self.fade_in_samples as f32, self.fade_in_curve)
            } else {
                1.0
            }
        } else if runs_past {
            let n = lower_end.saturating_sub(start).max(1);
            (t as f32 / n as f32).min(1.0)
        } else if start_inside && t < n {
            t as f32 / n as f32
        } else {
            1.0
        };
        let tail = if crossed {
            1.0
        } else if self.fade_out_samples > 0 {
            if r < self.fade_out_samples {
                shape_fade(r as f32 / self.fade_out_samples as f32, self.fade_out_curve)
            } else {
                1.0
            }
        } else if !runs_past && end < lower_end && r < n {
            r as f32 / n as f32
        } else {
            1.0
        };
        head.min(tail)
    }

    /// Whether this clip sounds over `other` where both cover a position
    /// (SPEC-044 §2): the later start wins, then the earlier end (a clip
    /// lying inside another), then the later list index. Depends on the
    /// list order only for clips with identical spans.
    #[must_use]
    pub fn is_above(&self, index: usize, other: &Self, other_index: usize) -> bool {
        overlap_key(self, index) > overlap_key(other, other_index)
    }

    /// Weight of this clip over `lower`, a clip it overlaps and is above
    /// (see [`Clip::is_above`]), at `position` (SPEC-044 §2); `lower` gets
    /// `1 - weight`.
    ///
    /// An equal-gain linear crossfade, as in Roads, *The Computer Music
    /// Tutorial* (1996), ch. 2:
    /// - this clip runs past the end of `lower`: the weight ramps `t / n`
    ///   over the whole overlap, `t = position - start`,
    ///   `n = lower.end - start`;
    /// - this clip lies inside `lower`: ramps of
    ///   `n = min(CROSSFADE_SAMPLES, length / 2)` samples at each edge that
    ///   is inside `lower`, exactly `1.0` between them.
    ///
    /// Only meaningful when both clips cover `position`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "an overlap longer than 2^24 samples only loses ramp precision"
    )]
    pub fn overlap_weight(&self, lower: &Self, position: u64) -> f32 {
        let start = self.start_samples;
        let end = self.end_samples();
        let lower_end = lower.end_samples();
        let t = position.wrapping_sub(start);
        if start > lower.start_samples && end >= lower_end {
            let n = lower_end.saturating_sub(start).max(1);
            return (t as f32 / n as f32).min(1.0);
        }
        let n = CROSSFADE_SAMPLES.min(self.length_samples / 2).max(1);
        let r = end.saturating_sub(position).saturating_sub(1);
        let mut weight = 1.0_f32;
        if start > lower.start_samples && t < n {
            weight = weight.min(t as f32 / n as f32);
        }
        if end < lower_end && r < n {
            weight = weight.min(r as f32 / n as f32);
        }
        weight
    }
}

/// Sort key of [`Clip::is_above`]: larger is above.
const fn overlap_key(clip: &Clip, index: usize) -> (u64, u64, usize) {
    (
        clip.start_samples,
        u64::MAX - clip.start_samples.saturating_add(clip.length_samples),
        index,
    )
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
    /// One parameter of the effect in chain slot `slot` (SPEC-041).
    Effect {
        /// Index in the track's effect chain.
        slot: u8,
        /// Which parameter of that effect.
        param: EffectParam,
    },
}

impl Lane {
    /// Inclusive range of accepted values.
    #[must_use]
    pub const fn range(self) -> (f32, f32) {
        match self {
            Self::Gain => (0.0, 1.0),
            Self::Pan => (-1.0, 1.0),
            Self::Effect { param, .. } => param.range(),
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
        match self {
            Self::Gain => f.write_str("gain"),
            Self::Pan => f.write_str("pan"),
            Self::Effect { slot, param } => write!(f, "effect {slot} {}", param.name()),
        }
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

/// Most effect lanes one track may hold: every parameter of a full chain.
pub const MAX_EFFECT_LANES: usize = MAX_EFFECTS_PER_TRACK * EffectParam::ALL.len();

/// Breakpoints of one effect parameter (SPEC-041).
#[derive(Debug, Clone, PartialEq)]
pub struct EffectLane {
    /// Index in the track's effect chain.
    pub slot: u8,
    /// Which parameter of that effect.
    pub param: EffectParam,
    /// Points strictly ordered by time.
    pub points: Vec<AutoPoint>,
}

/// Breakpoint lanes for one track; points are strictly ordered by time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Automation {
    /// Gain lane.
    pub gain: Vec<AutoPoint>,
    /// Pan lane.
    pub pan: Vec<AutoPoint>,
    /// Effect parameter lanes, at most one per slot and parameter (SPEC-041).
    pub effects: Vec<EffectLane>,
}

impl Automation {
    /// `true` when no lane has a point.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gain.is_empty()
            && self.pan.is_empty()
            && self.effects.iter().all(|lane| lane.points.is_empty())
    }

    /// `true` when some effect lane has a point.
    #[must_use]
    pub fn has_effect_points(&self) -> bool {
        self.effects.iter().any(|lane| !lane.points.is_empty())
    }

    /// Points of one lane; a missing effect lane is empty.
    #[must_use]
    pub fn lane(&self, lane: Lane) -> &[AutoPoint] {
        match lane {
            Lane::Gain => &self.gain,
            Lane::Pan => &self.pan,
            Lane::Effect { slot, param } => self
                .effects
                .iter()
                .find(|lane| lane.slot == slot && lane.param == param)
                .map_or(&[], |lane| &lane.points),
        }
    }

    /// Mutable points of one lane; a missing effect lane is created empty.
    pub fn lane_mut(&mut self, lane: Lane) -> &mut Vec<AutoPoint> {
        match lane {
            Lane::Gain => &mut self.gain,
            Lane::Pan => &mut self.pan,
            Lane::Effect { slot, param } => {
                let found = self
                    .effects
                    .iter()
                    .position(|lane| lane.slot == slot && lane.param == param);
                let index = found.unwrap_or_else(|| {
                    self.effects.push(EffectLane {
                        slot,
                        param,
                        points: Vec::new(),
                    });
                    self.effects.len() - 1
                });
                &mut self.effects[index].points
            }
        }
    }

    /// Drops effect lanes without points.
    pub fn prune(&mut self) {
        self.effects.retain(|lane| !lane.points.is_empty());
    }

    /// Every lane with its points: gain, pan, then the effect lanes.
    pub fn lanes(&self) -> impl Iterator<Item = (Lane, &[AutoPoint])> {
        [(Lane::Gain, &self.gain[..]), (Lane::Pan, &self.pan[..])]
            .into_iter()
            .chain(self.effects.iter().map(|lane| {
                (
                    Lane::Effect {
                        slot: lane.slot,
                        param: lane.param,
                    },
                    &lane.points[..],
                )
            }))
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

/// What a track holds (SPEC-049): audio regions or MIDI regions played
/// by its instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackKind {
    /// Audio regions; microphone takes are recorded here.
    #[default]
    Audio,
    /// Software instrument: MIDI regions.
    Instrument,
}

/// Channel format of an audio track (SPEC-050 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackFormat {
    /// Single channel; stereo sources are mixed down.
    #[default]
    Mono,
    /// Two channels; mono sources are duplicated into both.
    Stereo,
}

impl TrackKind {
    /// Whether a clip of this kind of content belongs on such a track.
    #[must_use]
    pub const fn accepts(self, clip: &Clip) -> bool {
        matches!(
            (self, clip.is_midi()),
            (Self::Audio, false) | (Self::Instrument, true)
        )
    }
}

/// A horizontal lane holding clips.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Display name.
    pub name: String,
    /// Audio or software instrument (SPEC-049).
    pub kind: TrackKind,
    /// Mono or stereo audio (SPEC-050 §5).
    pub format: TrackFormat,
    /// Linear track gain, `0.0..=1.0`.
    pub gain: f32,
    /// Balance pan, `-1.0` (left) ..= `1.0` (right); `0.0` is centre.
    pub pan: f32,
    /// Silent track when `true`.
    pub mute: bool,
    /// When any track is soloed, only soloed, unmuted tracks sound.
    pub solo: bool,
    /// Input monitoring (the I button, SPEC-044 §1): stored with the
    /// project; the live input path honours it once it exists.
    pub input_monitor: bool,
    /// Clips in any order; nothing depends on `Vec` order for rendering.
    pub clips: Vec<Clip>,
    /// Insert effects, processed in order after the clips are summed and
    /// before gain and pan (SPEC-022).
    pub effects: Vec<Effect>,
    /// Gain and pan lanes; a non-empty lane overrides the slider (SPEC-023).
    pub automation: Automation,
    /// Post-fader sends to the session's buses (SPEC-028).
    pub sends: Vec<BusSend>,
    /// Palette index below [`TRACK_COLORS`]; `None` is the default colour.
    /// Display only (SPEC-029).
    pub color: Option<u8>,
    /// Free text from the inspector's track notes tab (SPEC-043).
    pub notes: String,
}

/// A post-fader copy of a track's sound sent to one bus (SPEC-028).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusSend {
    /// Index into [`Session::buses`].
    pub bus: usize,
    /// Linear send level, `0.0..=1.0`.
    pub level: f32,
}

/// A shared effects return: tracks send to it, it adds its processed sum to
/// the master (SPEC-028). Buses may also send to one another (SPEC-051).
#[derive(Debug, Clone, PartialEq)]
pub struct Bus {
    /// Display name.
    pub name: String,
    /// Linear return gain, `0.0..=1.0`.
    pub gain: f32,
    /// Effects applied to the summed sends, in order.
    pub effects: Vec<Effect>,
    /// Silent bus return when `true` (SPEC-051); its sends to other buses
    /// still flow.
    pub mute: bool,
    /// When any bus is soloed, only soloed, unmuted buses return to the
    /// master (SPEC-051); solo never breaks a bus's outgoing sends.
    pub solo: bool,
    /// Post-fader sends to other buses, indexed into [`Session::buses`]
    /// (SPEC-051).
    pub sends: Vec<BusSend>,
}

impl Bus {
    /// Bus at unity gain with no effects, sends, or mute/solo.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            gain: 1.0,
            effects: Vec::new(),
            mute: false,
            solo: false,
            sends: Vec::new(),
        }
    }

    /// Send level to `bus`, `0.0` when this bus does not send there
    /// (SPEC-051).
    #[must_use]
    pub fn send_level(&self, bus: usize) -> f32 {
        self.sends
            .iter()
            .find(|send| send.bus == bus)
            .map_or(0.0, |send| send.level)
    }
}

impl Track {
    /// Empty software-instrument track (SPEC-049).
    #[must_use]
    pub fn instrument(name: &str) -> Self {
        Self {
            kind: TrackKind::Instrument,
            ..Self::new(name)
        }
    }

    /// Empty, unmuted, centred audio track at unity gain.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            kind: TrackKind::Audio,
            format: TrackFormat::Mono,
            gain: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
            input_monitor: false,
            clips: Vec::new(),
            effects: Vec::new(),
            automation: Automation::default(),
            sends: Vec::new(),
            color: None,
            notes: String::new(),
        }
    }

    /// Empty stereo audio track (SPEC-050 §5).
    #[must_use]
    pub fn stereo(name: &str) -> Self {
        Self {
            format: TrackFormat::Stereo,
            ..Self::new(name)
        }
    }

    /// Send level to `bus`, `0.0` when the track does not send there.
    #[must_use]
    pub fn send_level(&self, bus: usize) -> f32 {
        self.sends
            .iter()
            .find(|send| send.bus == bus)
            .map_or(0.0, |send| send.level)
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
    /// Master mute, the M button of Stereo Out (SPEC-044 §1).
    pub master_mute: bool,
    /// Master dim, the D button of Stereo Out: [`DIM_GAIN`] (SPEC-044 §1).
    pub master_dim: bool,
    /// Bar layout; `tempo_bpm` counts its beats (SPEC-026).
    pub time_signature: TimeSignature,
    /// Tracks in session order.
    pub tracks: Vec<Track>,
    /// Effect buses fed by track sends (SPEC-028).
    pub buses: Vec<Bus>,
    /// Named positions on the timeline, sorted by position (SPEC-030).
    pub markers: Vec<Marker>,
    /// Free text from the inspector's project notes tab (SPEC-043).
    pub notes: String,
}

/// A named position on the timeline; display only (SPEC-030).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// Label shown on the ruler.
    pub name: String,
    /// Position in project samples.
    pub position: u64,
}

/// Gain of the master dim: −20 dB (SPEC-044 §1).
pub const DIM_GAIN: f32 = 0.1;

impl Session {
    /// Master gain as heard: `0.0` when muted, times [`DIM_GAIN`] when
    /// dimmed (SPEC-044 §1).
    #[must_use]
    pub fn master_level(&self) -> f32 {
        if self.master_mute {
            0.0
        } else if self.master_dim {
            self.master_gain * DIM_GAIN
        } else {
            self.master_gain
        }
    }

    /// Empty session with no tracks.
    #[must_use]
    pub fn new(name: &str, sample_rate: u32, tempo_bpm: f64) -> Self {
        Self {
            name: name.to_owned(),
            sample_rate,
            tempo_bpm,
            master_gain: 1.0,
            master_mute: false,
            master_dim: false,
            time_signature: TimeSignature::default(),
            tracks: Vec::new(),
            buses: Vec::new(),
            markers: Vec::new(),
            notes: String::new(),
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

    /// Whether `bus` returns to the master: not muted, and either nothing is
    /// soloed or it is. Mute beats solo; an out-of-range index is `false`
    /// (SPEC-051). A bus's outgoing sends are not gated here, so solo never
    /// breaks a send chain.
    #[must_use]
    pub fn is_bus_audible(&self, bus: usize) -> bool {
        let Some(b) = self.buses.get(bus) else {
            return false;
        };
        !b.mute && (b.solo || !self.buses.iter().any(|other| other.solo))
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
        if self.notes.len() > MAX_NOTES_BYTES
            || self.tracks.iter().any(|t| t.notes.len() > MAX_NOTES_BYTES)
        {
            return Err(SessionError::NotesTooLong);
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
        if !valid_time_signature(self.time_signature) {
            return Err(SessionError::InvalidTimeSignature);
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(SessionError::TooManyTracks { max: MAX_TRACKS });
        }
        self.validate_buses()?;
        self.validate_markers()?;
        for (track_index, track) in self.tracks.iter().enumerate() {
            if track.name.len() > MAX_NAME_BYTES {
                return Err(SessionError::NameTooLong);
            }
            if !valid_gain(track.gain) {
                return Err(SessionError::InvalidGain { track: track_index });
            }
            if track.color.is_some_and(|color| color >= TRACK_COLORS) {
                return Err(SessionError::InvalidColor { track: track_index });
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
                validate_clip(clip, track_index, clip_index)?;
            }
            validate_effects(&track.effects, track_index)?;
            validate_automation(&track.automation, &track.effects, track_index)?;
            validate_sends(&track.sends, self.buses.len(), track_index)?;
        }
        Ok(())
    }

    /// SPEC-030 checks for the marker list.
    fn validate_markers(&self) -> Result<(), SessionError> {
        if self.markers.len() > MAX_MARKERS {
            return Err(SessionError::InvalidMarker {
                marker: MAX_MARKERS,
            });
        }
        for (index, marker) in self.markers.iter().enumerate() {
            let ordered = index == 0 || self.markers[index - 1].position < marker.position;
            if marker.name.len() > MAX_NAME_BYTES || !ordered {
                return Err(SessionError::InvalidMarker { marker: index });
            }
        }
        Ok(())
    }

    /// SPEC-028/SPEC-051 checks for the bus list: per-bus effects and gain,
    /// plus each bus's sends (no self-reference, no repeats, no cycles).
    fn validate_buses(&self) -> Result<(), SessionError> {
        if self.buses.len() > MAX_BUSES {
            return Err(SessionError::InvalidBus { bus: MAX_BUSES });
        }
        for (bus_index, bus) in self.buses.iter().enumerate() {
            let effects_ok = bus.effects.len() <= MAX_EFFECTS_PER_TRACK
                && bus.effects.iter().all(|effect| effect.kind.is_valid());
            if bus.name.len() > MAX_NAME_BYTES || !valid_gain(bus.gain) || !effects_ok {
                return Err(SessionError::InvalidBus { bus: bus_index });
            }
            validate_bus_sends(&bus.sends, self.buses.len(), bus_index)?;
        }
        self.validate_bus_cycles()
    }

    /// Rejects a directed cycle over the buses' sends (SPEC-051).
    fn validate_bus_cycles(&self) -> Result<(), SessionError> {
        // Three states: unvisited, on the current DFS stack, done. A back edge
        // to an on-stack bus is a cycle. With at most MAX_BUSES buses this is
        // trivially cheap and runs once at validate time, never on the audio
        // path.
        let mut state = vec![0u8; self.buses.len()];
        for start in 0..self.buses.len() {
            if state[start] != 0 {
                continue;
            }
            if let Err((bus, send)) = visit_bus(self, start, &mut state) {
                return Err(SessionError::InvalidSend { track: bus, send });
            }
        }
        Ok(())
    }
}

/// SPEC-028 checks for one track's sends.
fn validate_sends(sends: &[BusSend], buses: usize, track: usize) -> Result<(), SessionError> {
    if sends.len() > MAX_BUSES {
        return Err(SessionError::InvalidSend {
            track,
            send: MAX_BUSES,
        });
    }
    for (index, send) in sends.iter().enumerate() {
        let repeated = sends[..index].iter().any(|other| other.bus == send.bus);
        if send.bus >= buses || repeated || !valid_gain(send.level) {
            return Err(SessionError::InvalidSend { track, send: index });
        }
    }
    Ok(())
}

/// SPEC-051 checks for one bus's sends: same rules as a track's sends plus a
/// ban on the bus sending to itself. `bus_index` is reported as `track`.
fn validate_bus_sends(
    sends: &[BusSend],
    buses: usize,
    bus_index: usize,
) -> Result<(), SessionError> {
    if sends.len() > MAX_BUSES {
        return Err(SessionError::InvalidSend {
            track: bus_index,
            send: MAX_BUSES,
        });
    }
    for (index, send) in sends.iter().enumerate() {
        let repeated = sends[..index].iter().any(|other| other.bus == send.bus);
        if send.bus == bus_index || send.bus >= buses || repeated || !valid_gain(send.level) {
            return Err(SessionError::InvalidSend {
                track: bus_index,
                send: index,
            });
        }
    }
    Ok(())
}

/// Depth-first walk of `session`'s bus-send graph starting at `bus`. Returns
/// the offending bus index and send index for the first back edge found, if
/// any (SPEC-051).
fn visit_bus(session: &Session, bus: usize, state: &mut [u8]) -> Result<(), (usize, usize)> {
    state[bus] = 1;
    for (send_index, send) in session.buses[bus].sends.iter().enumerate() {
        let target = send.bus;
        match state[target] {
            0 => visit_bus(session, target, state)?,
            1 => return Err((bus, send_index)),
            _ => {}
        }
    }
    state[bus] = 2;
    Ok(())
}

/// Per-clip checks of [`Session::validate`].
fn validate_clip(clip: &Clip, track_index: usize, clip_index: usize) -> Result<(), SessionError> {
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
    if clip
        .fade_in_samples
        .checked_add(clip.fade_out_samples)
        .is_none_or(|fades| fades > clip.length_samples)
    {
        return Err(SessionError::InvalidFade {
            track: track_index,
            clip: clip_index,
        });
    }
    if ![clip.fade_in_curve, clip.fade_out_curve]
        .iter()
        .all(|c| c.is_finite() && (-1.0..=1.0).contains(c))
    {
        return Err(SessionError::InvalidFadeCurve {
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
    Ok(())
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

/// SPEC-023/041 checks for one track's automation lanes.
fn validate_automation(
    automation: &Automation,
    effects: &[Effect],
    track: usize,
) -> Result<(), SessionError> {
    if automation.effects.len() > MAX_EFFECT_LANES {
        let last = &automation.effects[MAX_EFFECT_LANES];
        return Err(SessionError::InvalidAutomation {
            track,
            lane: Lane::Effect {
                slot: last.slot,
                param: last.param,
            },
            point: 0,
        });
    }
    for (index, lane) in automation.effects.iter().enumerate() {
        let fits = effects
            .get(usize::from(lane.slot))
            .is_some_and(|effect| lane.param.get(&effect.kind).is_some());
        let unique = automation.effects[..index]
            .iter()
            .all(|other| (other.slot, other.param) != (lane.slot, lane.param));
        if !fits || !unique {
            return Err(SessionError::InvalidAutomation {
                track,
                lane: Lane::Effect {
                    slot: lane.slot,
                    param: lane.param,
                },
                point: 0,
            });
        }
    }
    for (lane, points) in automation.lanes() {
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
            fade_in_samples: 0,
            fade_out_samples: 0,
            fade_in_curve: 0.0,
            fade_out_curve: 0.0,
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
                knee_db: 0.0,
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

    #[test]
    fn bus_send_level_and_defaults_are_neutral() {
        let b = Bus::new("fx");
        assert_eq!(b.gain, 1.0);
        assert!(!b.mute);
        assert!(!b.solo);
        assert_eq!(b.sends, []);
        assert_eq!(b.send_level(0), 0.0);
        let mut b = Bus::new("fx");
        b.sends.push(BusSend { bus: 1, level: 0.5 });
        assert_eq!(b.send_level(1), 0.5);
        assert_eq!(b.send_level(0), 0.0);
    }

    #[test]
    fn bus_solo_and_mute_decide_audibility() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..3).map(|_| Bus::new("b")).collect();
        assert!((0..3).all(|i| s.is_bus_audible(i)));
        assert!(!s.is_bus_audible(3));
        s.buses[1].solo = true;
        assert_eq!(
            (0..3).map(|i| s.is_bus_audible(i)).collect::<Vec<_>>(),
            [false, true, false]
        );
        s.buses[2].solo = true;
        assert_eq!(
            (0..3).map(|i| s.is_bus_audible(i)).collect::<Vec<_>>(),
            [false, true, true]
        );
        s.buses[2].solo = false;
        s.buses[1].mute = true;
        assert!((0..3).all(|i| !s.is_bus_audible(i)), "mute beats solo");
    }

    #[test]
    fn a_bus_cannot_send_to_itself() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..2).map(|_| Bus::new("b")).collect();
        s.buses[0].sends.push(BusSend { bus: 0, level: 1.0 });
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidSend { track: 0, send: 0 })
        );
    }

    #[test]
    fn bus_sends_must_be_in_range_and_unique() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..2).map(|_| Bus::new("b")).collect();
        s.buses[0].sends.push(BusSend { bus: 2, level: 1.0 });
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidSend { track: 0, send: 0 })
        );
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..2).map(|_| Bus::new("b")).collect();
        s.buses[0].sends = vec![
            BusSend { bus: 1, level: 1.0 },
            BusSend { bus: 1, level: 1.0 },
        ];
        assert_eq!(
            s.validate(),
            Err(SessionError::InvalidSend { track: 0, send: 1 })
        );
    }

    #[test]
    fn bus_cycles_are_rejected() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..3).map(|_| Bus::new("b")).collect();
        // 0 -> 1 -> 2 -> 0.
        s.buses[0].sends.push(BusSend { bus: 1, level: 1.0 });
        s.buses[1].sends.push(BusSend { bus: 2, level: 1.0 });
        s.buses[2].sends.push(BusSend { bus: 0, level: 1.0 });
        assert!(matches!(
            s.validate(),
            Err(SessionError::InvalidSend { .. })
        ));
    }

    #[test]
    fn acyclic_bus_sends_pass() {
        let mut s = Session::new("s", 48_000, 120.0);
        s.buses = (0..3).map(|_| Bus::new("b")).collect();
        // 0 -> 1 -> 2 (a chain, no cycle).
        s.buses[0].sends.push(BusSend { bus: 1, level: 1.0 });
        s.buses[1].sends.push(BusSend { bus: 2, level: 1.0 });
        assert_eq!(s.validate(), Ok(()));
    }
}
