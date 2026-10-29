//! Project document integration tests (SPEC-008).
//!
//! A project is captured from a live [`Graph`], serialised, parsed back and
//! applied to another graph. The tests pin the round-trip, the determinism of
//! the text, and every row of the spec's edge-case table.

mod common;

use audio_core::graph::{Graph, NodeProcessor, NodeView, ProcessContext};
use audio_core::params::{ParamHandle, ParamId, ParamSpec};
use audio_core::{ParamState, Project, ProjectError};

const SR: u32 = 48_000;

/// Writes silence; only its parameter matters here.
struct Silent;

impl NodeProcessor for Silent {
    fn process(&mut self, ctx: &ProcessContext<'_>, output: &mut NodeView<'_>) {
        for ch in 0..output.channels() {
            output.channel_mut(ch)[..ctx.frames].fill(0.0);
        }
    }
}

fn add_silent(graph: &mut Graph) {
    graph.add_node("silent", Silent).unwrap();
}

fn level_spec() -> ParamSpec {
    ParamSpec::new(ParamId(0), "tone.level", 0.0, 1.0, 0.5, 10).unwrap()
}

fn demo_graph() -> (Graph, ParamHandle) {
    let mut graph = Graph::with_capacity(SR, 1, 256, 4, 4);
    let level = ParamHandle::new(level_spec());
    graph.register_param(level.clone()).unwrap();
    (graph, level)
}

fn many_param_graph(count: u32) -> Graph {
    let mut graph = Graph::with_capacity(SR, 1, 256, 4, usize::try_from(count).unwrap());
    for id in 0..count {
        let spec = ParamSpec::new(
            ParamId(id),
            "p",
            -1.0,
            1.0,
            0.0,
            id % 32, // exercise both "jump" (0) and ramped parameters
        )
        .unwrap();
        graph.register_param(ParamHandle::new(spec)).unwrap();
    }
    graph
}

fn minimal(name: &str) -> String {
    with_session(&format!(
        "{{\"name\": \"{name}\", \"sample_rate\": 48000, \"tempo_bpm\": 120, \"params\": []}}"
    ))
}

/// Appends an empty session to a document body built without one.
fn with_session(body: &str) -> String {
    let close = body.len() - 1;
    let mut out = String::with_capacity(body.len() + EMPTY_SESSION.len() + 16);
    out.push_str(&body[..close]);
    out.push_str(", \"session\": ");
    out.push_str(EMPTY_SESSION);
    out.push('}');
    out
}

/// Builds a document whose `params` array holds exactly `body`.
fn with_params(body: &str) -> String {
    with_session(&format!(
        r#"{{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": [{body}]}}"#
    ))
}

/// An empty session, as the document requires one (SPEC-010).
const EMPTY_SESSION: &str =
    r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "tracks": []}"#;

fn param_json(fields: &str) -> String {
    format!(
        "{{\"name\": \"x\", \"sample_rate\": 48000, \"tempo_bpm\": 120, \
         \"params\": [{{{fields}}}], \"session\": {EMPTY_SESSION}}}"
    )
}

#[test]
fn capture_round_trips_on_the_demo_graph() {
    let (mut graph, level) = demo_graph();
    add_silent(&mut graph);
    level.set(0.25);
    let project = Project::capture("demo", SR, 120.0, &graph);

    let text = project.to_json();
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back, project);
    assert_eq!(back.params.len(), 1);
    assert_eq!(back.params[0].name, "tone.level");
    assert_eq!(back.params[0].smoothing_ms, 10);
    assert_eq!(back.params[0].default.to_bits(), 0.5_f32.to_bits());
    assert_eq!(back.params[0].max.to_bits(), 1.0_f32.to_bits());
    assert_eq!(back.params[0].value.to_bits(), 0.25_f32.to_bits());
}

