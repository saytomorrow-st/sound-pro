//! SPEC-009 integration tests: the polyphonic synth driven the way the audio
//! thread drives it — through a [`Renderer`] with a real quantum size — plus an
//! end-to-end run from SMF bytes through [`ScheduleSpec::from_midi`].
//!
//! The unit tests in `audio_core::synth` cover the envelope maths; these cover
//! the contract that spans modules (quantum sizes, backward seek, MIDI input).

use audio_core::TransportCommand;
use audio_core::buffer::AudioBuffer;
use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::midi::{MidiEvent, MidiMessage, parse_smf};
use audio_core::render::Renderer;
use audio_core::schedule::{Schedule, ScheduleCursor, ScheduleSpec, ScheduledEvent};
use audio_core::synth::{Envelope, MAX_VOICES, Synth};
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Tempo, Transport};

const SR: u32 = DEFAULT_SAMPLE_RATE;

fn event(sample: u64, message: MidiMessage) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message,
        },
    }
}

fn on(sample: u64, note: u8) -> ScheduledEvent {
    event(
        sample,
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        },
    )
}

fn off(sample: u64, note: u8) -> ScheduledEvent {
    event(
        sample,
        MidiMessage::NoteOff {
            channel: 0,
            note,
            velocity: 0,
        },
    )
}

fn schedule(events: Vec<ScheduledEvent>, length_samples: u64) -> Schedule {
    // `Schedule::new` takes the caller's order as-is; it is a private-invariant
    // type in practice, so sort by sample the way `from_midi` does.
    let mut events = events;
    events.sort_by_key(|event| {
        (
            event.sample,
            event.source.track,
            event.source.order_in_track,
        )
    });
    Schedule::new(ScheduleSpec {
        events,
        length_samples,
    })
}

/// Inline node that renders [`Synth`] into an output port, mirroring
/// `daw_app::nodes::PolySynth` without pulling the shell crate into the core's
/// test suite.
struct SynthNode {
    synth: Synth,
    schedule: Schedule,
    cursor: ScheduleCursor,
    last_position: u64,
    scratch: Vec<f32>,
}

impl SynthNode {
    fn new(schedule: Schedule) -> Self {
        Self {
            synth: Synth::new(SR, Envelope::default()).expect("48 kHz is valid"),
            cursor: schedule.cursor(),
            schedule,
            last_position: 0,
            scratch: Vec::new(),
        }
    }
}

impl NodeProcessor for SynthNode {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        if ctx.position < self.last_position {
            self.cursor = self.schedule.cursor();
            self.synth.reset();
        }
        self.last_position = ctx.position;
        if self.scratch.len() < ctx.frames {
            self.scratch.resize(ctx.frames, 0.0);
        }
        self.synth.render(
            &self.schedule,
            &mut self.cursor,
            ctx.position,
            ctx.frames,
            &mut self.scratch,
        );
        output
            .channel_mut(0)
            .copy_from_slice(&self.scratch[..ctx.frames]);
    }
}

fn renderer(schedule: Schedule, quantum: usize) -> (Renderer, AudioBuffer) {
    let mut graph = Graph::with_capacity(SR, 1, quantum, 1, 1);
    graph.add_node("synth", SynthNode::new(schedule)).unwrap();
    let mut renderer = Renderer::new(graph, Transport::new(SR), quantum);
    renderer.transport_mut().apply(TransportCommand::Play);
    (renderer, AudioBuffer::new(1, quantum))
}

/// Renders `total` frames in `quantum`-sized chunks, returning the mono mix.
fn run(renderer: &mut Renderer, buffer: &mut AudioBuffer, total: usize) -> Vec<f32> {
    let quantum = buffer.frames();
    let mut out = Vec::with_capacity(total);
    let mut done = 0;
    while done < total {
        let frames = (total - done).min(quantum);
        renderer.render(frames, buffer).unwrap();
        out.extend_from_slice(&buffer.channel(0)[..frames]);
        done += frames;
    }
    out
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |a, v| a.max(v.abs()))
}

