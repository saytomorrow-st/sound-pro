//! SPEC-004 integration tests: robustness against random input and the
//! public parse API.
//!
//! Runs on any machine without audio hardware or external crates.

use std::time::Instant;

use audio_core::midi::{MidiError, MidiMessage, parse_smf};

/// Deterministic xorshift64 PRNG. Kept inline so the timeout test needs no
/// external dependency and never touches randomness from the OS.
struct XorShift64(u64);

impl XorShift64 {
    const fn new(seed: u64) -> Self {
        // Avoid the all-zero state, which xorshift cannot leave.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

#[test]
fn ten_megabytes_of_random_bytes_error_quickly_without_panicking() {
    let mut rng = XorShift64::new(0xDEAD_BEEF_CAFE_1234);
    let mut data = vec![0u8; 10 * 1024 * 1024];
    rng.fill(&mut data);

    let start = Instant::now();
    // Must not panic; any Err is acceptable, Ok only if it happens to parse.
    let result = parse_smf(&data);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 1,
        "parsing 10 MiB took {elapsed:?}, expected < 1s"
    );
    // Random bytes almost certainly don't start with "MThd"; assert the
    // documented error path so the test is meaningful, not vacuous.
    assert!(
        matches!(result, Err(MidiError::Malformed { .. })),
        "expected Malformed for random data, got {result:?}"
    );
}

#[test]
fn garbage_that_looks_like_a_header_still_terminates() {
    // A plausible header followed by 10 MiB of random track payload: the parser
    // must bound every read by the remaining length and finish fast.
    let mut rng = XorShift64::new(42);
    let mut data = Vec::new();
    data.extend_from_slice(b"MThd");
    data.extend_from_slice(&6u32.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&1u16.to_be_bytes());
    data.extend_from_slice(&480u16.to_be_bytes());
    let payload_len = 10 * 1024 * 1024;
    data.extend_from_slice(b"MTrk");
    data.extend_from_slice(&(payload_len as u32).to_be_bytes());
    let mut payload = vec![0u8; payload_len];
    rng.fill(&mut payload);
    data.extend_from_slice(&payload);

    let start = Instant::now();
    let result = parse_smf(&data);
    assert!(
        start.elapsed().as_secs() < 1,
        "10 MiB payload parse too slow"
    );
    // Whatever happens, it must be Ok or a clean Err, never a panic (a panic
    // would abort the test process before this point).
    let _ = result;
}

#[test]
fn truncated_public_input_reports_offset() {
    // Header says 6-byte body, then a track claiming 16 bytes but only 2 shown.
    let mut data = Vec::new();
    data.extend_from_slice(b"MThd");
    data.extend_from_slice(&6u32.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&1u16.to_be_bytes());
    data.extend_from_slice(&480u16.to_be_bytes());
    data.extend_from_slice(b"MTrk");
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&[0x00, 0x90]);

    match parse_smf(&data) {
        Err(MidiError::Truncated { at }) => {
            assert!(at >= 22, "offset should point into the missing body");
        }
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn note_helper_returns_only_note_ons_in_order() {
    // Two notes, second starts later; notes() must list them by tick.
    let mut data = Vec::new();
    data.extend_from_slice(b"MThd");
    data.extend_from_slice(&6u32.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&1u16.to_be_bytes());
    data.extend_from_slice(&480u16.to_be_bytes());

    let mut t = Vec::new();
    t.extend([0x00, 0x90, 60, 100]); // tick 0
    t.extend([0x60, 0x90, 64, 100]); // delta 0x60=96 -> tick 96
    t.extend([0x00, 0xFF, 0x2F, 0x00]);
    data.extend_from_slice(b"MTrk");
    data.extend_from_slice(&(t.len() as u32).to_be_bytes());
    data.extend_from_slice(&t);

    let file = parse_smf(&data).unwrap();
    let notes = file.notes();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].tick, 0);
    assert!(matches!(
        notes[0].message,
        MidiMessage::NoteOn { note: 60, .. }
    ));
    assert_eq!(notes[1].tick, 96);
    assert!(matches!(
        notes[1].message,
        MidiMessage::NoteOn { note: 64, .. }
    ));
}
