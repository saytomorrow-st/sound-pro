//! Project document: the session as human-readable JSON (SPEC-008).
//!
//! The engine deliberately has no `serde` dependency, so this module carries
//! its own small JSON subset: objects, arrays, strings, plain numbers and
//! `true`/`false`. `null`, `\u`-escapes, exponents and deep nesting are
//! rejected rather than silently ignored — a project file that a human edits
//! must fail loudly on a typo.
//!
//! The module knows nothing about files: it turns a [`Project`] into text and
//! back. Reading and writing the file is the shell's job (`daw-app`).

use std::fmt;
use std::fmt::Write as _;

use crate::fx::{Effect, EffectKind, MAX_EFFECTS_PER_TRACK};
use crate::graph::Graph;
use crate::params::{ParamHandle, ParamId, ParamSpec};
use crate::session::{
    Clip, MAX_CLIPS_PER_TRACK, MAX_NOTES_PER_CLIP, MAX_TRACKS, Note, Session, SessionError, Track,
};

/// Longest accepted `name`, in bytes.
pub const MAX_NAME_BYTES: usize = 128;

/// Deepest object/array nesting accepted by the parser.
///
/// SPEC-010 nests four levels (`session` → track → clip → value), so the
/// SPEC-008 limit of 8 was raised to leave the same headroom it had before.
const MAX_DEPTH: usize = 12;

/// Why a project document could not be read or validated.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectError {
    /// The text is not valid JSON in the supported subset.
    Malformed {
        /// Byte offset of the first offending character.
        at: usize,
    },
    /// Valid JSON, but using a feature this codec refuses (`null`, `\u`,
    /// exponents, deep nesting).
    Unsupported {
        /// Byte offset of the offending token.
        at: usize,
    },
    /// A required key is absent.
    MissingField {
        /// Name of the missing key.
        field: &'static str,
    },
    /// A key that the format does not define (usually a typo).
    UnknownField {
        /// Name of the unexpected key.
        field: String,
    },
    /// The name of the project or of a parameter is too long.
    NameTooLong,
    /// `sample_rate` is zero.
    InvalidSampleRate,
    /// `tempo_bpm` is not finite or not positive.
    InvalidTempo,
    /// A parameter's range or value is inconsistent.
    InvalidParam {
        /// Id of the offending parameter.
        id: u32,
    },
    /// Two parameters share an id.
    DuplicateParam {
        /// The repeated id.
        id: u32,
    },
    /// The project refers to a parameter the graph does not have.
    UnknownParam {
        /// The missing id.
        id: u32,
    },
    /// The embedded session does not hold together.
    Session(SessionError),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { at } => write!(f, "malformed JSON at byte {at}"),
            Self::Unsupported { at } => {
                write!(f, "unsupported JSON feature at byte {at}")
            }
            Self::MissingField { field } => write!(f, "missing field `{field}`"),
            Self::UnknownField { field } => write!(f, "unknown field `{field}`"),
            Self::NameTooLong => {
                write!(f, "name is longer than {MAX_NAME_BYTES} bytes")
            }
            Self::InvalidSampleRate => write!(f, "sample_rate must be positive"),
            Self::InvalidTempo => write!(f, "tempo_bpm must be finite and positive"),
            Self::InvalidParam { id } => {
                write!(f, "parameter {id} has an inconsistent range or value")
            }
            Self::DuplicateParam { id } => write!(f, "parameter {id} is declared twice"),
            Self::UnknownParam { id } => write!(f, "parameter {id} is not in the graph"),
            Self::Session(error) => write!(f, "session: {error}"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<SessionError> for ProjectError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// One parameter as it should be restored.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamState {
    /// Id inside the graph.
    pub id: u32,
    /// Human-readable name shown by the UI.
    pub name: String,
    /// Lower bound.
    pub min: f32,
    /// Upper bound.
    pub max: f32,
    /// Value used when a parameter is created from this document.
    pub default: f32,
    /// Ramp length in milliseconds; `0` means "jump".
    pub smoothing_ms: u32,
    /// Value stored by the session.
    pub value: f32,
}

/// A saved session: what to call it, what rate and tempo it ran at, and every
/// parameter's state.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Display name.
    pub name: String,
    /// Sample rate the session was recorded at.
    pub sample_rate: u32,
    /// Tempo in beats per minute.
    pub tempo_bpm: f64,
    /// Parameters in session order.
    pub params: Vec<ParamState>,
    /// Tracks and clips; serialised last, under `"session"`.
    pub session: Session,
}

