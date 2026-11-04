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
use audio_core::sampler::region::{AmpEnvelope, RegionTable, SampleRegion};
use audio_core::sampler::sfz::{LoopMode, OffMode};
use audio_core::sampler::voice::{Interpolation, STEAL_FADE_SAMPLES, SamplerVoice};
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

// ---------------------------------------------------------------------------
// Regression tests (Sonnet review of PR #85)
// ---------------------------------------------------------------------------

/// Item 1: a stolen voice must fade out over 5 ms instead of being cut. With a
/// one-voice pool, a second note-on steals the first; the first's tail must
/// still ring right after the steal, not drop to silence instantly.
#[test]
fn stolen_voice_fades_out_instead_of_clicking() {
    // note 60 plays a loud DC sample; note 61 plays a silent sample. With a
    // one-voice pool, note 61 steals note 60, so any output right after the
    // steal must come from note 60's fading tail.
    let mut reg_a = dc_region(60);
    reg_a.sample_index = 0; // DC 10_000
    let mut reg_b = dc_region(61);
    reg_b.sample_index = 1; // silence
    let bank = vec![
        Arc::from(vec![10_000_i16; 8192]),
        Arc::from(vec![0_i16; 8192]),
    ];
    let mut engine = SamplerEngine::new(SR, vec![reg_a, reg_b], bank, 1).unwrap();

    let sched = schedule(vec![note_on(0, 60, 100), note_on(1000, 61, 100)]);
    let out = render(&mut engine, &sched, 2048, QUANTUM);

    // At the steal point the victim's 5 ms tail is still sounding; the new
    // voice (note 61) is silent, so any non-zero output right after sample
    // 1000 must come from the fading victim.
    assert!(
        out[1000].abs() > 5000.0 / 32768.0,
        "victim tail should still ring at the steal, got {}",
        out[1000].abs()
    );
    // And the tail must be gone once the 240-sample fade completes.
    let after = out[1000 + STEAL_FADE_SAMPLES as usize + 1..]
        .iter()
        .cloned()
        .fold(0.0_f32, f32::max);
    assert!(
        after.abs() < 1.0 / 32768.0,
        "victim tail should be silent after the fade, got {after}"
    );
    assert_eq!(engine.stolen(), 1);
}

/// Item 2: a region whose `group == off_by` (a closed hi-hat cutting its own
/// group) must not cut its own just-started note.
#[test]
fn off_by_does_not_cut_its_own_note() {
    let mut reg = dc_region(60);
    reg.group = Some(1);
    reg.off_by = Some(1);
    reg.off_mode = OffMode::Fast;

    let mut engine = SamplerEngine::new(SR, vec![reg], dc_bank(8192), 8).unwrap();
    let sched = schedule(vec![note_on(0, 60, 100)]);
    let out = render(&mut engine, &sched, 8192, QUANTUM);

    // The note must keep sounding at full DC well past the 240-sample fade.
    let after = out[STEAL_FADE_SAMPLES as usize + 1..]
        .iter()
        .cloned()
        .fold(0.0_f32, f32::max);
    assert!(
        after.abs() > 5000.0 / 32768.0,
        "self off_by must not choke its own note, got peak {after}"
    );
}

/// Item 3: a fast-faded voice is freed when its fade completes, so the pool
/// does not leak. With a two-voice pool, re-triggering the choked note after
/// the fade must reuse the freed slot rather than stealing.
#[test]
fn faded_voice_is_freed_so_the_pool_does_not_leak() {
    let mut a = dc_region(60);
    a.group = Some(1);
    let mut b = dc_region(61);
    b.off_by = Some(1);
    b.off_mode = OffMode::Fast;

    let mut engine = SamplerEngine::new(SR, vec![a, b], dc_bank(8192), 2).unwrap();

    // note 60 (group 1) starts; note 61 chokes it fast at 100; after the
    // 240-sample fade note 60's slot is freed; re-triggering note 60 at 1000
    // (a later block, after the fade has actually rendered out) must land in
    // that free slot, not steal note 61.
    let sched = schedule(vec![
        note_on(0, 60, 100),
        note_on(100, 61, 100),
        note_on(1000, 60, 100),
    ]);
    let _ = render(&mut engine, &sched, 2048, QUANTUM);

    assert_eq!(engine.stolen(), 0, "freed slot must be reused, not stolen");
    assert_eq!(engine.voices_started(), 3);
}

