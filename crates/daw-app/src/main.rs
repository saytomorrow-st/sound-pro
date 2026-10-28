//! `daw` CLI: smoke tools that work without audio hardware.
//!
//! ```text
//! daw bounce <out.wav> [seconds]   # offline render
//! daw selftest                     # render + verify + exit non-zero on failure
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
         daw play [seconds]\n  daw selftest\n\n\
         --split <samples> writes part-01.wav, part-02.wav, ... each at most\n\
         <samples> long (samples, not frames; mono).\n\n\
         `daw play` needs `--features realtime`; without it the command exits 2.\n\
         Build with `--features realtime` to enable the CPAL backend."
    );
}

/// `daw play`: render the demo graph to the default output device for `seconds`.
#[cfg(feature = "realtime")]
fn play(seconds: f64) -> ExitCode {
    use std::time::Duration;

    // The graph is built by the backend at the rate the device actually runs.
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