impl Project {
    /// Takes a snapshot of `graph`'s parameters.
    ///
    /// Names and specs come from the registry, values from the live handles,
    /// so a later [`Self::apply_to`] restores exactly what the user heard.
    #[must_use]
    pub fn capture(name: &str, sample_rate: u32, tempo_bpm: f64, graph: &Graph) -> Self {
        let params = graph
            .params()
            .iter()
            .map(|registration| {
                let ParamSpec {
                    id,
                    name,
                    min,
                    max,
                    default,
                    smoothing_ms,
                } = registration.spec;
                ParamState {
                    id: id.0,
                    name: name.to_owned(),
                    min,
                    max,
                    default,
                    smoothing_ms,
                    value: registration.handle.value(),
                }
            })
            .collect();
        Self {
            name: name.to_owned(),
            sample_rate,
            tempo_bpm,
            params,
            session: Session::new_untitled(sample_rate, tempo_bpm),
        }
    }

    /// Replaces the empty session [`Self::capture`] stores with `session`.
    #[must_use]
    pub fn with_session(project: Self, session: Session) -> Self {
        Self { session, ..project }
    }

    /// The session stored in this document.
    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// Writes the stored values into the parameters the graph already has.
    ///
    /// # Errors
    ///
    /// [`ProjectError::UnknownParam`] if the graph lacks a parameter the
    /// document mentions.
    pub fn apply_to(&self, graph: &Graph) -> Result<(), ProjectError> {
        // Look every id up before touching anything, so a document that does
        // not fit this build leaves the session unchanged.
        let mut handles: Vec<ParamHandle> = Vec::with_capacity(self.params.len());
        for state in &self.params {
            let handle = graph
                .param(ParamId(state.id))
                .map_err(|_| ProjectError::UnknownParam { id: state.id })?;
            handles.push(handle);
        }
        for (state, handle) in self.params.iter().zip(handles) {
            handle.set(state.value);
        }
        Ok(())
    }

    /// Serialises the document.
    ///
    /// Deterministic: field order is the declaration order and `params` keeps
    /// the `Vec` order, so the same project always gives the same bytes.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256 + self.params.len() * 160);
        out.push_str("{\n");
        out.push_str("  \"name\": ");
        write_string(&mut out, &self.name);
        out.push_str(",\n  \"sample_rate\": ");
        out.push_str(&self.sample_rate.to_string());
        out.push_str(",\n  \"tempo_bpm\": ");
        out.push_str(&format_number(self.tempo_bpm));
        out.push_str(",\n  \"params\": [");
        for (index, param) in self.params.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("\n    {\n      \"id\": ");
            out.push_str(&param.id.to_string());
            out.push_str(",\n      \"name\": ");
            write_string(&mut out, &param.name);
            out.push_str(",\n      \"min\": ");
            out.push_str(&format_number(f64::from(param.min)));
            out.push_str(",\n      \"max\": ");
            out.push_str(&format_number(f64::from(param.max)));
            out.push_str(",\n      \"default\": ");
            out.push_str(&format_number(f64::from(param.default)));
            out.push_str(",\n      \"smoothing_ms\": ");
            out.push_str(&param.smoothing_ms.to_string());
            out.push_str(",\n      \"value\": ");
            out.push_str(&format_number(f64::from(param.value)));
            out.push_str("\n    }");
        }
        if self.params.is_empty() {
            out.push(']');
        } else {
            out.push_str("\n  ]");
        }
        out.push_str(",\n  \"session\": ");
        write_session(&mut out, &self.session);
        out.push_str("\n}\n");
        out
    }

    /// Parses and validates a document.
    ///
    /// # Errors
    ///
    /// [`ProjectError`] describing the first problem found: a syntax error,
    /// an unsupported JSON feature, a missing or unknown key, or an
    /// out-of-range value.
    pub fn from_json(text: &str) -> Result<Self, ProjectError> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
        };
        let value = parser.parse_document()?;
        let Value::Object(root) = value else {
            return Err(ProjectError::Malformed { at: 0 });
        };
        let project = project_from_object(&root)?;
        project.validate()?;
        Ok(project)
    }

    fn validate(&self) -> Result<(), ProjectError> {
        if self.name.len() > MAX_NAME_BYTES {
            return Err(ProjectError::NameTooLong);
        }
        if self.sample_rate == 0 {
            return Err(ProjectError::InvalidSampleRate);
        }
        if !self.tempo_bpm.is_finite() || self.tempo_bpm <= 0.0 {
            return Err(ProjectError::InvalidTempo);
        }
        let mut seen: Vec<u32> = Vec::with_capacity(self.params.len());
        for param in &self.params {
            if param.name.len() > MAX_NAME_BYTES {
                return Err(ProjectError::NameTooLong);
            }
            if seen.contains(&param.id) {
                return Err(ProjectError::DuplicateParam { id: param.id });
            }
            seen.push(param.id);
            let finite = param.min.is_finite()
                && param.max.is_finite()
                && param.default.is_finite()
                && param.value.is_finite();
            if !finite
                || param.min > param.max
                || param.default < param.min
                || param.default > param.max
                || param.value < param.min
                || param.value > param.max
            {
                return Err(ProjectError::InvalidParam { id: param.id });
            }
        }
        self.session.validate()?;
        Ok(())
    }
}

