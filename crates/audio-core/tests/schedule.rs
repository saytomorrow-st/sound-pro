//! End-to-end checks for the MIDI event scheduler (SPEC-007).
//!
//! The unit tests in `audio_core::schedule` cover the tick → sample formula;
//! these drive a real [`Renderer`] so the node-level contract (quantum ranges,
//! seek behaviour, determinism) is exercised the way the audio thread sees it.

use audio_core::TransportCommand;
use audio_core::buffer::AudioBuffer;
use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::midi::MidiMessage;
use audio_core::params::{ParamHandle, ParamId, ParamSpec};
use audio_core::render::Renderer;
use audio_core::schedule::{Schedule, ScheduleCursor, ScheduleSpec, ScheduledEvent};
use audio_core::transport::{DEFAULT_SAMPLE_RATE, Tempo, Transport};

const SR: u32 = DEFAULT_SAMPLE_RATE;
const QUANTUM: usize = 256;

fn note_on(note: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        note,
        velocity: 100,
    }
}

fn event(sample: u64) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        source: audio_core::midi::MidiEvent {
            track: 0,
            tick: 0,
            order_in_track: 0,
            message: note_on(60),
        },
    }
}

fn schedule(samples: &[u64]) -> Schedule {
    let events: Vec<ScheduledEvent> = samples.iter().copied().map(event).collect();
    let length = events.last().map_or(0, |e| e.sample);
    Schedule::new(ScheduleSpec {
        events,
        length_samples: length,
    })
}

#[test]
fn range_is_half_open_at_the_trailing_edge() {
    let s = schedule(&[QUANTUM as u64, QUANTUM as u64 + 1]);
    let mut cursor = s.cursor();
    // An event exactly at `position + frames` belongs to the next quantum.
    assert!(s.next_event(&mut cursor, 0, QUANTUM).is_none());
    // Both events from the *next* quantum's point of view.
    let next = QUANTUM as u64;
    let first = s.next_event(&mut cursor, next, QUANTUM).expect("in range");
    assert_eq!(first.sample, next);
    let second = s.next_event(&mut cursor, next, QUANTUM).expect("in range");
    assert_eq!(second.sample, next + 1);
    assert!(s.next_event(&mut cursor, next, QUANTUM).is_none());
}

#[test]
fn an_event_exactly_at_position_is_included() {
    let s = schedule(&[1024]);
    let mut cursor = s.cursor();
    let found = s.next_event(&mut cursor, 1024, QUANTUM).expect("in range");
    assert_eq!(found.sample, 1024);
}

#[test]
fn zero_frames_returns_nothing_and_keeps_the_cursor() {
    let s = schedule(&[0]);
    let mut cursor = s.cursor();
    assert!(s.next_event(&mut cursor, 0, 0).is_none());
    // The cursor must still point at the very first event.
    let found = s.next_event(&mut cursor, 0, 1).expect("in range");
    assert_eq!(found.sample, 0);
}

#[test]
fn skipping_ahead_consumes_the_events_behind_the_position() {
    let s = schedule(&[0, 100, 200, 300]);
    let mut cursor = s.cursor();
    // A forward seek (or an event missed by a long quantum) skips silently.
    let found = s.next_event(&mut cursor, 250, QUANTUM).expect("in range");
    assert_eq!(found.sample, 300, "everything below 250 is gone");
    assert!(s.next_event(&mut cursor, 250, QUANTUM).is_none());
}

#[test]
fn default_cursor_is_the_beginning() {
    let s = schedule(&[0, 10]);
    let mut cursor = ScheduleCursor::default();
    assert_eq!(s.next_event(&mut cursor, 0, 1).map(|e| e.sample), Some(0));
    assert_eq!(s.next_event(&mut cursor, 0, 11).map(|e| e.sample), Some(10));
}

/// Counts events into an unsmoothed parameter — the same shape as
/// `daw_app::nodes::MidiPlayer`, but local so the core stays free of shell code.
struct CounterNode {
    schedule: Schedule,
    cursor: ScheduleCursor,
    last_position: u64,
    events_seen: ParamHandle,
}

impl CounterNode {
    fn new(schedule: Schedule) -> (Self, ParamHandle) {
        let spec = ParamSpec::unsmoothed(ParamId(0), "seen", 0.0, 1_000_000.0, 0.0).unwrap();
        let handle = ParamHandle::new(spec);
        (
            Self {
                cursor: schedule.cursor(),
                schedule,
                last_position: 0,
                events_seen: handle.clone(),
            },
            handle,
        )
    }
}

impl NodeProcessor for CounterNode {
    fn process(&mut self, ctx: &ProcessContext<'_>, _output: &mut NodeView<'_>) {
        if ctx.position < self.last_position {
            self.cursor = self.schedule.cursor();
        }
        self.last_position = ctx.position;
        let mut seen = self.events_seen.value();
        while self
            .schedule
            .next_event(&mut self.cursor, ctx.position, ctx.frames)
            .is_some()
        {
            seen += 1.0;
        }
        self.events_seen.set(seen);
    }
}

