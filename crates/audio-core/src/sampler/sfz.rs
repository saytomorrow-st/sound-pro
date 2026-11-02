//! SFZ text parser for the v1 subset defined in RFC-001 §2 (SPEC-062).
//!
//! [`parse`] turns `.sfz` source text into a typed [`SfzDocument`] — a flat
//! list of [`Region`]s plus an optional `default_path` and a list of
//! [`SfzWarning`]s. It is a pure function: no file I/O, no platform APIs, no
//! `unsafe`, and no panics on malformed input.
//!
//! `#include` resolution is delegated to the caller through the
//! [`IncludeResolver`] trait so that `audio-core` never opens a file; a
//! real implementation lives in `daw-app` (RFC-001 §5).

use std::fmt;

/// Resolves the text of an `#include`d file.
///
/// The parser never touches the filesystem itself. Implementations return the
/// decoded text of the named file, or an error if it cannot be read. Path
/// traversal (`..`, absolute paths) is rejected by the parser before this
/// trait is consulted.
pub trait IncludeResolver {
    /// Load the contents of the included file as text.
    ///
    /// # Errors
    ///
    /// Returns an error describing why the file could not be read (missing,
    /// permission denied, invalid UTF-8, …).
    fn resolve(&mut self, path: &str) -> Result<String, String>;
}

/// A loader warning that does not prevent parsing.
///
/// Unknown opcodes and undefined `#define` variables are reported here; the
/// offending region is still emitted (RFC-001 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfzWarning {
    /// Byte offset in the (post-include) source where the warning arose.
    pub at: usize,
    /// Human-readable description of the problem.
    pub message: String,
}

/// Why an SFZ source could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfzError {
    /// A directive or comment ran to end-of-input before closing.
    UnterminatedComment {
        /// Byte offset of the `/*`.
        at: usize,
    },
    /// An `#include` path points outside the bank directory.
    UnsafePath {
        /// Byte offset of the offending path.
        at: usize,
        /// The path that was rejected.
        path: String,
    },
    /// The `#include`d file could not be loaded by the resolver.
    Include {
        /// Byte offset of the `#include` directive.
        at: usize,
        /// The path that failed to resolve.
        path: String,
        /// The resolver's own error message.
        reason: String,
    },
    /// A value that should be numeric was not.
    InvalidNumber {
        /// Byte offset of the value.
        at: usize,
        /// The offending token.
        value: String,
    },
    /// A value fell outside its legal range (e.g. `lovel`, `hivel`, `key`).
    InvalidRange {
        /// Byte offset of the offending value.
        at: usize,
        /// The opcode whose value was out of range.
        opcode: String,
        /// The offending token.
        value: String,
    },
    /// `#include` appeared without a quoted path.
    MalformedInclude {
        /// Byte offset of the directive.
        at: usize,
    },
}

impl fmt::Display for SfzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedComment { at } => {
                write!(f, "unterminated block comment starting at byte {at}")
            }
            Self::UnsafePath { path, .. } => {
                write!(f, "#include path escapes the bank directory: {path:?}")
            }
            Self::Include { path, reason, .. } => {
                write!(f, "could not load #include {path:?}: {reason}")
            }
            Self::InvalidNumber { value, .. } => {
                write!(f, "expected a number, got {value:?}")
            }
            Self::InvalidRange { opcode, value, .. } => {
                write!(f, "value {value:?} out of range for opcode {opcode:?}")
            }
            Self::MalformedInclude { .. } => write!(f, "#include requires a quoted path"),
        }
    }
}

impl std::error::Error for SfzError {}

/// Loop behaviour for a [`Region`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    /// Sample plays once and stops (SFZ `no_loop`).
    NoLoop,
    /// Sample plays once, ignoring loop points (SFZ `one_shot`).
    OneShot,
    /// Sample loops while the note is held (SFZ `loop_continuous`).
    #[default]
    LoopContinuous,
    /// Sample loops only while the sustain pedal is down (SFZ `loop_sustain`).
    LoopSustain,
}