/// Formats a number so that the parser (which rejects exponents and has no
/// notion of `inf`) always reads it back unchanged.
fn format_number(value: f64) -> String {
    // The comparison is exact on purpose: integral values are their own trunc.
    #[allow(clippy::float_cmp)]
    let is_integral = value == value.trunc();
    if is_integral && value.abs() < 1e15 {
        // "1" instead of "1.0": shorter and still exact.
        #[allow(clippy::cast_possible_truncation)]
        let as_int = value as i64;
        return as_int.to_string();
    }
    // `Display` is the shortest exact form and never uses an exponent for
    // `f64`, which is exactly the shape the parser accepts; the test
    // `awkward_floats_survive_the_text_format_exactly` pins that assumption.
    format!("{value}")
}

/// Writes `value` as a JSON string literal.
fn write_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u");
                // Writing cannot fail into a `String`.
                let _ = write!(out, "{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Writes a session object at the two-space indent used inside `"session"`.
///
/// Nested one level deeper than `params`: `session` → track → clip → field.
fn write_session(out: &mut String, session: &Session) {
    out.push_str("{\n    \"name\": ");
    write_string(out, &session.name);
    out.push_str(",\n    \"sample_rate\": ");
    out.push_str(&session.sample_rate.to_string());
    out.push_str(",\n    \"tempo_bpm\": ");
    out.push_str(&format_number(session.tempo_bpm));
    out.push_str(",\n    \"master_gain\": ");
    out.push_str(&format_number(f64::from(session.master_gain)));
    out.push_str(",\n    \"tracks\": [");
    for (track_index, track) in session.tracks.iter().enumerate() {
        if track_index > 0 {
            out.push(',');
        }
        out.push_str("\n      {\n        \"name\": ");
        write_string(out, &track.name);
        out.push_str(",\n        \"gain\": ");
        out.push_str(&format_number(f64::from(track.gain)));
        out.push_str(",\n        \"pan\": ");
        out.push_str(&format_number(f64::from(track.pan)));
        out.push_str(",\n        \"mute\": ");
        out.push_str(if track.mute { "true" } else { "false" });
        out.push_str(",\n        \"solo\": ");
        out.push_str(if track.solo { "true" } else { "false" });
        out.push_str(",\n        \"clips\": [");
        for (clip_index, clip) in track.clips.iter().enumerate() {
            if clip_index > 0 {
                out.push(',');
            }
            out.push_str("\n          {\n            \"start\": ");
            out.push_str(&clip.start_samples.to_string());
            out.push_str(",\n            \"length\": ");
            out.push_str(&clip.length_samples.to_string());
            out.push_str(",\n            \"offset\": ");
            out.push_str(&clip.offset_samples.to_string());
            out.push_str(",\n            \"source_length\": ");
            out.push_str(&clip.source_length.to_string());
            out.push_str(",\n            \"gain\": ");
            out.push_str(&format_number(f64::from(clip.gain)));
            out.push_str(",\n            \"name\": ");
            write_string(out, &clip.name);
            if let Some(source) = &clip.source {
                out.push_str(",\n            \"source\": ");
                write_string(out, source);
            }
            if let Some(notes) = &clip.notes {
                write_notes(out, notes);
            }
            out.push_str("\n          }");
        }
        if track.clips.is_empty() {
            out.push(']');
        } else {
            out.push_str("\n        ]");
        }
        if !track.effects.is_empty() {
            write_effects(out, &track.effects);
        }
        out.push_str("\n      }");
    }
    if session.tracks.is_empty() {
        out.push(']');
    } else {
        out.push_str("\n    ]");
    }
    out.push_str("\n  }");
}

/// Rebuilds a [`Session`] from its object, rejecting stray keys.
fn session_from_value(value: &Value) -> Result<Session, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut name = None;
    let mut sample_rate = None;
    let mut tempo_bpm = None;
    let mut master_gain = None;
    let mut tracks = None;
    for (key, value) in entries {
        match key.as_str() {
            "name" => name = Some(require_string(value)?),
            "sample_rate" => sample_rate = Some(require_u32(value)?),
            "tempo_bpm" => tempo_bpm = Some(require_f64(value)?),
            "master_gain" => master_gain = Some(require_f32(value)?),
            "tracks" => {
                let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
                // Reject an over-long session while parsing rather than after
                // decoding every track: a hand-written file with 10^5 tracks is
                // the cheapest denial-of-service there is, and the limit is the
                // same one `Session::validate` enforces (SPEC-010).
                if items.len() > MAX_TRACKS {
                    return Err(ProjectError::Session(SessionError::TooManyTracks {
                        max: MAX_TRACKS,
                    }));
                }
                tracks = Some(
                    items
                        .iter()
                        .map(track_from_value)
                        .collect::<Result<_, _>>()?,
                );
            }
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(Session {
        name: name.ok_or(ProjectError::MissingField {
            field: "session.name",
        })?,
        sample_rate: sample_rate.ok_or(ProjectError::MissingField {
            field: "session.sample_rate",
        })?,
        tempo_bpm: tempo_bpm.ok_or(ProjectError::MissingField {
            field: "session.tempo_bpm",
        })?,
        // Optional since SPEC-013 so older documents keep loading.
        master_gain: master_gain.unwrap_or(1.0),
        tracks: tracks.ok_or(ProjectError::MissingField {
            field: "session.tracks",
        })?,
    })
}

fn track_from_value(value: &Value) -> Result<Track, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut name = None;
    let mut gain = None;
    let mut pan = None;
    let mut mute = None;
    let mut solo = None;
    let mut clips = None;
    let mut effects = None;
    for (key, value) in entries {
        match key.as_str() {
            "effects" => effects = Some(effects_from_value(value)?),
            "name" => name = Some(require_string(value)?),
            "gain" => gain = Some(require_f32(value)?),
            "pan" => pan = Some(require_f32(value)?),
            "mute" => mute = Some(require_bool(value)?),
            "solo" => solo = Some(require_bool(value)?),
            "clips" => {
                let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
                // Same reasoning as for tracks: cap before decoding.
                if items.len() > MAX_CLIPS_PER_TRACK {
                    return Err(ProjectError::Session(SessionError::TooManyClips {
                        track: 0,
                        max: MAX_CLIPS_PER_TRACK,
                    }));
                }
                clips = Some(
                    items
                        .iter()
                        .map(clip_from_value)
                        .collect::<Result<_, _>>()?,
                );
            }
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(Track {
        name: name.ok_or(ProjectError::MissingField {
            field: "session.tracks[].name",
        })?,
        gain: gain.ok_or(ProjectError::MissingField {
            field: "session.tracks[].gain",
        })?,
        pan: pan.unwrap_or(0.0),
        mute: mute.ok_or(ProjectError::MissingField {
            field: "session.tracks[].mute",
        })?,
        solo: solo.unwrap_or(false),
        clips: clips.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips",
        })?,
        // Optional since SPEC-022.
        effects: effects.unwrap_or_default(),
    })
}

/// Appends a track's `"effects"` array, one effect per line (SPEC-022).
fn write_effects(out: &mut String, effects: &[Effect]) {
    out.push_str(",\n        \"effects\": [");
    for (index, effect) in effects.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n          {\"type\": ");
        write_string(out, effect.kind.type_name());
        out.push_str(", \"bypass\": ");
        out.push_str(if effect.bypass { "true" } else { "false" });
        let fields: &[(&str, f32)] = match effect.kind {
            EffectKind::Eq {
                low_db,
                mid_db,
                mid_hz,
                high_db,
            } => &[
                ("low_db", low_db),
                ("mid_db", mid_db),
                ("mid_hz", mid_hz),
                ("high_db", high_db),
            ],
            EffectKind::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => &[
                ("threshold_db", threshold_db),
                ("ratio", ratio),
                ("attack_ms", attack_ms),
                ("release_ms", release_ms),
                ("makeup_db", makeup_db),
            ],
            EffectKind::Reverb { room, damp, mix } => {
                &[("room", room), ("damp", damp), ("mix", mix)]
            }
        };
        for (key, value) in fields {
            out.push_str(", ");
            write_string(out, key);
            out.push_str(": ");
            out.push_str(&format_number(f64::from(*value)));
        }
        out.push('}');
    }
    out.push_str("\n        ]");
}

