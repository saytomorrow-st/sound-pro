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
        .replace("\"source_length\"", "\"src_length\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "src_length".to_owned()
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

#[test]
fn mixer_fields_survive_the_round_trip_in_order() {
    let (graph, _) = demo_graph();
    let mut session = demo_session();
    session.master_gain = 0.5;
    session.tracks[0].pan = -0.25;
    session.tracks[0].solo = true;
    let project = Project::with_session(Project::capture("demo", SR, 120.0, &graph), session);
    let text = project.to_json();
    assert_eq!(Project::from_json(&text).unwrap(), project);
    let after = &text[text.find("\"session\"").unwrap()..];
    let positions: Vec<usize> = [
        "\"tempo_bpm\"",
        "\"master_gain\": 0.5",
        "\"tracks\"",
        "\"gain\": 0.5",
        "\"pan\": -0.25",
        "\"mute\": false",
        "\"solo\": true",
        "\"clips\"",
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
fn documents_without_mixer_fields_load_with_neutral_defaults() {
    let (graph, _) = demo_graph();
    let project =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session());
    let text = project
        .to_json()
        .replace("    \"master_gain\": 1,\n", "")
        .replace("        \"pan\": 0,\n", "")
        .replace("        \"solo\": false,\n", "");
    assert!(!text.contains("master_gain") && !text.contains("\"pan\"") && !text.contains("solo"));
    assert_eq!(Project::from_json(&text).unwrap(), project);
}

#[test]
fn mixer_fields_of_the_wrong_type_are_malformed() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    for (from, to) in [
        ("\"solo\": false", "\"solo\": 1"),
        ("\"pan\": 0", "\"pan\": \"x\""),
        ("\"master_gain\": 1", "\"master_gain\": true"),
    ] {
        assert!(
            matches!(
                Project::from_json(&text.replace(from, to)).unwrap_err(),
                ProjectError::Malformed { .. }
            ),
            "{to}"
        );
    }
}

#[test]
fn mixer_values_out_of_range_are_session_errors() {
    use audio_core::SessionError;
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert_eq!(
        Project::from_json(&text.replace("\"pan\": 0", "\"pan\": 1.5")).unwrap_err(),
        ProjectError::Session(SessionError::InvalidPan { track: 0 })
    );
    assert_eq!(
        Project::from_json(&text.replace("\"master_gain\": 1", "\"master_gain\": 2")).unwrap_err(),
        ProjectError::Session(SessionError::InvalidMasterGain)
    );
}

// --- SPEC-014: clip source paths -------------------------------------------

fn sourced_session() -> audio_core::Session {
    let mut session = demo_session();
    session.tracks[0].clips[0].source = Some("audio/take 1 — №1.wav".to_owned());
    session
}

#[test]
fn a_clip_source_survives_the_round_trip_as_the_last_clip_key() {
    let (graph, _) = demo_graph();
    let project = Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        sourced_session(),
    );
    let text = project.to_json();
    assert_eq!(Project::from_json(&text).unwrap(), project);
    assert!(
        text.contains(
            "\"name\": \"take 1\",\n            \"source\": \"audio/take 1 — №1.wav\"\n          }"
        ),
        "{text}"
    );
}

#[test]
fn documents_without_a_source_key_load_and_save_unchanged() {
    let (graph, _) = demo_graph();
    let project =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session());
    let text = project.to_json();
    assert!(!text.contains("\"source\""));
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session().tracks[0].clips[0].source, None);
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_bad_source_path_is_rejected() {
    use audio_core::{MAX_SOURCE_BYTES, SessionError};
    let (graph, _) = demo_graph();
    let text = Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        sourced_session(),
    )
    .to_json();
    let invalid = ProjectError::Session(SessionError::InvalidSource { track: 0, clip: 0 });
    assert_eq!(
        Project::from_json(&text.replace("audio/take 1 — №1.wav", "")).unwrap_err(),
        invalid
    );
    let long = "a".repeat(MAX_SOURCE_BYTES + 1);
    assert_eq!(
        Project::from_json(&text.replace("audio/take 1 — №1.wav", &long)).unwrap_err(),
        invalid
    );
    let exact = "a".repeat(MAX_SOURCE_BYTES);
    assert!(Project::from_json(&text.replace("audio/take 1 — №1.wav", &exact)).is_ok());
    assert!(matches!(
        Project::from_json(&text.replace("\"audio/take 1 — №1.wav\"", "5")).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

// --- SPEC-022: track effects ---------------------------------------------

fn effects_session() -> audio_core::Session {
    use audio_core::{Effect, EffectKind};
    let mut session = demo_session();
    session.tracks[0].effects = vec![
        Effect::new(EffectKind::Eq {
            low_db: 3.5,
            mid_db: -6.0,
            mid_hz: 1250.0,
            high_db: 2.0,
        }),
        Effect {
            bypass: true,
            kind: EffectKind::compressor(),
        },
        Effect::new(EffectKind::reverb()),
    ];
    session
}

fn effects_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        effects_session(),
    )
    .to_json()
}