/// How a release sample's group cut behaves (SFZ `off_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OffMode {
    /// Release tail is cut immediately (SFZ `fast`).
    Fast,
    /// Release tail fades normally (SFZ `normal`).
    #[default]
    Normal,
}

/// When a sample is triggered (SFZ `trigger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trigger {
    /// Triggered on note-on.
    #[default]
    Attack,
    /// Triggered on note-off (release sample).
    Release,
}

/// A fully-resolved SFZ region (RFC-001 §2).
///
/// Every field is `Option` so that inheritance and `key=` expansion can be
/// represented uniformly: a missing value means "inherit from the parent
/// scope". Consumers apply the same inheritance that the parser already
/// flattened into [`SfzDocument::regions`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Region {
    /// Sample path, relative to `default_path` and the `.sfz` file.
    pub sample: Option<String>,
    /// Lowest MIDI note this region responds to.
    pub lokey: Option<u8>,
    /// Highest MIDI note this region responds to.
    pub hikey: Option<u8>,
    /// Root note used for pitch reference.
    pub pitch_keycenter: Option<u8>,
    /// Lowest velocity this region responds to (`1..=127`).
    pub lovel: Option<u8>,
    /// Highest velocity this region responds to (`1..=127`).
    pub hivel: Option<u8>,
    /// Fine tuning in cents.
    pub tune: Option<i32>,
    /// Coarse transpose in semitones.
    pub transpose: Option<i32>,
    /// Gain in decibels.
    pub volume: Option<f32>,
    /// Velocity tracking `0..=100` (default 100).
    pub amp_veltrack: Option<f32>,
    /// Stereo pan `-100..=100`.
    pub pan: Option<f32>,
    /// Loop behaviour.
    pub loop_mode: Option<LoopMode>,
    /// Loop start, in sample frames.
    pub loop_start: Option<u32>,
    /// Loop end, in sample frames.
    pub loop_end: Option<u32>,
    /// Playback start offset, in sample frames.
    pub offset: Option<u32>,
    /// Playback end, in sample frames.
    pub end: Option<u32>,
    /// Envelope attack time, in seconds.
    pub ampeg_attack: Option<f32>,
    /// Envelope hold time, in seconds.
    pub ampeg_hold: Option<f32>,
    /// Envelope decay time, in seconds.
    pub ampeg_decay: Option<f32>,
    /// Envelope sustain level, as a percentage of peak (`0..=100`).
    pub ampeg_sustain: Option<f32>,
    /// Envelope release time, in seconds.
    pub ampeg_release: Option<f32>,
    /// Group id for group cutting.
    pub group: Option<i32>,
    /// Group id cut when this region triggers.
    pub off_by: Option<i32>,
    /// How the cut group's release tail behaves.
    pub off_mode: Option<OffMode>,
    /// Trigger timing.
    pub trigger: Option<Trigger>,
    /// Round-robin sequence length.
    pub seq_length: Option<u32>,
    /// Round-robin position within the sequence.
    pub seq_position: Option<u32>,
    /// Lower bound of the random range (SFZ `lorand`).
    pub lorand: Option<f32>,
    /// Upper bound of the random range (SFZ `hirand`).
    pub hirand: Option<f32>,
}

/// The parsed result of an SFZ file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SfzDocument {
    /// `default_path` from `<control>`, if present.
    pub default_path: Option<String>,
    /// Resolved regions, in source order.
    pub regions: Vec<Region>,
    /// Non-fatal warnings collected during parsing.
    pub warnings: Vec<SfzWarning>,
}

