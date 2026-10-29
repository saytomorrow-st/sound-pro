//! Track effects (SPEC-022): three-band EQ, compressor and reverb.
//!
//! [`Effect`] is the plain, serialisable description stored in a session;
//! [`EffectProcessor`] is its stateful runtime. All memory is allocated in
//! [`EffectProcessor::new`]; [`EffectProcessor::process`] works in place on a
//! mono block without allocating, locking or panicking.

/// Most effects one track may hold.
pub const MAX_EFFECTS_PER_TRACK: usize = 8;

/// Corner frequency of the EQ low shelf, in hertz.
pub const EQ_LOW_HZ: f32 = 200.0;
/// Corner frequency of the EQ high shelf, in hertz.
pub const EQ_HIGH_HZ: f32 = 5000.0;

/// Parameters of one effect type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectKind {
    /// Three-band equaliser: low shelf at [`EQ_LOW_HZ`], peak with Q = 1 at
    /// `mid_hz`, high shelf at [`EQ_HIGH_HZ`].
    Eq {
        /// Low shelf gain, dB, `-24..=24`.
        low_db: f32,
        /// Peak gain, dB, `-24..=24`.
        mid_db: f32,
        /// Peak centre, Hz, `100..=10000`.
        mid_hz: f32,
        /// High shelf gain, dB, `-24..=24`.
        high_db: f32,
    },
    /// Feed-forward peak compressor.
    Compressor {
        /// Threshold, dBFS, `-60..=0`.
        threshold_db: f32,
        /// Compression ratio, `1..=20`.
        ratio: f32,
        /// Attack time, ms, `0.1..=100`.
        attack_ms: f32,
        /// Release time, ms, `10..=1000`.
        release_ms: f32,
        /// Make-up gain, dB, `0..=24`.
        makeup_db: f32,
    },
    /// Mono Schroeder–Moorer reverb in the Freeverb layout.
    Reverb {
        /// Room size, `0..=1`.
        room: f32,
        /// High-frequency damping, `0..=1`.
        damp: f32,
        /// Wet share, `0..=1`; the dry share is `1 - mix`.
        mix: f32,
    },
}

impl EffectKind {
    /// Flat EQ: 0 dB everywhere, peak at 1 kHz.
    #[must_use]
    pub const fn eq() -> Self {
        Self::Eq {
            low_db: 0.0,
            mid_db: 0.0,
            mid_hz: 1000.0,
            high_db: 0.0,
        }
    }

    /// Compressor at -18 dB, 4:1, 10 ms attack, 100 ms release, no make-up.
    #[must_use]
    pub const fn compressor() -> Self {
        Self::Compressor {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
        }
    }

    /// Medium room, medium damping, 25 % wet.
    #[must_use]
    pub const fn reverb() -> Self {
        Self::Reverb {
            room: 0.5,
            damp: 0.5,
            mix: 0.25,
        }
    }

    /// Type tag used in project files: `"eq"`, `"compressor"` or `"reverb"`.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Eq { .. } => "eq",
            Self::Compressor { .. } => "compressor",
            Self::Reverb { .. } => "reverb",
        }
    }

    /// Every parameter is finite and inside its SPEC-022 range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let within =
            |value: f32, low: f32, high: f32| value.is_finite() && (low..=high).contains(&value);
        match *self {
            Self::Eq {
                low_db,
                mid_db,
                mid_hz,
                high_db,
            } => {
                within(low_db, -24.0, 24.0)
                    && within(mid_db, -24.0, 24.0)
                    && within(mid_hz, 100.0, 10_000.0)
                    && within(high_db, -24.0, 24.0)
            }
            Self::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => {
                within(threshold_db, -60.0, 0.0)
                    && within(ratio, 1.0, 20.0)
                    && within(attack_ms, 0.1, 100.0)
                    && within(release_ms, 10.0, 1000.0)
                    && within(makeup_db, 0.0, 24.0)
            }
            Self::Reverb { room, damp, mix } => {
                within(room, 0.0, 1.0) && within(damp, 0.0, 1.0) && within(mix, 0.0, 1.0)
            }
        }
    }
}

/// One slot in a track's effect chain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effect {
    /// When `true` the slot passes audio through untouched.
    pub bypass: bool,
    /// Effect type and parameters.
    pub kind: EffectKind,
}

impl Effect {
    /// Active (not bypassed) slot.
    #[must_use]
    pub const fn new(kind: EffectKind) -> Self {
        Self {
            bypass: false,
            kind,
        }
    }
}