#[test]
fn effects_survive_the_round_trip() {
    let text = effects_json();
    assert!(text.contains("\"effects\""), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &effects_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_track_without_effects_writes_no_effects_key() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("effects"), "{text}");
    assert!(
        Project::from_json(&text).unwrap().session().tracks[0]
            .effects
            .is_empty()
    );
}

#[test]
fn bypass_is_optional_when_reading() {
    let text = effects_json().replace("\"bypass\": false, ", "");
    let back = Project::from_json(&text).unwrap();
    assert!(!back.session().tracks[0].effects[0].bypass);
    assert!(back.session().tracks[0].effects[1].bypass);
}

#[test]
fn an_unknown_effect_type_is_rejected() {
    let text = effects_json().replace("\"reverb\"", "\"flanger\"");
    assert!(Project::from_json(&text).is_err());
}

#[test]
fn a_parameter_of_another_effect_type_is_an_unknown_field() {
    let text = effects_json().replace("\"room\"", "\"ratio\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "ratio".to_owned()
        }
    );
}

#[test]
fn a_missing_effect_parameter_is_reported() {
    let text = effects_json().replace(", \"mix\": 0.25", "");
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::MissingField { .. }
    ));
}

#[test]
fn an_out_of_range_effect_parameter_fails_validation() {
    let text = effects_json().replace("\"ratio\": 4", "\"ratio\": 0.5");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidEffect {
            track: 0,
            effect: 1
        })
    );
}

#[test]
fn too_many_effects_are_rejected_before_decoding() {
    let one = r#"{"type": "nonsense"}"#;
    let list = [one; audio_core::MAX_EFFECTS_PER_TRACK + 1].join(", ");
    let text = effects_json();
    let start = text.find("\"effects\": [").unwrap();
    let end = start + text[start..].find(']').unwrap();
    let text = format!("{}\"effects\": [{list}{}", &text[..start], &text[end..]);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidEffect { effect, .. })
            if effect == audio_core::MAX_EFFECTS_PER_TRACK
    ));
}

// --- SPEC-023: automation ------------------------------------------------

fn automation_session() -> audio_core::Session {
    use audio_core::AutoPoint;
    let mut session = demo_session();
    session.tracks[0].automation.gain = vec![
        AutoPoint {
            time: 0,
            value: 0.0,
        },
        AutoPoint {
            time: 48_000,
            value: 0.75,
        },
    ];
    session.tracks[0].automation.pan = vec![AutoPoint {
        time: 100,
        value: -0.5,
    }];
    session
}

fn automation_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        automation_session(),
    )
    .to_json()
}

#[test]
fn automation_survives_the_round_trip() {
    let text = automation_json();
    assert!(text.contains("\"automation\""), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &automation_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_track_without_automation_writes_no_automation_key() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("automation"), "{text}");
    assert!(
        Project::from_json(&text).unwrap().session().tracks[0]
            .automation
            .is_empty()
    );
}

#[test]
fn an_empty_lane_is_not_written() {
    let mut session = automation_session();
    session.tracks[0].automation.pan.clear();
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), session.clone())
        .to_json();
    assert!(text.contains("\"gain\": [{"), "{text}");
    assert!(!text.contains("\"pan\": [{"), "{text}");
    assert_eq!(Project::from_json(&text).unwrap().session(), &session);
}

#[test]
fn an_unknown_automation_lane_is_rejected() {
    let text = automation_json().replace("\"pan\": [{", "\"tempo\": [{");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "tempo".to_owned()
        }
    );
}