/// Parse SFZ source text into a typed document.
///
/// `resolver` supplies the text of `#include`d files; see [`IncludeResolver`].
/// The parser is pure and never panics on malformed input.
///
/// # Errors
///
/// Returns [`SfzError`] for structurally invalid input. Unknown opcodes and
/// headers are not errors; they are reported through [`SfzDocument::warnings`]
/// or skipped, per RFC-001 §2.
pub fn parse(text: &str, resolver: &mut dyn IncludeResolver) -> Result<SfzDocument, SfzError> {
    let mut parser = Parser::new(resolver);
    parser.run(text)
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

/// Section kinds recognised by the parser. Unknown headers map to `None` and
/// are skipped entirely (their opcodes are neither inherited nor emitted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Control,
    Global,
    Master,
    Group,
    Region,
}

impl Section {
    /// Parse a header name (without angle brackets) into a section kind.
    fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "control" => Some(Self::Control),
            "global" => Some(Self::Global),
            "master" => Some(Self::Master),
            "group" => Some(Self::Group),
            "region" => Some(Self::Region),
            _ => None,
        }
    }
}

/// The parser's running state: current header, inherited opcode values, and
/// the output accumulator.
struct Parser<'a> {
    resolver: &'a mut dyn IncludeResolver,
    /// Output: resolved regions.
    regions: Vec<Region>,
    /// Output: non-fatal warnings.
    warnings: Vec<SfzWarning>,
    /// `default_path` from `<control>`.
    default_path: Option<String>,
    /// `#define` substitutions: name → value.
    defines: std::collections::HashMap<String, String>,
    /// The current header kind. `None` means "not inside a known header".
    section: Option<Section>,
    /// Values inherited from `<control>`/`<global>`.
    global: Region,
    /// Values inherited from `<master>`.
    master: Region,
    /// Values inherited from `<group>`.
    group: Region,
    /// The region currently being filled, if any.
    current: Region,
    /// Whether `current` has been touched since the last `<region>` open.
    region_open: bool,
}

impl<'a> Parser<'a> {
    fn new(resolver: &'a mut dyn IncludeResolver) -> Self {
        Self {
            resolver,
            regions: Vec::new(),
            warnings: Vec::new(),
            default_path: None,
            defines: std::collections::HashMap::new(),
            section: None,
            global: Region::default(),
            master: Region::default(),
            group: Region::default(),
            current: Region::default(),
            region_open: false,
        }
    }

    /// Drive the tokeniser over `text`, expanding `#include` inline.
    fn run(&mut self, text: &str) -> Result<SfzDocument, SfzError> {
        let tokens = self.expand(text, 0)?;
        for token in tokens {
            self.consume(&token)?;
        }
        if self.region_open {
            self.regions.push(std::mem::take(&mut self.current));
        }
        Ok(SfzDocument {
            default_path: self.default_path.take(),
            regions: std::mem::take(&mut self.regions),
            warnings: std::mem::take(&mut self.warnings),
        })
    }