/// Values below this are flushed to zero so filter state never goes
/// denormal and silence decays to exact zeros.
const DENORMAL: f32 = 1.0e-20;

fn flush(value: f32) -> f32 {
    if value.abs() < DENORMAL { 0.0 } else { value }
}

/// `10^(db / 20)`.
fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Second-order IIR section in transposed direct form II, `f64` state.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

/// Which Audio EQ Cookbook response to design.
#[derive(Debug, Clone, Copy)]
enum Shape {
    LowShelf,
    Peak,
    HighShelf,
}

impl Biquad {
    /// Coefficients from R. Bristow-Johnson, "Cookbook formulae for audio EQ
    /// biquad filter coefficients":
    ///
    /// `A = 10^(dB/40)`, `w0 = 2π·f0/Fs`.
    /// Peak (Q = 1): `α = sin w0 / 2`,
    /// `b = [1 + αA, −2cos w0, 1 − αA]`, `a = [1 + α/A, −2cos w0, 1 − α/A]`.
    /// Shelves (S = 1): `α = sin w0 / 2 · √2`, `k = 2√A·α`,
    /// low: `b = [A((A+1) − (A−1)c + k), 2A((A−1) − (A+1)c), A((A+1) − (A−1)c − k)]`,
    /// `a = [(A+1) + (A−1)c + k, −2((A−1) + (A+1)c), (A+1) + (A−1)c − k]`;
    /// high: the same with every `(A−1)` replaced by `−(A−1)`.
    ///
    /// `f0` is clamped below Nyquist so low sample rates stay stable.
    fn design(shape: Shape, f0: f32, db: f32, sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate);
        let f0 = f64::from(f0).min(rate * 0.45);
        let a = 10.0_f64.powf(f64::from(db) / 40.0);
        let w0 = std::f64::consts::TAU * f0 / rate;
        let (sin, cos) = w0.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peak => {
                let alpha = sin / 2.0;
                (
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            Shape::LowShelf | Shape::HighShelf => {
                let alpha = sin / 2.0 * std::f64::consts::SQRT_2;
                let k = 2.0 * a.sqrt() * alpha;
                let direction = if matches!(shape, Shape::LowShelf) {
                    1.0
                } else {
                    -1.0
                };
                let am1 = direction * (a - 1.0);
                (
                    a * ((a + 1.0) - am1 * cos + k),
                    2.0 * a * (am1 - (a + 1.0) * cos),
                    a * ((a + 1.0) - am1 * cos - k),
                    (a + 1.0) + am1 * cos + k,
                    -2.0 * (am1 + (a + 1.0) * cos),
                    (a + 1.0) + am1 * cos - k,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        for sample in samples {
            let x = f64::from(*sample);
            let y = self.b0.mul_add(x, self.z1);
            self.z1 = self.b1.mul_add(x, self.z2) - self.a1 * y;
            self.z2 = self.b2 * x - self.a2 * y;
            if self.z1.abs() < f64::from(DENORMAL) {
                self.z1 = 0.0;
            }
            if self.z2.abs() < f64::from(DENORMAL) {
                self.z2 = 0.0;
            }
            #[allow(clippy::cast_possible_truncation, reason = "audio samples are f32")]
            let out = y as f32;
            *sample = out;
        }
    }
}

/// Feed-forward peak compressor.
///
/// Envelope: one-pole follower `e ← c·e + (1 − c)·|x|` with
/// `c = exp(−1 / (t·Fs))`, `t` = attack while `|x| > e`, release otherwise.
/// Gain computer: `L = 20·log10 e`; above the threshold `T` the gain drops by
/// `(L − T)(1 − 1/ratio)` dB; make-up is added afterwards.
#[derive(Debug, Clone, Copy)]
struct Compressor {
    threshold_db: f32,
    slope: f32,
    attack: f32,
    release: f32,
    makeup: f32,
    envelope: f32,
}

impl Compressor {
    #[allow(
        clippy::cast_precision_loss,
        reason = "sample rates are far below 2^24"
    )]
    fn new(
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        sample_rate: u32,
    ) -> Self {
        let rate = sample_rate as f32;
        let coefficient = |ms: f32| (-1.0 / (ms.max(0.001) * 0.001 * rate)).exp();
        Self {
            threshold_db,
            slope: 1.0 - 1.0 / ratio.max(1.0),
            attack: coefficient(attack_ms),
            release: coefficient(release_ms),
            makeup: db_to_gain(makeup_db),
            envelope: 0.0,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        let threshold = db_to_gain(self.threshold_db);
        for sample in samples {
            let level = sample.abs();
            let c = if level > self.envelope {
                self.attack
            } else {
                self.release
            };
            self.envelope = flush(c.mul_add(self.envelope, (1.0 - c) * level));
            let mut gain = self.makeup;
            if self.envelope > threshold && self.slope > 0.0 {
                let over_db = 20.0 * (self.envelope / threshold).log10();
                gain *= db_to_gain(-over_db * self.slope);
            }
            // Multiplying by exactly 1.0 is exact, so no special case.
            *sample *= gain;
        }
    }
}

/// Freeverb comb delays at 44.1 kHz (Jezar, public domain, 2000).
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Freeverb all-pass delays at 44.1 kHz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
/// Input attenuation before the comb bank.
const REVERB_INPUT_GAIN: f32 = 0.015;
/// Wet level that makes `mix = 1` roughly as loud as the dry signal.
const REVERB_WET_SCALE: f32 = 3.0;

/// Lowpass-feedback comb: `y = buf[i]; s = y(1 − d) + s·d; buf[i] = x + s·fb`.
#[derive(Debug, Clone)]
struct Comb {
    buffer: Box<[f32]>,
    index: usize,
    store: f32,
}

impl Comb {
    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let Some(slot) = self.buffer.get_mut(self.index) else {
            return 0.0;
        };
        let output = *slot;
        self.store = flush(output.mul_add(1.0 - damp, self.store * damp));
        *slot = flush(self.store.mul_add(feedback, input));
        self.index += 1;
        if self.index >= self.buffer.len() {
            self.index = 0;
        }
        output
    }
}

/// Schroeder all-pass with gain 0.5: `b = buf[i]; y = b − x; buf[i] = x + b/2`.
#[derive(Debug, Clone)]
struct Allpass {
    buffer: Box<[f32]>,
    index: usize,
}

impl Allpass {
    fn tick(&mut self, input: f32) -> f32 {
        let Some(slot) = self.buffer.get_mut(self.index) else {
            return input;
        };
        let delayed = *slot;
        *slot = flush(delayed.mul_add(0.5, input));
        self.index += 1;
        if self.index >= self.buffer.len() {
            self.index = 0;
        }
        delayed - input
    }
}

/// Mono reverb: eight parallel [`Comb`]s summed, then four [`Allpass`]es in
/// series. Feedback `0.7 + 0.28·room`, damping `0.4·damp`.
#[derive(Debug, Clone)]
struct Reverb {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
    feedback: f32,
    damp: f32,
    wet: f32,
    dry: f32,
}

impl Reverb {
    fn new(room: f32, damp: f32, mix: f32, sample_rate: u32) -> Self {
        let scale = |length: usize| {
            let scaled = length as u64 * u64::from(sample_rate) / 44_100;
            usize::try_from(scaled).unwrap_or(length).max(1)
        };
        Self {
            combs: COMB_TUNING
                .iter()
                .map(|&length| Comb {
                    buffer: vec![0.0; scale(length)].into_boxed_slice(),
                    index: 0,
                    store: 0.0,
                })
                .collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|&length| Allpass {
                    buffer: vec![0.0; scale(length)].into_boxed_slice(),
                    index: 0,
                })
                .collect(),
            feedback: 0.28f32.mul_add(room, 0.7),
            damp: 0.4 * damp,
            wet: mix * REVERB_WET_SCALE,
            dry: 1.0 - mix,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        if self.wet == 0.0 {
            return;
        }
        for sample in samples {
            let input = *sample * REVERB_INPUT_GAIN;
            let mut wet = 0.0;
            for comb in &mut self.combs {
                wet += comb.tick(input, self.feedback, self.damp);
            }
            for allpass in &mut self.allpasses {
                wet = allpass.tick(wet);
            }
            *sample = sample.mul_add(self.dry, wet * self.wet);
        }
    }
}

/// Runtime state of one effect slot.
#[derive(Debug, Clone)]
pub struct EffectProcessor {
    state: State,
}

#[derive(Debug, Clone)]
enum State {
    Bypass,
    Eq(Vec<Biquad>),
    Compressor(Compressor),
    Reverb(Reverb),
}

impl EffectProcessor {
    /// Allocates every buffer `effect` needs at `sample_rate`.
    ///
    /// Out-of-range parameters are not rejected here; validate the session
    /// first ([`crate::Session::validate`]).
    #[must_use]
    pub fn new(effect: &Effect, sample_rate: u32) -> Self {
        let state = if effect.bypass || sample_rate == 0 {
            State::Bypass
        } else {
            match effect.kind {
                EffectKind::Eq {
                    low_db,
                    mid_db,
                    mid_hz,
                    high_db,
                } => State::Eq(
                    [
                        (Shape::LowShelf, EQ_LOW_HZ, low_db),
                        (Shape::Peak, mid_hz, mid_db),
                        (Shape::HighShelf, EQ_HIGH_HZ, high_db),
                    ]
                    .into_iter()
                    .filter(|&(_, _, db)| db != 0.0)
                    .map(|(shape, hz, db)| Biquad::design(shape, hz, db, sample_rate))
                    .collect(),
                ),
                EffectKind::Compressor {
                    threshold_db,
                    ratio,
                    attack_ms,
                    release_ms,
                    makeup_db,
                } => State::Compressor(Compressor::new(
                    threshold_db,
                    ratio,
                    attack_ms,
                    release_ms,
                    makeup_db,
                    sample_rate,
                )),
                EffectKind::Reverb { room, damp, mix } => {
                    State::Reverb(Reverb::new(room, damp, mix, sample_rate))
                }
            }
        };
        Self { state }
    }

