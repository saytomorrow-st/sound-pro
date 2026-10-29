//! Offline sample-rate conversion (SPEC-019).
//!
//! Band-limited interpolation after Julius O. Smith, "Digital Audio
//! Resampling" (ccrma.stanford.edu/~jos/resample). An output frame `n` sits at
//! input time `t = n · from / to`; its value is
//!
//! ```text
//! y[n] = Σ_k x[k] · c · sinc(c · (t − k)) · w((t − k) · c / Z)
//! ```
//!
//! with `c = 0.95 · min(1, to / from)` the cutoff relative to the input
//! Nyquist, `Z` zero crossings per side and `w` a Blackman window on `[-1, 1]`.
//! Samples outside the input are zeros. The kernel `sinc · w` is read from a
//! table with linear interpolation.
//!
//! This allocates and is meant for loading files, never for the audio
//! callback.

use std::sync::OnceLock;

use thiserror::Error;

/// Zero crossings of the kernel on each side.
pub const ZERO_CROSSINGS: usize = 32;

/// Table entries per zero crossing.
const RESOLUTION: usize = 512;

/// Fraction of the lower Nyquist frequency that passes.
const CUTOFF: f64 = 0.95;

/// Errors from [`resample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResampleError {
    /// A sample rate was zero.
    #[error("sample rate must be positive")]
    ZeroRate,
}

/// Converts mono `input` from `from` Hz to `to` Hz.
///
/// The output holds `ceil(len · to / from)` frames; equal rates return an
/// exact copy.
///
/// # Errors
///
/// [`ResampleError::ZeroRate`] when either rate is zero.
pub fn resample(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>, ResampleError> {
    if from == 0 || to == 0 {
        return Err(ResampleError::ZeroRate);
    }
    if from == to {
        return Ok(input.to_vec());
    }
    let len = input.len() as u64;
    let (from64, to64) = (u64::from(from), u64::from(to));
    let out_len = usize::try_from((len * to64).div_ceil(from64)).unwrap_or(usize::MAX);
    let table = kernel_table();
    let c = CUTOFF * (f64::from(to) / f64::from(from)).min(1.0);
    #[allow(clippy::cast_precision_loss)]
    let zeros = ZERO_CROSSINGS as f64;
    let half_width = zeros / c;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len as u64 {
        // Exact position: whole input frame plus a fraction.
        let whole = n * from64 / to64;
        #[allow(clippy::cast_precision_loss)]
        let t = whole as f64 + ((n * from64) % to64) as f64 / f64::from(to);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first = (t - half_width).ceil().max(0.0) as u64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let last = ((t + half_width).floor() as u64).min(len.saturating_sub(1));
        let mut acc = 0.0_f64;
        if len > 0 {
            for k in first..=last {
                #[allow(clippy::cast_possible_truncation)]
                let sample = input[k as usize];
                #[allow(clippy::cast_precision_loss)]
                let x = (t - k as f64).abs() * c;
                acc += f64::from(sample) * kernel(table, x);
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        out.push((acc * c) as f32);
    }
    Ok(out)
}

/// `sinc(x) · w(x / Z)` for `x ≥ 0` in zero-crossing units.
fn kernel(table: &[f64], x: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let pos = x * RESOLUTION as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = pos as usize;
    if i + 1 >= table.len() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let frac = pos - i as f64;
    table[i] + (table[i + 1] - table[i]) * frac
}

fn kernel_table() -> &'static [f64] {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let size = ZERO_CROSSINGS * RESOLUTION + 2;
        (0..size)
            .map(|i| {
                // Small table indices and constants: exact in f64.
                #[allow(clippy::cast_precision_loss)]
                let (x, zeros) = (i as f64 / RESOLUTION as f64, ZERO_CROSSINGS as f64);
                sinc(x) * blackman(x / zeros)
            })
            .collect()
    })
}

/// Normalised sinc: `sin(πx) / (πx)`, 1 at 0.
fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

/// Blackman window centred at 0, zero at `|u| ≥ 1`.
fn blackman(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        return 0.0;
    }
    let a = std::f64::consts::PI * (u + 1.0);
    0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let v = (std::f64::consts::TAU * freq * i as f64 / f64::from(rate)).sin() as f32;
                v
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let n = x.len() as f64;
        (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / n).sqrt()
    }

    #[test]
    fn zero_rate_is_an_error() {
        assert_eq!(resample(&[1.0], 0, 48_000), Err(ResampleError::ZeroRate));
        assert_eq!(resample(&[1.0], 44_100, 0), Err(ResampleError::ZeroRate));
    }

    #[test]
    fn equal_rates_copy_bit_for_bit() {
        let x = [0.1_f32, -0.7, 0.3, f32::MIN_POSITIVE];
        let y = resample(&x, 44_100, 44_100).unwrap();
        assert_eq!(
            x.map(f32::to_bits).to_vec(),
            y.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn output_length_is_rounded_up() {
        assert_eq!(resample(&[], 44_100, 48_000).unwrap().len(), 0);
        assert_eq!(resample(&[0.5], 44_100, 48_000).unwrap().len(), 2);
        assert_eq!(
            resample(&vec![0.0; 44_100], 44_100, 48_000).unwrap().len(),
            48_000
        );
        assert_eq!(resample(&[0.0; 3], 96_000, 48_000).unwrap().len(), 2);
    }

    #[test]
    fn a_sine_survives_upsampling() {
        let input = sine(1000.0, 44_100, 4410);
        let out = resample(&input, 44_100, 48_000).unwrap();
        let ideal = sine(1000.0, 48_000, out.len());
        for i in 1000..out.len() - 1000 {
            assert!(
                (out[i] - ideal[i]).abs() < 1e-3,
                "frame {i}: {} vs {}",
                out[i],
                ideal[i]
            );
        }
    }

    #[test]
    fn a_sine_survives_downsampling() {
        let input = sine(1000.0, 96_000, 9600);
        let out = resample(&input, 96_000, 48_000).unwrap();
        let ideal = sine(1000.0, 48_000, out.len());
        for i in 1000..out.len() - 1000 {
            assert!(
                (out[i] - ideal[i]).abs() < 1e-3,
                "frame {i}: {} vs {}",
                out[i],
                ideal[i]
            );
        }
    }

    #[test]
    fn a_constant_stays_constant() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (96_000, 48_000)] {
            let out = resample(&[1.0; 2000], from, to).unwrap();
            for (i, &v) in out.iter().enumerate().skip(200).take(out.len() - 400) {
                assert!((v - 1.0).abs() < 1e-3, "{from}->{to} frame {i}: {v}");
            }
        }
    }

    #[test]
    fn tones_above_the_new_nyquist_are_removed() {
        let input = sine(30_000.0, 96_000, 9600);
        let out = resample(&input, 96_000, 48_000).unwrap();
        let level = rms(&out[500..out.len() - 500]);
        assert!(level < 0.01, "rms {level}");
    }

    #[test]
    fn the_result_is_deterministic() {
        let input = sine(440.0, 44_100, 1000);
        let a = resample(&input, 44_100, 48_000).unwrap();
        let b = resample(&input, 44_100, 48_000).unwrap();
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}
