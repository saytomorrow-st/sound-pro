//! End-to-end checks for the sampler voice engine (SPEC-063 / RFC-001 §3–5).
//!
//! These drive [`SamplerEngine::render`] directly with a caller-provided
//! buffer, the same way the offline renderer will in a later PR. They cover
//! pitch accuracy, velocity mapping, voice stealing, group choke (`off_by`),
//! the amplitude envelope, block-size determinism, and dropped-start
//! accounting. The no-allocation contract is proven separately in
//! `sampler_no_alloc.rs` (it needs a process-global counting allocator that
//! parallel tests here would corrupt).

use std::sync::Arc;

use audio_core::midi::{MidiEvent, MidiMessage};
use audio_core::sampler::engine::{SamplerEngine, playback_ratio};
use audio_core::sampler::region::{AmpEnvelope, SampleRegion};
use audio_core::sampler::sfz::{LoopMode, OffMode};
use audio_core::sampler::voice::STEAL_FADE_SAMPLES;
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};

const SR: u32 = 48_000;
const QUANTUM: usize = 256;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn note_on(sample: u64, note: u8, velocity: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity,
            },
        },
    }
}

fn note_off(sample: u64, note: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: MidiMessage::NoteOff {
                channel: 0,
                note,
                velocity: 0,
            },
        },
    }
}

fn schedule(events: Vec<ScheduledEvent>) -> Schedule {
    let length = events.last().map_or(0, |e| e.sample);
    Schedule::new(ScheduleSpec {
        events,
        length_samples: length,
    })
}

/// Renders the whole schedule into a fresh stereo pair and returns the left
/// channel.
fn render(
    engine: &mut SamplerEngine,
    schedule: &Schedule,
    frames: u64,
    quantum: usize,
) -> Vec<f32> {
    let mut left = vec![0.0_f32; frames as usize];
    let mut right = vec![0.0_f32; frames as usize];
    let mut cursor = schedule.cursor();
    let mut position = 0_u64;
    while position < frames {
        let n = ((frames - position) as usize).min(quantum);
        engine.render(
            schedule,
            &mut cursor,
            position,
            n,
            &mut left[position as usize..],
            &mut right[position as usize..],
        );
        position += n as u64;
    }
    left
}

/// A constant-amplitude mono sample region over a single note.
fn dc_region(note: u8) -> SampleRegion {
    SampleRegion {
        sample_index: 0,
        root: note,
        key_lo: note,
        key_hi: note,
        ..SampleRegion::default()
    }
}

/// A 1-sample bank of constant DC, which makes peak measurements exact.
fn dc_bank(len: usize) -> Vec<Arc<[i16]>> {
    vec![Arc::from(vec![10_000_i16; len])]
}

// ---------------------------------------------------------------------------
// Pitch / frequency accuracy
// ---------------------------------------------------------------------------

#[test]
fn pitch_ratio_matches_equal_temperament() {
    assert!((playback_ratio(81, 69, 0, SR) - 2.0).abs() < 1e-12);
    assert!((playback_ratio(57, 69, 0, SR) - 0.5).abs() < 1e-12);
    assert!((playback_ratio(69, 69, 1200, SR) - 2.0).abs() < 1e-12);
}

#[test]
fn rendered_sine_has_expected_frequency() {
    // One cycle of a 1 kHz sine at 48 kHz, looped continuously, played at its
    // own root note (60) replays 1:1, so the output is ~1 kHz.
    let frames = SR as usize;
    let sample: Vec<i16> = (0..frames)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let s = (2.0 * std::f64::consts::PI * 1000.0 * t).sin();
            (s * 30_000.0) as i16
        })
        .collect();

    let reg = SampleRegion {
        sample_index: 0,
        root: 60,
        key_lo: 60,
        key_hi: 60,
        loop_mode: LoopMode::LoopContinuous,
        ..SampleRegion::default()
    };

    let mut engine = SamplerEngine::new(SR, vec![reg], vec![Arc::from(sample)], 8).unwrap();
    let sched = schedule(vec![note_on(0, 60, 100)]);
    let out = render(&mut engine, &sched, SR as u64, QUANTUM);

    // Count zero crossings over the last half-second (settled, post-attack):
    // ~1000 crossings for 1 kHz at 48 kHz over 0.5 s.
    let crossings = out[(SR as usize) / 2..]
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    assert!(
        (900..=1100).contains(&crossings),
        "expected ~1000 zero crossings over 0.5 s of 1 kHz, got {crossings}"
    );
}

// ---------------------------------------------------------------------------
// Velocity mapping
// ---------------------------------------------------------------------------

#[test]
fn velocity_maps_to_squared_curve_at_full_track() {
    // vel_track = 1.0: curve = (vel/127)^2 (RFC-001 §2).
    let peak_for = |velocity: u8| -> f32 {
        let mut reg = dc_region(60);
        reg.vel_track = 1.0;
        reg.gain = 1.0;
        let mut engine = SamplerEngine::new(SR, vec![reg], dc_bank(4096), 8).unwrap();
        let sched = schedule(vec![note_on(0, 60, velocity)]);
        let out = render(&mut engine, &sched, 1024, QUANTUM);
        out.iter().cloned().fold(0.0_f32, f32::max)
    };

    let loud = peak_for(127);
    let quiet = peak_for(64);
    let expected = (64.0_f32 / 127.0).powi(2);
    assert!(
        (quiet / loud - expected).abs() < 1e-3,
        "velocity ratio {} != expected {expected}",
        quiet / loud
    );
}

