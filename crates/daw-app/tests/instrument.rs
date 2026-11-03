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
    // A legacy absolute path still resolves directly (backward compatibility).
    let sampler = make(audio_core::TrackInstrument::Sampler {
        bank_id: dir.0.to_string_lossy().into_owned(),
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

    // A missing bank falls back to the built-in piano rather than failing
    // (SPEC-065 redesign): it renders the same audio as the piano track.
    let broken = make(audio_core::TrackInstrument::Sampler {
        bank_id: dir.join("nope").to_string_lossy().into_owned(),
    });
    let broken_out = render_session(
        load_session_cached(
            broken,
            std::path::Path::new(""),
            dir.join("x.wav"),
            &mut NoCache,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(broken_out.frames > 0);
    let x_wav = dir.join("x.wav");
    let x = read(&x_wav);
    assert_eq!(
        x, a,
        "a broken sampler bank must fall back to the piano timbre"
    );
}

/// Points bank-id resolution at a private data directory for the duration of
/// the closure, so a test can install a synthetic bank into the registry
/// without touching the user's real `sound-pro/banks`. The tests of this file
/// that call `load_session_cached` with a sampler share this lock: the
/// environment is process-wide.
fn with_bank_data_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var_os("XDG_DATA_HOME");
    // SAFETY: `set_var` is unsafe because the environment is process-global;
    // `LOCK` serialises every write here, and no other test in this binary
    // reads `XDG_DATA_HOME`, so no thread observes a concurrent mutation.
    unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
    // A panic in `body` must not leak the override into later tests.
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: as above, guarded by `LOCK` for the whole closure.
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                    None => std::env::remove_var("XDG_DATA_HOME"),
                }
            }
        }
    }
    let _restore = Restore(previous);
    body()
}

/// Installs a synthetic one-region SFZ bank under `data` and records it in the
/// registry, returning its id.
fn install_synthetic_bank(data: &Path, id: &str, root_pitch: u8) -> String {
    use daw_app::bank_library::{InstalledBank, InstalledRegistry, REGISTRY_SCHEMA};

    let bank_dir = data.join("sound-pro").join("banks").join(id);
    std::fs::create_dir_all(&bank_dir).unwrap();
    // A 440 Hz sine at `root_pitch`; an octave up must double its pitch.
    write_wav(&bank_dir.join("tone.wav"), &sine(440.0, RATE as usize));
    let sfz = format!(
        "<region> sample=tone.wav lokey=0 hikey=127 pitch_keycenter={root_pitch} \
         loop_mode=loop_continuous\n"
    );
    std::fs::write(bank_dir.join("bank.sfz"), sfz).unwrap();

    let registry = InstalledRegistry {
        schema: REGISTRY_SCHEMA,
        banks: vec![InstalledBank {
            id: id.to_owned(),
            name: id.to_owned(),
            family: "test".to_owned(),
            path: bank_dir.to_string_lossy().into_owned(),
            license_spdx: "CC0-1.0".to_owned(),
            attribution: String::new(),
            license_acknowledged: false,
            installed_at: "0T00:00:00Z".to_owned(),
        }],
    };
    registry
        .save(&data.join("sound-pro").join("banks").join("banks.json"))
        .unwrap();
    id.to_owned()
}

/// One MIDI-clip track carrying `instrument`, half a second long.
fn midi_session(instrument: audio_core::TrackInstrument) -> audio_core::Session {
    use audio_core::session::{Clip, Session, Track as SessionTrack};

    let mut clip = Clip::full(0, RATE as u64 / 2);
    clip.notes = Some(vec![Note {
        pitch: 60,
        start: 0,
        length: RATE as u64 / 2,
        velocity: 100,
    }]);
    let mut track = SessionTrack::new("sampler");
    track.instrument = instrument;
    track.clips.push(clip);
    let mut session = Session::new("sampler-export", RATE, 120.0);
    session.tracks.push(track);
    session
}

fn read_mono(path: &Path) -> Vec<f32> {
    audio_core::wav::read_wav_mono(path)
        .unwrap()
        .1
        .channel(0)
        .to_vec()
}