    /// Tokenise `text`, resolving `#include` recursively, then substitute
    /// `$var` for `#define`s and strip comments. Returns flat token list.
    fn expand(&mut self, text: &str, depth: usize) -> Result<Vec<Token>, SfzError> {
        let mut out: Vec<Token> = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0usize;

        while i < bytes.len() {
            let b = bytes[i];
            match b {
                // Block comment: skip to the closing `*/`.
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    i = Self::skip_block_comment(text, i)?;
                }
                // Line comment: skip to the end of the line.
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    i = Self::skip_line_comment(bytes, i);
                }
                // `#include` / `#define` directive.
                b'#' => {
                    i = self.expand_directive(text, i, &mut out, depth)?;
                }
                b'<' => {
                    i = Self::push_header(text, i, &mut out);
                }
                _ if b.is_ascii_whitespace() => {
                    i += 1;
                }
                _ => {
                    i = self.push_opcode(text, i, &mut out);
                }
            }
        }

        Ok(out)
    }

    /// Skip a `/* ... */` comment, returning the index past its closing `*/`.
    fn skip_block_comment(text: &str, start: usize) -> Result<usize, SfzError> {
        let close = text[start + 2..]
            .find("*/")
            .map(|n| start + 2 + n)
            .ok_or(SfzError::UnterminatedComment { at: start })?;
        Ok(close + 2)
    }

    /// Skip a `//` comment, returning the index past the newline.
    fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
        let mut i = start;
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i
    }

    /// Handle a `#include` or `#define` directive at `i` (the `#` byte).
    fn expand_directive(
        &mut self,
        text: &str,
        i: usize,
        out: &mut Vec<Token>,
        depth: usize,
    ) -> Result<usize, SfzError> {
        let (word, next) = read_identifier(text, i + 1);
        match word.as_str() {
            "include" => self.expand_include(text, i, next, out, depth),
            "define" => {
                self.register_define(text, next);
                Ok(skip_line(text, next))
            }
            _ => Ok(skip_line(text, i)),
        }
    }

    /// Resolve and inline an `#include "path"`.
    fn expand_include(
        &mut self,
        text: &str,
        i: usize,
        next: usize,
        out: &mut Vec<Token>,
        depth: usize,
    ) -> Result<usize, SfzError> {
        let (path, end) = read_quoted(text, next)?;
        if path.is_empty() {
            return Err(SfzError::MalformedInclude { at: i });
        }
        if is_unsafe_path(&path) {
            return Err(SfzError::UnsafePath { at: i, path });
        }
        let included = self
            .resolver
            .resolve(&path)
            .map_err(|reason| SfzError::Include {
                at: i,
                path: path.clone(),
                reason,
            })?;
        let sub = self.expand(&included, depth + 1)?;
        out.extend(sub);
        Ok(end)
    }

    /// Register a `#define` substitution from the directive body.
    fn register_define(&mut self, text: &str, next: usize) {
        let bytes = text.as_bytes();
        let mut name_start = next;
        while name_start < bytes.len() && bytes[name_start].is_ascii_whitespace() {
            name_start += 1;
        }
        if bytes.get(name_start) == Some(&b'$') {
            name_start += 1;
        }
        let (name, name_end) = read_identifier(text, name_start);
        let (value, _) = read_define_value(text, name_end);
        self.defines.insert(name, value);
    }

    /// Push a `<header>` token, returning the index past the closing `>`.
    fn push_header(text: &str, i: usize, out: &mut Vec<Token>) -> usize {
        match text[i..].find('>') {
            Some(rel) => {
                let name = text[i + 1..i + rel].trim();
                out.push(Token::Header(name.to_string()));
                i + rel + 1
            }
            None => text.len(),
        }
    }

    /// Push an `opcode=value` token, returning the index past the token.
    fn push_opcode(&mut self, text: &str, i: usize, out: &mut Vec<Token>) -> usize {
        let bytes = text.as_bytes();
        let start = i;
        let mut j = i;
        let mut eq = None;
        while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
            if bytes[j] == b'=' {
                eq = Some(j);
                j += 1;
                break;
            }
            j += 1;
        }
        let Some(eq) = eq else {
            return skip_line(text, j);
        };
        let name = text[start..eq].trim().to_string();
        let value = if bytes.get(j) == Some(&b'"') {
            match read_quoted(text, j) {
                Ok((v, end)) => {
                    j = end;
                    v
                }
                Err(_) => String::new(),
            }
        } else {
            let vs = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            text[vs..j].trim().to_string()
        };
        let value = self.substitute(&value);
        out.push(Token::Opcode {
            name,
            value,
            at: start,
        });
        j
    }

    /// Substitute `$var` occurrences in an opcode value using current defines.
    fn substitute(&mut self, value: &str) -> String {
        if !value.contains('$') {
            return value.to_string();
        }
        let mut result = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(dollar) = rest.find('$') {
            result.push_str(&rest[..dollar]);
            let after = &rest[dollar + 1..];
            let (name, consumed) = read_identifier(after, 0);
            if name.is_empty() {
                // Lone `$`: keep it literally.
                result.push('$');
                rest = after;
                continue;
            }
            match self.defines.get(&name) {
                Some(v) => result.push_str(v),
                None => {
                    // Undefined variable: warn once, substitute empty.
                    self.warnings.push(SfzWarning {
                        at: 0,
                        message: format!("undefined #define variable ${name}"),
                    });
                }
            }
            rest = &after[consumed..];
        }
        result.push_str(rest);
        result
    }

    /// Process a single token.
    fn consume(&mut self, token: &Token) -> Result<(), SfzError> {
        match token {
            Token::Header(name) => {
                self.open_header(name);
                Ok(())
            }
            Token::Opcode { name, value, at } => self.apply_opcode(name, value, *at),
        }
    }

    fn open_header(&mut self, name: &str) {
        // Close any open region.
        if self.region_open {
            self.regions.push(std::mem::take(&mut self.current));
            self.region_open = false;
        }
        self.section = Section::from_name(name);
        if self.section == Some(Section::Region) {
            // A new region starts from the inherited group+master+global
            // base; it is lazily pushed on the first opcode.
            self.current = merge(&self.global, &self.master, &self.group);
            self.region_open = true;
        } else {
            self.current = Region::default();
            self.region_open = false;
        }
    }

    fn apply_opcode(&mut self, name: &str, value: &str, at: usize) -> Result<(), SfzError> {
        match self.section {
            None => {
                // Opcode outside a known header: ignored.
                return Ok(());
            }
            Some(Section::Control) => {
                if name == "default_path" {
                    self.default_path = Some(value.to_string());
                } else {
                    self.warn_unknown(name, at);
                }
            }
            Some(Section::Global) => {
                if !apply_field(&mut self.global, name, value, at, &mut self.warnings)? {
                    self.warn_unknown(name, at);
                }
            }
            Some(Section::Master) => {
                if !apply_field(&mut self.master, name, value, at, &mut self.warnings)? {
                    self.warn_unknown(name, at);
                }
            }
            Some(Section::Group) => {
                if !apply_field(&mut self.group, name, value, at, &mut self.warnings)? {
                    self.warn_unknown(name, at);
                }
            }
            Some(Section::Region) => {
                if !apply_field(&mut self.current, name, value, at, &mut self.warnings)? {
                    self.warn_unknown(name, at);
                }
            }
        }
        Ok(())
    }

    fn warn_unknown(&mut self, name: &str, at: usize) {
        self.warnings.push(SfzWarning {
            at,
            message: format!("unknown opcode {name:?}"),
        });
    }
}

