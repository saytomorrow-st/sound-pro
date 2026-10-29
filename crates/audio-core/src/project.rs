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

use crate::graph::Graph;
use crate::params::{ParamHandle, ParamId, ParamSpec};

/// Longest accepted `name`, in bytes.
pub const MAX_NAME_BYTES: usize = 128;

/// Deepest object/array nesting accepted by the parser.
const MAX_DEPTH: usize = 8;

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
        }
    }
}

impl std::error::Error for ProjectError {}

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
        }
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
            out.push_str("]\n}\n");
        } else {
            out.push_str("\n  ]\n}\n");
        }
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
        self.skip_whitespace();
        if self.pos != self.bytes.len() {
            return Err(ProjectError::Malformed { at: self.pos });
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
                // Multi-byte UTF-8 passes through: only the quote and the
                // backslash are special, everything else is raw.
                0x00..=0x1f => return Err(ProjectError::Malformed { at: self.pos }),
                _ => {
                    let rest = &self.bytes[self.pos..];
                    let Some(ch) = std::str::from_utf8(rest)
                        .ok()
                        .and_then(|text| text.chars().next())
                    else {
                        return Err(ProjectError::Malformed { at: self.pos });
                    };
                    out.push(ch);
                    self.pos += ch.len_utf8();
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
    for (key, value) in entries {
        match key.as_str() {
            "name" => name = Some(require_string(value)?),
            "sample_rate" => sample_rate = Some(require_u32(value)?),
            "tempo_bpm" => tempo_bpm = Some(require_f64(value)?),
            "params" => params = Some(params_from_array(value)?),
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
