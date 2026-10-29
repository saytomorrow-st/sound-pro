//! `daw` CLI: smoke tools that work without audio hardware.
//!
//! ```text
//! daw bounce <out.wav> [seconds]   # offline render
//! daw selftest                     # render + verify + exit non-zero on failure
//! daw project save <path>          # write the demo session as JSON
//! daw project show <path>          # print a session document
//! daw render-midi <in.mid> <out.wav> [--tempo BPM] [--seconds S]
//! ```

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use daw_app::{OfflineJob, run_offline};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    match command.as_str() {
        "bounce" => {
            let Some(output) = args.next() else {
                usage();
                return ExitCode::from(2);
            };
            let mut seconds = 1.0;
            let mut seen_seconds = false;
            let mut split_part_samples = None;
            while let Some(arg) = args.next() {
                if arg == "--split" {
                    let Some(value) = args.next() else {
                        eprintln!("--split requires a value (samples per part)");
                        return ExitCode::from(2);
                    };
                    match value.parse::<u64>() {
                        Ok(n) if n > 0 => split_part_samples = Some(n),
                        _ => {
                            eprintln!("--split expects a positive sample count, got {value}");
                            return ExitCode::from(2);
                        }
                    }
                } else if !seen_seconds {
                    match arg.parse::<f64>() {
                        Ok(s) => {
                            seconds = s;
                            seen_seconds = true;
                        }
                        Err(_) => {
                            eprintln!("invalid duration: {arg}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    eprintln!("unexpected argument: {arg}");
                    usage();
                    return ExitCode::from(2);
                }
            }
            let job = OfflineJob {
                output: PathBuf::from(output),
                seconds,
                split_part_samples,
                ..OfflineJob::default()
            };
            match run_offline(&job) {
                Ok(report) => {
                    println!(
                        "rendered {} frames to {} file(s)",
                        report.frames,
                        report.parts.len()
                    );
                    for part in &report.parts {
                        println!("  {}", part.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("bounce failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "selftest" => {
            let dir = std::env::temp_dir().join(format!("daw-selftest-{}", std::process::id()));
            let output = dir.join("selftest.wav");
            let job = OfflineJob {
                output: output.clone(),
                seconds: 0.1,
                ..OfflineJob::default()
            };
            let result = std::fs::create_dir_all(&dir)
                .map_err(|e| e.to_string())
                .and_then(|()| run_offline(&job).map_err(|e| e.to_string()));
            let _ = std::fs::remove_file(&output);
            let _ = std::fs::remove_dir(&dir);
            match result {
                Ok(report) if report.frames == 4_800 => {
                    println!(
                        "selftest OK: {} frames, engine, wav and transport agree",
                        report.frames
                    );
                    ExitCode::SUCCESS
                }
                Ok(report) => {
                    eprintln!(
                        "selftest FAILED: expected 4800 frames, got {}",
                        report.frames
                    );
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("selftest FAILED: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "project" => project(args),
        "render-midi" => render_midi(args),
        "play" => {
            let seconds = args
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(3.0);
            play(seconds)
        }
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "usage:\n  daw bounce <out.wav> [seconds] [--split <samples>]\n  \
         daw play [seconds]\n  daw selftest\n  daw project save <path>\n  \
         daw project show <path>\n  daw render-midi <in.mid> <out.wav> [--tempo BPM] [--seconds S]\n\n\
         --split <samples> writes part-01.wav, part-02.wav, ... each at most\n\
         <samples> long (samples, not frames; mono).\n\n\
         `daw play` needs `--features realtime`; without it the command exits 2.\n\
         Build with `--features realtime` to enable the CPAL backend."
    );
}

/// `daw project save|show`: the only place that touches the filesystem for
/// session documents. Parsing and serialising live in `audio-core`.
fn project(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(action) = args.next() else {
        eprintln!("project requires `save <path>` or `show <path>`");
        return ExitCode::FAILURE;
    };
    let path = args.next().map(PathBuf::from);
    match (action.as_str(), path) {
        ("save", Some(path)) => save_project(&path),
        ("show", Some(path)) => show_project(&path),
        ("save" | "show", None) => {
            eprintln!("project {action} requires a path");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("unknown project action: {action}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn save_project(path: &std::path::Path) -> ExitCode {
    let graph = match daw_app::build_demo_graph(48_000) {
        Ok(graph) => graph,
        Err(e) => {
            eprintln!("project save failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // A non-default value makes the saved file visibly a session, not a spec.
    let level = match graph.param(audio_core::ParamId(0)) {
        Ok(handle) => handle,
        Err(e) => {
            eprintln!("project save failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    level.set(0.25);
    let project = audio_core::Project::capture("demo", 48_000, 120.0, &graph);
    let text = project.to_json();
    match std::fs::write(path, text.as_bytes()) {
        Ok(()) => {
            println!(
                "saved {} parameter(s) to {}",
                project.params.len(),
                path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("project save failed: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

fn show_project(path: &std::path::Path) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("project show failed: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match audio_core::Project::from_json(&text) {
        Ok(project) => {
            println!("name: {}", project.name);
            println!("sample_rate: {}", project.sample_rate);
            println!("tempo_bpm: {}", project.tempo_bpm);
            for param in &project.params {
                println!("{} {} = {}", param.id, param.name, param.value);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("project show failed: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// `daw play`: render the demo graph to the default output device for `seconds`.
#[cfg(feature = "realtime")]
fn play(seconds: f64) -> ExitCode {
    use std::time::Duration;

    // The graph is built by the backend at the rate the device actually
    // runs. The demo instrument is silent without a schedule, so this proves
    // the stream, not the sound; `daw render-midi` is the audible path.
    let mut handle = match daw_app::realtime::play_default(daw_app::build_demo_graph) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("play failed: {e}");
            // No device / no config are user-facing setup problems: exit 2.
            return ExitCode::from(2);
        }
    };
    std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
    handle.stop();
    let stats = handle.stats();
    println!(
        "played: {} frames, {} dropped callbacks, {} render errors, {} stream errors",
        stats.rendered_frames(),
        stats.dropped_callbacks(),
        stats.render_errors(),
        stats.stream_errors()
    );
    ExitCode::SUCCESS
}

/// `daw play` without the realtime feature: explain and exit 2.
#[cfg(not(feature = "realtime"))]
fn play(_seconds: f64) -> ExitCode {
    eprintln!("play is unavailable: rebuild with `--features realtime`");
    ExitCode::from(2)
}

/// `daw render-midi`: the only place that touches the filesystem for MIDI.
fn render_midi(mut args: impl Iterator<Item = String>) -> ExitCode {
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: daw render-midi <in.mid> <out.wav> [--tempo BPM] [--seconds S]");
        return ExitCode::from(2);
    };
    let mut tempo = audio_core::transport::Tempo::default();
    let mut seconds = None;
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--tempo" | "--seconds" => args.next(),
            other => {
                eprintln!("unexpected argument: {other}");
                return ExitCode::from(2);
            }
        };
        let Some(value) = value else {
            eprintln!("{arg} requires a value");
            return ExitCode::from(2);
        };
        match (arg.as_str(), value.parse::<f64>()) {
            ("--tempo", Ok(bpm)) if bpm.is_finite() && bpm > 0.0 => {
                tempo = audio_core::transport::Tempo(bpm);
            }
            ("--seconds", Ok(s)) if s.is_finite() && s > 0.0 => seconds = Some(s),
            (flag, _) => {
                eprintln!("{flag} expects a positive number, got {value}");
                return ExitCode::from(2);
            }
        }
    }

    let bytes = match std::fs::read(&input) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("render-midi failed: {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let file = match audio_core::midi::parse_smf(&bytes) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("render-midi failed: {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let sample_rate = audio_core::transport::DEFAULT_SAMPLE_RATE;
    let spec = match audio_core::schedule::ScheduleSpec::from_midi(
        &file,
        sample_rate,
        tempo,
        u64::from(sample_rate),
    ) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("render-midi failed: {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = seconds.map(|s| (s * f64::from(sample_rate)).round() as u64);
    let job = daw_app::MidiRenderJob {
        output: PathBuf::from(&output),
        schedule: audio_core::schedule::Schedule::new(spec),
        notes: file.notes().len() as u64,
        sample_rate,
        block_frames: 256,
        frames,
    };
    match daw_app::render_midi(&job) {
        Ok(report) => {
            println!(
                "rendered {} frames (schedule {}), {} notes, {} voice starts to {}",
                report.frames, report.scheduled_frames, job.notes, report.voices_started, output
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render-midi failed: {e}");
            ExitCode::FAILURE
        }
    }
}
