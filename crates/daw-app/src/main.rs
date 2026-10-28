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
            let seconds = args
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0);
            let job = OfflineJob {
                output: PathBuf::from(output),
                seconds,
                ..OfflineJob::default()
            };
            match run_offline(&job) {
                Ok(frames) => {
                    println!("rendered {frames} frames to {}", job.output.display());
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
                Ok(frames) if frames == 4_800 => {
                    println!("selftest OK: {frames} frames, engine, wav and transport agree");
                    ExitCode::SUCCESS
                }
                Ok(frames) => {
                    eprintln!("selftest FAILED: expected 4800 frames, got {frames}");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("selftest FAILED: {e}");
                    ExitCode::FAILURE
                }
            }
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
        "usage:\n  daw bounce <out.wav> [seconds]\n  daw selftest\n\n\
         Build with `--features realtime` to enable the CPAL backend."
    );
}