fn effects_from_value(value: &Value) -> Result<Vec<Effect>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    // Cap before decoding, like tracks, clips and notes.
    if items.len() > MAX_EFFECTS_PER_TRACK {
        return Err(ProjectError::Session(SessionError::InvalidEffect {
            track: 0,
            effect: MAX_EFFECTS_PER_TRACK,
        }));
    }
    items.iter().map(effect_from_value).collect()
}

fn effect_from_value(value: &Value) -> Result<Effect, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut kind_name = None;
    let mut bypass = None;
    for (key, value) in entries {
        match key.as_str() {
            "type" => kind_name = Some(require_string(value)?),
            "bypass" => bypass = Some(require_bool(value)?),
            _ => {}
        }
    }
    let kind_name = kind_name.ok_or(ProjectError::MissingField {
        field: "session.tracks[].effects[].type",
    })?;
    let allowed: &[&str] = match kind_name.as_str() {
        "eq" => &["low_db", "mid_db", "mid_hz", "high_db"],
        "compressor" => &[
            "threshold_db",
            "ratio",
            "attack_ms",
            "release_ms",
            "makeup_db",
        ],
        "reverb" => &["room", "damp", "mix"],
        _ => return Err(ProjectError::Malformed { at: 0 }),
    };
    let mut values = [None; 5];
    for (key, value) in entries {
        if key == "type" || key == "bypass" {
            continue;
        }
        let Some(slot) = allowed.iter().position(|name| name == key) else {
            return Err(ProjectError::UnknownField { field: key.clone() });
        };
        values[slot] = Some(require_f32(value)?);
    }
    let get = |slot: usize| {
        values[slot].ok_or(ProjectError::MissingField {
            field: "session.tracks[].effects[] parameter",
        })
    };
    let kind = match kind_name.as_str() {
        "eq" => EffectKind::Eq {
            low_db: get(0)?,
            mid_db: get(1)?,
            mid_hz: get(2)?,
            high_db: get(3)?,
        },
        "compressor" => EffectKind::Compressor {
            threshold_db: get(0)?,
            ratio: get(1)?,
            attack_ms: get(2)?,
            release_ms: get(3)?,
            makeup_db: get(4)?,
        },
        _ => EffectKind::Reverb {
            room: get(0)?,
            damp: get(1)?,
            mix: get(2)?,
        },
    };
    Ok(Effect {
        bypass: bypass.unwrap_or(false),
        kind,
    })
}

