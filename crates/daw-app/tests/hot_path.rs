//! Guards the realtime contract: rendering must not allocate
//! (SPEC-011 §1, `docs/REALTIME.md`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use audio_core::buffer::AudioBuffer;
use audio_core::graph::NodeProcessor;
use audio_core::params::{ParamHandle, ParamId, ParamSpec};
use audio_core::schedule::{Schedule, ScheduleSpec, ScheduledEvent};
use audio_core::session::{Clip, Session, Track as SessionTrack};
use audio_core::{Graph, MidiEvent, MidiMessage, Renderer, Transport, TransportCommand};
use daw_app::nodes::{ClipSource, MidiPlayer, MonoClip, Strip, Tone};
use daw_app::{SessionJob, TrackSource, build_session_graph};

/// Counts allocations made by the current thread while it is armed.
///
/// The counter is thread-local so that tests running in parallel do not see
/// each other's allocations. A `const`-initialised `Cell` has no destructor
/// and needs no lazy setup, so touching it from the allocator cannot recurse.
struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note_allocation() {
    if ARMED.with(Cell::get) {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `body` on this thread.
///
/// `body` runs once: setup must already be done, otherwise the counter would
/// report the setup's `Box::new`/`Arc::new` calls as render allocations.
fn allocations_of(body: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

const BLOCK: usize = 256;
const RATE: u32 = 48_000;
const QUANTA: usize = 1_000;

fn graph_with(node: impl NodeProcessor + Send + 'static) -> Graph {
    let mut graph = Graph::with_capacity(RATE, 1, BLOCK, 4, 0);
    graph.add_node("node", node).unwrap();
    graph
}

fn renderer_for(graph: Graph) -> Renderer {
    let mut transport = Transport::new(RATE);
    transport.apply(TransportCommand::Play);
    Renderer::new(graph, transport, BLOCK)
}

/// Renders `frames` quanta through `renderer`.
///
/// No warm-up: the very first `render` must be allocation-free too. Only
/// construction (`Renderer::new`, `Graph::with_capacity`) may allocate.
fn run_quanta(renderer: &mut Renderer, mix: &mut AudioBuffer, frames: usize) {
    for _ in 0..frames {
        renderer.render(BLOCK, mix).unwrap();
    }
}

fn param(name: &'static str) -> ParamHandle {
    ParamHandle::new(ParamSpec::new(ParamId(0), name, 0.0, 1_000_000.0, 0.0, 0).unwrap())
}

fn busy_schedule() -> Schedule {
    let events: Vec<ScheduledEvent> = (0..200_u64)
        .map(|i| ScheduledEvent {
            sample: i * 37,
            source: MidiEvent {
                tick: i * 37,
                track: 0,
                order_in_track: 0,
                message: MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            },
        })
        .collect();
    Schedule::new(ScheduleSpec {
        events,
        length_samples: 10_000,
    })
}

fn overlapping_track() -> SessionTrack {
    let mut track = SessionTrack::new("t");
    track.clips.push(Clip::full(0, 10_000));
    let mut second = Clip::full(5_000, 10_000);
    second.gain = 0.5;
    track.clips.push(second);
    track
}

fn sources() -> Vec<Box<dyn ClipSource + Send>> {
    vec![
        Box::new(MonoClip::sine(220.0, RATE, 10_000)),
        Box::new(MonoClip::sine(330.0, RATE, 10_000)),
    ]
}

#[test]
fn tone_renders_without_allocating() {
    let mut renderer = renderer_for(graph_with(Tone::new(440.0, param("level"))));
    let mut mix = AudioBuffer::new(1, BLOCK);
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "Tone::process allocated {count} times");
}

#[test]
fn midi_player_with_a_busy_schedule_does_not_allocate() {
    let mut renderer = renderer_for(graph_with(MidiPlayer::new(busy_schedule(), param("seen"))));
    let mut mix = AudioBuffer::new(1, BLOCK);
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "MidiPlayer::process allocated {count} times");
}

#[test]
fn strip_does_not_allocate() {
    let strip = Strip::new(Box::new(MonoClip::sine(220.0, RATE, 10_000)), 1_234, 0.5);
    let mut renderer = renderer_for(graph_with(strip));
    let mut mix = AudioBuffer::new(1, BLOCK);
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "Strip::process allocated {count} times");
}

#[test]
fn a_session_track_does_not_allocate() {
    let track = daw_app::nodes::Track::new(&overlapping_track(), sources()).unwrap();
    let mut renderer = renderer_for(graph_with(track));
    let mut mix = AudioBuffer::new(1, BLOCK);
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "Track::process allocated {count} times");
}

#[test]
fn a_two_track_session_graph_does_not_allocate() {
    let job = || {
        let mut session = Session::new("s", RATE, 120.0);
        session.tracks.push(overlapping_track());
        session.tracks.push(overlapping_track());
        SessionJob {
            session,
            tracks: vec![
                TrackSource {
                    track: overlapping_track(),
                    clips: sources(),
                },
                TrackSource {
                    track: overlapping_track(),
                    clips: sources(),
                },
            ],
            sample_rate: RATE,
            block_frames: BLOCK,
            frames: None,
            output: std::path::PathBuf::new(),
        }
    };
    // Everything above is built with the counter disarmed: sources, labels
    // and the graph itself are setup, not render.
    let mut renderer = renderer_for(build_session_graph(job()).unwrap());
    // Session graphs are stereo since SPEC-013.
    let mut mix = AudioBuffer::new(2, BLOCK);
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "session graph allocated {count} times");
}

#[test]
fn a_panned_soloed_stereo_session_does_not_allocate() {
    let mut left = overlapping_track();
    left.pan = -0.75;
    left.solo = true;
    let mut right = overlapping_track();
    right.pan = 0.4;
    let mut session = Session::new("s", RATE, 120.0);
    session.master_gain = 0.8;
    session.tracks.push(left.clone());
    session.tracks.push(right.clone());
    let job = SessionJob {
        session,
        tracks: vec![
            TrackSource {
                track: left,
                clips: sources(),
            },
            TrackSource {
                track: right,
                clips: sources(),
            },
        ],
        sample_rate: RATE,
        block_frames: BLOCK,
        frames: None,
        output: std::path::PathBuf::new(),
    };
    let mut renderer = renderer_for(build_session_graph(job).unwrap());
    let mut mix = AudioBuffer::new(2, BLOCK);
    // The clips end long before QUANTA blocks, so check the sound up front.
    renderer.render(BLOCK, &mut mix).unwrap();
    assert!(
        mix.channel(0).iter().any(|&s| s != 0.0),
        "solo track sounds"
    );
    let count = allocations_of(|| run_quanta(&mut renderer, &mut mix, QUANTA));
    assert_eq!(count, 0, "stereo mixer allocated {count} times");
}
