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

use crate::fx::{ChannelEqBand, Effect, EffectKind, EffectParam, MAX_EFFECTS_PER_TRACK};
use crate::graph::Graph;
use crate::params::{ParamHandle, ParamId, ParamSpec};
use crate::session::{
    AutoPoint, Automation, Bus, BusSend, Clip, EffectLane, Lane, MAX_AUTOMATION_POINTS, MAX_BUSES,
    MAX_CLIPS_PER_TRACK, MAX_EFFECT_LANES, MAX_MARKERS, MAX_NOTES_PER_CLIP, MAX_SOURCE_BYTES,
    MAX_TRACKS, Marker, Note, Session, SessionError, Track, TrackFolder, TrackFormat,
    TrackInstrument, TrackKind,
};
use crate::transport::{TempoPoint, TimeSignature};

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

/// Writes one clip object at the indent used inside a track's `"clips"`.
fn write_clip(out: &mut String, clip: &crate::session::Clip) {
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
    if clip.fade_in_samples > 0 {
        out.push_str(",\n            \"fade_in\": ");
        out.push_str(&clip.fade_in_samples.to_string());
    }
    if clip.fade_out_samples > 0 {
        out.push_str(",\n            \"fade_out\": ");
        out.push_str(&clip.fade_out_samples.to_string());
    }
    if clip.fade_in_curve != 0.0 {
        out.push_str(",\n            \"fade_in_curve\": ");
        out.push_str(&format_number(f64::from(clip.fade_in_curve)));
    }
    if clip.fade_out_curve != 0.0 {
        out.push_str(",\n            \"fade_out_curve\": ");
        out.push_str(&format_number(f64::from(clip.fade_out_curve)));
    }
    if let Some(notes) = &clip.notes {
        write_notes(out, notes);
    }
    out.push_str("\n          }");
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
    // Written only when there are additional points, so single-tempo documents
    // keep their exact text (SPEC-054).
    write_tempo_map(out, session);
    out.push_str(",\n    \"master_gain\": ");
    out.push_str(&format_number(f64::from(session.master_gain)));
    // Written only when set, so older documents keep their exact text.
    if session.master_mute {
        out.push_str(",\n    \"master_mute\": true");
    }
    if session.master_dim {
        out.push_str(",\n    \"master_dim\": true");
    }
    // Written only when it differs from 4/4, so older documents keep their
    // exact text (SPEC-026).
    if session.time_signature != TimeSignature::default() {
        out.push_str(",\n    \"time_signature\": [");
        out.push_str(&session.time_signature.numerator.to_string());
        out.push_str(", ");
        out.push_str(&session.time_signature.denominator.to_string());
        out.push(']');
    }
    out.push_str(",\n    \"tracks\": [");
    for (track_index, track) in session.tracks.iter().enumerate() {
        if track_index > 0 {
            out.push(',');
        }
        out.push_str("\n      {\n        \"name\": ");
        write_string(out, &track.name);
        if track.kind == TrackKind::Instrument {
            out.push_str(",\n        \"kind\": \"instrument\"");
        }
        // Written only when not the default, so pre-SPEC-065 documents keep
        // their exact text (SPEC-065).
        if let TrackInstrument::Sampler { bank_path } = &track.instrument {
            out.push_str(",\n        \"instrument\": { \"sampler\": ");
            write_string(out, bank_path);
            out.push_str(" }");
        }
        if track.format == TrackFormat::Stereo {
            out.push_str(",\n        \"format\": \"stereo\"");
        }
        out.push_str(",\n        \"gain\": ");
        out.push_str(&format_number(f64::from(track.gain)));
        out.push_str(",\n        \"pan\": ");
        out.push_str(&format_number(f64::from(track.pan)));
        out.push_str(",\n        \"mute\": ");
        out.push_str(if track.mute { "true" } else { "false" });
        out.push_str(",\n        \"solo\": ");
        out.push_str(if track.solo { "true" } else { "false" });
        if track.input_monitor {
            out.push_str(",\n        \"input_monitor\": true");
        }
        out.push_str(",\n        \"clips\": [");
        for (clip_index, clip) in track.clips.iter().enumerate() {
            if clip_index > 0 {
                out.push(',');
            }
            write_clip(out, clip);
        }
        if track.clips.is_empty() {
            out.push(']');
        } else {
            out.push_str("\n        ]");
        }
        if !track.effects.is_empty() {
            write_effects(out, &track.effects);
        }
        if !track.automation.is_empty() {
            write_automation(out, &track.automation);
        }
        if !track.sends.is_empty() {
            write_sends(out, &track.sends);
        }
        if let Some(color) = track.color {
            out.push_str(",\n        \"color\": ");
            out.push_str(&color.to_string());
        }
        write_text_notes(out, "        ", &track.notes);
        out.push_str("\n      }");
    }
    if session.tracks.is_empty() {
        out.push(']');
    } else {
        out.push_str("\n    ]");
    }
    // Written only when present, so older documents keep their exact text
    // (SPEC-028).
    if !session.buses.is_empty() {
        write_buses(out, &session.buses);
    }
    // Written only when present (SPEC-030).
    if !session.markers.is_empty() {
        write_markers(out, &session.markers);
    }
    // Written only when present, so older documents keep their exact text
    // (SPEC-055).
    if !session.folders.is_empty() {
        write_folders(out, &session.folders, &session.folder_children);
    }
    // Written only when present (SPEC-043).
    write_text_notes(out, "    ", &session.notes);
    out.push_str("\n  }");
}