#[test]
fn an_unknown_key_inside_a_point_is_rejected() {
    let text = automation_json().replace("\"value\": -0.5", "\"curve\": -0.5");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "curve".to_owned()
        }
    );
}

#[test]
fn a_point_without_a_value_is_reported() {
    let text = automation_json().replace("{\"time\": 100, \"value\": -0.5}", "{\"time\": 100}");
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::MissingField { .. }
    ));
}

#[test]
fn a_fractional_point_time_is_malformed() {
    let text = automation_json().replace("\"time\": 100", "\"time\": 100.5");
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
}

#[test]
fn an_out_of_range_point_fails_validation() {
    let text = automation_json().replace("\"value\": -0.5", "\"value\": -3");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidAutomation {
            track: 0,
            lane: audio_core::Lane::Pan,
            point: 0
        })
    );
}

#[test]
fn too_many_points_are_rejected_before_decoding() {
    let list = ["\"junk\""; audio_core::MAX_AUTOMATION_POINTS + 1].join(", ");
    let text = automation_json();
    let start = text.find("\"pan\": [").unwrap();
    let end = start + text[start..].find(']').unwrap();
    let text = format!("{}\"pan\": [{list}{}", &text[..start], &text[end..]);
    assert!(matches!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidAutomation { point, .. })
            if point == audio_core::MAX_AUTOMATION_POINTS
    ));
}

// ---- SPEC-026: time signature ----

#[test]
fn a_four_four_session_writes_no_time_signature() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("time_signature"), "{text}");
}

#[test]
fn a_time_signature_survives_the_round_trip() {
    let (graph, _) = demo_graph();
    let mut session = demo_session();
    session.time_signature = audio_core::TimeSignature {
        numerator: 7,
        denominator: 8,
    };
    let project = Project::with_session(Project::capture("demo", SR, 120.0, &graph), session);
    let text = project.to_json();
    assert!(text.contains("\"time_signature\": [7, 8]"), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back, project);
    assert_eq!(back.session().time_signature.numerator, 7);
}

#[test]
fn a_document_without_a_time_signature_is_four_four() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    let back = Project::from_json(&text).unwrap();
    assert_eq!(
        back.session().time_signature,
        audio_core::TimeSignature::default()
    );
}

#[test]
fn an_unsupported_time_signature_is_a_session_error() {
    use audio_core::SessionError;
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json()
        .replace(
            "\"master_gain\": 1",
            "\"master_gain\": 1,\n    \"time_signature\": [4, 3]",
        );
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(SessionError::InvalidTimeSignature)
    );
}

#[test]
fn a_malformed_time_signature_is_rejected() {
    let (graph, _) = demo_graph();
    let base = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    for bad in ["[4]", "[4, 4, 4]", "4", "[4, 300]", "[\"4\", 4]"] {
        let text = base.replace(
            "\"master_gain\": 1",
            &format!("\"master_gain\": 1,\n    \"time_signature\": {bad}"),
        );
        assert!(
            matches!(
                Project::from_json(&text).unwrap_err(),
                ProjectError::Malformed { .. }
            ),
            "{bad}"
        );
    }
}

// --- SPEC-028: buses and sends -------------------------------------------

fn buses_session() -> audio_core::Session {
    use audio_core::{Bus, BusSend, Effect, EffectKind};
    let mut session = demo_session();
    let mut verb = Bus::new("Verb");
    verb.gain = 0.75;
    verb.effects.push(Effect::new(EffectKind::reverb()));
    session.buses = vec![verb, Bus::new("Delay")];
    session.tracks[0].sends = vec![
        BusSend {
            bus: 1,
            level: 0.25,
        },
        BusSend { bus: 0, level: 0.5 },
    ];
    session
}

fn buses_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(Project::capture("demo", SR, 120.0, &graph), buses_session()).to_json()
}

#[test]
fn buses_and_sends_survive_the_round_trip() {
    let text = buses_json();
    assert!(text.contains("\"buses\""), "{text}");
    assert!(text.contains("\"sends\""), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &buses_session());
    assert_eq!(back.session().tracks[0].send_level(0), 0.5);
    assert_eq!(back.session().tracks[0].send_level(2), 0.0);
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_session_without_buses_writes_no_bus_keys() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("buses"), "{text}");
    assert!(!text.contains("sends"), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert!(back.session().buses.is_empty());
    assert!(back.session().tracks[0].sends.is_empty());
}

