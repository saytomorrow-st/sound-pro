//! Standard MIDI File (SMF) parser for formats 0 and 1 (SPEC-004).
//!
//! Read-only: [`parse_smf`] turns a byte slice into a flat, deterministic list
//! of note and tempo events. No file I/O, no platform APIs, no `unsafe`, and no
//! panics on garbage input — every malformed byte is reported as a
//! [`MidiError`].
//!
//! Supported today: `MThd` header (formats 0 and 1, PPQN division), `MTrk`
//! chunks, `EndOfTrack`, `NoteOn`/`NoteOff` (velocity 0 = `NoteOff`), the `Tempo`
//! meta event (µs per quarter note), running status, and length-based skipping
//! of every other event. SMPTE division, `SysEx`, MIDI 2.0 and writing are out of
//! scope.

use std::fmt;

/// Why an SMF byte stream could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// Input was zero bytes.
    Empty,
    /// A chunk or event claimed more bytes than remain.
    Truncated {
        /// Byte offset where the data ran out.
        at: usize,
    },
    /// Structurally invalid bytes (bad magic, bad length, oversized value).
    Malformed {
        /// Byte offset of the offending byte.
        at: usize,
    },
    /// The division field uses SMPTE frames-per-second timing, which this
    /// parser does not convert.
    UnsupportedDivision,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty MIDI input"),
            Self::Truncated { at } => write!(f, "truncated MIDI data at byte {at}"),
            Self::Malformed { at } => write!(f, "malformed MIDI data at byte {at}"),
            Self::UnsupportedDivision => write!(f, "unsupported SMPTE time division"),
        }
    }
}

impl std::error::Error for MidiError {}

/// A single musical message extracted from a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    /// Note starts. `velocity` is in `1..=127` (velocity 0 becomes [`Self::NoteOff`]).
    NoteOn {
        /// MIDI channel, 0–15.
        channel: u8,
        /// MIDI note number, 0–127.
        note: u8,
        /// Note velocity, 1–127.
        velocity: u8,
    },
    /// Note ends.
    NoteOff {
        /// MIDI channel, 0–15.
        channel: u8,
        /// MIDI note number, 0–127.
        note: u8,
        /// Release velocity, 0–127.
        velocity: u8,
    },
    /// Tempo change: microseconds per quarter note.
    Tempo {
        /// Microseconds per quarter note (MIDI's tempo unit).
        micros_per_quarter: u32,
    },
}

/// One timestamped event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    /// Track index the event came from (0-based).
    pub track: u16,
    /// Absolute tick position.
    pub tick: u64,
    /// Position within the track among events sharing `tick`, preserving file
    /// order. Makes the sort total and deterministic.
    pub order_in_track: u32,
    /// The message itself.
    pub message: MidiMessage,
}

/// A parsed SMF: header info plus a deterministic, sorted event list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiFile {
    /// SMF format: 0 or 1.
    pub format: u16,
    /// Pulses per quarter note.
    pub ppqn: u16,
    /// Number of tracks declared in the header.
    pub track_count: u16,
    /// Events sorted by `(tick, track, order_in_track)`.
    pub events: Vec<MidiEvent>,
}

impl MidiFile {
    /// Note-on events only, in sorted order. Convenience for callers building a
    /// note list.
    #[must_use]
    pub fn notes(&self) -> Vec<MidiEvent> {
        self.events
            .iter()
            .filter(|e| matches!(e.message, MidiMessage::NoteOn { .. }))
            .copied()
            .collect()
    }
}

/// Parses a Standard MIDI File from `bytes`.
///
/// # Errors
///
/// [`MidiError::Empty`] for empty input, [`MidiError::Truncated`] /
/// [`MidiError::Malformed`] for bad structure, and
/// [`MidiError::UnsupportedDivision`] for SMPTE time division.
pub fn parse_smf(bytes: &[u8]) -> Result<MidiFile, MidiError> {
    Parser::new(bytes).parse()
}