/// Appends the session's `"tempo_map"` array (SPEC-054). Written only when
/// there are additional points, so single-tempo documents keep their exact
/// text.
fn write_tempo_map(out: &mut String, session: &Session) {
    if session.tempo_map.is_empty() {
        return;
    }
    out.push_str(",\n    \"tempo_map\": [");
    for (index, point) in session.tempo_map.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n      [");
        out.push_str(&point.position_ticks.to_string());
        out.push_str(", ");
        out.push_str(&format_number(point.bpm));
        out.push(']');
    }
    out.push_str("\n    ]");
}

/// Appends a track's `"sends"` array (SPEC-028).
fn write_sends(out: &mut String, sends: &[BusSend]) {
    out.push_str(",\n        \"sends\": [");
    for (index, send) in sends.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str("{\"bus\": ");
        out.push_str(&send.bus.to_string());
        out.push_str(", \"level\": ");
        out.push_str(&format_number(f64::from(send.level)));
        out.push('}');
    }
    out.push(']');
}

/// Appends the session's `"buses"` array (SPEC-028). Bus effects reuse the
/// track effect format at the same indent; SPEC-051 adds `sends`, `mute` and
/// `solo`, written only when present/`true`.
fn write_buses(out: &mut String, buses: &[Bus]) {
    out.push_str(",\n    \"buses\": [");
    for (index, bus) in buses.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n      {\n        \"name\": ");
        write_string(out, &bus.name);
        out.push_str(",\n        \"gain\": ");
        out.push_str(&format_number(f64::from(bus.gain)));
        if !bus.effects.is_empty() {
            write_effects(out, &bus.effects);
        }
        if !bus.sends.is_empty() {
            out.push_str(",\n        \"sends\": [");
            for (send_index, send) in bus.sends.iter().enumerate() {
                if send_index > 0 {
                    out.push_str(", ");
                }
                out.push_str("{\"bus\": ");
                out.push_str(&send.bus.to_string());
                out.push_str(", \"level\": ");
                out.push_str(&format_number(f64::from(send.level)));
                out.push('}');
            }
            out.push(']');
        }
        if bus.mute {
            out.push_str(",\n        \"mute\": true");
        }
        if bus.solo {
            out.push_str(",\n        \"solo\": true");
        }
        out.push_str("\n      }");
    }
    out.push_str("\n    ]");
}

/// Appends the session's `"folders"` array (SPEC-055).
fn write_folders(out: &mut String, folders: &[TrackFolder], children: &[Vec<usize>]) {
    out.push_str(",\n    \"folders\": [");
    for (index, folder) in folders.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n      {\"name\": ");
        write_string(out, &folder.name);
        out.push_str(", \"children\": [");
        for (child_index, child) in children[index].iter().enumerate() {
            if child_index > 0 {
                out.push_str(", ");
            }
            out.push_str(&child.to_string());
        }
        out.push(']');
        if folder.collapsed {
            out.push_str(", \"collapsed\": true");
        }
        if let Some(color) = folder.color {
            out.push_str(", \"color\": ");
            out.push_str(&color.to_string());
        }
        out.push('}');
    }
    out.push_str("\n    ]");
}