#[test]
fn a_bus_gain_and_effects_are_optional_when_reading() {
    let text = buses_json().replace(
        "\"name\": \"Delay\",\n        \"gain\": 1",
        "\"name\": \"Delay\"",
    );
    assert_ne!(text, buses_json());
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session().buses[1], audio_core::Bus::new("Delay"));
}

#[test]
fn a_send_to_a_missing_bus_fails_validation() {
    let text = buses_json().replace("\"bus\": 1", "\"bus\": 2");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidSend { track: 0, send: 0 })
    );
}

#[test]
fn two_sends_to_one_bus_fail_validation() {
    let text = buses_json().replace("\"bus\": 1", "\"bus\": 0");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidSend { track: 0, send: 1 })
    );
}

#[test]
fn an_out_of_range_bus_gain_fails_validation() {
    let text = buses_json().replace("\"gain\": 0.75", "\"gain\": 1.5");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidBus { bus: 0 })
    );
}

#[test]
fn an_unknown_key_inside_a_send_is_rejected() {
    let text = buses_json().replace("\"level\": 0.5", "\"lvl\": 0.5");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "lvl".to_owned()
        }
    );
}

#[test]
fn too_many_buses_are_rejected() {
    let mut session = demo_session();
    session.buses = vec![audio_core::Bus::new("b"); audio_core::MAX_BUSES + 1];
    assert_eq!(
        session.validate().unwrap_err(),
        audio_core::SessionError::InvalidBus {
            bus: audio_core::MAX_BUSES
        }
    );
}

// --- SPEC-029: track colour ------------------------------------------------

fn coloured_session() -> audio_core::Session {
    let mut session = demo_session();
    session.tracks[0].color = Some(3);
    session
}

fn coloured_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        coloured_session(),
    )
    .to_json()
}

#[test]
fn a_track_colour_survives_the_round_trip() {
    let text = coloured_json();
    assert!(text.contains("\"color\": 3"), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &coloured_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_track_without_colour_writes_no_colour_key() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("color"), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session().tracks[0].color, None);
}

#[test]
fn a_colour_outside_the_palette_fails_validation() {
    let text = coloured_json().replace("\"color\": 3", "\"color\": 8");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidColor { track: 0 })
    );
}

#[test]
fn a_fractional_or_huge_colour_is_malformed() {
    for bad in ["\"color\": 1.5", "\"color\": 300", "\"color\": \"red\""] {
        let text = coloured_json().replace("\"color\": 3", bad);
        assert!(
            matches!(
                Project::from_json(&text).unwrap_err(),
                ProjectError::Malformed { .. }
            ),
            "{bad}"
        );
    }
}

#[test]
fn the_palette_has_eight_colours() {
    assert_eq!(audio_core::TRACK_COLORS, 8);
    let mut session = demo_session();
    session.tracks[0].color = Some(audio_core::TRACK_COLORS - 1);
    assert!(session.validate().is_ok());
}

// --- SPEC-030: markers ------------------------------------------------------

fn marked_session() -> audio_core::Session {
    let mut session = demo_session();
    session.markers = vec![
        audio_core::Marker {
            name: "Куплет".into(),
            position: 0,
        },
        audio_core::Marker {
            name: "Припев".into(),
            position: 96_000,
        },
    ];
    session
}

fn marked_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        marked_session(),
    )
    .to_json()
}

#[test]
fn markers_survive_the_round_trip() {
    let text = marked_json();
    assert!(text.contains("\"markers\""), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &marked_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn a_session_without_markers_writes_no_markers_key() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("markers"), "{text}");
    assert!(
        Project::from_json(&text)
            .unwrap()
            .session()
            .markers
            .is_empty()
    );
}

#[test]
fn markers_out_of_order_fail_validation() {
    let text = marked_json().replace("\"position\": 96000", "\"position\": 0");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidMarker { marker: 1 })
    );
}