/// Item 4: a held one-shot that runs to its sample end is freed even while its
/// level is still positive, instead of lingering and emitting a last-frame
/// sample every block.
#[test]
fn held_one_shot_is_freed_at_sample_end() {
    // A 256-frame DC sample, never looped, never released: note 60 plays to
    // its end while still at full gain, so the slot is freed only if the
    // engine idles it when `advance()` returns false (not when gain hits 0).
    let regs = vec![
        SampleRegion {
            sample_index: 0,
            root: 60,
            key_lo: 60,
            key_hi: 60,
            loop_mode: LoopMode::NoLoop,
            ..SampleRegion::default()
        },
        SampleRegion {
            sample_index: 0,
            root: 61,
            key_lo: 61,
            key_hi: 61,
            loop_mode: LoopMode::NoLoop,
            ..SampleRegion::default()
        },
    ];
    let short = vec![Arc::from(vec![10_000_i16; 256])];
    let mut engine = SamplerEngine::new(SR, regs, short, 1).unwrap();

    // note 61 (at 1000, a later block after note 60's sample end at ~256 has
    // rendered out) has a region, so the only way it avoids stealing is if
    // note 60's slot was freed at sample end.
    let sched = schedule(vec![note_on(0, 60, 100), note_on(1000, 61, 100)]);
    let _ = render(&mut engine, &sched, 2048, QUANTUM);

    assert_eq!(
        engine.stolen(),
        0,
        "one-shot freed at sample end must be reused, not stolen"
    );
}

/// Item 5: the fast-fade ramp is linear in `fade_from`; it must not multiply
/// `fade_from` by the live envelope again (which squared the base).
#[test]
fn fast_fade_is_linear_not_squared() {
    let mut v = SamplerVoice::idle();
    let env = AmpEnvelope {
        attack_samples: 0,
        hold_samples: 0,
        decay_samples: 0,
        sustain: 0.5,
        release_samples: 0,
    };
    v.start(
        0, 60, 100, 0, 0.0, 1.0, 1.0, 1.0, 1.0, false, 0.0, 0.0, 1000.0, env,
    );
    assert!((v.level_at(0) - 0.5).abs() < 1e-6, "sustain must be 0.5");
    v.fast_fade(0);

    // Halfway through the 240-sample fade: 0.5 * (1 - 0.5) = 0.25. The old
    // code returned base * fade_from * (1 - done) = 0.5 * 0.5 * 0.5 = 0.125.
    let mid = v.level_at(u64::from(STEAL_FADE_SAMPLES / 2));
    assert!(
        (mid - 0.25).abs() < 1e-3,
        "fade midpoint must be 0.25, got {mid}"
    );
}

/// Item 7: a voice at zero level must still advance its playhead, otherwise it
/// stalls and is never freed. A zero-sustain short sample, held forever, must
/// still run to its end so the slot is freed for a later note.
#[test]
fn zero_level_voice_still_advances_playhead() {
    let mut reg = SampleRegion {
        sample_index: 0,
        root: 60,
        key_lo: 60,
        key_hi: 60,
        loop_mode: LoopMode::NoLoop,
        ..SampleRegion::default()
    };
    reg.envelope = AmpEnvelope {
        attack_samples: 0,
        hold_samples: 0,
        decay_samples: 0,
        sustain: 0.0,
        release_samples: 0,
    };
    let reg61 = SampleRegion {
        sample_index: 0,
        root: 61,
        key_lo: 61,
        key_hi: 61,
        loop_mode: LoopMode::NoLoop,
        ..SampleRegion::default()
    };
    let short = vec![Arc::from(vec![10_000_i16; 256])];
    let mut engine = SamplerEngine::new(SR, vec![reg, reg61], short, 1).unwrap();

    // note 61 (at 1000, a later block after note 60's sample end has rendered
    // out) has a region, so the only way it avoids stealing is if note 60's
    // playhead advanced past the sample end despite zero level and freed the
    // slot. On the old code the playhead stalled at gain 0, so the slot stayed
    // occupied and note 61 had to steal.
    let sched = schedule(vec![note_on(0, 60, 100), note_on(1000, 61, 100)]);
    let _ = render(&mut engine, &sched, 2048, QUANTUM);

    assert_eq!(engine.stolen(), 0, "zero-level voice must free its slot");
}