    /// Processes a mono block in place. Real-time safe.
    pub fn process(&mut self, samples: &mut [f32]) {
        match &mut self.state {
            State::Bypass => {}
            State::Eq(bands) => {
                for band in bands {
                    band.process(samples);
                }
            }
            State::Compressor(compressor) => compressor.process(samples),
            State::Reverb(reverb) => reverb.process(samples),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[allow(clippy::cast_precision_loss, reason = "test signal lengths are small")]
    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn run(kind: EffectKind, input: &[f32]) -> Vec<f32> {
        let mut processor = EffectProcessor::new(&Effect::new(kind), RATE);
        let mut out = input.to_vec();
        for block in out.chunks_mut(128) {
            processor.process(block);
        }
        out
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |max, s| max.max(s.abs()))
    }

    fn eq(low_db: f32, mid_db: f32, mid_hz: f32, high_db: f32) -> EffectKind {
        EffectKind::Eq {
            low_db,
            mid_db,
            mid_hz,
            high_db,
        }
    }

    #[test]
    fn defaults_are_valid() {
        for kind in [
            EffectKind::eq(),
            EffectKind::compressor(),
            EffectKind::reverb(),
        ] {
            assert!(kind.is_valid(), "{kind:?}");
        }
    }

    #[test]
    fn out_of_range_and_nan_parameters_are_invalid() {
        assert!(!eq(25.0, 0.0, 1000.0, 0.0).is_valid());
        assert!(!eq(0.0, 0.0, f32::NAN, 0.0).is_valid());
        assert!(!eq(0.0, 0.0, 50.0, 0.0).is_valid());
        let compressor =
            |threshold_db, ratio, attack_ms, release_ms, makeup_db| EffectKind::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            };
        assert!(!compressor(-18.0, 0.5, 10.0, 100.0, 0.0).is_valid());
        assert!(!compressor(1.0, 4.0, 10.0, 100.0, 0.0).is_valid());
        assert!(!compressor(-18.0, 4.0, 0.0, 100.0, 0.0).is_valid());
        assert!(!compressor(-18.0, 4.0, 10.0, 5.0, 0.0).is_valid());
        assert!(!compressor(-18.0, 4.0, 10.0, 100.0, -1.0).is_valid());
        assert!(
            !EffectKind::Reverb {
                room: 1.5,
                damp: 0.5,
                mix: 0.5
            }
            .is_valid()
        );
        assert!(
            !EffectKind::Reverb {
                room: 0.5,
                damp: 0.5,
                mix: f32::INFINITY
            }
            .is_valid()
        );
    }