#[test]
fn capture_round_trips_with_sixty_four_parameters() {
    let graph = many_param_graph(64);
    for id in 0..64 {
        let handle = graph.param(ParamId(id)).unwrap();
        // Values that are not exactly representable in decimal.
        #[allow(clippy::cast_precision_loss)]
        let scaled = id as f32;
        handle.set(scaled / 7.0);
    }
    let project = Project::capture("many", SR, 128.5, &graph);
    assert_eq!(project.params.len(), 64);

    let back = Project::from_json(&project.to_json()).unwrap();
    assert_eq!(back, project);
    for (before, after) in project.params.iter().zip(&back.params) {
        assert_eq!(
            before.value.to_bits(),
            after.value.to_bits(),
            "{}",
            before.id
        );
    }
}

#[test]
fn to_json_is_deterministic_and_field_order_is_fixed() {
    let graph = many_param_graph(8);
    let project = Project::capture("order", SR, 90.0, &graph);

    assert_eq!(project.to_json(), project.to_json());
    assert_eq!(
        project.to_json(),
        Project::from_json(&project.to_json()).unwrap().to_json()
    );

    let text = project.to_json();
    let keys = [
        "\"name\"",
        "\"sample_rate\"",
        "\"tempo_bpm\"",
        "\"params\"",
        "\"id\"",
        "\"name\"",
        "\"min\"",
        "\"max\"",
        "\"default\"",
        "\"smoothing_ms\"",
        "\"value\"",
    ];
    let mut cursor = 0;
    for key in keys {
        let found = text[cursor..]
            .find(key)
            .unwrap_or_else(|| panic!("{key} missing after byte {cursor}"));
        cursor += found + key.len();
    }
}

#[test]
fn apply_to_changes_live_parameters() {
    let (source, level) = demo_graph();
    level.set(0.75);
    let project = Project::capture("applied", SR, 120.0, &source);

    let (mut target, target_level) = demo_graph();
    add_silent(&mut target);
    assert_eq!(target_level.value().to_bits(), 0.5_f32.to_bits());

    let reloaded = Project::from_json(&project.to_json()).unwrap();
    reloaded.apply_to(&target).unwrap();
    assert_eq!(target_level.value().to_bits(), 0.75_f32.to_bits());
}

#[test]
fn apply_to_rejects_a_parameter_this_build_does_not_have() {
    let (mut target, _level) = demo_graph();
    add_silent(&mut target);
    let project = Project::from_json(&param_json(
        "\"id\": 3, \"name\": \"ghost\", \"min\": 0, \"max\": 1, \
         \"default\": 0, \"smoothing_ms\": 0, \"value\": 0.5",
    ))
    .unwrap();
    assert_eq!(
        project.apply_to(&target).unwrap_err(),
        ProjectError::UnknownParam { id: 3 }
    );
}

#[test]
fn apply_to_leaves_the_session_untouched_when_one_id_is_unknown() {
    let (mut target, level) = demo_graph();
    add_silent(&mut target);
    // Id 0 exists, id 9 does not: id 0 must not be applied half-way.
    let text = with_params(
        r#"{"id": 0, "name": "tone.level", "min": 0, "max": 1, "default": 0.5,
         "smoothing_ms": 10, "value": 1},
        {"id": 9, "name": "ghost", "min": 0, "max": 1, "default": 0,
         "smoothing_ms": 0, "value": 0}"#,
    );
    let project = Project::from_json(&text).unwrap();
    assert_eq!(
        project.apply_to(&target).unwrap_err(),
        ProjectError::UnknownParam { id: 9 }
    );
    assert_eq!(level.value().to_bits(), 0.5_f32.to_bits());
}

#[test]
fn empty_string_is_malformed_at_zero() {
    assert_eq!(
        Project::from_json("").unwrap_err(),
        ProjectError::Malformed { at: 0 }
    );
}

#[test]
fn empty_object_reports_the_first_missing_field() {
    assert_eq!(
        Project::from_json("{}").unwrap_err(),
        ProjectError::MissingField { field: "name" }
    );
}

#[test]
fn a_typo_in_a_key_is_an_unknown_field() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempoBPM": 120, "params": []}"#);
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "tempoBPM".to_owned()
        }
    );
}

#[test]
fn null_is_unsupported() {
    let text =
        with_session(r#"{"name": null, "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Unsupported { .. }
    ));
}

#[test]
fn exponents_are_unsupported() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 1e3, "params": []}"#);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Unsupported { .. }
    ));
}

