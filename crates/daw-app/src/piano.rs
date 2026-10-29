//! Sampled piano for MIDI clips (SPEC-039).
//!
//! Plays recordings of the Salamander Grand Piano by Alexander Holm
//! (CC-BY 3.0, see `assets/piano/README.md`). Control thread only: decoding
//! and rendering allocate.

use std::sync::OnceLock;

use audio_core::{Note, SynthError};

/// How long a released note takes to fade out, in seconds.
pub const PIANO_RELEASE_SECONDS: f64 = 0.25;

/// Credit line shown in the About window and the README.
pub const PIANO_CREDIT: &str = "Piano samples: Salamander Grand Piano by Alexander Holm, CC-BY 3.0";

macro_rules! sample {
    ($root:expr, $name:literal) => {
        (
            $root,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/piano/",
                $name,
                ".mp3"
            )) as &'static [u8],
        )
    };
}

/// `(root MIDI pitch, mp3 bytes)`, sorted by pitch.
const SAMPLES: [(u8, &[u8]); 30] = [
    sample!(21, "A0"),
    sample!(24, "C1"),
    sample!(27, "Ds1"),
    sample!(30, "Fs1"),
    sample!(33, "A1"),
    sample!(36, "C2"),
    sample!(39, "Ds2"),
    sample!(42, "Fs2"),
    sample!(45, "A2"),
    sample!(48, "C3"),
    sample!(51, "Ds3"),
    sample!(54, "Fs3"),
    sample!(57, "A3"),
    sample!(60, "C4"),
    sample!(63, "Ds4"),
    sample!(66, "Fs4"),
    sample!(69, "A4"),
    sample!(72, "C5"),
    sample!(75, "Ds5"),
    sample!(78, "Fs5"),
    sample!(81, "A5"),
    sample!(84, "C6"),
    sample!(87, "Ds6"),
    sample!(90, "Fs6"),
    sample!(93, "A6"),
    sample!(96, "C7"),
    sample!(99, "Ds7"),
    sample!(102, "Fs7"),
    sample!(105, "A7"),
    sample!(108, "C8"),
];

/// A decoded sample: mono audio and its sample rate.
struct Decoded {
    rate: u32,
    data: Vec<f32>,
}