fn require_bool(value: &Value) -> Result<bool, ProjectError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        _ => Err(ProjectError::Malformed { at: 0 }),
    }
}

fn clip_from_value(value: &Value) -> Result<Clip, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut start = None;
    let mut length = None;
    let mut offset = None;
    let mut source_length = None;
    let mut gain = None;
    let mut name = None;
    let mut source = None;
    let mut notes = None;
    for (key, value) in entries {
        match key.as_str() {
            "notes" => notes = Some(notes_from_value(value)?),
            "start" => start = Some(require_u64(value)?),
            "length" => length = Some(require_u64(value)?),
            "offset" => offset = Some(require_u64(value)?),
            "source_length" => source_length = Some(require_u64(value)?),
            "gain" => gain = Some(require_f32(value)?),
            "name" => name = Some(require_string(value)?),
            "source" => source = Some(require_string(value)?),
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(Clip {
        start_samples: start.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].start",
        })?,
        length_samples: length.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].length",
        })?,
        offset_samples: offset.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].offset",
        })?,
        source_length: source_length.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].source_length",
        })?,
        gain: gain.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].gain",
        })?,
        name: name.ok_or(ProjectError::MissingField {
            field: "session.tracks[].clips[].name",
        })?,
        source,
        notes,
    })
}

/// Appends a MIDI clip's `"notes"` array, one note per line (SPEC-021).
fn write_notes(out: &mut String, notes: &[Note]) {
    out.push_str(",\n            \"notes\": [");
    for (index, note) in notes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n              {\"pitch\": ");
        out.push_str(&note.pitch.to_string());
        out.push_str(", \"start\": ");
        out.push_str(&note.start.to_string());
        out.push_str(", \"length\": ");
        out.push_str(&note.length.to_string());
        out.push_str(", \"velocity\": ");
        out.push_str(&note.velocity.to_string());
        out.push('}');
    }
    if notes.is_empty() {
        out.push(']');
    } else {
        out.push_str("\n            ]");
    }
}

fn notes_from_value(value: &Value) -> Result<Vec<Note>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    // Cap before decoding, like tracks and clips.
    if items.len() > MAX_NOTES_PER_CLIP {
        return Err(ProjectError::Session(SessionError::InvalidNote {
            track: 0,
            clip: 0,
            note: MAX_NOTES_PER_CLIP,
        }));
    }
    items.iter().map(note_from_value).collect()
}