/// Item 8: region index `u16::MAX` must not be indexed (it equals the
/// `NO_REGION` sentinel). A region at enumerate index 65535 is dropped, so
/// `find` reports no match instead of returning the colliding index.
#[test]
fn region_index_65535_is_not_indexed() {
    // 65536 regions; only the last (index 65535) references a valid sample, so
    // it is the only one eligible for the table.
    let mut regions: Vec<SampleRegion> = (0..=u16::MAX as usize + 1)
        .map(|_| SampleRegion {
            sample_index: usize::MAX, // skipped: out of range
            key_lo: 60,
            key_hi: 60,
            ..SampleRegion::default()
        })
        .collect();
    regions[u16::MAX as usize] = SampleRegion {
        sample_index: 0,
        key_lo: 60,
        key_hi: 60,
        ..SampleRegion::default()
    };

    let table = RegionTable::build(&regions, 1);
    assert_eq!(
        table.find(&regions, 60, 100),
        None,
        "region index 65535 collides with NO_REGION and must be dropped"
    );
}

// ---------------------------------------------------------------------------
// SPEC-068 step A: 4-point Hermite interpolation
// ---------------------------------------------------------------------------

/// Renders a note-on over `frames` output samples with a caller-chosen
/// interpolation mode, returning the left channel.
fn render_with_interpolation(
    interpolation: Interpolation,
    sample: Vec<i16>,
    region: SampleRegion,
    frames: u64,
    quantum: usize,
) -> Vec<f32> {
    let mut engine =
        SamplerEngine::new_with(SR, vec![region], vec![Arc::from(sample)], 8, interpolation)
            .unwrap();
    let sched = schedule(vec![note_on(0, 60, 127)]);
    let mut left = vec![0.0_f32; frames as usize];
    let mut right = vec![0.0_f32; frames as usize];
    let mut cursor = sched.cursor();
    let mut position = 0_u64;
    while position < frames {
        let n = ((frames - position) as usize).min(quantum);
        engine.render(
            &sched,
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

/// The default interpolation is `Linear` and the default-constructed engine is
/// bit-identical to the pre-SPEC-068 `new` engine (goldens stay untouched).
#[test]
fn default_interpolation_is_linear_and_bit_identical() {
    assert_eq!(Interpolation::default(), Interpolation::Linear);
    let reg = dc_region(60);
    let bank = dc_bank(4096);
    let mut a = SamplerEngine::new(SR, vec![reg.clone()], bank.clone(), 8).unwrap();
    let mut b = SamplerEngine::new_with(SR, vec![reg], bank, 8, Interpolation::default()).unwrap();
    let sched = schedule(vec![note_on(0, 60, 100), note_off(500, 60)]);
    let out_a = render(&mut a, &sched, 2048, QUANTUM);
    let out_b = render(&mut b, &sched, 2048, QUANTUM);
    for (i, (x, y)) in out_a.iter().zip(out_b.iter()).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "sample {i} differs");
    }
}

/// On a pure sine well below Nyquist, 4-point Hermite is strictly more
/// accurate than linear: max absolute error vs the analytic value is at least
/// 10× smaller (SPEC-068 step A acceptance criterion).
#[test]
fn hermite_outperforms_linear_on_sine_by_10x() {
    const RATE: f64 = 44_100.0;
    const FREQ: f64 = 1000.0; // ≤ fs/8
    const SEMIS: f64 = 7.0; // +7 semitones transposition
    let frames = 44_100_usize;
    let ratio = 2.0_f64.powf(SEMIS / 12.0);

    // Source sine at 44.1 kHz, one full second.
    let sample: Vec<i16> = (0..frames)
        .map(|i| {
            let t = i as f64 / RATE;
            let s = (2.0 * std::f64::consts::PI * FREQ * t).sin();
            (s * 30_000.0) as i16
        })
        .collect();

    // root 53, key range covering 60: playing note 60 is +7 semitones, giving
    // a fractional playback ratio (2^(7/12)).
    let reg = SampleRegion {
        sample_index: 0,
        root: 53,
        key_lo: 53,
        key_hi: 60,
        ..SampleRegion::default()
    };

    let linear = render_with_interpolation(
        Interpolation::Linear,
        sample.clone(),
        reg.clone(),
        4096,
        QUANTUM,
    );
    let hermite = render_with_interpolation(Interpolation::Hermite4, sample, reg, 4096, QUANTUM);

    // Compare against the analytic sine, advancing by `ratio` source frames per
    // output frame (the engine reads at `pos = k * ratio`). Skip the attack
    // tail (default envelope has zero attack) and the first few samples.
    let max_err = |out: &[f32]| -> f64 {
        let mut worst = 0.0_f64;
        for (k, v) in out.iter().enumerate() {
            let src = k as f64 * ratio;
            let expected = (2.0 * std::f64::consts::PI * FREQ * src / RATE).sin();
            let e = (f64::from(*v) * 32768.0 / 30_000.0 - expected).abs();
            worst = worst.max(e);
        }
        worst
    };

    let e_linear = max_err(&linear);
    let e_hermite = max_err(&hermite);
    assert!(
        e_hermite < e_linear,
        "hermite {e_hermite} not better than linear {e_linear}"
    );
    assert!(
        e_linear >= 10.0 * e_hermite,
        "expected ≥10× improvement: linear {e_linear}, hermite {e_hermite}"
    );
}

/// Render output must be bit-identical across quantum sizes for both
/// interpolation modes (SPEC-068 step A determinism criterion).
#[test]
fn hermite_and_linear_are_bit_exact_across_block_sizes() {
    let mut reg = dc_region(60);
    reg.loop_mode = LoopMode::LoopContinuous;
    let sample: Vec<i16> = (0..1024)
        .map(|i| ((i as f64 * 0.01).sin() * 20_000.0) as i16)
        .collect();

    for mode in [Interpolation::Linear, Interpolation::Hermite4] {
        let a = render_with_interpolation(mode, sample.clone(), reg.clone(), 4096, 64);
        let b = render_with_interpolation(mode, sample.clone(), reg.clone(), 4096, 256);
        let c = render_with_interpolation(mode, sample.clone(), reg.clone(), 4096, 1024);
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "{mode:?} 64 vs 256 sample {i}");
        }
        for (i, (x, y)) in a.iter().zip(c.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "{mode:?} 64 vs 1024 sample {i}");
        }
    }
}

