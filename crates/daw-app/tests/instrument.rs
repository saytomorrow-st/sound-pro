//! Acceptance tests for the [`Instrument`] trait and its implementations
//! (SPEC-064): offline pre-render through the piano (bit-identical fallback)
//! and through a loaded SFZ bank (pitch and timing).

use std::path::{Path, PathBuf};

use audio_core::Note;
use audio_core::buffer::AudioBuffer;
use audio_core::wav::{WavSpec, write_wav_mono};
use daw_app::instrument::{Instrument, PianoInstrument, SamplerInstrument};
use daw_app::sample_bank::{DEFAULT_RAM_BUDGET_MIB, load_bank};

const RATE: u32 = 48_000;

/// Per-test temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("daw-inst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &Path, data: &[f32]) {
    let mut buf = AudioBuffer::new(1, data.len());
    buf.channel_mut(0).copy_from_slice(data);
    write_wav_mono(
        path,
        WavSpec {
            sample_rate: RATE,
            channels: 1,
        },
        &buf,
    )
    .unwrap();
}

/// A sine at `freq` Hz, `frames` long, peak 0.5 (so i16 conversion is lossless
/// enough for zero-crossing pitch tests).
fn sine(freq: f32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            (std::f32::consts::TAU * freq * t).sin() * 0.5
        })
        .collect()
}

/// Counts zero crossings (sign flips) in `data`, a cheap period estimator.
fn zero_crossings(data: &[f32]) -> usize {
    let mut count = 0;
    for pair in data.windows(2) {
        if (pair[0] < 0.0) != (pair[1] < 0.0) {
            count += 1;
        }
    }
    count
}

#[test]
fn piano_instrument_is_bit_identical_to_piano() {
    let notes = [
        Note {
            pitch: 60,
            start: 0,
            length: 48_000,
            velocity: 100,
        },
        Note {
            pitch: 64,
            start: 24_000,
            length: 24_000,
            velocity: 80,
        },
    ];
    let frames = 96_000;
    let a = PianoInstrument.render_notes(&notes, RATE, frames).unwrap();
    let b = daw_app::piano::render_piano(&notes, RATE, frames).unwrap();
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "frame {i}");
    }
}

#[test]
fn sampler_instrument_renders_expected_pitch() {
    let dir = Scratch::new("pitch");
    // A 440 Hz sine at middle-C (root 60). Playing note 72 (an octave up) must
    // double its frequency → roughly double the zero crossings in a fixed
    // window.
    let sample = sine(440.0, RATE as usize * 2);
    write_wav(&dir.join("tone.wav"), &sample);
    let sfz =
        "<region> sample=tone.wav lokey=0 hikey=127 pitch_keycenter=60 loop_mode=loop_continuous\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();
    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    let instrument = SamplerInstrument::new(&bank);

    let frames = (RATE as u64) * 2; // two seconds
    let low = instrument
        .render_notes(
            &[Note {
                pitch: 60,
                start: 0,
                length: frames,
                velocity: 127,
            }],
            RATE,
            frames,
        )
        .unwrap();
    let high = instrument
        .render_notes(
            &[Note {
                pitch: 72,
                start: 0,
                length: frames,
                velocity: 127,
            }],
            RATE,
            frames,
        )
        .unwrap();

    // Skip the attack/first samples; count crossings over the steady state.
    let window = 40_000..90_000;
    let low_count = zero_crossings(&low[window.clone()]);
    let high_count = zero_crossings(&high[window.clone()]);
    assert!(low_count > 100, "low {low_count}");
    assert!(
        high_count > low_count * 3 / 2,
        "octave-up should cross more: low {low_count} high {high_count}"
    );
}