    #[test]
    fn flat_eq_is_bit_exact() {
        let input = sine(440.0, 0.8, 4096);
        assert_eq!(run(EffectKind::eq(), &input), input);
    }

    #[test]
    fn bypassed_effects_are_bit_exact() {
        let input = sine(440.0, 0.8, 4096);
        for kind in [
            eq(12.0, -6.0, 800.0, 6.0),
            EffectKind::compressor(),
            EffectKind::reverb(),
        ] {
            let mut processor = EffectProcessor::new(&Effect { bypass: true, kind }, RATE);
            let mut out = input.clone();
            processor.process(&mut out);
            assert_eq!(out, input, "{kind:?}");
        }
    }

    #[test]
    fn silence_stays_exact_silence() {
        let input = vec![0.0; 8192];
        for kind in [
            eq(12.0, -6.0, 800.0, 6.0),
            EffectKind::compressor(),
            EffectKind::reverb(),
        ] {
            assert!(run(kind, &input).iter().all(|&s| s == 0.0), "{kind:?}");
        }
    }

    #[test]
    fn low_shelf_boosts_lows_and_leaves_highs() {
        let kind = eq(12.0, 0.0, 1000.0, 0.0);
        let low = run(kind, &sine(50.0, 0.1, 48_000));
        let high = run(kind, &sine(10_000.0, 0.1, 48_000));
        let low_gain = peak(&low[24_000..]) / 0.1;
        let high_gain = peak(&high[24_000..]) / 0.1;
        assert!((3.5..4.3).contains(&low_gain), "50 Hz gain {low_gain}");
        assert!((0.95..1.06).contains(&high_gain), "10 kHz gain {high_gain}");
    }