/// Cursor over the input with bounds-checked reads.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    const fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn u8(&mut self) -> Result<u8, MidiError> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or(MidiError::Truncated { at: self.pos })?;
        self.pos += 1;
        Ok(b)
    }

    fn u16_be(&mut self) -> Result<u16, MidiError> {
        let hi = self.u8()?;
        let lo = self.u8()?;
        Ok(u16::from(hi) << 8 | u16::from(lo))
    }

    fn u32_be(&mut self) -> Result<u32, MidiError> {
        let hi = self.u16_be()?;
        let lo = self.u16_be()?;
        Ok(u32::from(hi) << 16 | u32::from(lo))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], MidiError> {
        if self.remaining() < n {
            return Err(MidiError::Truncated { at: self.pos });
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

struct Parser<'a> {
    r: Reader<'a>,
}

impl<'a> Parser<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self {
            r: Reader::new(bytes),
        }
    }

    fn parse(mut self) -> Result<MidiFile, MidiError> {
        if self.r.data.is_empty() {
            return Err(MidiError::Empty);
        }
        // Header chunk: "MThd" + u32 length (>= 6) + format + ntrks + division.
        self.expect_magic(*b"MThd")?;
        let header_len = self.r.u32_be()?;
        if header_len < 6 {
            return Err(MidiError::Malformed { at: 4 });
        }
        let format = self.r.u16_be()?;
        if format > 1 {
            return Err(MidiError::Malformed { at: 8 });
        }
        let track_count = self.r.u16_be()?;
        let division = self.r.u16_be()?;
        // Skip any extra header bytes beyond the standard 6.
        let extra = header_len as usize - 6;
        self.r.take(extra)?;
        if division & 0x8000 != 0 {
            return Err(MidiError::UnsupportedDivision);
        }
        if division == 0 {
            return Err(MidiError::Malformed { at: 12 });
        }

        let mut events: Vec<MidiEvent> = Vec::new();
        let mut track_index: u16 = 0;
        while !self.r.at_end() {
            let magic = self.r.take(4)?;
            if magic != b"MTrk" {
                // Unknown chunk type: skip it by its declared length.
                let len = self.r.u32_be()? as usize;
                self.r.take(len)?;
                continue;
            }
            let len = self.r.u32_be()? as usize;
            let start = self.r.pos;
            if self.r.remaining() < len {
                return Err(MidiError::Truncated { at: self.r.pos });
            }
            let body = self.r.take(len)?;
            TrackParser {
                r: Reader::new(body),
                body_start: start,
                track_no: track_index,
                tick: 0,
                order: 0,
                running_status: None,
                sounding: Vec::new(),
            }
            .run(&mut events)?;
            track_index = track_index.saturating_add(1);
        }

        events.sort_by_key(|e| (e.tick, e.track, e.order_in_track));
        Ok(MidiFile {
            format,
            ppqn: division,
            track_count,
            events,
        })
    }

    fn expect_magic(&mut self, magic: [u8; 4]) -> Result<(), MidiError> {
        let start = self.r.pos;
        let got = self.r.take(4)?;
        if got != magic {
            return Err(MidiError::Malformed { at: start });
        }
        Ok(())
    }
}

/// State and per-event logic for one `MTrk` chunk.
struct TrackParser<'a> {
    r: Reader<'a>,
    body_start: usize,
    track_no: u16,
    tick: u64,
    order: u32,
    running_status: Option<u8>,
    /// (channel, note) pairs still sounding; released at `EndOfTrack`.
    sounding: Vec<(u8, u8)>,
}