fn note_from_value(value: &Value) -> Result<Note, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut pitch = None;
    let mut start = None;
    let mut length = None;
    let mut velocity = None;
    for (key, value) in entries {
        match key.as_str() {
            "pitch" => pitch = Some(require_u8(value)?),
            "start" => start = Some(require_u64(value)?),
            "length" => length = Some(require_u64(value)?),
            "velocity" => velocity = Some(require_u8(value)?),
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    let field = |field| ProjectError::MissingField { field };
    Ok(Note {
        pitch: pitch.ok_or(field("session.tracks[].clips[].notes[].pitch"))?,
        start: start.ok_or(field("session.tracks[].clips[].notes[].start"))?,
        length: length.ok_or(field("session.tracks[].clips[].notes[].length"))?,
        velocity: velocity.ok_or(field("session.tracks[].clips[].notes[].velocity"))?,
    })
}

fn require_u8(value: &Value) -> Result<u8, ProjectError> {
    u8::try_from(require_u32(value)?).map_err(|_| ProjectError::Malformed { at: 0 })
}

/// The subset of JSON values the codec can hold.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn parse_document(&mut self) -> Result<Value, ProjectError> {
        self.skip_whitespace();
        if self.pos >= self.bytes.len() {
            return Err(ProjectError::Malformed { at: 0 });
        }
        let value = self.parse_value(0)?;
        // Anything left over, whitespace included, is a syntax error. Checking
        // only whether the rest is whitespace keeps this linear: trimming the
        // tail frame by frame would be quadratic on a long trailing comment
        // of spaces (a 1 MB hostile input is a spec case).
        if let Some(offset) = self.bytes[self.pos..]
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
        {
            return Err(ProjectError::Malformed {
                at: self.pos + offset,
            });
        }
        Ok(value)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, ProjectError> {
        if depth > MAX_DEPTH {
            return Err(ProjectError::Unsupported { at: self.pos });
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => Ok(Value::String(self.parse_string()?)),
            Some(b't') => self.parse_literal(b"true", Value::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", Value::Bool(false)),
            Some(b'n') => Err(ProjectError::Unsupported { at: self.pos }),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(ProjectError::Malformed { at: self.pos }),
        }
    }

    fn parse_literal(&mut self, literal: &[u8], value: Value) -> Result<Value, ProjectError> {
        let end = self.pos + literal.len();
        if self.bytes.get(self.pos..end) == Some(literal) {
            self.pos = end;
            Ok(value)
        } else {
            Err(ProjectError::Malformed { at: self.pos })
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, ProjectError> {
        self.pos += 1; // '{'
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(ProjectError::Malformed { at: self.pos });
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(ProjectError::Malformed { at: self.pos });
            }
            self.pos += 1;
            let value = self.parse_value(depth + 1)?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(ProjectError::Malformed { at: self.pos }),
            }
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, ProjectError> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            let value = self.parse_value(depth + 1)?;
            items.push(value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(ProjectError::Malformed { at: self.pos }),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, ProjectError> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(ProjectError::Malformed { at: self.pos });
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let escape_at = self.pos;
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'u') => {
                            return Err(ProjectError::Unsupported { at: escape_at });
                        }
                        _ => return Err(ProjectError::Malformed { at: escape_at }),
                    }
                    self.pos += 1;
                }
                0x00..=0x1f => return Err(ProjectError::Malformed { at: self.pos }),
                // Copy the whole run up to the next special byte at once.
                // Every special byte is ASCII, so the run ends on a char
                // boundary and multi-byte UTF-8 passes through unchanged.
                // Decoding only the run keeps this linear (SPEC-012).
                _ => {
                    let start = self.pos;
                    while matches!(self.peek(), Some(byte) if byte >= 0x20 && byte != b'"' && byte != b'\\')
                    {
                        self.pos += 1;
                    }
                    let run = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|_| ProjectError::Malformed { at: start })?;
                    out.push_str(run);
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, ProjectError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let mut digits = 0_usize;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
            digits += 1;
        }
        if digits == 0 {
            return Err(ProjectError::Malformed { at: start });
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let mut fraction = 0_usize;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
                fraction += 1;
            }
            if fraction == 0 {
                return Err(ProjectError::Malformed { at: start });
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            return Err(ProjectError::Unsupported { at: self.pos });
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| ProjectError::Malformed { at: start })?;
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| ProjectError::Malformed { at: start })
    }
}