#[test]
fn export_renders_a_sampler_instrument_not_the_fallback() {
    use audio_core::TrackInstrument;
    use daw_app::{NoCache, load_session_cached, render_session};

    let dir = Scratch::new("export-sampler");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    let (sampler_out, sampler_wav, piano_wav) = with_bank_data_dir(&data, || {
        let bank_id = install_synthetic_bank(&data, "synthetic-sine", 69);
        let sampler_session = midi_session(TrackInstrument::Sampler { bank_id });
        let piano_session = midi_session(TrackInstrument::BuiltinPiano);

        let sampler = render_session(
            load_session_cached(
                sampler_session,
                Path::new(""),
                dir.join("sampler.wav"),
                &mut NoCache,
            )
            .expect("sampler session loads"),
        )
        .expect("sampler session renders");
        let _piano = render_session(
            load_session_cached(
                piano_session,
                Path::new(""),
                dir.join("piano.wav"),
                &mut NoCache,
            )
            .expect("piano session loads"),
        )
        .expect("piano session renders");
        (sampler, dir.join("sampler.wav"), dir.join("piano.wav"))
    });

    assert!(sampler_out.frames > 0, "export wrote no frames");
    let sampler = read_mono(&sampler_wav);
    let piano = read_mono(&piano_wav);

    let peak = sampler.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(
        peak > 0.01,
        "exported sampler clip must not be silent: {peak}"
    );

    assert_eq!(
        sampler.len(),
        piano.len(),
        "both renders cover the same session length"
    );
    assert!(
        sampler
            .iter()
            .zip(&piano)
            .any(|(a, b)| a.to_bits() != b.to_bits()),
        "the exported audio must come from the sampler bank, not the piano fallback"
    );
}

#[test]
fn export_falls_back_to_piano_for_a_missing_bank() {
    use audio_core::TrackInstrument;
    use daw_app::{NoCache, load_session_cached, render_session};

    let dir = Scratch::new("export-fallback");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let missing = dir.join("no-such-bank").to_string_lossy().into_owned();

    with_bank_data_dir(&data, || {
        let report = render_session(
            load_session_cached(
                midi_session(TrackInstrument::Sampler { bank_id: missing }),
                Path::new(""),
                dir.join("missing.wav"),
                &mut NoCache,
            )
            .expect("a missing bank must not block loading"),
        )
        .expect("a missing bank must fall back to the piano, not error");

        assert!(report.frames > 0, "fallback export wrote no frames");
        let rendered = read_mono(&dir.join("missing.wav"));
        let peak = rendered.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "fallback render must not be silent: {peak}");

        // The fallback timbre is exactly the built-in piano.
        let _piano = render_session(
            load_session_cached(
                midi_session(TrackInstrument::BuiltinPiano),
                Path::new(""),
                dir.join("piano.wav"),
                &mut NoCache,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(_piano.frames > 0);
        assert_eq!(
            rendered,
            read_mono(&dir.join("piano.wav")),
            "a missing bank must render the built-in piano"
        );
    });
}

#[test]
fn hermite_export_differs_from_linear_and_is_deterministic() {
    use audio_core::{Interpolation, TrackInstrument};
    use daw_app::{NoCache, load_session_cached_with, render_session};

    let dir = Scratch::new("export-interp");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    let render = |name: &str, mode: Interpolation| {
        let out = dir.join(name);
        with_bank_data_dir(&data, || {
            let bank_id = install_synthetic_bank(&data, "synthetic-sine", 69);
            let session = midi_session(TrackInstrument::Sampler { bank_id });
            let job =
                load_session_cached_with(session, Path::new(""), out.clone(), &mut NoCache, mode)
                    .expect("session loads");
            render_session(job).expect("session renders");
        });
        read_mono(&out)
    };

    let linear = render("linear.wav", Interpolation::Linear);
    let hermite_a = render("hermite-a.wav", Interpolation::Hermite4);
    let hermite_b = render("hermite-b.wav", Interpolation::Hermite4);
    let linear_b = render("linear-b.wav", Interpolation::Linear);

    assert_eq!(linear.len(), hermite_a.len());
    assert!(
        linear
            .iter()
            .zip(&hermite_a)
            .any(|(a, b)| a.to_bits() != b.to_bits()),
        "hermite export must differ from linear"
    );
    assert!(
        hermite_a
            .iter()
            .zip(&hermite_b)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    assert!(
        linear
            .iter()
            .zip(&linear_b)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
}