// ---------------------------------------------------------------------------
// Voice stealing
// ---------------------------------------------------------------------------

#[test]
fn voice_pool_steals_oldest_held_when_full() {
    // Nine regions (notes 60..=68) all sharing one sample; four-voice pool.
    let regions: Vec<SampleRegion> = (60..=68).map(dc_region).collect();
    let mut engine = SamplerEngine::new(SR, regions, dc_bank(4096), 4).unwrap();
    assert_eq!(engine.voice_count(), 4);

    // Fill the pool with four held notes.
    let events = (60..64).map(|n| note_on(0, n, 100)).collect();
    let sched = schedule(events);
    let _ = render(&mut engine, &sched, 1024, QUANTUM);
    assert_eq!(engine.voices_started(), 4);
    assert_eq!(engine.stolen(), 0);

    // A fifth note-on (note 64 has a region) forces a steal of the oldest held
    // voice.
    let sched = schedule(vec![note_on(1024, 64, 100)]);
    let _ = render(&mut engine, &sched, 2048, QUANTUM);
    assert_eq!(engine.stolen(), 1);
}

#[test]
fn note_outside_any_region_counts_dropped_start() {
    // Only note 60 exists; a note-on for note 100 has no region and must be
    // counted as a dropped start rather than starting a voice.
    let mut engine = SamplerEngine::new(SR, vec![dc_region(60)], dc_bank(4096), 8).unwrap();
    let sched = schedule(vec![note_on(0, 100, 100)]);
    let _ = render(&mut engine, &sched, 1024, QUANTUM);

    assert_eq!(engine.voices_started(), 0);
    assert_eq!(engine.dropped_starts(), 1);
}

// ---------------------------------------------------------------------------
// Group choke (off_by)
// ---------------------------------------------------------------------------

#[test]
fn off_by_fast_chokes_group_with_five_ms_fade() {
    // Region A belongs to group 1; region B cuts group 1 with a fast fade.
    let mut a = dc_region(60);
    a.group = Some(1);

    let mut b = dc_region(61);
    b.off_by = Some(1);
    b.off_mode = OffMode::Fast;

    let mut engine = SamplerEngine::new(SR, vec![a, b], dc_bank(8192), 8).unwrap();

    // A starts at 0; B at 1024 chokes A.
    let sched = schedule(vec![note_on(0, 60, 100), note_on(1024, 61, 100)]);
    let out = render(&mut engine, &sched, 2048, QUANTUM);

    // Before the choke, A is at full amplitude (DC sample, no attack).
    let before = out[512].abs();
    assert!(before > 0.0, "A must sound before the choke");

    // After the choke plus the 5 ms (240-sample) fast fade, A's contribution is
    // gone; only B (note 61) keeps ringing.
    let fade_end = 1024 + STEAL_FADE_SAMPLES as usize;
    let a_after = out[fade_end + 1..].iter().cloned().fold(0.0_f32, f32::max);
    assert!(a_after.is_finite());
    assert_eq!(engine.stolen(), 0, "off_by must not count as stealing");
    assert_eq!(engine.voices_started(), 2);
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[test]
fn adsr_envelope_shapes_amplitude() {
    let mut reg = dc_region(60);
    reg.envelope = AmpEnvelope {
        attack_samples: 480, // 10 ms
        hold_samples: 480,   // 10 ms
        decay_samples: 480,  // 10 ms
        sustain: 0.5,
        release_samples: 480, // 10 ms
    };

    let mut engine = SamplerEngine::new(SR, vec![reg], dc_bank(8192), 8).unwrap();
    let sched = schedule(vec![note_on(0, 60, 127)]);
    let out = render(&mut engine, &sched, 480 * 4, QUANTUM);

    let at_240 = out[240].abs();
    let at_peak = out[480].abs();
    assert!(at_240 < at_peak, "attack must ramp up to peak");

    // After hold+decay (480 + 480 + 480 = 1440), level settles at sustain.
    let at_sustain = out[1440].abs();
    assert!(
        (at_sustain / at_peak - 0.5).abs() < 1e-2,
        "sustain must be half peak, got {}",
        at_sustain / at_peak
    );
}

// ---------------------------------------------------------------------------
// Determinism (block-size independence)
// ---------------------------------------------------------------------------

#[test]
fn render_is_bit_exact_across_quantum_sizes() {
    let reg = dc_region(60);
    let sched = schedule(vec![note_on(0, 60, 100), note_off(500, 60)]);

    let mut a = SamplerEngine::new(SR, vec![reg.clone()], dc_bank(4096), 8).unwrap();
    let mut b = SamplerEngine::new(SR, vec![reg], dc_bank(4096), 8).unwrap();

    let out_a = render(&mut a, &sched, 2048, 16);
    let out_b = render(&mut b, &sched, 2048, 1024);

    assert_eq!(out_a.len(), out_b.len());
    for (i, (x, y)) in out_a.iter().zip(out_b.iter()).enumerate() {
        assert_eq!(
            x.to_bits(),
            y.to_bits(),
            "sample {i} differs between quantum sizes"
        );
    }
}