#[test]
fn deep_nesting_is_unsupported() {
    // Well past the parser's depth limit, inside the `name` position.
    let nesting = format!("{}{}", "[".repeat(64), "]".repeat(64));
    let text = with_session(&format!(
        r#"{{"name": {nesting}, "sample_rate": 48000, "tempo_bpm": 120, "params": []}}"#
    ));
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Unsupported { .. }
    ));
}

#[test]
fn an_unterminated_string_is_malformed() {
    let text =
        with_session(r#"{"name": "x, "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

#[test]
fn a_negative_sample_rate_is_malformed() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": -48000, "tempo_bpm": 120, "params": []}"#);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

#[test]
fn a_value_outside_its_range_is_rejected() {
    let project = Project::from_json(&param_json(
        "\"id\": 3, \"name\": \"x\", \"min\": 0, \"max\": 1, \
         \"default\": 0, \"smoothing_ms\": 0, \"value\": 2",
    ))
    .unwrap_err();
    assert_eq!(project, ProjectError::InvalidParam { id: 3 });
}

#[test]
fn duplicate_ids_are_rejected() {
    let text = with_params(
        r#"{"id": 3, "name": "a", "min": 0, "max": 1, "default": 0,
         "smoothing_ms": 0, "value": 0},
        {"id": 3, "name": "b", "min": 0, "max": 1, "default": 0,
         "smoothing_ms": 0, "value": 0}"#,
    );
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::DuplicateParam { id: 3 }
    );
}

#[test]
fn zero_tempo_is_rejected() {
    let text = with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 0, "params": []}"#);
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::InvalidTempo
    );
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": -1, "params": []}"#);
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::InvalidTempo
    );
}

#[test]
fn unicode_escapes_are_unsupported() {
    let text = with_session(
        r#"{"name": "a\u0041", "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#,
    );
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Unsupported { .. }
    ));
}

#[test]
fn a_too_long_name_is_rejected() {
    let long = "a".repeat(audio_core::MAX_NAME_BYTES + 1);
    assert_eq!(
        Project::from_json(&minimal(&long)).unwrap_err(),
        ProjectError::NameTooLong
    );
    // Exactly at the limit is fine.
    let at_limit = "a".repeat(audio_core::MAX_NAME_BYTES);
    assert_eq!(
        Project::from_json(&minimal(&at_limit)).unwrap().name,
        at_limit
    );
}

#[test]
fn zero_sample_rate_is_rejected() {
    let text = with_session(r#"{"name": "x", "sample_rate": 0, "tempo_bpm": 120, "params": []}"#);
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::InvalidSampleRate
    );
}

#[test]
fn a_hand_written_file_parses() {
    let text = r#"
        {
          "name": "hand",
          "sample_rate": 44100,
          "tempo_bpm": 96.5,
          "params": [
            {
              "id": 0,
              "name": "tone.level",
              "min": 0,
              "max": 1,
              "default": 0.5,
              "smoothing_ms": 10,
              "value": 0.125
            }
          ],
          "session": {
            "name": "hand",
            "sample_rate": 44100,
            "tempo_bpm": 96.5,
            "tracks": []
          }
        }
    "#;
    let project = Project::from_json(text).unwrap();
    assert_eq!(
        project,
        Project {
            name: "hand".to_owned(),
            sample_rate: 44_100,
            tempo_bpm: 96.5,
            params: vec![ParamState {
                id: 0,
                name: "tone.level".to_owned(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                smoothing_ms: 10,
                value: 0.125,
            }],
            session: audio_core::Session::new("hand", 44_100, 96.5),
        }
    );
}

#[test]
fn awkward_floats_survive_the_text_format_exactly() {
    // `Display` falls back to an exponent for these; the writer must not.
    let graphemes = [
        f64::from_bits(1), // smallest subnormal
        0.1_f64 + 0.2,
        1.0 / 3.0,
        1e-7,
        1e21,
        f64::MAX,
    ];
    for value in graphemes {
        let project = Project {
            name: "floats".to_owned(),
            sample_rate: SR,
            tempo_bpm: value,
            params: Vec::new(),
            session: audio_core::Session::new("x", SR, value),
        };
        let text = project.to_json();
        // The writer relies on `Display` never using an exponent; check the
        // number itself, not the whole document (parameter names are free text).
        let written = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("\"tempo_bpm\": "))
            .expect("tempo_bpm line");
        let written = written.trim_end_matches(',');
        assert!(
            !written.contains(['e', 'E']),
            "{value} was written with an exponent: {written}"
        );
        let back = Project::from_json(&text).unwrap();
        assert_eq!(back.tempo_bpm.to_bits(), value.to_bits(), "{value}");
    }
}