/// A token produced by [`Parser::expand`].
#[derive(Debug, Clone)]
enum Token {
    /// A `<header>`.
    Header(String),
    /// An `opcode=value` pair.
    Opcode {
        /// Opcode name.
        name: String,
        /// Raw value (after `$` substitution).
        value: String,
        /// Byte offset in the source.
        at: usize,
    },
}

// ---------------------------------------------------------------------------
// Field application
// ---------------------------------------------------------------------------

/// Apply a single opcode to `region`, returning `Ok(false)` when the opcode is
/// unknown (the caller then emits a warning). `key=` expands to three fields.
fn apply_field(
    region: &mut Region,
    name: &str,
    value: &str,
    at: usize,
    warnings: &mut Vec<SfzWarning>,
) -> Result<bool, SfzError> {
    // Parse a bounded f32, returning `InvalidRange` when out of bounds.
    let ranged_f32 = |lo: f32, hi: f32| -> Result<f32, SfzError> {
        let v = parse_f32(value, at)?;
        if (lo..=hi).contains(&v) {
            Ok(v)
        } else {
            Err(SfzError::InvalidRange {
                at,
                opcode: name.to_string(),
                value: value.to_string(),
            })
        }
    };
    // Parse a `1..=127` velocity value.
    let velocity = || -> Result<u8, SfzError> {
        let v = parse_u8(value, at)?;
        if (1..=127).contains(&v) {
            Ok(v)
        } else {
            Err(SfzError::InvalidRange {
                at,
                opcode: name.to_string(),
                value: value.to_string(),
            })
        }
    };

    match name {
        "sample" => region.sample = Some(value.to_string()),
        "key" => {
            let key = parse_u8(value, at)?;
            region.lokey = Some(key);
            region.hikey = Some(key);
            region.pitch_keycenter = Some(key);
        }
        "lokey" => region.lokey = Some(parse_u8(value, at)?),
        "hikey" => region.hikey = Some(parse_u8(value, at)?),
        "pitch_keycenter" => region.pitch_keycenter = Some(parse_u8(value, at)?),
        "lovel" => region.lovel = Some(velocity()?),
        "hivel" => region.hivel = Some(velocity()?),
        "tune" => region.tune = Some(parse_i32(value, at)?),
        "transpose" => region.transpose = Some(parse_i32(value, at)?),
        "volume" => region.volume = Some(parse_f32(value, at)?),
        "amp_veltrack" => region.amp_veltrack = Some(ranged_f32(0.0, 100.0)?),
        "pan" => region.pan = Some(ranged_f32(-100.0, 100.0)?),
        "loop_mode" => region.loop_mode = Some(parse_loop_mode(value, at, warnings)),
        "loop_start" => region.loop_start = Some(parse_u32(value, at)?),
        "loop_end" => region.loop_end = Some(parse_u32(value, at)?),
        "offset" => region.offset = Some(parse_u32(value, at)?),
        "end" => region.end = Some(parse_u32(value, at)?),
        "ampeg_attack" => region.ampeg_attack = Some(parse_f32(value, at)?),
        "ampeg_hold" => region.ampeg_hold = Some(parse_f32(value, at)?),
        "ampeg_decay" => region.ampeg_decay = Some(parse_f32(value, at)?),
        "ampeg_sustain" => region.ampeg_sustain = Some(ranged_f32(0.0, 100.0)?),
        "ampeg_release" => region.ampeg_release = Some(parse_f32(value, at)?),
        "group" => region.group = Some(parse_i32(value, at)?),
        "off_by" => region.off_by = Some(parse_i32(value, at)?),
        "off_mode" => region.off_mode = Some(parse_off_mode(value, at, warnings)),
        "trigger" => region.trigger = Some(parse_trigger(value, at, warnings)),
        "seq_length" => region.seq_length = Some(parse_u32(value, at)?),
        "seq_position" => region.seq_position = Some(parse_u32(value, at)?),
        "lorand" => region.lorand = Some(parse_f32(value, at)?),
        "hirand" => region.hirand = Some(parse_f32(value, at)?),
        _ => return Ok(false),
    }
    Ok(true)
}