fn folders_from_value(value: &Value) -> Result<(Vec<TrackFolder>, Vec<Vec<usize>>), ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    if items.len() > MAX_TRACKS {
        return Err(ProjectError::Session(SessionError::TooManyTracks {
            max: MAX_TRACKS,
        }));
    }
    let mut folders = Vec::with_capacity(items.len());
    let mut children = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(entries) = item else {
            return Err(ProjectError::Malformed { at: 0 });
        };
        let mut name = None;
        let mut child_indices = None;
        let mut collapsed = None;
        let mut color = None;
        for (key, value) in entries {
            match key.as_str() {
                "name" => name = Some(require_string(value)?),
                "children" => {
                    let list = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
                    let mut indices = Vec::with_capacity(list.len());
                    for entry in list {
                        let idx = require_u64(entry)?;
                        let idx =
                            usize::try_from(idx).map_err(|_| ProjectError::Malformed { at: 0 })?;
                        indices.push(idx);
                    }
                    child_indices = Some(indices);
                }
                "collapsed" => collapsed = Some(require_bool(value)?),
                "color" => color = Some(require_u8(value)?),
                other => {
                    return Err(ProjectError::UnknownField {
                        field: other.to_owned(),
                    });
                }
            }
        }
        folders.push(TrackFolder {
            name: name.ok_or(ProjectError::MissingField {
                field: "session.folders[].name",
            })?,
            color,
            collapsed: collapsed.unwrap_or(false),
        });
        children.push(child_indices.ok_or(ProjectError::MissingField {
            field: "session.folders[].children",
        })?);
    }
    Ok((folders, children))
}

/// Appends the session's `"markers"` array (SPEC-030).
fn write_markers(out: &mut String, markers: &[Marker]) {
    out.push_str(",\n    \"markers\": [");
    for (index, marker) in markers.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n      {\"name\": ");
        write_string(out, &marker.name);
        out.push_str(", \"position\": ");
        out.push_str(&marker.position.to_string());
        out.push('}');
    }
    out.push_str("\n    ]");
}

fn markers_from_value(value: &Value) -> Result<Vec<Marker>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    if items.len() > MAX_MARKERS {
        return Err(ProjectError::Session(SessionError::InvalidMarker {
            marker: MAX_MARKERS,
        }));
    }
    items
        .iter()
        .map(|item| {
            let Value::Object(entries) = item else {
                return Err(ProjectError::Malformed { at: 0 });
            };
            let mut name = None;
            let mut position = None;
            for (key, value) in entries {
                match key.as_str() {
                    "name" => name = Some(require_string(value)?),
                    "position" => position = Some(require_u64(value)?),
                    other => {
                        return Err(ProjectError::UnknownField {
                            field: other.to_owned(),
                        });
                    }
                }
            }
            Ok(Marker {
                name: name.ok_or(ProjectError::MissingField {
                    field: "session.markers[].name",
                })?,
                position: position.ok_or(ProjectError::MissingField {
                    field: "session.markers[].position",
                })?,
            })
        })
        .collect()
}

fn sends_from_value(value: &Value) -> Result<Vec<BusSend>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    if items.len() > MAX_BUSES {
        return Err(ProjectError::Session(SessionError::InvalidSend {
            track: 0,
            send: MAX_BUSES,
        }));
    }
    items
        .iter()
        .map(|item| {
            let Value::Object(entries) = item else {
                return Err(ProjectError::Malformed { at: 0 });
            };
            let mut bus = None;
            let mut level = None;
            for (key, value) in entries {
                match key.as_str() {
                    "bus" => bus = Some(require_u32(value)?),
                    "level" => level = Some(require_f32(value)?),
                    other => {
                        return Err(ProjectError::UnknownField {
                            field: other.to_owned(),
                        });
                    }
                }
            }
            Ok(BusSend {
                bus: bus.ok_or(ProjectError::MissingField {
                    field: "session.tracks[].sends[].bus",
                })? as usize,
                level: level.ok_or(ProjectError::MissingField {
                    field: "session.tracks[].sends[].level",
                })?,
            })
        })
        .collect()
}