#[test]
fn sampler_instrument_respects_note_timing() {
    let dir = Scratch::new("timing");
    // Constant tone so a sounding region is clearly non-zero.
    let sample = vec![0.5_f32; RATE as usize];
    write_wav(&dir.join("tone.wav"), &sample);
    let sfz =
        "<region> sample=tone.wav lokey=0 hikey=127 pitch_keycenter=60 loop_mode=loop_continuous\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();
    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    let instrument = SamplerInstrument::new(&bank);

    let start = 24_000_u64;
    let frames = RATE as u64;
    let out = instrument
        .render_notes(
            &[Note {
                pitch: 60,
                start,
                length: 24_000,
                velocity: 127,
            }],
            RATE,
            frames,
        )
        .unwrap();

    // Silence before the note starts.
    assert!(out[..start as usize].iter().all(|&s| s == 0.0));
    // Sound after the note starts.
    let peak = out[start as usize..(start + 24_000) as usize]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "peak {peak}");
}

#[test]
fn sampler_instrument_rejects_wrong_rate() {
    let dir = Scratch::new("rate");
    write_wav(&dir.join("tone.wav"), &vec![0.5_f32; 100]);
    std::fs::write(dir.join("bank.sfz"), "<region> sample=tone.wav\n").unwrap();
    let bank = load_bank(&dir.0, DEFAULT_RAM_BUDGET_MIB).unwrap();
    let instrument = SamplerInstrument::new(&bank);
    assert!(instrument.render_notes(&[], 44_100, 0).is_err());
    assert!(instrument.render_notes(&[], 0, 0).is_err());
}

#[test]
fn pre_render_selects_the_chosen_instrument() {
    use audio_core::session::{Clip, Session, Track as SessionTrack};
    use daw_app::{NoCache, load_session, load_session_cached, render_session};

    // A 440 Hz sine bank at root 60, distinct from the piano timbre.
    let dir = Scratch::new("select");
    let sample = sine(440.0, RATE as usize * 2);
    write_wav(&dir.join("tone.wav"), &sample);
    let sfz =
        "<region> sample=tone.wav lokey=0 hikey=127 pitch_keycenter=60 loop_mode=loop_continuous\n";
    std::fs::write(dir.join("bank.sfz"), sfz).unwrap();

    let note = Note {
        pitch: 60,
        start: 0,
        length: RATE as u64 / 2,
        velocity: 100,
    };

    // Session A: built-in piano. Session B: the same notes through the sampler.
    let make = |instrument| {
        let mut clip = Clip::full(0, RATE as u64 / 2);
        clip.notes = Some(vec![note]);
        let mut track = SessionTrack::new("m");
        track.instrument = instrument;
        track.clips.push(clip);
        let mut session = Session::new("s", RATE, 120.0);
        session.tracks.push(track);
        session
    };

    let piano = make(audio_core::TrackInstrument::BuiltinPiano);
    let sampler = make(audio_core::TrackInstrument::Sampler {
        bank_path: dir.0.to_string_lossy().into_owned(),
    });

    let piano_out =
        render_session(load_session(piano, std::path::Path::new(""), dir.join("p.wav")).unwrap())
            .unwrap();
    let sampler_out =
        render_session(load_session(sampler, std::path::Path::new(""), dir.join("s.wav")).unwrap())
            .unwrap();

    // Both produce audio, and the two instruments differ.
    assert!(piano_out.frames > 0 && sampler_out.frames > 0);
    let read = |p| {
        let (_, buf) = audio_core::wav::read_wav_mono(p).unwrap();
        buf.channel(0).to_vec()
    };
    let p_wav = dir.join("p.wav");
    let s_wav = dir.join("s.wav");
    let a = read(&p_wav);
    let b = read(&s_wav);
    assert_eq!(a.len(), b.len());
    assert!(
        a.iter().zip(&b).any(|(x, y)| x.to_bits() != y.to_bits()),
        "piano and sampler must render differently"
    );

    // A missing bank must fail loudly, not silently fall back to piano.
    let broken = make(audio_core::TrackInstrument::Sampler {
        bank_path: dir.join("nope").to_string_lossy().into_owned(),
    });
    assert!(
        load_session_cached(
            broken,
            std::path::Path::new(""),
            dir.join("x.wav"),
            &mut NoCache
        )
        .is_err()
    );
}