/// Parse a `loop_mode` value, warning on unknown input.
fn parse_loop_mode(value: &str, at: usize, warnings: &mut Vec<SfzWarning>) -> LoopMode {
    match value {
        "no_loop" => LoopMode::NoLoop,
        "one_shot" => LoopMode::OneShot,
        "loop_continuous" => LoopMode::LoopContinuous,
        "loop_sustain" => LoopMode::LoopSustain,
        _ => {
            warnings.push(SfzWarning {
                at,
                message: format!("unknown loop_mode {value:?}; using loop_continuous"),
            });
            LoopMode::LoopContinuous
        }
    }
}

/// Parse an `off_mode` value, warning on unknown input.
fn parse_off_mode(value: &str, at: usize, warnings: &mut Vec<SfzWarning>) -> OffMode {
    match value {
        "fast" => OffMode::Fast,
        "normal" => OffMode::Normal,
        _ => {
            warnings.push(SfzWarning {
                at,
                message: format!("unknown off_mode {value:?}; using normal"),
            });
            OffMode::Normal
        }
    }
}

/// Parse a `trigger` value, warning on unknown input.
fn parse_trigger(value: &str, at: usize, warnings: &mut Vec<SfzWarning>) -> Trigger {
    match value {
        "attack" => Trigger::Attack,
        "release" => Trigger::Release,
        _ => {
            warnings.push(SfzWarning {
                at,
                message: format!("unknown trigger {value:?}; using attack"),
            });
            Trigger::Attack
        }
    }
}