fn buses_from_value(value: &Value) -> Result<Vec<Bus>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    if items.len() > MAX_BUSES {
        return Err(ProjectError::Session(SessionError::InvalidBus {
            bus: MAX_BUSES,
        }));
    }
    items
        .iter()
        .map(|item| {
            let Value::Object(entries) = item else {
                return Err(ProjectError::Malformed { at: 0 });
            };
            let mut name = None;
            let mut gain = None;
            let mut effects = None;
            let mut sends = None;
            let mut mute = None;
            let mut solo = None;
            for (key, value) in entries {
                match key.as_str() {
                    "name" => name = Some(require_string(value)?),
                    "gain" => gain = Some(require_f32(value)?),
                    "effects" => effects = Some(effects_from_value(value)?),
                    "sends" => sends = Some(sends_from_value(value)?),
                    "mute" => mute = Some(require_bool(value)?),
                    "solo" => solo = Some(require_bool(value)?),
                    other => {
                        return Err(ProjectError::UnknownField {
                            field: other.to_owned(),
                        });
                    }
                }
            }
            Ok(Bus {
                name: name.ok_or(ProjectError::MissingField {
                    field: "session.buses[].name",
                })?,
                gain: gain.unwrap_or(1.0),
                effects: effects.unwrap_or_default(),
                mute: mute.unwrap_or(false),
                solo: solo.unwrap_or(false),
                sends: sends.unwrap_or_default(),
            })
        })
        .collect()
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
    let mut master_mute = None;
    let mut master_dim = None;
    let mut time_signature = None;
    let mut tracks = None;
    let mut buses = None;
    let mut markers = None;
    let mut notes = None;
    let mut tempo_map = None;
    let mut folders = None;
    let mut folder_children = None;
    for (key, value) in entries {
        match key.as_str() {
            "notes" => notes = Some(require_string(value)?),
            "buses" => buses = Some(buses_from_value(value)?),
            "markers" => markers = Some(markers_from_value(value)?),
            "folders" => {
                let (folders_val, children_val) = folders_from_value(value)?;
                folders = Some(folders_val);
                folder_children = Some(children_val);
            }
            "name" => name = Some(require_string(value)?),
            "sample_rate" => sample_rate = Some(require_u32(value)?),
            "tempo_bpm" => tempo_bpm = Some(require_f64(value)?),
            "master_gain" => master_gain = Some(require_f32(value)?),
            "master_mute" => master_mute = Some(require_bool(value)?),
            "master_dim" => master_dim = Some(require_bool(value)?),
            "time_signature" => time_signature = Some(time_signature_from_value(value)?),
            "tempo_map" => tempo_map = Some(tempo_map_from_value(value)?),
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
        // Optional since SPEC-044.
        master_mute: master_mute.unwrap_or(false),
        master_dim: master_dim.unwrap_or(false),
        // Optional since SPEC-026; absent means 4/4.
        time_signature: time_signature.unwrap_or_default(),
        tracks: tracks.ok_or(ProjectError::MissingField {
            field: "session.tracks",
        })?,
        // Optional since SPEC-028.
        buses: buses.unwrap_or_default(),
        // Optional since SPEC-030.
        markers: markers.unwrap_or_default(),
        // Optional since SPEC-043.
        notes: notes.unwrap_or_default(),
        // Optional since SPEC-054; absent means a single point at `tempo_bpm`.
        tempo_map: tempo_map.unwrap_or_default(),
        // Optional since SPEC-055; absent means no track folders.
        folders: folders.unwrap_or_default(),
        folder_children: folder_children.unwrap_or_default(),
    })
}

fn time_signature_from_value(value: &Value) -> Result<TimeSignature, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    let [numerator, denominator] = items else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    Ok(TimeSignature {
        numerator: require_u8(numerator)?,
        denominator: require_u8(denominator)?,
    })
}

