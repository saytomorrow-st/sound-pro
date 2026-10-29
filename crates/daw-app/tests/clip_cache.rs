//! Loading a session from already decoded material (SPEC-035).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use audio_core::session::{Clip, Note, Session, Track as SessionTrack};
use audio_core::wav::read_wav_mono;
use daw_app::{ClipCache, LoadError, NoCache, load_session, load_session_cached, render_session};

const RATE: u32 = 48_000;

#[derive(Default)]
struct MapCache {
    audio: HashMap<String, Arc<Vec<f32>>>,
    notes: HashMap<(Vec<Note>, u32, u64), Arc<Vec<f32>>>,
    note_hits: usize,
}

impl ClipCache for MapCache {
    fn audio(&mut self, source: &str) -> Option<Arc<Vec<f32>>> {
        self.audio.get(source).cloned()
    }
    fn notes(&mut self, notes: &[Note], rate: u32, frames: u64) -> Option<Arc<Vec<f32>>> {
        let hit = self.notes.get(&(notes.to_vec(), rate, frames)).cloned();
        self.note_hits += usize::from(hit.is_some());
        hit
    }
    fn store_notes(&mut self, notes: &[Note], rate: u32, frames: u64, samples: Arc<Vec<f32>>) {
        self.notes.insert((notes.to_vec(), rate, frames), samples);
    }
}

fn out(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("daw-cache-{tag}-{}.wav", std::process::id()))
}

fn render(job: daw_app::SessionJob) -> Vec<f32> {
    let path = job.output.clone();
    render_session(job).unwrap();
    let (_, buf) = read_wav_mono(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    buf.channel(0).to_vec()
}

fn audio_session(len: u64) -> Session {
    let mut clip = Clip::full(100, len);
    clip.source = Some("audio/missing.wav".to_owned());
    let mut track = SessionTrack::new("a");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn cached_audio_sounds_like_the_file_and_plays_after_it_is_gone() {
    let dir = std::env::temp_dir().join(format!("daw-cache-dir-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    let samples: Vec<f32> = (0..2_000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
    let mut buf = audio_core::buffer::AudioBuffer::new(1, samples.len());
    buf.channel_mut(0).copy_from_slice(&samples);
    let spec = audio_core::wav::WavSpec {
        sample_rate: RATE,
        channels: 1,
    };
    audio_core::wav::write_wav_mono(dir.join("audio/missing.wav"), spec, &buf).unwrap();
    let session = audio_session(2_000);
    let from_disk = render(load_session(session.clone(), &dir, out("a0")).unwrap());

    let (_, decoded) = read_wav_mono(dir.join("audio/missing.wav")).unwrap();
    let mut cache = MapCache::default();
    cache.audio.insert(
        "audio/missing.wav".to_owned(),
        Arc::new(decoded.channel(0).to_vec()),
    );
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(load_session(session.clone(), &dir, out("x")).is_err());
    let from_cache = render(load_session_cached(session, &dir, out("a"), &mut cache).unwrap());
    assert_eq!(from_disk, from_cache);
    assert!(from_cache.iter().any(|&s| s != 0.0));
}

#[test]
fn cached_audio_shorter_than_the_clip_is_too_short() {
    let mut cache = MapCache::default();
    cache
        .audio
        .insert("audio/missing.wav".to_owned(), Arc::new(vec![0.1; 10]));
    let err = load_session_cached(
        audio_session(20),
        std::path::Path::new(""),
        out("b"),
        &mut cache,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::SourceTooShort {
            file_frames: 10,
            source_length: 20,
            ..
        }
    ));
}

fn midi_session() -> Session {
    let mut clip = Clip::full(0, RATE as u64 / 2);
    clip.notes = Some(vec![Note {
        start: 0,
        length: RATE as u64 / 4,
        pitch: 60,
        velocity: 100,
    }]);
    let mut track = SessionTrack::new("m");
    track.clips.push(clip);
    let mut session = Session::new("s", RATE, 120.0);
    session.tracks.push(track);
    session
}

#[test]
fn rendered_notes_are_reused_and_sound_the_same() {
    let mut cache = MapCache::default();
    let base = std::path::Path::new("");
    let first = render(load_session_cached(midi_session(), base, out("c"), &mut cache).unwrap());
    assert_eq!(cache.note_hits, 0);
    assert_eq!(cache.notes.len(), 1);
    let second = render(load_session_cached(midi_session(), base, out("d"), &mut cache).unwrap());
    assert_eq!(cache.note_hits, 1);
    let plain = render(load_session_cached(midi_session(), base, out("e"), &mut NoCache).unwrap());
    assert_eq!(first, second);
    assert_eq!(first, plain);
    assert!(first.iter().any(|&s| s != 0.0));
}