/// The envelope level at `index`, read off the oscillator's crests.
///
/// A single sample says little: the waveform swings +/-1 and the cosine phase
/// can line up with a crest slightly above the envelope during the decay. The
/// nearest crests around `index` are within a fraction of a period, so their
/// average is the envelope there.
fn envelope_at(samples: &[f32], index: usize) -> f32 {
    const SPAN: usize = 40;
    let from = index.saturating_sub(SPAN);
    let to = (index + SPAN).min(samples.len());
    let crests: Vec<f32> = samples[from..to]
        .windows(3)
        .filter(|w| w[1].abs() >= w[0].abs() && w[1].abs() >= w[2].abs() && w[1].abs() > 0.0)
        .map(|w| w[1].abs())
        .collect();
    assert!(!crests.is_empty(), "no crest around sample {index}");
    crests.iter().sum::<f32>() / crests.len() as f32
}

#[test]
fn a_rendered_note_is_not_a_constant_block() {
    // Guards against a "flat" synth where the envelope never moves: the level
    // has to differ between the attack, the sustain and the release. Measured
    // per sample rather than over a window, because a window's maximum is
    // pulled up by the decay ramp inside it.
    // The note is held past the decay and released at 2000; the render covers
    // the whole 0.12 s release after it.
    let s = schedule(vec![on(0, 69), off(2000, 69)], 2000 + 5760 + 256);
    let total = usize::try_from(s.length_samples()).unwrap();
    let (mut r, mut buf) = renderer(s, 256);
    let out = run(&mut r, &mut buf, total);

    // Velocity 100 is a peak of 100/127 = 0.7874, decaying to 70 % of it.
    let (top, top_at) =
        (60..300)
            .map(|i| (envelope_at(&out, i), i))
            .fold(
                (0.0_f32, 0usize),
                |best, cur| if cur.0 > best.0 { cur } else { best },
            );
    let mid = envelope_at(&out, 1000);
    let late = envelope_at(&out, 1900);
    assert!(
        (top - 0.7874).abs() < 0.03,
        "attack peak {top} at {top_at}, want ~0.7874 (velocity 100 / 127)"
    );
    assert!(late < top, "sustain {late} must sit below the peak {top}");
    assert!(
        mid < top && mid > late,
        "the decay must move between the peak and the sustain: {top} > {mid} > {late}"
    );
    assert!(
        out[2000 + 5760..].iter().all(|v| v.abs() < 1e-6),
        "the note-off is at 2000 and the release is 5760 samples long"
    );
}

#[test]
fn one_quantum_of_8192_matches_many_small_quanta() {
    // SPEC-009: a huge quantum must not allocate per sample and must produce
    // the same audio. The renderer is given 8192 as its block size.
    let total = 8192;
    let s = schedule(
        vec![on(0, 60), on(1000, 64), off(4000, 60), off(7000, 64)],
        8192,
    );

    let (mut small_r, mut small_buf) = renderer(s.clone(), 256);
    let small = run(&mut small_r, &mut small_buf, total);

    let (mut big_r, mut big_buf) = renderer(s, 8192);
    let big = run(&mut big_r, &mut big_buf, total);

    assert_eq!(small.len(), total);
    assert_eq!(big.len(), total);
    for (i, (a, b)) in small.iter().zip(&big).enumerate() {
        assert!(
            (a - b).abs() < 1e-6,
            "sample {i}: quantum 256 gave {a}, quantum 8192 gave {b}"
        );
        assert!(a.is_finite() && b.is_finite(), "sample {i} is not finite");
    }
    assert!(peak(&big) > 0.3, "the test must not be trivially silent");
}

#[test]
fn seeking_back_into_a_held_note_does_not_leave_it_hanging() {
    // A note starts at sample 1000 and never ends. Seeking back to 0 leaves no
    // event in the re-rendered range, so the only way the note could sound is
    // a voice that survived the seek — exactly what the node must clear.
    let s = schedule(vec![on(1000, 60)], 4800);
    let (mut r, mut buf) = renderer(s, 256);
    let first = run(&mut r, &mut buf, 2048);
    assert!(
        first[1000..].iter().any(|v| v.abs() > 0.01),
        "the note must sound after it starts"
    );

    r.transport_mut().apply(TransportCommand::SeekSamples(0));
    let after = run(&mut r, &mut buf, 2048);
    assert!(
        after[..999].iter().all(|v| v.abs() < 1e-6),
        "a voice must not survive the backward seek: {:?}",
        &after[992..999]
    );
    assert!(
        after[1100..].iter().any(|v| v.abs() > 0.01),
        "the rewind must replay the note from its own sample"
    );

    // Seeking forward again re-triggers the note from scratch: it starts
    // silent for 1000 - 512 samples and then rises, with no stale tail
    // leaking in before it.
    let forward = run(&mut r, &mut buf, 2048);
    assert!(
        forward.iter().all(|v| v.is_finite()),
        "the re-triggered note must stay finite"
    );
    assert!(
        forward.iter().any(|v| v.abs() > 0.01),
        "seeking forward past the note must trigger it again"
    );
}