#[test]
fn hostile_input_never_panics_and_stays_fast() {
    let mut seed = 0x0bad_c0de_dead_beef_u64;
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
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a megabyte of garbage took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_plausible_header_with_garbage_and_deep_lists_is_rejected() {
    // Valid prefix, then a megabyte of nesting: the depth guard, not the
    // length, must stop it.
    let mut text =
        String::from(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": ["#);
    text.push_str(&"[".repeat(1024 * 1024));
    let started = std::time::Instant::now();
    assert!(Project::from_json(&text).is_err());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a hand-written session with 4_000 tracks took {:?}",
        started.elapsed()
    );
}

// ---- SPEC-010: the session embedded in the document ----

fn demo_session() -> audio_core::Session {
    let mut clip = audio_core::Clip::full(240, 96_000);
    clip.name = "take 1".to_owned();
    clip.offset_samples = 120;
    clip.length_samples = 48_000;
    clip.gain = 0.75;
    let mut track = audio_core::SessionTrack::new("lead");
    track.gain = 0.5;
    track.clips.push(clip);
    let mut session = audio_core::Session::new("liners", SR, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn capture_stores_an_empty_session_named_after_the_project() {
    let (graph, _) = demo_graph();
    let project = Project::capture("demo", SR, 120.0, &graph);
    let session = project.session();
    assert_eq!(session.name, "untitled");
    assert_eq!(session.sample_rate, SR);
    assert_eq!(session.tracks.len(), 0);
    assert_eq!(session.length_samples(), 0);
}

#[test]
fn a_session_survives_the_round_trip() {
    let (graph, _) = demo_graph();
    let project =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session());
    let text = project.to_json();
    assert_eq!(Project::from_json(&text).unwrap(), project);
    assert_eq!(text.matches("\"session\"").count(), 1);
    let session_at = text.find("\"session\"").unwrap();
    assert!(
        session_at > text.find("\"params\"").unwrap(),
        "session is last"
    );
    assert!(text.ends_with("\n}\n"));
}

#[test]
fn session_field_order_is_fixed() {
    let (graph, _) = demo_graph();
    let project =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session());
    let text = project.to_json();
    let after = &text[text.find("\"session\"").unwrap()..];
    let positions: Vec<usize> = [
        "\"tracks\"",
        "\"clips\"",
        "\"start\": ",
        "\"length\"",
        "\"offset\"",
        "\"source_length\"",
        "\"gain\": 0.75", // the clip's, not the track's 0.5 that comes earlier
        "\"name\": \"take 1\"",
    ]
    .iter()
    .map(|needle| after.find(needle).unwrap())
    .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{positions:?}"
    );
}

#[test]
fn a_missing_session_key_is_named() {
    let (graph, _) = demo_graph();
    let text = Project::capture("demo", SR, 120.0, &graph)
        .to_json()
        .replace("\"session\"", "\"sesion\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "sesion".to_owned()
        }
    );
}

#[test]
fn an_unknown_key_inside_a_track_is_rejected() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json()
        .replace("\"mute\": false", "\"muted\": false");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "muted".to_owned()
        }
    );
}

#[test]
fn an_invalid_session_is_reported_as_a_session_error() {
    use audio_core::SessionError;
    let (graph, _) = demo_graph();
    let mut session = demo_session();
    session.tracks[0].clips[0].offset_samples = 96_001;
    let text =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), session).to_json();
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(SessionError::ClipOutOfSource { track: 0, clip: 0 })
    );
}