fn tempo_map_from_value(value: &Value) -> Result<Vec<TempoPoint>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    items
        .iter()
        .map(|point| {
            let pair = point.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
            let [ticks, bpm] = pair else {
                return Err(ProjectError::Malformed { at: 0 });
            };
            Ok(TempoPoint {
                position_ticks: require_u64(ticks)?,
                bpm: require_f64(bpm)?,
            })
        })
        .collect()
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
    let mut input_monitor = None;
    let mut clips = None;
    let mut effects = None;
    let mut automation = None;
    let mut sends = None;
    let mut color = None;
    let mut notes = None;
    let mut kind = None;
    let mut format = None;
    let mut instrument = None;
    for (key, value) in entries {
        match key.as_str() {
            "kind" => kind = Some(kind_from_value(value)?),
            "format" => format = Some(format_from_value(value)?),
            "instrument" => instrument = Some(instrument_from_value(value)?),
            "notes" => notes = Some(require_string(value)?),
            "color" => color = Some(require_u8(value)?),
            "sends" => sends = Some(sends_from_value(value)?),
            "effects" => effects = Some(effects_from_value(value)?),
            "automation" => automation = Some(automation_from_value(value)?),
            "name" => name = Some(require_string(value)?),
            "gain" => gain = Some(require_f32(value)?),
            "pan" => pan = Some(require_f32(value)?),
            "mute" => mute = Some(require_bool(value)?),
            "solo" => solo = Some(require_bool(value)?),
            "input_monitor" => input_monitor = Some(require_bool(value)?),
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
    let clips: Vec<Clip> = clips.ok_or(ProjectError::MissingField {
        field: "session.tracks[].clips",
    })?;
    // Optional since SPEC-049: older projects made MIDI tracks implicitly.
    let kind = kind.unwrap_or(if clips.iter().any(Clip::is_midi) {
        TrackKind::Instrument
    } else {
        TrackKind::Audio
    });
    Ok(Track {
        name: name.ok_or(ProjectError::MissingField {
            field: "session.tracks[].name",
        })?,
        kind,
        // Optional since SPEC-065: older projects have no instrument field.
        instrument: instrument.unwrap_or_default(),
        format: format.unwrap_or_default(),
        gain: gain.ok_or(ProjectError::MissingField {
            field: "session.tracks[].gain",
        })?,
        pan: pan.unwrap_or(0.0),
        mute: mute.ok_or(ProjectError::MissingField {
            field: "session.tracks[].mute",
        })?,
        solo: solo.unwrap_or(false),
        // Optional since SPEC-044.
        input_monitor: input_monitor.unwrap_or(false),
        clips,
        // Optional since SPEC-022.
        effects: effects.unwrap_or_default(),
        // Optional since SPEC-023.
        automation: automation.unwrap_or_default(),
        // Optional since SPEC-028.
        sends: sends.unwrap_or_default(),
        // Optional since SPEC-029.
        color,
        // Optional since SPEC-043.
        notes: notes.unwrap_or_default(),
    })
}

/// Appends a track's `"automation"` object; empty lanes are skipped
/// (SPEC-023).
fn write_automation(out: &mut String, automation: &Automation) {
    out.push_str(",\n        \"automation\": {");
    let mut first = true;
    for lane in [Lane::Gain, Lane::Pan] {
        let points = automation.lane(lane);
        if points.is_empty() {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str("\n          ");
        write_string(out, &lane.to_string());
        out.push_str(": ");
        write_points(out, points);
    }
    if automation.has_effect_points() {
        if !first {
            out.push(',');
        }
        out.push_str("\n          \"effects\": [");
        let lanes = automation
            .effects
            .iter()
            .filter(|lane| !lane.points.is_empty());
        for (index, lane) in lanes.enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "\n            {{\"slot\": {}, \"param\": ", lane.slot);
            write_string(out, lane.param.name());
            out.push_str(", \"points\": ");
            write_points(out, &lane.points);
            out.push('}');
        }
        out.push_str("\n          ]");
    }
    out.push_str("\n        }");
}

/// Appends `[{"time": .., "value": ..}, ..]`.
fn write_points(out: &mut String, points: &[AutoPoint]) {
    out.push('[');
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{{\"time\": {}, \"value\": ", point.time);
        out.push_str(&format_number(f64::from(point.value)));
        out.push('}');
    }
    out.push(']');
}

fn automation_from_value(value: &Value) -> Result<Automation, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut automation = Automation::default();
    for (key, value) in entries {
        let lane = match key.as_str() {
            "gain" => Lane::Gain,
            "pan" => Lane::Pan,
            "effects" => {
                automation.effects = effect_lanes_from_value(value)?;
                continue;
            }
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        };
        let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
        // Cap before decoding, like tracks, clips, notes and effects.
        if items.len() > MAX_AUTOMATION_POINTS {
            return Err(ProjectError::Session(SessionError::InvalidAutomation {
                track: 0,
                lane,
                point: MAX_AUTOMATION_POINTS,
            }));
        }
        *automation.lane_mut(lane) = points_from_items(items)?;
    }
    Ok(automation)
}

fn points_from_items(items: &[Value]) -> Result<Vec<AutoPoint>, ProjectError> {
    items.iter().map(point_from_value).collect()
}

/// The `"effects"` array of an automation object (SPEC-041).
fn effect_lanes_from_value(value: &Value) -> Result<Vec<EffectLane>, ProjectError> {
    let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
    if items.len() > MAX_EFFECT_LANES {
        return Err(ProjectError::Session(SessionError::InvalidAutomation {
            track: 0,
            lane: Lane::Gain,
            point: 0,
        }));
    }
    items.iter().map(effect_lane_from_value).collect()
}