#[test]
fn a_marker_with_a_bad_field_is_rejected() {
    let text = marked_json();
    let fractional = text.replace("\"position\": 96000", "\"position\": 1.5");
    assert!(matches!(
        Project::from_json(&fractional).unwrap_err(),
        ProjectError::Malformed { .. }
    ));
    let stray = text.replace("\"position\": 96000", "\"position\": 96000, \"x\": 1");
    assert!(matches!(
        Project::from_json(&stray).unwrap_err(),
        ProjectError::UnknownField { .. }
    ));
    let missing = text.replace(", \"position\": 96000", "");
    assert!(matches!(
        Project::from_json(&missing).unwrap_err(),
        ProjectError::MissingField { .. }
    ));
}

#[test]
fn too_many_markers_are_rejected() {
    let mut session = demo_session();
    session.markers = (0..=audio_core::MAX_MARKERS as u64)
        .map(|position| audio_core::Marker {
            name: String::new(),
            position,
        })
        .collect();
    assert_eq!(
        session.validate(),
        Err(audio_core::SessionError::InvalidMarker {
            marker: audio_core::MAX_MARKERS
        })
    );
}

// --- SPEC-041: effect automation -------------------------------------------

fn effect_automation_session() -> audio_core::Session {
    use audio_core::{AutoPoint, Effect, EffectKind, EffectParam, Lane};
    let mut session = demo_session();
    session.tracks[0]
        .effects
        .push(Effect::new(EffectKind::reverb()));
    *session.tracks[0].automation.lane_mut(Lane::Effect {
        slot: 0,
        param: EffectParam::Mix,
    }) = vec![
        AutoPoint {
            time: 0,
            value: 0.0,
        },
        AutoPoint {
            time: 48_000,
            value: 0.8,
        },
    ];
    session
}

fn effect_automation_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        effect_automation_session(),
    )
    .to_json()
}

#[test]
fn effect_lanes_survive_the_round_trip() {
    let text = effect_automation_json();
    assert!(text.contains("\"param\": \"mix\""), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &effect_automation_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn an_unknown_effect_param_is_rejected() {
    let text = effect_automation_json().replace("\"param\": \"mix\"", "\"param\": \"wobble\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "wobble".to_owned()
        }
    );
}

#[test]
fn an_unknown_effect_lane_key_is_rejected() {
    let text = effect_automation_json().replace("\"slot\": 0,", "\"slot\": 0, \"curve\": 1,");
    assert!(Project::from_json(&text).is_err());
}

#[test]
fn an_effect_lane_on_a_missing_slot_fails_validation() {
    let text = effect_automation_json().replace("\"slot\": 0,", "\"slot\": 3,");
    assert!(Project::from_json(&text).is_err());
}

#[test]
fn empty_effect_lanes_are_not_written() {
    use audio_core::{EffectParam, Lane};
    let mut session = demo_session();
    session.tracks[0].automation.lane_mut(Lane::Effect {
        slot: 0,
        param: EffectParam::Mix,
    });
    let (graph, _) = demo_graph();
    let text =
        Project::with_session(Project::capture("demo", SR, 120.0, &graph), session).to_json();
    assert!(!text.contains("automation"), "{text}");
}

// --- SPEC-043: track and project notes ---------------------------------------

fn noted_session() -> audio_core::Session {
    let mut session = demo_session();
    session.notes = "Релиз в пятницу\n\"финал\"".into();
    session.tracks[0].notes = "SM57, 10 см".into();
    session
}

#[test]
fn notes_survive_the_round_trip() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), noted_session())
        .to_json();
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &noted_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn empty_notes_write_no_notes_key() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    assert!(!text.contains("\"notes\""), "{text}");
}

#[test]
fn notes_longer_than_the_limit_fail_validation() {
    let mut session = demo_session();
    session.tracks[0].notes = "x".repeat(audio_core::MAX_NOTES_BYTES + 1);
    assert_eq!(
        session.validate(),
        Err(audio_core::SessionError::NotesTooLong)
    );
    let mut session = demo_session();
    session.notes = "x".repeat(audio_core::MAX_NOTES_BYTES);
    assert!(session.validate().is_ok());
}

// --- SPEC-044 §1: master mute/dim and input monitoring --------------------