#[test]
fn an_unknown_key_inside_a_clip_is_rejected() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json()
        .replace("\"source_length\"", "\"source\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "source".to_owned()
        }
    );
}

#[test]
fn an_empty_session_object_reports_the_nested_field() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#)
            .replace(EMPTY_SESSION, "{}");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::MissingField {
            field: "session.name"
        }
    );
}

#[test]
fn session_numbers_must_be_integers_in_range() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#)
            .replace(
                EMPTY_SESSION,
                r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "tracks": [
                 {"name": "t", "gain": 1, "mute": false, "clips": [
                   {"start": 0.5, "length": 1, "offset": 0,
                    "source_length": 1, "gain": 1, "name": ""}]}]}"#,
            );
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

#[test]
fn mute_is_a_boolean() {
    let text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#)
            .replace(
                EMPTY_SESSION,
                r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "tracks": [
                 {"name": "t", "gain": 1, "mute": 1, "clips": []}]}"#,
            );
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

#[test]
fn a_long_hand_written_session_is_still_rejected_fast() {
    let mut text =
        with_session(r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": []}"#);
    // 4_000 tracks of ~60 bytes: far past the 32-track limit and big enough
    // that a per-entry quadratic pass could not finish in a second. The parser
    // must stop as soon as the track array proves too long, not after decoding
    // all of it (`SessionError::TooManyTracks`).
    use audio_core::SessionError;
    let track = r#", {"name": "t", "gain": 1, "mute": false, "clips": []}"#.repeat(4_000);
    text = text.replace(
        r#""tracks": []"#,
        &format!(r#""tracks": [{{"name": "t"}} {track}]"#),
    );
    let started = std::time::Instant::now();
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(SessionError::TooManyTracks { .. })
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a hand-written session with 4_000 tracks took {:?}",
        started.elapsed()
    );
}

// ---- SPEC-012: parsing time is linear in the input size ----

const ONE_MB: usize = 1024 * 1024;

fn assert_fast(what: &str, text: &str) -> ProjectError {
    let started = std::time::Instant::now();
    let error = Project::from_json(text).unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "{what} took {:?}",
        started.elapsed()
    );
    error
}

#[test]
fn a_megabyte_ascii_name_is_rejected_fast() {
    let text = format!(
        r#"{{"name": "{}", "sample_rate": 48000}}"#,
        "a".repeat(ONE_MB)
    );
    let error = assert_fast("a 1 MB ASCII name", &text);
    assert_eq!(error, ProjectError::MissingField { field: "tempo_bpm" });
}

#[test]
fn a_megabyte_cyrillic_name_is_rejected_fast() {
    // Two bytes per letter: ~1 MB of multi-byte UTF-8.
    let text = format!(
        r#"{{"name": "{}", "sample_rate": 48000}}"#,
        "ж".repeat(ONE_MB / 2)
    );
    let error = assert_fast("a 1 MB Cyrillic name", &text);
    assert_eq!(error, ProjectError::MissingField { field: "tempo_bpm" });
}

#[test]
fn a_megabyte_unterminated_string_is_malformed_fast() {
    let text = format!(r#"{{"name": "{}"#, "a".repeat(ONE_MB));
    let error = assert_fast("a 1 MB unterminated string", &text);
    assert_eq!(error, ProjectError::Malformed { at: text.len() });
}

#[test]
fn a_megabyte_array_of_numbers_is_rejected_fast() {
    let text = format!(r#"{{"params": [{}1]}}"#, "1, ".repeat(ONE_MB / 3));
    let error = assert_fast("a 1 MB array of numbers", &text);
    assert!(matches!(error, ProjectError::Malformed { .. }), "{error:?}");
}

#[test]
fn multi_byte_names_round_trip_unchanged() {
    let mut project = Project::from_json(
        r#"{"name": "x", "sample_rate": 48000, "tempo_bpm": 120, "params": [],
            "session": {"name": "s", "sample_rate": 48000, "tempo_bpm": 120, "tracks": []}}"#,
    )
    .unwrap();
    project.name = "Песня №1 — 日本 🎹".to_owned();
    let back = Project::from_json(&project.to_json()).unwrap();
    assert_eq!(back, project);
}