/// Merge inherited scopes into one region base (later scopes win).
fn merge(global: &Region, master: &Region, group: &Region) -> Region {
    let mut base = Region::default();
    overlay(&mut base, global);
    overlay(&mut base, master);
    overlay(&mut base, group);
    base
}

/// Copy every set field from `src` onto `dst`.
fn overlay(dst: &mut Region, src: &Region) {
    macro_rules! cp {
        ($field:ident) => {
            if src.$field.is_some() {
                dst.$field = src.$field.clone();
            }
        };
    }
    cp!(sample);
    cp!(lokey);
    cp!(hikey);
    cp!(pitch_keycenter);
    cp!(lovel);
    cp!(hivel);
    cp!(tune);
    cp!(transpose);
    cp!(volume);
    cp!(amp_veltrack);
    cp!(pan);
    cp!(loop_mode);
    cp!(loop_start);
    cp!(loop_end);
    cp!(offset);
    cp!(end);
    cp!(ampeg_attack);
    cp!(ampeg_hold);
    cp!(ampeg_decay);
    cp!(ampeg_sustain);
    cp!(ampeg_release);
    cp!(group);
    cp!(off_by);
    cp!(off_mode);
    cp!(trigger);
    cp!(seq_length);
    cp!(seq_position);
    cp!(lorand);
    cp!(hirand);
}

// ---------------------------------------------------------------------------
// Small parsing helpers (no panics)
// ---------------------------------------------------------------------------

fn parse_u8(value: &str, at: usize) -> Result<u8, SfzError> {
    value
        .trim()
        .parse::<u8>()
        .map_err(|_| SfzError::InvalidNumber {
            at,
            value: value.to_string(),
        })
}

fn parse_u32(value: &str, at: usize) -> Result<u32, SfzError> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| SfzError::InvalidNumber {
            at,
            value: value.to_string(),
        })
}

fn parse_i32(value: &str, at: usize) -> Result<i32, SfzError> {
    value
        .trim()
        .parse::<i32>()
        .map_err(|_| SfzError::InvalidNumber {
            at,
            value: value.to_string(),
        })
}

fn parse_f32(value: &str, at: usize) -> Result<f32, SfzError> {
    value
        .trim()
        .parse::<f32>()
        .map_err(|_| SfzError::InvalidNumber {
            at,
            value: value.to_string(),
        })
}

/// Skip from `start` to just past the next newline (or end of input).
fn skip_line(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() { i + 1 } else { i }
}

/// Reject absolute paths and any `..` component (RFC-001 §2 security rule).
fn is_unsafe_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') {
        return true;
    }
    // Windows drive letter (e.g. `C:`).
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return true;
    }
    path.split(['/', '\\']).any(|c| c == "..")
}

/// Read an identifier (ASCII letters, digits, `_`) starting at `start`.
/// Returns the identifier and the byte index just past it.
fn read_identifier(text: &str, start: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' {
            i += 1;
        } else {
            break;
        }
    }
    (text[start..i].to_string(), i)
}

/// Read a double-quoted string starting at `start` (which should point at the
/// opening quote). Returns the unescaped contents and the index past the
/// closing quote.
fn read_quoted(text: &str, start: usize) -> Result<(String, usize), SfzError> {
    let bytes = text.as_bytes();
    let mut i = start;
    // Skip whitespace before the quote.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return Err(SfzError::MalformedInclude { at: start });
    }
    i += 1;
    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                out.push(bytes[i] as char);
                i += 1;
            }
            b'\n' | b'\r' => return Err(SfzError::MalformedInclude { at: start }),
            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    Err(SfzError::MalformedInclude { at: start })
}

/// Read the value of a `#define`: everything up to end-of-line, trimmed.
/// Returns the value and the index past the trailing newline (if any).
fn read_define_value(text: &str, start: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
        i += 1;
    }
    let value_start = i;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    let value = text[value_start..i].trim().to_string();
    // Consume the newline.
    if i < bytes.len() {
        i += 1;
    }
    (value, i)
}