#[test]
fn master_mute_dim_and_input_monitor_survive_the_round_trip() {
    let (graph, _) = demo_graph();
    let mut session = demo_session();
    session.master_mute = true;
    session.master_dim = true;
    session.tracks[0].input_monitor = true;
    let project = Project::with_session(Project::capture("demo", SR, 120.0, &graph), session);
    let text = project.to_json();
    assert!(text.contains("\"master_mute\": true"), "{text}");
    assert!(text.contains("\"master_dim\": true"), "{text}");
    assert!(text.contains("\"input_monitor\": true"), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back, project);
}

#[test]
fn unset_master_and_monitor_flags_are_not_written() {
    let (graph, _) = demo_graph();
    let text = Project::with_session(Project::capture("demo", SR, 120.0, &graph), demo_session())
        .to_json();
    for key in ["master_mute", "master_dim", "input_monitor"] {
        assert!(!text.contains(key), "{key} written: {text}");
    }
    let back = Project::from_json(&text).unwrap();
    assert!(!back.session().master_mute && !back.session().master_dim);
}

#[test]
fn master_level_folds_mute_and_dim_into_the_gain() {
    let mut session = audio_core::Session::new("s", SR, 120.0);
    session.master_gain = 0.8;
    assert!((session.master_level() - 0.8).abs() < 1e-6);
    session.master_dim = true;
    assert!((session.master_level() - 0.8 * audio_core::DIM_GAIN).abs() < 1e-6);
    session.master_mute = true;
    assert!(session.master_level() == 0.0);
}

// --- SPEC-051: channel EQ -------------------------------------------------

fn channel_eq_session() -> audio_core::Session {
    use audio_core::{Effect, EffectKind, EffectParam};
    let mut kind = EffectKind::channel_eq();
    assert!(EffectParam::HpHz.set(&mut kind, 40.0));
    assert!(EffectParam::P2Db.set(&mut kind, -4.5));
    assert!(EffectParam::P2Q.set(&mut kind, 2.5));
    assert!(EffectParam::HighShelfDb.set(&mut kind, 3.0));
    if let EffectKind::ChannelEq { bands } = &mut kind {
        bands[0].enabled = true;
        bands[4].enabled = false;
    }
    let mut session = demo_session();
    session.tracks[0].effects = vec![Effect::new(kind)];
    session
}

fn channel_eq_json() -> String {
    let (graph, _) = demo_graph();
    Project::with_session(
        Project::capture("demo", SR, 120.0, &graph),
        channel_eq_session(),
    )
    .to_json()
}

#[test]
fn a_channel_eq_survives_the_round_trip() {
    let text = channel_eq_json();
    for key in [
        "\"channel_eq\"",
        "\"hp_on\": true",
        "\"hp_hz\"",
        "\"low_shelf_db\"",
        "\"p2_q\": 2.5",
        "\"p3_on\": false",
        "\"high_shelf_db\": 3",
        "\"lp_hz\"",
    ] {
        assert!(text.contains(key), "{key} missing in {text}");
    }
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back.session(), &channel_eq_session());
    assert_eq!(back.to_json(), text);
}

#[test]
fn an_unknown_channel_eq_key_is_rejected() {
    let text = channel_eq_json().replace("\"lp_hz\"", "\"lp_db\"");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::UnknownField {
            field: "lp_db".to_owned()
        }
    );
}

#[test]
fn a_missing_channel_eq_key_is_reported() {
    let text = channel_eq_json().replace("\"p4_q\"", "\"low_db\"");
    assert!(Project::from_json(&text).is_err());
    let text = channel_eq_json();
    let start = text.find("\"p1_on\"").unwrap();
    let end = start + text[start..].find(", ").unwrap() + 2;
    let mut cut = text.clone();
    cut.replace_range(start..end, "");
    assert!(matches!(
        Project::from_json(&cut).unwrap_err(),
        ProjectError::MissingField { .. }
    ));
}

#[test]
fn a_channel_eq_flag_of_the_wrong_type_is_rejected() {
    let text = channel_eq_json().replace("\"hp_on\": true", "\"hp_on\": 1");
    assert!(Project::from_json(&text).is_err());
}

#[test]
fn an_out_of_range_channel_eq_value_fails_validation() {
    let text = channel_eq_json().replace("\"p2_q\": 2.5", "\"p2_q\": 40");
    assert_eq!(
        Project::from_json(&text).unwrap_err(),
        ProjectError::Session(audio_core::SessionError::InvalidEffect {
            track: 0,
            effect: 0
        })
    );
}