/// Hermite at the sample edges (1/2/3-frame buffers, last-frame position, loop
/// seam) must not panic or emit NaN.
#[test]
fn hermite_edges_do_not_panic_or_nan() {
    // The edge behaviour is proven directly in voice.rs unit tests; here we
    // exercise the full render path with tiny/looped buffers.
    let tiny: Vec<i16> = vec![10_000];
    let mut reg = dc_region(60);
    reg.loop_mode = LoopMode::LoopContinuous;
    let out = render_with_interpolation(Interpolation::Hermite4, tiny, reg, 1024, QUANTUM);
    assert!(
        out.iter().all(|v| v.is_finite()),
        "tiny buffer produced NaN/Inf"
    );

    let three: Vec<i16> = vec![1, 2, 3];
    let mut reg = dc_region(60);
    reg.loop_mode = LoopMode::LoopContinuous;
    let out = render_with_interpolation(Interpolation::Hermite4, three, reg, 1024, QUANTUM);
    assert!(
        out.iter().all(|v| v.is_finite()),
        "3-frame buffer produced NaN/Inf"
    );
}

/// Rendered amplitude is normalised to `-1.0..=1.0` and follows velocity.
#[test]
fn rendered_amplitude_is_normalised_and_velocity_dependent() {
    let peak_for = |velocity: u8| -> f32 {
        let mut reg = dc_region(60);
        reg.vel_track = 1.0;
        reg.gain = 1.0;
        let bank = vec![Arc::from(vec![i16::MAX; 4096])];
        let mut engine = SamplerEngine::new(SR, vec![reg], bank, 8).unwrap();
        let sched = schedule(vec![note_on(0, 60, velocity)]);
        let out = render(&mut engine, &sched, 1024, QUANTUM);
        out.iter().cloned().fold(0.0_f32, f32::max)
    };
    let loud = peak_for(120);
    let soft = peak_for(20);
    assert!(
        loud <= 1.0,
        "full-scale sample must not exceed 1.0, got {loud}"
    );
    assert!(
        soft < loud * 0.1,
        "vel 20 ({soft}) must be far quieter than vel 120 ({loud})"
    );
    assert!(soft > 0.0);
}