/// Wraps the parser's map back into a [`Project`], rejecting stray keys.
fn project_from_object(entries: &[(String, Value)]) -> Result<Project, ProjectError> {
    let mut name = None;
    let mut sample_rate = None;
    let mut tempo_bpm = None;
    let mut params = None;
    let mut session = None;
    for (key, value) in entries {
        match key.as_str() {
            "name" => name = Some(require_string(value)?),
            "sample_rate" => sample_rate = Some(require_u32(value)?),
            "tempo_bpm" => tempo_bpm = Some(require_f64(value)?),
            "params" => params = Some(params_from_array(value)?),
            "session" => session = Some(session_from_value(value)?),
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(Project {
        name: name.ok_or(ProjectError::MissingField { field: "name" })?,
        sample_rate: sample_rate.ok_or(ProjectError::MissingField {
            field: "sample_rate",
        })?,
        tempo_bpm: tempo_bpm.ok_or(ProjectError::MissingField { field: "tempo_bpm" })?,
        params: params.ok_or(ProjectError::MissingField { field: "params" })?,
        session: session.ok_or(ProjectError::MissingField { field: "session" })?,
    })
}

fn require_string(value: &Value) -> Result<String, ProjectError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(ProjectError::Malformed { at: 0 })
}

fn require_f64(value: &Value) -> Result<f64, ProjectError> {
    value.as_f64().ok_or(ProjectError::Malformed { at: 0 })
}

fn require_u32(value: &Value) -> Result<u32, ProjectError> {
    let number = value.as_f64().ok_or(ProjectError::Malformed { at: 0 })?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > f64::from(u32::MAX)
    {
        return Err(ProjectError::Malformed { at: 0 });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = number as u32;
    Ok(out)
}

fn require_u64(value: &Value) -> Result<u64, ProjectError> {
    let number = value.as_f64().ok_or(ProjectError::Malformed { at: 0 })?;
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > 9.007_199_254_740_992e15
    {
        return Err(ProjectError::Malformed { at: 0 });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = number as u64;
    Ok(out)
}

#[allow(clippy::cast_possible_truncation)]
fn require_f32(value: &Value) -> Result<f32, ProjectError> {
    let number = value.as_f64().ok_or(ProjectError::Malformed { at: 0 })?;
    if number.abs() > f64::from(f32::MAX) {
        return Err(ProjectError::Malformed { at: 0 });
    }
    Ok(number as f32)
}

fn params_from_array(value: &Value) -> Result<Vec<ParamState>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    items.iter().map(param_from_object).collect()
}

fn param_from_object(value: &Value) -> Result<ParamState, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut id = None;
    let mut name = None;
    let mut min = None;
    let mut max = None;
    let mut default = None;
    let mut smoothing_ms = None;
    let mut stored = None;
    for (key, value) in entries {
        match key.as_str() {
            "id" => id = Some(require_u32(value)?),
            "name" => name = Some(require_string(value)?),
            "min" => min = Some(require_f32(value)?),
            "max" => max = Some(require_f32(value)?),
            "default" => default = Some(require_f32(value)?),
            "smoothing_ms" => smoothing_ms = Some(require_u32(value)?),
            "value" => stored = Some(require_f32(value)?),
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(ParamState {
        id: id.ok_or(ProjectError::MissingField { field: "id" })?,
        name: name.ok_or(ProjectError::MissingField { field: "name" })?,
        min: min.ok_or(ProjectError::MissingField { field: "min" })?,
        max: max.ok_or(ProjectError::MissingField { field: "max" })?,
        default: default.ok_or(ProjectError::MissingField { field: "default" })?,
        smoothing_ms: smoothing_ms.ok_or(ProjectError::MissingField {
            field: "smoothing_ms",
        })?,
        value: stored.ok_or(ProjectError::MissingField { field: "value" })?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(id: u32, value: f32) -> ParamState {
        ParamState {
            id,
            name: format!("p{id}"),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            smoothing_ms: 10,
            value,
        }
    }

    fn project() -> Project {
        Project {
            name: "demo".to_owned(),
            sample_rate: 48_000,
            tempo_bpm: 120.0,
            params: vec![param(0, 0.25), param(1, 1.0)],
            session: Session::new("demo", 48_000, 120.0),
        }
    }

    #[test]
    fn round_trip_is_lossless() {
        let original = project();
        let text = original.to_json();
        let parsed = Project::from_json(&text).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn output_is_deterministic_and_field_order_is_fixed() {
        let first = project().to_json();
        let second = project().to_json();
        assert_eq!(first, second);
        let name = first.find("\"name\"").unwrap();
        let rate = first.find("\"sample_rate\"").unwrap();
        let tempo = first.find("\"tempo_bpm\"").unwrap();
        let params = first.find("\"params\"").unwrap();
        assert!(name < rate && rate < tempo && tempo < params);
        assert!(first.ends_with("}\n"), "trailing newline");
    }

    #[test]
    fn empty_param_list_round_trips() {
        let empty = Project {
            params: Vec::new(),
            ..project()
        };
        assert_eq!(Project::from_json(&empty.to_json()).unwrap(), empty);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let text = project().to_json().replace("tempo_bpm", "tempoBPM");
        assert_eq!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::UnknownField {
                field: "tempoBPM".to_owned()
            }
        );
    }

    #[test]
    fn missing_key_is_named() {
        let err = Project::from_json("{}").unwrap_err();
        assert_eq!(err, ProjectError::MissingField { field: "name" });
    }

    #[test]
    fn null_and_exponents_are_unsupported() {
        let text = project()
            .to_json()
            .replace("\"sample_rate\": 48000", "\"sample_rate\": null");
        assert!(matches!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::Unsupported { .. }
        ));
        let text = project().to_json().replace("120", "1.2e2");
        let err = Project::from_json(&text).unwrap_err();
        assert!(matches!(err, ProjectError::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn unicode_escape_is_unsupported() {
        let text = project().to_json().replace("\"demo\"", "\"d\\u0041mo\"");
        assert!(matches!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::Unsupported { .. }
        ));
    }

    #[test]
    fn deep_nesting_is_unsupported() {
        let mut text = String::new();
        for _ in 0..MAX_DEPTH + 2 {
            text.push('[');
        }
        for _ in 0..MAX_DEPTH + 2 {
            text.push(']');
        }
        assert!(matches!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::Unsupported { .. }
        ));
    }

    #[test]
    fn unterminated_string_is_malformed() {
        let text = project().to_json().replace("\"demo\"", "\"demo");
        assert!(matches!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::Malformed { .. }
        ));
    }

    #[test]
    fn empty_input_is_malformed_at_zero() {
        assert_eq!(
            Project::from_json("").unwrap_err(),
            ProjectError::Malformed { at: 0 }
        );
    }

    #[test]
    fn negative_sample_rate_is_rejected() {
        let text = project()
            .to_json()
            .replace("\"sample_rate\": 48000", "\"sample_rate\": -48000");
        assert!(matches!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::Malformed { .. }
        ));
    }

    #[test]
    fn zero_tempo_is_rejected() {
        let mut p = project();
        p.tempo_bpm = 0.0;
        let text = p.to_json();
        assert!(text.contains("\"tempo_bpm\": 0,"), "{text}");
        // `to_json` writes 0 for 0.0, and validation must reject it.
        assert_eq!(
            Project::from_json(&text).unwrap_err(),
            ProjectError::InvalidTempo
        );
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        let mut p = project();
        p.sample_rate = 0;
        assert_eq!(
            Project::from_json(&p.to_json()).unwrap_err(),
            ProjectError::InvalidSampleRate
        );
    }

    #[test]
    fn duplicate_param_id_is_rejected() {
        let mut p = project();
        p.params[1].id = 0;
        assert_eq!(
            Project::from_json(&p.to_json()).unwrap_err(),
            ProjectError::DuplicateParam { id: 0 }
        );
    }

    #[test]
    fn value_outside_range_is_rejected() {
        let mut p = project();
        p.params[0].value = 2.0;
        assert_eq!(
            Project::from_json(&p.to_json()).unwrap_err(),
            ProjectError::InvalidParam { id: 0 }
        );
    }

    #[test]
    fn inverted_range_is_rejected() {
        let mut p = project();
        p.params[0].min = 2.0;
        p.params[0].max = 1.0;
        assert!(matches!(
            Project::from_json(&p.to_json()).unwrap_err(),
            ProjectError::InvalidParam { id: 0 }
        ));
    }

    #[test]
    fn long_name_is_rejected() {
        let mut p = project();
        p.name = "x".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(
            Project::from_json(&p.to_json()).unwrap_err(),
            ProjectError::NameTooLong
        );
    }

    #[test]
    fn numbers_with_fractions_round_trip_exactly() {
        let mut p = project();
        p.tempo_bpm = 128.5;
        p.params[0].value = 0.1;
        let text = p.to_json();
        let back = Project::from_json(&text).unwrap();
        assert_eq!(back.tempo_bpm.to_bits(), 128.5_f64.to_bits());
        assert_eq!(back.params[0].value.to_bits(), 0.1_f32.to_bits());
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut buf = vec![0_u8; 1024 * 1024];
        for byte in &mut buf {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            #[allow(clippy::cast_possible_truncation)]
            let low = (seed >> 24) as u8;
            *byte = low;
        }
        let started = std::time::Instant::now();
        assert!(Project::from_json(&String::from_utf8_lossy(&buf)).is_err());
        assert!(started.elapsed().as_secs() < 1);
    }
}
