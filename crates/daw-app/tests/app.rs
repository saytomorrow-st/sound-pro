//! End-to-end tests for the application shell.

use audio_core::buffer::AudioBuffer;
use daw_app::{OfflineJob, interleave, run_offline};

#[test]
fn offline_job_writes_expected_frame_count() {
    let dir = std::env::temp_dir().join(format!("daw-app-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("job.wav");
    let job = OfflineJob {
        output: output.clone(),
        seconds: 0.25,
        block_frames: 64,
        ..OfflineJob::default()
    };
    let frames = run_offline(&job).unwrap();
    assert_eq!(frames, 12_000);

    let (spec, buf) = audio_core::wav::read_wav_mono(&output).unwrap();
    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(buf.frames(), 12_000);
    assert!(buf.channel(0).iter().any(|s| s.abs() > 0.1), "not silent");
    std::fs::remove_file(&output).unwrap();
}

#[test]
fn interleave_is_channel_major_to_frame_major() {
    let mut buf = AudioBuffer::new(2, 3);
    buf.channel_mut(0).copy_from_slice(&[1.0, 2.0, 3.0]);
    buf.channel_mut(1).copy_from_slice(&[10.0, 20.0, 30.0]);

    let mut out = vec![0.0; 8];
    interleave(&buf, 3, &mut out);
    assert_eq!(&out[..6], &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
    assert_eq!(&out[6..], &[0.0, 0.0], "tail must be silenced");
}

#[test]
fn run_offline_rejects_invalid_block_size_instead_of_allocating() {
    let job = OfflineJob {
        output: std::env::temp_dir().join("never-written.wav"),
        block_frames: 4096, // larger than the renderer quantum
        ..OfflineJob::default()
    };
    let result = std::panic::catch_unwind(|| run_offline(&job));
    assert!(result.is_err(), "must fail loudly, not silently truncate");
    assert!(!job.output.exists());
}