fn effect_lane_from_value(value: &Value) -> Result<EffectLane, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut slot = None;
    let mut param = None;
    let mut points = None;
    for (key, value) in entries {
        match key.as_str() {
            "slot" => slot = Some(require_u8(value)?),
            "param" => {
                let name = require_string(value)?;
                param = Some(
                    EffectParam::from_name(&name)
                        .ok_or(ProjectError::UnknownField { field: name })?,
                );
            }
            "points" => {
                let items = value.as_array().ok_or(ProjectError::Malformed { at: 0 })?;
                if items.len() > MAX_AUTOMATION_POINTS {
                    return Err(ProjectError::Session(SessionError::InvalidAutomation {
                        track: 0,
                        lane: Lane::Gain,
                        point: MAX_AUTOMATION_POINTS,
                    }));
                }
                points = Some(points_from_items(items)?);
            }
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(EffectLane {
        slot: slot.ok_or(ProjectError::MissingField {
            field: "session.tracks[].automation.effects[].slot",
        })?,
        param: param.ok_or(ProjectError::MissingField {
            field: "session.tracks[].automation.effects[].param",
        })?,
        points: points.ok_or(ProjectError::MissingField {
            field: "session.tracks[].automation.effects[].points",
        })?,
    })
}

fn point_from_value(value: &Value) -> Result<AutoPoint, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut time = None;
    let mut point_value = None;
    for (key, value) in entries {
        match key.as_str() {
            "time" => time = Some(require_u64(value)?),
            "value" => point_value = Some(require_f32(value)?),
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    Ok(AutoPoint {
        time: time.ok_or(ProjectError::MissingField {
            field: "session.tracks[].automation[].time",
        })?,
        value: point_value.ok_or(ProjectError::MissingField {
            field: "session.tracks[].automation[].value",
        })?,
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
        if let EffectKind::ChannelEq { bands } = &effect.kind {
            write_channel_eq(out, bands);
            out.push('}');
            continue;
        }
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
                knee_db,
            } => {
                let mut pairs = vec![
                    ("threshold_db", threshold_db),
                    ("ratio", ratio),
                    ("attack_ms", attack_ms),
                    ("release_ms", release_ms),
                    ("makeup_db", makeup_db),
                ];
                if knee_db != 0.0 {
                    pairs.push(("knee_db", knee_db));
                }
                write_fields(out, &pairs);
                out.push('}');
                continue;
            }
            EffectKind::Reverb { room, damp, mix } => {
                &[("room", room), ("damp", damp), ("mix", mix)]
            }
            EffectKind::Gate {
                threshold_db,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
            } => &[
                ("gate_threshold_db", threshold_db),
                ("gate_range_db", range_db),
                ("gate_attack_ms", attack_ms),
                ("gate_hold_ms", hold_ms),
                ("gate_release_ms", release_ms),
            ],
            EffectKind::Limiter {
                ceiling_db,
                release_ms,
            } => &[
                ("ceiling_db", ceiling_db),
                ("limiter_release_ms", release_ms),
            ],
            EffectKind::ChannelEq { .. } => &[],
        };
        write_fields(out, fields);
        out.push('}');
    }
    out.push_str("\n        ]");
}

/// Appends a channel EQ's band flags and parameters (SPEC-051).
fn write_channel_eq(out: &mut String, bands: &[ChannelEqBand; 8]) {
    // Channel EQ (SPEC-051) also writes per-band enable flags.
    const BAND_KEYS: [&str; 8] = ["hp", "low", "p1", "p2", "p3", "p4", "high", "lp"];
    for (index, band) in bands.iter().enumerate() {
        let mut key = String::from(BAND_KEYS[index]);
        key.push_str("_on");
        out.push_str(", ");
        write_string(out, &key);
        out.push_str(": ");
        out.push_str(if band.enabled { "true" } else { "false" });
    }
    let fields: [(&str, f32); 18] = [
        ("hp_hz", bands[0].hz),
        ("low_hz", bands[1].hz),
        ("low_shelf_db", bands[1].db),
        ("p1_hz", bands[2].hz),
        ("p1_db", bands[2].db),
        ("p1_q", bands[2].q),
        ("p2_hz", bands[3].hz),
        ("p2_db", bands[3].db),
        ("p2_q", bands[3].q),
        ("p3_hz", bands[4].hz),
        ("p3_db", bands[4].db),
        ("p3_q", bands[4].q),
        ("p4_hz", bands[5].hz),
        ("p4_db", bands[5].db),
        ("p4_q", bands[5].q),
        ("high_hz", bands[6].hz),
        ("high_shelf_db", bands[6].db),
        ("lp_hz", bands[7].hz),
    ];
    write_fields(out, &fields);
}

/// Appends `, "key": value` for every pair.
fn write_fields(out: &mut String, fields: &[(&str, f32)]) {
    for (key, value) in fields {
        out.push_str(", ");
        write_string(out, key);
        out.push_str(": ");
        out.push_str(&format_number(f64::from(*value)));
    }
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
    if kind_name == "channel_eq" {
        return channel_eq_from_value(entries, bypass);
    }
    let allowed: &[&str] = match kind_name.as_str() {
        "eq" => &["low_db", "mid_db", "mid_hz", "high_db"],
        "compressor" => &[
            "threshold_db",
            "ratio",
            "attack_ms",
            "release_ms",
            "makeup_db",
            "knee_db",
        ],
        "reverb" => &["room", "damp", "mix"],
        "gate" => &[
            "gate_threshold_db",
            "gate_range_db",
            "gate_attack_ms",
            "gate_hold_ms",
            "gate_release_ms",
        ],
        "limiter" => &["ceiling_db", "limiter_release_ms"],
        _ => return Err(ProjectError::Malformed { at: 0 }),
    };
    let mut values = [None; 6];
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
            // Optional so documents written before SPEC-052 keep loading.
            knee_db: values[5].unwrap_or(0.0),
        },
        "gate" => EffectKind::Gate {
            threshold_db: get(0)?,
            range_db: get(1)?,
            attack_ms: get(2)?,
            hold_ms: get(3)?,
            release_ms: get(4)?,
        },
        "limiter" => EffectKind::Limiter {
            ceiling_db: get(0)?,
            release_ms: get(1)?,
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

/// Decodes one `"channel_eq"` effect: 8 enable flags plus 18 numbers
/// (SPEC-051). Unknown, missing or mistyped fields are rejected.
fn channel_eq_from_value(
    entries: &[(String, Value)],
    bypass: Option<bool>,
) -> Result<Effect, ProjectError> {
    const ON_KEYS: [&str; 8] = [
        "hp_on", "low_on", "p1_on", "p2_on", "p3_on", "p4_on", "high_on", "lp_on",
    ];
    const NUMBER_KEYS: [&str; 18] = [
        "hp_hz",
        "low_hz",
        "low_shelf_db",
        "p1_hz",
        "p1_db",
        "p1_q",
        "p2_hz",
        "p2_db",
        "p2_q",
        "p3_hz",
        "p3_db",
        "p3_q",
        "p4_hz",
        "p4_db",
        "p4_q",
        "high_hz",
        "high_shelf_db",
        "lp_hz",
    ];
    // Per band: slots of hz, db and q in NUMBER_KEYS; `None` keeps the default.
    const LAYOUT: [(usize, Option<usize>, Option<usize>); 8] = [
        (0, None, None),
        (1, Some(2), None),
        (3, Some(4), Some(5)),
        (6, Some(7), Some(8)),
        (9, Some(10), Some(11)),
        (12, Some(13), Some(14)),
        (15, Some(16), None),
        (17, None, None),
    ];
    let mut on = [None; 8];
    let mut numbers = [None; 18];
    for (key, value) in entries {
        match key.as_str() {
            "type" | "bypass" => {}
            _ => {
                if let Some(slot) = ON_KEYS.iter().position(|name| *name == key) {
                    on[slot] = Some(require_bool(value)?);
                } else if let Some(slot) = NUMBER_KEYS.iter().position(|name| *name == key) {
                    numbers[slot] = Some(require_f32(value)?);
                } else {
                    return Err(ProjectError::UnknownField { field: key.clone() });
                }
            }
        }
    }
    let need = |slot: usize| {
        numbers[slot].ok_or(ProjectError::MissingField {
            field: "session.tracks[].effects[] parameter",
        })
    };
    let flag = |slot: usize| {
        on[slot].ok_or(ProjectError::MissingField {
            field: "session.tracks[].effects[] channel_eq band",
        })
    };
    let mut bands = [ChannelEqBand {
        enabled: false,
        hz: 0.0,
        db: 0.0,
        q: 1.0,
    }; 8];
    for (index, (hz, db, q)) in LAYOUT.into_iter().enumerate() {
        let band = &mut bands[index];
        band.enabled = flag(index)?;
        band.hz = need(hz)?;
        if let Some(db) = db {
            band.db = need(db)?;
        }
        if let Some(q) = q {
            band.q = need(q)?;
        }
    }
    Ok(Effect {
        bypass: bypass.unwrap_or(false),
        kind: EffectKind::ChannelEq { bands },
    })
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
    let mut fade_in = 0;
    let mut fade_out = 0;
    let mut curves = (0.0, 0.0);
    for (key, value) in entries {
        match key.as_str() {
            "notes" => notes = Some(notes_from_value(value)?),
            "fade_in" => fade_in = require_u64(value)?,
            "fade_out" => fade_out = require_u64(value)?,
            "fade_in_curve" => curves.0 = require_f32(value)?,
            "fade_out_curve" => curves.1 = require_f32(value)?,
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
        fade_in_samples: fade_in,
        fade_out_samples: fade_out,
        fade_in_curve: curves.0,
        fade_out_curve: curves.1,
    })
}

/// Appends a track or session `"notes"` text, only when it is not empty
/// (SPEC-043 §6).
fn write_text_notes(out: &mut String, indent: &str, notes: &str) {
    if !notes.is_empty() {
        out.push_str(",\n");
        out.push_str(indent);
        out.push_str("\"notes\": ");
        write_string(out, notes);
    }
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

/// `"audio"` or `"instrument"` (SPEC-049).
fn kind_from_value(value: &Value) -> Result<TrackKind, ProjectError> {
    match require_string(value)?.as_str() {
        "audio" => Ok(TrackKind::Audio),
        "instrument" => Ok(TrackKind::Instrument),
        _ => Err(ProjectError::Malformed { at: 0 }),
    }
}

/// `"mono"` or `"stereo"` (SPEC-050 §5).
fn format_from_value(value: &Value) -> Result<TrackFormat, ProjectError> {
    match require_string(value)?.as_str() {
        "mono" => Ok(TrackFormat::Mono),
        "stereo" => Ok(TrackFormat::Stereo),
        _ => Err(ProjectError::Malformed { at: 0 }),
    }
}

/// `{ "sampler": "<bank_path>" }` (SPEC-065). `BuiltinPiano` is never written
/// and is represented by the field being absent.
fn instrument_from_value(value: &Value) -> Result<TrackInstrument, ProjectError> {
    let Value::Object(entries) = value else {
        return Err(ProjectError::Malformed { at: 0 });
    };
    let mut bank_path = None;
    for (key, value) in entries {
        match key.as_str() {
            "sampler" => {
                let path = require_string(value)?;
                if path.len() > MAX_SOURCE_BYTES {
                    return Err(ProjectError::Malformed { at: 0 });
                }
                bank_path = Some(path);
            }
            other => {
                return Err(ProjectError::UnknownField {
                    field: other.to_owned(),
                });
            }
        }
    }
    let bank_path = bank_path.ok_or(ProjectError::MissingField {
        field: "session.tracks[].instrument.sampler",
    })?;
    Ok(TrackInstrument::Sampler { bank_path })
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
    fn track_kind_round_trips_and_legacy_is_inferred() {
        use crate::session::{Clip, Track, TrackKind};
        let mut session = Session::new_untitled(48_000, 120.0);
        session.tracks.push(Track::instrument("Синт"));
        session.tracks.push(Track::new("Аудио"));
        let mut legacy = Track::new("Старый MIDI");
        legacy.clips.push(Clip::midi(0, 100));
        session.tracks.push(legacy);
        let text = Project::with_session(project(), session).to_json();
        assert_eq!(text.matches("\"kind\": \"instrument\"").count(), 1);
        let back = Project::from_json(&text).unwrap();
        let kinds: Vec<_> = back.session.tracks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            [
                TrackKind::Instrument,
                TrackKind::Audio,
                TrackKind::Instrument
            ],
            "a track with MIDI regions and no kind loads as instrument"
        );
        let bad = text.replace("\"kind\": \"instrument\"", "\"kind\": \"drums\"");
        assert!(matches!(
            Project::from_json(&bad).unwrap_err(),
            ProjectError::Malformed { .. }
        ));
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