impl TrackParser<'_> {
    fn run(mut self, out: &mut Vec<MidiEvent>) -> Result<(), MidiError> {
        while !self.r.at_end() {
            let delta = read_var_len(&mut self.r)?;
            self.tick = self.tick.saturating_add(u64::from(delta));
            let first = self.r.u8()?;
            if first == 0xFF {
                if self.event_meta(first, out)? {
                    break; // EndOfTrack
                }
            } else {
                self.event_channel(first, out)?;
            }
        }
        Ok(())
    }

    /// Handles a meta event. Returns `true` when it was `EndOfTrack`.
    fn event_meta(&mut self, _marker: u8, out: &mut Vec<MidiEvent>) -> Result<bool, MidiError> {
        let meta_type = self.r.u8()?;
        let len = read_var_len(&mut self.r)? as usize;
        let data_at = self.body_start + self.r.pos;
        let data = self.r.take(len)?;
        match meta_type {
            0x2F => {
                self.release_all(out);
                return Ok(true);
            }
            0x51 => {
                if len != 3 {
                    return Err(MidiError::Malformed { at: data_at });
                }
                let micros =
                    u32::from(data[0]) << 16 | u32::from(data[1]) << 8 | u32::from(data[2]);
                self.push(
                    out,
                    MidiMessage::Tempo {
                        micros_per_quarter: micros,
                    },
                );
            }
            _ => {}
        }
        Ok(false)
    }

    /// Releases every dangling note at the current tick.
    fn release_all(&mut self, out: &mut Vec<MidiEvent>) {
        let dangling: Vec<(u8, u8)> = std::mem::take(&mut self.sounding);
        for (channel, note) in dangling {
            self.push(
                out,
                MidiMessage::NoteOff {
                    channel,
                    note,
                    velocity: 0,
                },
            );
        }
    }

    fn event_channel(&mut self, first: u8, out: &mut Vec<MidiEvent>) -> Result<(), MidiError> {
        let status = self.resolve_status(first)?;
        let kind = status & 0xF0;
        let channel = status & 0x0F;
        match kind {
            0x80 | 0x90 => self.event_note(kind, channel, out)?,
            // Two data bytes: poly aftertouch, control change, pitch bend.
            0xA0 | 0xB0 | 0xE0 => {
                self.r.take(2)?;
            }
            // One data byte: program change, channel aftertouch.
            0xC0 | 0xD0 => {
                self.r.take(1)?;
            }
            _ => match status {
                0xF0 | 0xF7 => {
                    let len = read_var_len(&mut self.r)? as usize;
                    self.r.take(len)?;
                }
                _ => {}
            },
        }
        Ok(())
    }

    fn resolve_status(&mut self, first: u8) -> Result<u8, MidiError> {
        if first & 0x80 != 0 {
            self.running_status = Some(first);
            Ok(first)
        } else {
            let st = self.running_status.ok_or(MidiError::Malformed {
                at: self.body_start + self.r.pos - 1,
            })?;
            // `first` is the first data byte; re-read it as such.
            self.r.pos -= 1;
            Ok(st)
        }
    }

    fn event_note(
        &mut self,
        kind: u8,
        channel: u8,
        out: &mut Vec<MidiEvent>,
    ) -> Result<(), MidiError> {
        let note = self.r.u8()?;
        let velocity = self.r.u8()?;
        if note > 127 || velocity > 127 {
            return Err(MidiError::Malformed {
                at: self.body_start + self.r.pos,
            });
        }
        if kind == 0x90 && velocity > 0 {
            self.sounding.push((channel, note));
            self.push(
                out,
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                },
            );
        } else {
            remove_sounding(&mut self.sounding, channel, note);
            self.push(
                out,
                MidiMessage::NoteOff {
                    channel,
                    note,
                    velocity,
                },
            );
        }
        Ok(())
    }

    fn push(&mut self, out: &mut Vec<MidiEvent>, message: MidiMessage) {
        out.push(MidiEvent {
            track: self.track_no,
            tick: self.tick,
            order_in_track: self.order,
            message,
        });
        self.order = self.order.saturating_add(1);
    }
}

fn remove_sounding(sounding: &mut Vec<(u8, u8)>, channel: u8, note: u8) {
    if let Some(idx) = sounding
        .iter()
        .position(|&(c, n)| c == channel && n == note)
    {
        sounding.remove(idx);
    }
}