fn counter_renderer(schedule: Schedule) -> (Renderer, ParamHandle) {
    let mut graph = Graph::with_capacity(SR, 1, QUANTUM, 2, 1);
    let (node, handle) = CounterNode::new(schedule);
    graph.register_param(node.events_seen.clone()).unwrap();
    graph.add_node("counter", node).unwrap();
    let mut renderer = Renderer::new(graph, Transport::new(SR), QUANTUM);
    renderer.transport_mut().apply(TransportCommand::Play);
    (renderer, handle)
}

fn run_quanta(renderer: &mut Renderer, quanta: usize, buffer: &mut AudioBuffer) {
    for _ in 0..quanta {
        renderer.render(QUANTUM, buffer).unwrap();
    }
}

#[test]
fn playing_forward_counts_every_event_once() {
    let samples: Vec<u64> = (0..40_u64).map(|i| i * 64).collect();
    let total = samples.len();
    let (mut renderer, seen) = counter_renderer(schedule(&samples));
    let mut buffer = AudioBuffer::new(1, QUANTUM);
    // 40 events spread over 2560 frames: ten quanta of 256 covers them all.
    run_quanta(&mut renderer, 10, &mut buffer);
    assert_eq!(seen.value(), total as f32);
    // The player must stay silent: it is an event source, not an instrument.
    run_quanta(&mut renderer, 5, &mut buffer);
    assert_eq!(seen.value(), total as f32, "no event is counted twice");
}

#[test]
fn seeking_backwards_replays_the_events_in_order() {
    let samples: Vec<u64> = vec![0, 100, 200, 300];
    let (mut renderer, seen) = counter_renderer(schedule(&samples));
    let mut buffer = AudioBuffer::new(1, QUANTUM);
    run_quanta(&mut renderer, 2, &mut buffer);
    assert_eq!(seen.value(), 4.0, "all four events were played");

    // Backwards: the node resets the cursor, so the same events play again.
    renderer
        .transport_mut()
        .apply(TransportCommand::SeekSamples(0));
    run_quanta(&mut renderer, 2, &mut buffer);
    assert_eq!(seen.value(), 8.0, "replayed once, in order");
}

#[test]
fn seeking_forward_does_not_count_the_skipped_events() {
    let samples: Vec<u64> = vec![0, 100, 200, 300, 400, 500];
    let (mut renderer, seen) = counter_renderer(schedule(&samples));
    let mut buffer = AudioBuffer::new(1, QUANTUM);
    renderer
        .transport_mut()
        .apply(TransportCommand::SeekSamples(320));
    run_quanta(&mut renderer, 1, &mut buffer);
    // Only the events at 400 and 500 remain; 0..=300 are behind the position.
    assert_eq!(seen.value(), 2.0);
}

#[test]
fn hundred_thousand_events_are_deterministic_bit_for_bit() {
    let samples: Vec<u64> = (0..100_000_u64).map(|i| i * 3).collect();
    let run = || {
        let (mut renderer, seen) = counter_renderer(schedule(&samples));
        let mut buffer = AudioBuffer::new(1, QUANTUM);
        run_quanta(&mut renderer, 1000, &mut buffer);
        seen.value().to_bits()
    };
    let first = run();
    let second = run();
    assert_eq!(first, second, "the scheduler must be bit-exact");
    // 1000 quanta cover 256 000 frames, so every third sample up to that point
    // is seen: 256 000 / 3 = 85 333 events.
    assert_eq!(f32::from_bits(first), 85_334.0);
}

#[test]
fn a_parsed_midi_file_becomes_a_playable_schedule() {
    // One track, 480 ppqn, 120 bpm: note on at tick 0, note off at tick 480.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"MThd");
    bytes.extend_from_slice(&6_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&480_u16.to_be_bytes());
    bytes.extend_from_slice(b"MTrk");
    let track: [u8; 13] = [
        0x00, 0x90, 0x3C, 0x64, // note on C4 at tick 0
        0x83, 0x60, 0x80, 0x3C, 0x00, // note off C4 at tick 480
        0x00, 0xFF, 0x2F, 0x00, // end of track
    ];
    bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&track);

    let file = audio_core::midi::parse_smf(&bytes).expect("valid SMF");
    let spec = ScheduleSpec::from_midi(&file, SR, Tempo(120.0), 4800).unwrap();
    let s = Schedule::new(spec);
    assert_eq!(s.len(), 2);
    let notes: Vec<u64> = s.events().iter().map(|e| e.sample).collect();
    // 120 bpm, 480 ppqn, 48 kHz: one beat is 24 000 frames.
    assert_eq!(notes, vec![0, 24_000]);
    assert_eq!(s.length_samples(), 24_000 + 4800);
    assert!(
        s.events()
            .iter()
            .all(|e| !matches!(e.source.message, MidiMessage::Tempo { .. }))
    );
}
