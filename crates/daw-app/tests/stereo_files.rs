//! A stereo file keeps its channels on a stereo track and is mixed to one
//! on a mono track (SPEC-050 §5).

use std::path::PathBuf;
use std::sync::Arc;

use audio_core::buffer::AudioBuffer;
use audio_core::session::{Clip, Note, Session, Track as SessionTrack};
use audio_core::wav::{WavSpec, read_wav_mono, write_wav_mono};
use daw_app::audio_file::read_channels_at;
use daw_app::{ClipCache, load_session, load_session_cached, render_session};

const RATE: u32 = 48_000;
const FRAMES: usize = 1_000;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("daw-stereo-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_stereo(dir: &std::path::Path) -> (Vec<f32>, Vec<f32>) {
    let left: Vec<f32> = (0..FRAMES).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
    let right: Vec<f32> = (0..FRAMES)
        .map(|i| (i as f32 * 0.03).cos() * 0.25)
        .collect();
    let mut buf = AudioBuffer::new(2, FRAMES);
    buf.channel_mut(0).copy_from_slice(&left);
    buf.channel_mut(1).copy_from_slice(&right);
    let spec = WavSpec {
        sample_rate: RATE,
        channels: 2,
    };
    write_wav_mono(dir.join("st.wav"), spec, &buf).unwrap();
    (left, right)
}

fn session(stereo: bool) -> Session {
    let mut clip = Clip::full(0, FRAMES as u64);
    clip.source = Some("st.wav".to_owned());
    let mut track = if stereo {
        SessionTrack::stereo("s")
    } else {
        SessionTrack::new("m")
    };
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    session
}

fn render(job: daw_app::SessionJob) -> (Vec<f32>, Vec<f32>) {
    let path = job.output.clone();
    render_session(job).unwrap();
    let (_, buf) = read_wav_mono(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        buf.channel(0)[..FRAMES].to_vec(),
        buf.channel(1)[..FRAMES].to_vec(),
    )
}

fn assert_close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x - y).abs() < 1e-6, "frame {i}: {x} vs {y}");
    }
}

#[test]
fn read_channels_keeps_left_and_right_apart() {
    let dir = temp_dir("read");
    let (left, right) = write_stereo(&dir);
    let channels = read_channels_at(&dir.join("st.wav"), RATE).unwrap();
    assert_eq!(channels.left, left);
    assert_eq!(channels.right.as_deref(), Some(right.as_slice()));
    let mono: Vec<f32> = left
        .iter()
        .zip(&right)
        .map(|(l, r)| (l + r) * 0.5)
        .collect();
    assert_eq!(channels.mono(), mono);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stereo_track_plays_each_channel_on_its_side() {
    let dir = temp_dir("track");
    let (left, right) = write_stereo(&dir);
    let out = dir.join("out.wav");
    let (l, r) = render(load_session(session(true), &dir, out).unwrap());
    assert_close(&l, &left);
    assert_close(&r, &right);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mono_track_mixes_a_stereo_file_to_one_channel() {
    let dir = temp_dir("mono");
    let (left, right) = write_stereo(&dir);
    let out = dir.join("out.wav");
    let (l, r) = render(load_session(session(false), &dir, out).unwrap());
    let mono: Vec<f32> = left
        .iter()
        .zip(&right)
        .map(|(l, r)| (l + r) * 0.5)
        .collect();
    assert_close(&l, &mono);
    assert_close(&r, &mono);
    std::fs::remove_dir_all(&dir).unwrap();
}

struct StereoCache {
    left: Arc<Vec<f32>>,
    right: Arc<Vec<f32>>,
}

impl ClipCache for StereoCache {
    fn audio(&mut self, _source: &str) -> Option<Arc<Vec<f32>>> {
        let mono = self
            .left
            .iter()
            .zip(self.right.iter())
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        Some(Arc::new(mono))
    }
    fn audio_stereo(&mut self, _source: &str) -> Option<(Arc<Vec<f32>>, Arc<Vec<f32>>)> {
        Some((Arc::clone(&self.left), Arc::clone(&self.right)))
    }
    fn notes(&mut self, _: &str, _: &[Note], _: u32, _: u64) -> Option<Arc<Vec<f32>>> {
        None
    }
    fn store_notes(&mut self, _: &str, _: &[Note], _: u32, _: u64, _: Arc<Vec<f32>>) {}
}

#[test]
fn cached_stereo_sounds_like_the_file() {
    let dir = temp_dir("cache");
    let (left, right) = write_stereo(&dir);
    let from_disk = render(load_session(session(true), &dir, dir.join("a.wav")).unwrap());
    let mut cache = StereoCache {
        left: Arc::new(left),
        right: Arc::new(right),
    };
    let missing = dir.join("gone");
    let from_cache = render(
        load_session_cached(session(true), &missing, dir.join("b.wav"), &mut cache).unwrap(),
    );
    assert_eq!(from_disk, from_cache);
    std::fs::remove_dir_all(&dir).unwrap();
}