/// Reads a MIDI variable-length quantity (1–4 bytes, 7 data bits each).
fn read_var_len(r: &mut Reader<'_>) -> Result<u32, MidiError> {
    let mut value: u32 = 0;
    for _ in 0..4 {
        let b = r.u8()?;
        value = (value << 7) | u32::from(b & 0x7F);
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(MidiError::Malformed { at: r.pos })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SmfBuilder {
        tracks: Vec<Vec<u8>>,
    }

    impl SmfBuilder {
        const fn new() -> Self {
            Self { tracks: Vec::new() }
        }

        fn format0(&self) -> Vec<u8> {
            let division: u16 = 480;
            let mut out = Vec::new();
            out.extend_from_slice(b"MThd");
            out.extend_from_slice(&6u32.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&division.to_be_bytes());
            for t in &self.tracks {
                out.extend_from_slice(b"MTrk");
                out.extend_from_slice(&u32::try_from(t.len()).unwrap().to_be_bytes());
                out.extend_from_slice(t);
            }
            out
        }
    }

    fn var_len(mut v: u32) -> Vec<u8> {
        let mut buf = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            buf.insert(0, ((v & 0x7F) as u8) | 0x80);
            v >>= 7;
        }
        buf
    }

    fn note_pair(note: u8, on_vel: u8) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0x90, note, on_vel]);
        t.extend(var_len(480));
        t.extend([0x80, note, 0x40]);
        t.extend(var_len(0));
        t.extend([0xFF, 0x2F, 0x00]);
        t
    }

    #[test]
    fn empty_input_is_empty_error() {
        assert_eq!(parse_smf(&[]), Err(MidiError::Empty));
    }

    #[test]
    fn parses_note_on_off_with_absolute_ticks() {
        let mut b = SmfBuilder::new();
        b.tracks.push(note_pair(60, 100));
        let file = parse_smf(&b.format0()).unwrap();
        assert_eq!(file.format, 0);
        assert_eq!(file.ppqn, 480);
        assert_eq!(file.track_count, 1);
        assert_eq!(file.events.len(), 2);
        assert_eq!(
            file.events[0],
            MidiEvent {
                track: 0,
                tick: 0,
                order_in_track: 0,
                message: MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100
                }
            }
        );
        assert_eq!(
            file.events[1].message,
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0x40
            }
        );
        assert_eq!(file.events[1].tick, 480);
    }

    #[test]
    fn tempo_meta_is_decoded_as_micros_per_quarter() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // 500000 us/qn
        t.extend(var_len(0));
        t.extend([0xFF, 0x2F, 0x00]);
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let file = parse_smf(&b.format0()).unwrap();
        assert_eq!(file.events.len(), 1);
        assert_eq!(
            file.events[0].message,
            MidiMessage::Tempo {
                micros_per_quarter: 500_000
            }
        );
    }

    #[test]
    fn velocity_zero_is_note_off() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0x90, 60, 0x00]);
        t.extend(var_len(0));
        t.extend([0xFF, 0x2F, 0x00]);
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let file = parse_smf(&b.format0()).unwrap();
        assert_eq!(file.events.len(), 1);
        assert_eq!(
            file.events[0].message,
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            }
        );
    }

    #[test]
    fn running_status_reuses_previous_status() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0x90, 60, 100]);
        t.extend(var_len(0));
        t.extend([62, 100]);
        t.extend(var_len(0));
        t.extend([64, 100]);
        t.extend(var_len(0));
        t.extend([0xFF, 0x2F, 0x00]);
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let file = parse_smf(&b.format0()).unwrap();
        let ons: Vec<u8> = file
            .events
            .iter()
            .filter_map(|e| match e.message {
                MidiMessage::NoteOn { note, .. } => Some(note),
                _ => None,
            })
            .collect();
        assert_eq!(ons, vec![60, 62, 64]);
    }

    #[test]
    fn dangling_note_is_released_at_end_of_track() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0x90, 70, 90]);
        t.extend(var_len(96));
        t.extend([0xFF, 0x2F, 0x00]);
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let file = parse_smf(&b.format0()).unwrap();
        assert_eq!(file.events.len(), 2);
        assert!(matches!(
            file.events[0].message,
            MidiMessage::NoteOn { note: 70, .. }
        ));
        assert_eq!(
            file.events[1].message,
            MidiMessage::NoteOff {
                channel: 0,
                note: 70,
                velocity: 0
            }
        );
        assert_eq!(file.events[1].tick, 96);
    }

    #[test]
    fn unknown_meta_is_skipped_by_length() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0xFF, 0x7F, 0x02, 0xAA, 0xBB]);
        t.extend(var_len(0));
        t.extend([0x90, 60, 100]);
        t.extend(var_len(0));
        t.extend([0xFF, 0x2F, 0x00]);
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let file = parse_smf(&b.format0()).unwrap();
        // The NoteOn survives the skipped meta; EOT then releases it as a
        // NoteOff (dangling-note rule), so there are two events.
        assert_eq!(file.events.len(), 2);
        assert!(matches!(
            file.events[0].message,
            MidiMessage::NoteOn { note: 60, .. }
        ));
        assert!(matches!(
            file.events[1].message,
            MidiMessage::NoteOff { note: 60, .. }
        ));
    }

    #[test]
    fn events_are_sorted_by_tick_then_track() {
        let mut t0 = Vec::new();
        t0.extend(var_len(480));
        t0.extend([0x90, 60, 100]);
        t0.extend(var_len(0));
        t0.extend([0xFF, 0x2F, 0x00]);
        let mut t1 = Vec::new();
        t1.extend(var_len(0));
        t1.extend([0x90, 72, 100]);
        t1.extend(var_len(0));
        t1.extend([0xFF, 0x2F, 0x00]);

        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&480u16.to_be_bytes());
        // Chunk order [t0 (tick 480), t1 (tick 0)]: file/track order puts the
        // late event first, so only a real sort by tick yields tick 0 first.
        for t in [&t0, &t1] {
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&u32::try_from(t.len()).unwrap().to_be_bytes());
            out.extend_from_slice(t);
        }
        let file = parse_smf(&out).unwrap();
        // t0 is track 0 (tick 480), t1 is track 1 (tick 0).
        assert_eq!(file.events[0].tick, 0);
        assert_eq!(file.events[0].track, 1);
        assert!(matches!(
            file.events[0].message,
            MidiMessage::NoteOn { note: 72, .. }
        ));
        let late = file
            .events
            .iter()
            .find(|e| matches!(e.message, MidiMessage::NoteOn { note: 60, .. }))
            .unwrap();
        assert_eq!(late.tick, 480);
        assert_eq!(late.track, 0);
    }

    #[test]
    fn truncated_track_errors_instead_of_panicking() {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&480u16.to_be_bytes());
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&100u32.to_be_bytes());
        out.extend_from_slice(&[0x00, 0x90, 60, 100]);
        let err = parse_smf(&out).unwrap_err();
        assert!(matches!(err, MidiError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn smpte_division_is_unsupported() {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0xE728u16.to_be_bytes());
        let err = parse_smf(&out).unwrap_err();
        assert_eq!(err, MidiError::UnsupportedDivision);
    }

    #[test]
    fn bad_header_magic_is_malformed() {
        let err = parse_smf(b"XXXX\x00\x00\x00\x06").unwrap_err();
        assert!(matches!(err, MidiError::Malformed { .. }));
    }

    #[test]
    fn oversized_note_is_malformed() {
        let mut t = Vec::new();
        t.extend(var_len(0));
        t.extend([0x90, 0xBC, 100]); // note 188 > 127 (0xBC)
        let mut b = SmfBuilder::new();
        b.tracks.push(t);
        let err = parse_smf(&b.format0()).unwrap_err();
        assert!(matches!(err, MidiError::Malformed { .. }), "got {err:?}");
    }
}