    #[test]
    fn high_shelf_boosts_highs_and_leaves_lows() {
        let kind = eq(0.0, 0.0, 1000.0, -12.0);
        let low = run(kind, &sine(50.0, 0.5, 48_000));
        let high = run(kind, &sine(15_000.0, 0.5, 48_000));
        let low_gain = peak(&low[24_000..]) / 0.5;
        let high_gain = peak(&high[24_000..]) / 0.5;
        assert!((0.97..1.03).contains(&low_gain), "50 Hz gain {low_gain}");
        assert!((0.23..0.30).contains(&high_gain), "15 kHz gain {high_gain}");
    }

    #[test]
    fn peak_hits_its_centre_frequency() {
        let kind = eq(0.0, 6.0, 2000.0, 0.0);
        let centre = run(kind, &sine(2000.0, 0.1, 48_000));
        let far = run(kind, &sine(100.0, 0.1, 48_000));
        let centre_gain = peak(&centre[24_000..]) / 0.1;
        let far_gain = peak(&far[24_000..]) / 0.1;
        assert!(
            (1.97..2.02).contains(&centre_gain),
            "2 kHz gain {centre_gain}"
        );
        assert!((0.98..1.05).contains(&far_gain), "100 Hz gain {far_gain}");
    }

    #[test]
    fn compressor_below_threshold_without_makeup_is_bit_exact() {
        let input = sine(440.0, 0.05, 8192); // about -26 dBFS, threshold -18
        assert_eq!(run(EffectKind::compressor(), &input), input);
    }

    #[test]
    fn compressor_makeup_applies_below_threshold() {
        let input = sine(440.0, 0.05, 8192);
        let kind = EffectKind::Compressor {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 6.0,
        };
        let gain = peak(&run(kind, &input)) / 0.05;
        assert!((1.98..2.0).contains(&gain), "{gain}");
    }

    #[test]
    fn hard_compressor_pulls_a_loud_sine_down() {
        let kind = EffectKind::Compressor {
            threshold_db: -20.0,
            ratio: 20.0,
            attack_ms: 1.0,
            release_ms: 100.0,
            makeup_db: 0.0,
        };
        let out = run(kind, &sine(440.0, 1.0, 48_000));
        let level_db = 20.0 * peak(&out[24_000..]).log10();
        assert!(level_db <= -17.0, "{level_db} dB");
        assert!(level_db > -24.0, "{level_db} dB");
    }

    #[test]
    fn reverb_leaves_a_tail_after_an_impulse() {
        let mut input = vec![0.0; RATE as usize];
        input[0] = 1.0;
        let out = run(EffectKind::reverb(), &input);
        let tail = &out[RATE as usize / 2..];
        assert!(peak(tail) > 1.0e-5, "tail peak {}", peak(tail));
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn dry_reverb_is_bit_exact() {
        let input = sine(440.0, 0.8, 4096);
        let kind = EffectKind::Reverb {
            room: 0.9,
            damp: 0.2,
            mix: 0.0,
        };
        assert_eq!(run(kind, &input), input);
    }

    #[test]
    fn extreme_reverb_stays_bounded() {
        let kind = EffectKind::Reverb {
            room: 1.0,
            damp: 0.0,
            mix: 1.0,
        };
        let out = run(kind, &sine(220.0, 1.0, RATE as usize * 3));
        assert!(peak(&out) < 8.0, "{}", peak(&out));
    }

    #[test]
    fn low_sample_rates_stay_stable() {
        let mut processor = EffectProcessor::new(&Effect::new(eq(6.0, 6.0, 10_000.0, 6.0)), 8000);
        let mut block = sine(440.0, 0.5, 8000);
        processor.process(&mut block);
        assert!(block.iter().all(|s| s.is_finite() && s.abs() < 10.0));
    }
}