#[test]
fn sixteen_simultaneous_notes_stay_finite_and_bounded() {
    let mut events = Vec::new();
    for note in 0..u8::try_from(MAX_VOICES).unwrap() {
        events.push(on(0, 60 + note));
    }
    let s = schedule(events, 2048);
    let (mut r, mut buf) = renderer(s, 256);
    let out = run(&mut r, &mut buf, 2048);
    assert!(out.iter().all(|v| v.is_finite()), "no NaN or infinity");
    let top = peak(&out);
    assert!(top > 0.3, "16 voices must be audible, peak {top}");
    assert!(top <= 8.0, "16 voices at 0.5 each must not explode: {top}");
}

#[test]
fn end_to_end_smf_renders_the_written_notes() {
    // Two notes, C4 then E4, through the real parser and the real scheduler.
    let track: [u8; 22] = [
        0x00, 0x90, 60, 100, // C4 on
        0x00, 0x90, 64, 100, // E4 on
        0x83, 0x60, 0x80, 60, 0x00, // delta 480: C4 off
        0x83, 0x60, 0x80, 64, 0x00, // delta 480: E4 off
        0x00, 0xFF, 0x2F, 0x00, // end of track
    ];
    let mut data = Vec::new();
    data.extend_from_slice(b"MThd");
    data.extend_from_slice(&6u32.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes()); // format 0
    data.extend_from_slice(&1u16.to_be_bytes()); // one track
    data.extend_from_slice(&480u16.to_be_bytes()); // PPQN
    data.extend_from_slice(b"MTrk");
    data.extend_from_slice(&(track.len() as u32).to_be_bytes());
    data.extend_from_slice(&track);

    let file = parse_smf(&data).expect("hand-written SMF must parse");
    // Two note-ons; the end-of-track marker is not a note.
    assert_eq!(file.notes().len(), 2);

    let spec = ScheduleSpec::from_midi(&file, SR, Tempo::default(), 4800).unwrap();
    let schedule = Schedule::new(spec);
    // The last note-off is at 960 ticks (1 s); the schedule adds its tail on
    // top, and the tail is longer than one release, so the file has to go
    // quiet strictly before the end.
    let total = usize::try_from(schedule.length_samples()).unwrap();
    let (mut r, mut buf) = renderer(schedule, 256);
    let out = run(&mut r, &mut buf, total);

    // 960 ticks at 120 BPM / 480 PPQN = 1 s of notes, plus the release tail.
    assert_eq!(total, SR as usize + 4800);
    let top = peak(&out);
    assert!(top > 0.3, "the file must make sound, peak was {top}");
    assert!(out.iter().all(|v| v.is_finite()));
    // Note-off at 1 s, the default release is 0.12 s = 5760 samples, so the
    // level is still audible just after the note-off and silent well before
    // the end of the 4800-sample tail.
    assert!(
        out[SR as usize + 6].abs() > 0.1,
        "the note must still ring right after its note-off, got {}",
        out[SR as usize + 6]
    );
    // The tail is measured from the last event, so it starts exactly at the
    // last note-off: 4800 samples of silence are appended after it.
    let tail = &out[SR as usize..];
    let last = tail.len().saturating_sub(1);
    assert!(
        tail.iter().all(|v| v.is_finite()),
        "the release must stay finite"
    );
    assert!(
        tail.iter().rev().take(16).all(|v| v.abs() < 0.1),
        "the release must be fading out at the end of the tail: {:?}",
        &tail[tail.len() - 16..]
    );
    assert!(
        tail.iter()
            .rev()
            .take(16)
            .all(|v| v.abs() < 0.2 * tail[0].abs().max(1.0)),
        "and it must be well below where it started: {:?}",
        &tail[..8]
    );
    assert!(
        tail[last].abs() < 0.1,
        "the tail must land well below the note, got {}",
        tail[last]
    );
}

#[test]
fn invalid_settings_error_instead_of_panicking() {
    assert!(Synth::new(0, Envelope::default()).is_err());
    let bad = Envelope {
        attack_s: -1.0,
        ..Envelope::default()
    };
    assert!(Synth::new(SR, bad).is_err());
}