fn decoded(index: usize) -> &'static Decoded {
    static CACHE: [OnceLock<Decoded>; 30] = [const { OnceLock::new() }; 30];
    CACHE[index].get_or_init(|| {
        // The bytes are part of the program and checked by a test; a failure
        // here would only make that one sample silent.
        match crate::audio_file::decode_mp3_bytes(SAMPLES[index].1) {
            Ok((spec, buffer)) => {
                let channels = usize::from(spec.channels).max(1);
                let frames = buffer.channel(0).len();
                let mut data = vec![0.0_f32; frames];
                for ch in 0..channels {
                    for (out, s) in data.iter_mut().zip(buffer.channel(ch)) {
                        *out += s;
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                let scale = 1.0 / channels as f32;
                data.iter_mut().for_each(|s| *s *= scale);
                Decoded {
                    rate: spec.sample_rate,
                    data,
                }
            }
            Err(_) => Decoded {
                rate: 44_100,
                data: Vec::new(),
            },
        }
    })
}

/// Index into [`SAMPLES`] of the sample whose root is closest to `pitch`;
/// on a tie the lower one.
fn nearest(pitch: u8) -> usize {
    let mut best = 0;
    for (i, (root, _)) in SAMPLES.iter().enumerate() {
        if root.abs_diff(pitch) < SAMPLES[best].0.abs_diff(pitch) {
            best = i;
        }
    }
    best
}

/// Playback speed that turns a recording of `root` at `sample_rate` into
/// `pitch` at `out_rate`: equal temperament, `2^((pitch − root) / 12)`
/// (12-TET, e.g. J. O. Smith, *Mathematics of the DFT*, "Musical pitch").
#[must_use]
pub fn playback_ratio(pitch: u8, root: u8, sample_rate: u32, out_rate: u32) -> f64 {
    let semis = f64::from(pitch) - f64::from(root);
    2.0_f64.powf(semis / 12.0) * f64::from(sample_rate) / f64::from(out_rate)
}

/// Renders `notes` on the sampled piano into `frames` mono samples at
/// `sample_rate`. Deterministic: equal inputs give bit-identical output.
///
/// # Errors
///
/// [`SynthError::InvalidSampleRate`] for a zero `sample_rate`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn render_piano(notes: &[Note], sample_rate: u32, frames: u64) -> Result<Vec<f32>, SynthError> {
    if sample_rate == 0 {
        return Err(SynthError::InvalidSampleRate);
    }
    let len = usize::try_from(frames).unwrap_or(usize::MAX);
    let mut out = vec![0.0_f32; len];
    let release = (PIANO_RELEASE_SECONDS * f64::from(sample_rate)).round() as u64;
    for note in notes {
        let index = nearest(note.pitch);
        let sample = decoded(index);
        if sample.data.len() < 2 || note.start >= frames {
            continue;
        }
        let ratio = playback_ratio(note.pitch, SAMPLES[index].0, sample.rate, sample_rate);
        let gain = f32::from(note.velocity.min(127)) / 127.0;
        let held = note.length;
        let audible = held.saturating_add(release);
        let last = (sample.data.len() - 1) as f64;
        let end = note.start.saturating_add(audible).min(frames);
        for t in note.start..end {
            let k = t - note.start;
            let pos = k as f64 * ratio;
            if pos >= last {
                break;
            }
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            let a = sample.data[i];
            let b = sample.data[i + 1];
            let mut s = a + (b - a) * frac;
            if k >= held {
                s *= 1.0 - (k - held) as f32 / release.max(1) as f32;
            }
            out[t as usize] += s * gain;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: u64, length: u64, velocity: u8) -> Note {
        Note {
            pitch,
            start,
            length,
            velocity,
        }
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn every_built_in_sample_decodes() {
        for i in 0..SAMPLES.len() {
            let d = decoded(i);
            assert!(d.rate > 0, "sample {i}");
            assert!(d.data.len() > 44_100, "sample {i} has {}", d.data.len());
            assert!(peak(&d.data) > 0.01, "sample {i} is silent");
        }
    }

    #[test]
    fn nearest_sample_prefers_the_closest_root_and_the_lower_on_ties() {
        assert_eq!(SAMPLES[nearest(69)].0, 69);
        assert_eq!(SAMPLES[nearest(70)].0, 69);
        assert_eq!(SAMPLES[nearest(71)].0, 72);
        assert_eq!(SAMPLES[nearest(0)].0, 21);
        assert_eq!(SAMPLES[nearest(127)].0, 108);
    }

    #[test]
    fn an_octave_doubles_the_speed_and_rates_scale_it() {
        assert!((playback_ratio(81, 69, 48_000, 48_000) - 2.0).abs() < 1e-12);
        assert!((playback_ratio(57, 69, 48_000, 48_000) - 0.5).abs() < 1e-12);
        assert!((playback_ratio(69, 69, 44_100, 48_000) - 44_100.0 / 48_000.0).abs() < 1e-12);
    }

    #[test]
    fn zero_rate_is_an_error_and_no_notes_is_silence() {
        assert!(matches!(
            render_piano(&[], 0, 10),
            Err(SynthError::InvalidSampleRate)
        ));
        let out = render_piano(&[], 48_000, 1_000).unwrap();
        assert_eq!(out.len(), 1_000);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_note_sounds_from_its_start_and_fades_after_release() {
        let rate = 48_000;
        let out = render_piano(&[note(60, 1_000, 4_800, 127)], rate, 48_000).unwrap();
        assert!(out[..1_000].iter().all(|&s| s == 0.0));
        assert!(peak(&out[1_000..5_800]) > 0.05);
        let release_end = 1_000 + 4_800 + 12_000;
        assert!(out[release_end..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn velocity_scales_the_level() {
        let loud = render_piano(&[note(64, 0, 9_600, 127)], 48_000, 9_600).unwrap();
        let soft = render_piano(&[note(64, 0, 9_600, 32)], 48_000, 9_600).unwrap();
        let ratio = peak(&soft) / peak(&loud);
        assert!((ratio - 32.0 / 127.0).abs() < 1e-4, "{ratio}");
    }

    #[test]
    fn rendering_is_deterministic_and_clipped_to_the_clip() {
        let notes = [note(40, 0, 2_000, 90), note(90, 500, 100_000, 60)];
        let a = render_piano(&notes, 44_100, 5_000).unwrap();
        let b = render_piano(&notes, 44_100, 5_000).unwrap();
        assert_eq!(a.len(), 5_000);
        assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
    }
}
