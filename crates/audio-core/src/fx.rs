//! Track effects (SPEC-022, SPEC-051): three-band EQ, eight-band channel EQ,
//! compressor and reverb.
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

/// One band of the eight-band channel EQ (SPEC-051). The band index fixes
/// the shape: 0 high-pass, 1 low shelf, 2–5 peak, 6 high shelf, 7 low-pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelEqBand {
    /// Whether the band is applied at all.
    pub enabled: bool,
    /// Corner or centre frequency, Hz, `20..=20000`.
    pub hz: f32,
    /// Gain, dB, `-24..=24`; ignored by the filter bands 0 and 7.
    pub db: f32,
    /// Quality factor, `0.1..=18`; ignored by every band except peaks 2–5.
    pub q: f32,
}

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
    /// Eight-band channel EQ (SPEC-051): high-pass, low shelf, four peaks,
    /// high shelf and low-pass, in that order.
    ChannelEq {
        /// The eight bands; the index determines the shape.
        bands: [ChannelEqBand; 8],
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

    /// Flat channel EQ (SPEC-051): filters disabled, every gain at 0 dB, so
    /// the effect passes audio through bit-exactly.
    #[must_use]
    pub const fn channel_eq() -> Self {
        Self::ChannelEq {
            bands: [
                ChannelEqBand {
                    enabled: false,
                    hz: 20.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 100.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 300.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 1000.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 3000.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 8000.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: true,
                    hz: 10_000.0,
                    db: 0.0,
                    q: 1.0,
                },
                ChannelEqBand {
                    enabled: false,
                    hz: 20_000.0,
                    db: 0.0,
                    q: 1.0,
                },
            ],
        }
    }

    /// Type tag used in project files, e.g. `"eq"` or `"channel_eq"`.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Eq { .. } => "eq",
            Self::Compressor { .. } => "compressor",
            Self::Reverb { .. } => "reverb",
            Self::ChannelEq { .. } => "channel_eq",
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
            Self::ChannelEq { bands } => bands.iter().all(|band| {
                within(band.hz, 20.0, 20_000.0)
                    && within(band.db, -24.0, 24.0)
                    && within(band.q, 0.1, 18.0)
            }),
        }
    }
}

/// One automatable parameter of a stock effect (SPEC-041). Names match the
/// keys of the effect in project files and are unique across effect types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectParam {
    /// EQ low shelf gain, dB.
    LowDb,
    /// EQ peak gain, dB.
    MidDb,
    /// EQ peak centre, Hz.
    MidHz,
    /// EQ high shelf gain, dB.
    HighDb,
    /// Compressor threshold, dBFS.
    ThresholdDb,
    /// Compressor ratio.
    Ratio,
    /// Compressor attack, ms.
    AttackMs,
    /// Compressor release, ms.
    ReleaseMs,
    /// Compressor make-up gain, dB.
    MakeupDb,
    /// Reverb room size.
    Room,
    /// Reverb damping.
    Damp,
    /// Reverb wet share.
    Mix,
    /// Channel EQ high-pass corner, Hz (SPEC-051).
    HpHz,
    /// Channel EQ low shelf corner, Hz.
    LowHz,
    /// Channel EQ low shelf gain, dB.
    LowShelfDb,
    /// Channel EQ peak 1 centre, Hz.
    P1Hz,
    /// Channel EQ peak 1 gain, dB.
    P1Db,
    /// Channel EQ peak 1 Q.
    P1Q,
    /// Channel EQ peak 2 centre, Hz.
    P2Hz,
    /// Channel EQ peak 2 gain, dB.
    P2Db,
    /// Channel EQ peak 2 Q.
    P2Q,
    /// Channel EQ peak 3 centre, Hz.
    P3Hz,
    /// Channel EQ peak 3 gain, dB.
    P3Db,
    /// Channel EQ peak 3 Q.
    P3Q,
    /// Channel EQ peak 4 centre, Hz.
    P4Hz,
    /// Channel EQ peak 4 gain, dB.
    P4Db,
    /// Channel EQ peak 4 Q.
    P4Q,
    /// Channel EQ high shelf corner, Hz.
    HighHz,
    /// Channel EQ high shelf gain, dB.
    HighShelfDb,
    /// Channel EQ low-pass corner, Hz.
    LpHz,
}

impl EffectParam {
    /// Every parameter, EQ first, then compressor, then reverb, then
    /// channel EQ (SPEC-051).
    pub const ALL: [Self; 30] = [
        Self::LowDb,
        Self::MidDb,
        Self::MidHz,
        Self::HighDb,
        Self::ThresholdDb,
        Self::Ratio,
        Self::AttackMs,
        Self::ReleaseMs,
        Self::MakeupDb,
        Self::Room,
        Self::Damp,
        Self::Mix,
        Self::HpHz,
        Self::LowHz,
        Self::LowShelfDb,
        Self::P1Hz,
        Self::P1Db,
        Self::P1Q,
        Self::P2Hz,
        Self::P2Db,
        Self::P2Q,
        Self::P3Hz,
        Self::P3Db,
        Self::P3Q,
        Self::P4Hz,
        Self::P4Db,
        Self::P4Q,
        Self::HighHz,
        Self::HighShelfDb,
        Self::LpHz,
    ];

    /// Key used in project files, e.g. `"mix"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LowDb => "low_db",
            Self::MidDb => "mid_db",
            Self::MidHz => "mid_hz",
            Self::HighDb => "high_db",
            Self::ThresholdDb => "threshold_db",
            Self::Ratio => "ratio",
            Self::AttackMs => "attack_ms",
            Self::ReleaseMs => "release_ms",
            Self::MakeupDb => "makeup_db",
            Self::Room => "room",
            Self::Damp => "damp",
            Self::Mix => "mix",
            Self::HpHz => "hp_hz",
            Self::LowHz => "low_hz",
            Self::LowShelfDb => "low_shelf_db",
            Self::P1Hz => "p1_hz",
            Self::P1Db => "p1_db",
            Self::P1Q => "p1_q",
            Self::P2Hz => "p2_hz",
            Self::P2Db => "p2_db",
            Self::P2Q => "p2_q",
            Self::P3Hz => "p3_hz",
            Self::P3Db => "p3_db",
            Self::P3Q => "p3_q",
            Self::P4Hz => "p4_hz",
            Self::P4Db => "p4_db",
            Self::P4Q => "p4_q",
            Self::HighHz => "high_hz",
            Self::HighShelfDb => "high_shelf_db",
            Self::LpHz => "lp_hz",
        }
    }

    /// The parameter called `name`, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|param| param.name() == name)
    }

    /// Inclusive range accepted by [`EffectKind::is_valid`].
    #[must_use]
    pub const fn range(self) -> (f32, f32) {
        match self {
            Self::LowDb
            | Self::MidDb
            | Self::HighDb
            | Self::LowShelfDb
            | Self::HighShelfDb
            | Self::P1Db
            | Self::P2Db
            | Self::P3Db
            | Self::P4Db => (-24.0, 24.0),
            Self::MidHz => (100.0, 10_000.0),
            Self::ThresholdDb => (-60.0, 0.0),
            Self::Ratio => (1.0, 20.0),
            Self::AttackMs => (0.1, 100.0),
            Self::ReleaseMs => (10.0, 1000.0),
            Self::MakeupDb => (0.0, 24.0),
            Self::Room | Self::Damp | Self::Mix => (0.0, 1.0),
            Self::HpHz
            | Self::LowHz
            | Self::HighHz
            | Self::LpHz
            | Self::P1Hz
            | Self::P2Hz
            | Self::P3Hz
            | Self::P4Hz => (20.0, 20_000.0),
            Self::P1Q | Self::P2Q | Self::P3Q | Self::P4Q => (0.1, 18.0),
        }
    }

    /// Parameters of the effect type of `kind`, in display order.
    #[must_use]
    pub fn of(kind: &EffectKind) -> &'static [Self] {
        match kind {
            EffectKind::Eq { .. } => &Self::ALL[0..4],
            EffectKind::Compressor { .. } => &Self::ALL[4..9],
            EffectKind::Reverb { .. } => &Self::ALL[9..12],
            EffectKind::ChannelEq { .. } => &Self::ALL[12..30],
        }
    }

    /// Current value in `kind`; `None` when `kind` has no such parameter.
    #[must_use]
    pub fn get(self, kind: &EffectKind) -> Option<f32> {
        let mut copy = *kind;
        self.slot(&mut copy).map(|value| *value)
    }

    /// Writes `value` into `kind`; `false` when `kind` has no such parameter.
    pub fn set(self, kind: &mut EffectKind, value: f32) -> bool {
        match self.slot(kind) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    fn slot(self, kind: &mut EffectKind) -> Option<&mut f32> {
        match (self, kind) {
            (Self::LowDb, EffectKind::Eq { low_db, .. }) => Some(low_db),
            (Self::MidDb, EffectKind::Eq { mid_db, .. }) => Some(mid_db),
            (Self::MidHz, EffectKind::Eq { mid_hz, .. }) => Some(mid_hz),
            (Self::HighDb, EffectKind::Eq { high_db, .. }) => Some(high_db),
            (Self::ThresholdDb, EffectKind::Compressor { threshold_db, .. }) => Some(threshold_db),
            (Self::Ratio, EffectKind::Compressor { ratio, .. }) => Some(ratio),
            (Self::AttackMs, EffectKind::Compressor { attack_ms, .. }) => Some(attack_ms),
            (Self::ReleaseMs, EffectKind::Compressor { release_ms, .. }) => Some(release_ms),
            (Self::MakeupDb, EffectKind::Compressor { makeup_db, .. }) => Some(makeup_db),
            (Self::Room, EffectKind::Reverb { room, .. }) => Some(room),
            (Self::Damp, EffectKind::Reverb { damp, .. }) => Some(damp),
            (Self::Mix, EffectKind::Reverb { mix, .. }) => Some(mix),
            (param, EffectKind::ChannelEq { bands }) => {
                let (index, field) = param.channel_eq_band()?;
                let band = &mut bands[index];
                Some(match field {
                    BandField::Hz => &mut band.hz,
                    BandField::Db => &mut band.db,
                    BandField::Q => &mut band.q,
                })
            }
            _ => None,
        }
    }

    /// Band index and field of a channel EQ parameter (SPEC-051).
    const fn channel_eq_band(self) -> Option<(usize, BandField)> {
        Some(match self {
            Self::HpHz => (0, BandField::Hz),
            Self::LowHz => (1, BandField::Hz),
            Self::LowShelfDb => (1, BandField::Db),
            Self::P1Hz => (2, BandField::Hz),
            Self::P1Db => (2, BandField::Db),
            Self::P1Q => (2, BandField::Q),
            Self::P2Hz => (3, BandField::Hz),
            Self::P2Db => (3, BandField::Db),
            Self::P2Q => (3, BandField::Q),
            Self::P3Hz => (4, BandField::Hz),
            Self::P3Db => (4, BandField::Db),
            Self::P3Q => (4, BandField::Q),
            Self::P4Hz => (5, BandField::Hz),
            Self::P4Db => (5, BandField::Db),
            Self::P4Q => (5, BandField::Q),
            Self::HighHz => (6, BandField::Hz),
            Self::HighShelfDb => (6, BandField::Db),
            Self::LpHz => (7, BandField::Hz),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
enum BandField {
    Hz,
    Db,
    Q,
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
    /// Butterworth high-pass, 12 dB/octave, Q = 1/√2.
    HighPass,
    /// Butterworth low-pass, 12 dB/octave, Q = 1/√2.
    LowPass,
}

impl Biquad {
    /// Coefficients from R. Bristow-Johnson, "Cookbook formulae for audio EQ
    /// biquad filter coefficients":
    ///
    /// `A = 10^(dB/40)`, `w0 = 2π·f0/Fs`.
    /// Peak (Q = q): `α = sin w0 / (2q)`,
    /// `b = [1 + αA, −2cos w0, 1 − αA]`, `a = [1 + α/A, −2cos w0, 1 − α/A]`.
    /// Shelves (S = 1): `α = sin w0 / 2 · √2`, `k = 2√A·α`,
    /// low: `b = [A((A+1) − (A−1)c + k), 2A((A−1) − (A+1)c), A((A+1) − (A−1)c − k)]`,
    /// `a = [(A+1) + (A−1)c + k, −2((A−1) + (A+1)c), (A+1) + (A−1)c − k]`;
    /// high: the same with every `(A−1)` replaced by `−(A−1)`.
    /// HP/LP (Butterworth, Q = 1/√2): `α = sin w0 / √2`,
    /// HP: `b = [(1+c)/2, −(1+c), (1+c)/2]`, `a = [1+α, −2c, 1−α]`;
    /// LP: `b = [(1−c)/2, 1−c, (1−c)/2]`, `a = [1+α, −2c, 1−α]`.
    ///
    /// `f0` is clamped below Nyquist so low sample rates stay stable.
    fn design(shape: Shape, f0: f32, db: f32, q: f32, sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate);
        let f0 = f64::from(f0).min(rate * 0.45);
        let a = 10.0_f64.powf(f64::from(db) / 40.0);
        let w0 = std::f64::consts::TAU * f0 / rate;
        let (sin, cos) = w0.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peak => {
                let alpha = sin / (2.0 * f64::from(q));
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
            Shape::HighPass => {
                let alpha = sin / std::f64::consts::SQRT_2;
                (
                    f64::midpoint(1.0, cos),
                    -(1.0 + cos),
                    f64::midpoint(1.0, cos),
                    1.0 + alpha,
                    -2.0 * cos,
                    1.0 - alpha,
                )
            }
            Shape::LowPass => {
                let alpha = sin / std::f64::consts::SQRT_2;
                (
                    (1.0 - cos) / 2.0,
                    1.0 - cos,
                    (1.0 - cos) / 2.0,
                    1.0 + alpha,
                    -2.0 * cos,
                    1.0 - alpha,
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

    /// New coefficients, same `z1`/`z2` state.
    fn retune(&mut self, shape: Shape, f0: f32, db: f32, q: f32, sample_rate: u32) {
        let fresh = Self::design(shape, f0, db, q, sample_rate);
        *self = Self {
            z1: self.z1,
            z2: self.z2,
            ..fresh
        };
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

    /// New parameters, same delay lines.
    fn retune(&mut self, room: f32, damp: f32, mix: f32) {
        self.feedback = 0.28f32.mul_add(room, 0.7);
        self.damp = 0.4 * damp;
        self.wet = mix * REVERB_WET_SCALE;
        self.dry = 1.0 - mix;
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
    sample_rate: u32,
}

/// The three EQ bands in processing order.
#[derive(Debug, Clone, Copy)]
struct Eq {
    bands: [Biquad; 3],
    /// A 0 dB band is skipped, so a flat EQ stays bit-exact.
    active: [bool; 3],
}

/// The eight channel-EQ bands in processing order (SPEC-051).
#[derive(Debug, Clone, Copy)]
struct ChannelEqState {
    bands: [Biquad; 8],
    /// Inactive bands are skipped so a flat EQ stays bit-exact.
    active: [bool; 8],
}

#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "states live inline in a preallocated processor; boxing would allocate on update"
)]
enum State {
    Bypass,
    Eq(Eq),
    ChannelEq(ChannelEqState),
    Compressor(Compressor),
    Reverb(Reverb),
}

/// `(shape, frequency, gain, q)` of each EQ band.
const fn eq_bands(
    low_db: f32,
    mid_db: f32,
    mid_hz: f32,
    high_db: f32,
) -> [(Shape, f32, f32, f32); 3] {
    [
        (Shape::LowShelf, EQ_LOW_HZ, low_db, 1.0),
        (Shape::Peak, mid_hz, mid_db, 1.0),
        (Shape::HighShelf, EQ_HIGH_HZ, high_db, 1.0),
    ]
}

/// Shape of each channel-EQ band by index (SPEC-051).
const CHANNEL_EQ_SHAPES: [Shape; 8] = [
    Shape::HighPass,
    Shape::LowShelf,
    Shape::Peak,
    Shape::Peak,
    Shape::Peak,
    Shape::Peak,
    Shape::HighShelf,
    Shape::LowPass,
];

/// Whether a channel-EQ band is active given its parameters and shape index.
const fn channel_eq_band_active(index: usize, band: &ChannelEqBand) -> bool {
    if !band.enabled {
        return false;
    }
    // Filter bands (0, 7) are active when enabled; gain bands need db != 0.
    matches!(index, 0 | 7) || band.db != 0.0
}

/// Builds the initial channel-EQ state from band parameters.
fn channel_eq_state(bands: &[ChannelEqBand; 8], sample_rate: u32) -> ChannelEqState {
    let mut biquads = [Biquad::design(Shape::Peak, 1000.0, 0.0, 1.0, sample_rate); 8];
    let mut active = [false; 8];
    for i in 0..8 {
        let shape = CHANNEL_EQ_SHAPES[i];
        let q = if matches!(shape, Shape::Peak) {
            bands[i].q
        } else {
            1.0
        };
        biquads[i] = Biquad::design(shape, bands[i].hz, bands[i].db, q, sample_rate);
        active[i] = channel_eq_band_active(i, &bands[i]);
    }
    ChannelEqState {
        bands: biquads,
        active,
    }
}

/// Retunes channel-EQ bands in place, preserving filter state.
fn update_channel_eq(state: &mut ChannelEqState, bands: &[ChannelEqBand; 8], rate: u32) {
    for i in 0..8 {
        let shape = CHANNEL_EQ_SHAPES[i];
        let q = if matches!(shape, Shape::Peak) {
            bands[i].q
        } else {
            1.0
        };
        state.bands[i].retune(shape, bands[i].hz, bands[i].db, q, rate);
        if !state.active[i] {
            state.bands[i].z1 = 0.0;
            state.bands[i].z2 = 0.0;
        }
        state.active[i] = channel_eq_band_active(i, &bands[i]);
    }
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
                } => {
                    let bands = eq_bands(low_db, mid_db, mid_hz, high_db);
                    State::Eq(Eq {
                        bands: bands.map(|(shape, hz, db, q)| {
                            Biquad::design(shape, hz, db, q, sample_rate)
                        }),
                        active: bands.map(|(_, _, db, _)| db != 0.0),
                    })
                }
                EffectKind::ChannelEq { bands } => {
                    State::ChannelEq(channel_eq_state(&bands, sample_rate))
                }
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
        Self { state, sample_rate }
    }

    /// Applies new parameters in place (SPEC-041), keeping filter state,
    /// the compressor envelope and the reverb tail. A different effect type
    /// or a bypassed slot is left unchanged. Real-time safe.
    pub fn update(&mut self, kind: &EffectKind) {
        let rate = self.sample_rate;
        match (&mut self.state, *kind) {
            (
                State::Eq(eq),
                EffectKind::Eq {
                    low_db,
                    mid_db,
                    mid_hz,
                    high_db,
                },
            ) => {
                let bands = eq_bands(low_db, mid_db, mid_hz, high_db);
                for ((band, active), (shape, hz, db, q)) in
                    eq.bands.iter_mut().zip(&mut eq.active).zip(bands)
                {
                    band.retune(shape, hz, db, q, rate);
                    if !*active {
                        // A skipped band starts clean, not from stale state.
                        band.z1 = 0.0;
                        band.z2 = 0.0;
                    }
                    *active = db != 0.0;
                }
            }
            (State::ChannelEq(state), EffectKind::ChannelEq { bands }) => {
                update_channel_eq(state, &bands, rate);
            }
            (
                State::Compressor(compressor),
                EffectKind::Compressor {
                    threshold_db,
                    ratio,
                    attack_ms,
                    release_ms,
                    makeup_db,
                },
            ) => {
                let envelope = compressor.envelope;
                *compressor =
                    Compressor::new(threshold_db, ratio, attack_ms, release_ms, makeup_db, rate);
                compressor.envelope = envelope;
            }
            (State::Reverb(reverb), EffectKind::Reverb { room, damp, mix }) => {
                reverb.retune(room, damp, mix);
            }
            _ => {}
        }
    }

    /// Processes a mono block in place. Real-time safe.
    pub fn process(&mut self, samples: &mut [f32]) {
        match &mut self.state {
            State::Bypass => {}
            State::Eq(eq) => {
                for (band, active) in eq.bands.iter_mut().zip(eq.active) {
                    if active {
                        band.process(samples);
                    }
                }
            }
            State::ChannelEq(state) => {
                for (band, active) in state.bands.iter_mut().zip(state.active) {
                    if active {
                        band.process(samples);
                    }
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

    #[test]
    fn param_names_round_trip_and_ranges_match_validation() {
        for param in EffectParam::ALL {
            assert_eq!(EffectParam::from_name(param.name()), Some(param));
        }
        assert_eq!(EffectParam::from_name("nope"), None);
        for kind in [
            EffectKind::eq(),
            EffectKind::compressor(),
            EffectKind::reverb(),
        ] {
            for &param in EffectParam::of(&kind) {
                let (low, high) = param.range();
                for (value, ok) in [(low, true), (high, true), (high + 1.0, false)] {
                    let mut changed = kind;
                    assert!(param.set(&mut changed, value));
                    assert_eq!(param.get(&changed), Some(value));
                    assert_eq!(changed.is_valid(), ok, "{param:?} = {value}");
                }
            }
        }
        let mut reverb = EffectKind::reverb();
        assert!(!EffectParam::LowDb.set(&mut reverb, 1.0));
        assert_eq!(EffectParam::LowDb.get(&reverb), None);
    }

    #[test]
    fn update_matches_a_fresh_processor_on_silence_history() {
        let input = sine(440.0, 0.5, 2048);
        for (from, to) in [
            (EffectKind::eq(), eq(6.0, -3.0, 2000.0, 4.0)),
            (
                EffectKind::compressor(),
                EffectKind::Compressor {
                    threshold_db: -30.0,
                    ratio: 8.0,
                    attack_ms: 1.0,
                    release_ms: 50.0,
                    makeup_db: 3.0,
                },
            ),
            (
                EffectKind::reverb(),
                EffectKind::Reverb {
                    room: 0.9,
                    damp: 0.1,
                    mix: 0.7,
                },
            ),
        ] {
            let mut updated = EffectProcessor::new(&Effect::new(from), 48_000);
            updated.update(&to);
            let mut a = input.clone();
            updated.process(&mut a);
            assert_eq!(a, run(to, &input), "{to:?}");
        }
    }

    #[test]
    fn update_keeps_the_reverb_tail() {
        let mut processor = EffectProcessor::new(&Effect::new(EffectKind::reverb()), 48_000);
        let mut impulse = vec![0.0; 256];
        impulse[0] = 1.0;
        processor.process(&mut impulse);
        processor.update(&EffectKind::Reverb {
            room: 0.5,
            damp: 0.5,
            mix: 1.0,
        });
        let mut tail = vec![0.0; 4096];
        processor.process(&mut tail);
        assert!(peak(&tail) > 0.0, "the tail survives a parameter change");
    }

    #[test]
    fn update_ignores_another_type_and_bypass() {
        let input = sine(440.0, 0.5, 512);
        let mut processor = EffectProcessor::new(&Effect::new(EffectKind::eq()), 48_000);
        processor.update(&EffectKind::reverb());
        let mut out = input.clone();
        processor.process(&mut out);
        assert_eq!(out, input);
        let mut bypassed = EffectProcessor::new(
            &Effect {
                bypass: true,
                kind: EffectKind::reverb(),
            },
            48_000,
        );
        bypassed.update(&EffectKind::Reverb {
            room: 1.0,
            damp: 0.0,
            mix: 1.0,
        });
        let mut out = input.clone();
        bypassed.process(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn eq_back_to_flat_is_bit_exact() {
        let input = sine(440.0, 0.5, 512);
        let mut processor = EffectProcessor::new(&Effect::new(eq(6.0, 0.0, 1000.0, 0.0)), 48_000);
        let mut warm = input.clone();
        processor.process(&mut warm);
        processor.update(&EffectKind::eq());
        let mut out = input.clone();
        processor.process(&mut out);
        assert_eq!(out, input);
    }

    // --- SPEC-051: channel EQ -------------------------------------------

    fn channel(edit: impl FnOnce(&mut [ChannelEqBand; 8])) -> EffectKind {
        let mut kind = EffectKind::channel_eq();
        if let EffectKind::ChannelEq { bands } = &mut kind {
            edit(bands);
        }
        kind
    }

    /// Steady-state gain of `kind` on a sine at `hz`, ignoring the onset.
    fn gain_at(kind: EffectKind, hz: f32) -> f32 {
        let input = sine(hz, 0.25, 24_000);
        let out = run(kind, &input);
        peak(&out[12_000..]) / peak(&input[12_000..])
    }

    fn db(ratio: f32) -> f32 {
        20.0 * ratio.log10()
    }

    #[test]
    fn channel_eq_default_is_valid_and_bit_exact() {
        let kind = EffectKind::channel_eq();
        assert!(kind.is_valid());
        let input = sine(440.0, 0.5, 4096);
        assert_eq!(run(kind, &input), input);
        let silence = vec![0.0; 1024];
        assert_eq!(run(kind, &silence), silence);
    }

    #[test]
    fn channel_eq_with_every_band_off_or_flat_is_bit_exact() {
        let input = sine(1000.0, 0.5, 2048);
        let off = channel(|bands| bands.iter_mut().for_each(|b| b.enabled = false));
        assert_eq!(run(off, &input), input);
        let flat_but_tweaked = channel(|bands| {
            for band in &mut bands[1..7] {
                band.hz = 2500.0;
                band.q = 8.0;
            }
        });
        assert_eq!(run(flat_but_tweaked, &input), input);
    }

    #[test]
    fn a_bypassed_channel_eq_is_bit_exact() {
        let kind = channel(|bands| {
            bands[0].enabled = true;
            bands[3].db = 12.0;
        });
        let mut processor = EffectProcessor::new(&Effect { bypass: true, kind }, RATE);
        let input = sine(1000.0, 0.5, 1024);
        let mut out = input.clone();
        processor.process(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn channel_eq_peaks_boost_and_cut_their_centre() {
        for (index, hz) in [(2, 300.0), (3, 1000.0), (4, 3000.0), (5, 8000.0)] {
            let boost = channel(|bands| bands[index].db = 12.0);
            let at = db(gain_at(boost, hz));
            assert!((at - 12.0).abs() < 0.5, "band {index} boost {at} dB");
            let cut = channel(|bands| bands[index].db = -12.0);
            let at = db(gain_at(cut, hz));
            assert!((at + 12.0).abs() < 0.5, "band {index} cut {at} dB");
        }
    }

    #[test]
    fn a_narrow_peak_leaves_distant_frequencies_alone() {
        let kind = channel(|bands| {
            bands[3].db = 12.0;
            bands[3].q = 10.0;
        });
        assert!(db(gain_at(kind, 1000.0)) > 11.5);
        assert!(db(gain_at(kind, 100.0)).abs() < 0.5);
        assert!(db(gain_at(kind, 10_000.0)).abs() < 0.5);
    }

    #[test]
    fn channel_eq_shelves_lift_their_side_only() {
        let low = channel(|bands| bands[1].db = 9.0);
        assert!((db(gain_at(low, 30.0)) - 9.0).abs() < 0.5);
        assert!(db(gain_at(low, 5000.0)).abs() < 0.5);
        let high = channel(|bands| bands[6].db = -9.0);
        assert!((db(gain_at(high, 20_000.0)) + 9.0).abs() < 1.0);
        assert!(db(gain_at(high, 200.0)).abs() < 0.5);
    }

    #[test]
    fn channel_eq_high_and_low_pass_attenuate_their_side() {
        let hp = channel(|bands| {
            bands[0].enabled = true;
            bands[0].hz = 1000.0;
        });
        assert!(db(gain_at(hp, 100.0)) < -35.0, "HP lets lows through");
        assert!(db(gain_at(hp, 10_000.0)).abs() < 0.5);
        assert!(
            (db(gain_at(hp, 1000.0)) + 3.0).abs() < 0.5,
            "Butterworth corner"
        );
        let lp = channel(|bands| {
            bands[7].enabled = true;
            bands[7].hz = 1000.0;
        });
        assert!(db(gain_at(lp, 10_000.0)) < -35.0, "LP lets highs through");
        assert!(db(gain_at(lp, 100.0)).abs() < 0.5);
    }

    #[test]
    fn filter_bands_ignore_db_and_q() {
        let plain = channel(|bands| {
            bands[0].enabled = true;
            bands[0].hz = 500.0;
        });
        let noisy = channel(|bands| {
            bands[0].enabled = true;
            bands[0].hz = 500.0;
            bands[0].db = 18.0;
            bands[0].q = 12.0;
        });
        let input = sine(300.0, 0.5, 2048);
        assert_eq!(run(plain, &input), run(noisy, &input));
    }

    #[test]
    fn channel_eq_stays_stable_at_a_low_sample_rate() {
        let kind = channel(|bands| {
            for band in bands.iter_mut() {
                band.enabled = true;
                band.hz = 20_000.0;
                band.db = 24.0;
                band.q = 18.0;
            }
        });
        let mut processor = EffectProcessor::new(&Effect::new(kind), 8000);
        let mut block = sine(440.0, 0.5, 16_000);
        for chunk in block.chunks_mut(128) {
            processor.process(chunk);
        }
        assert!(block.iter().all(|s| s.is_finite() && s.abs() < 100.0));
    }

    #[test]
    fn channel_eq_validation_rejects_bad_values_and_accepts_bounds() {
        for (field, value, ok) in [
            (0, 20.0, true),
            (0, 20_000.0, true),
            (0, 19.9, false),
            (0, f32::NAN, false),
            (1, 24.0, true),
            (1, -24.0, true),
            (1, 24.1, false),
            (1, f32::INFINITY, false),
            (2, 0.1, true),
            (2, 18.0, true),
            (2, 0.09, false),
            (2, f32::NAN, false),
        ] {
            let kind = channel(|bands| match field {
                0 => bands[4].hz = value,
                1 => bands[4].db = value,
                _ => bands[4].q = value,
            });
            assert_eq!(kind.is_valid(), ok, "field {field} = {value}");
        }
    }

    #[test]
    fn channel_eq_params_round_trip_and_ranges_match_validation() {
        assert_eq!(EffectParam::ALL.len(), 30);
        let kind = EffectKind::channel_eq();
        let params = EffectParam::of(&kind);
        assert_eq!(params.len(), 18);
        for &param in params {
            assert_eq!(EffectParam::from_name(param.name()), Some(param));
            assert!(param.get(&kind).is_some(), "{param:?}");
            let (low, high) = param.range();
            for (value, ok) in [(low, true), (high, true), (high + 1.0, false)] {
                let mut changed = kind;
                assert!(param.set(&mut changed, value));
                assert_eq!(param.get(&changed), Some(value));
                assert_eq!(changed.is_valid(), ok, "{param:?} = {value}");
            }
        }
        assert!(!EffectParam::LowDb.set(&mut EffectKind::channel_eq(), 1.0));
        assert!(!EffectParam::P1Db.set(&mut EffectKind::eq(), 1.0));
        let mut kind = EffectKind::channel_eq();
        assert!(EffectParam::P2Q.set(&mut kind, 4.0));
        let EffectKind::ChannelEq { bands } = kind else {
            unreachable!()
        };
        assert!((bands[3].q - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn channel_eq_update_matches_a_fresh_processor_on_silence_history() {
        let to = channel(|bands| {
            bands[0].enabled = true;
            bands[0].hz = 80.0;
            bands[2].db = 6.0;
            bands[6].db = -4.0;
        });
        let mut updated = EffectProcessor::new(&Effect::new(EffectKind::channel_eq()), RATE);
        updated.update(&to);
        let input = sine(440.0, 0.5, 2048);
        let mut a = input.clone();
        for chunk in a.chunks_mut(128) {
            updated.process(chunk);
        }
        assert_eq!(a, run(to, &input));
    }

    #[test]
    fn channel_eq_update_keeps_state_and_returns_to_bit_exact() {
        let boosted = channel(|bands| bands[3].db = 12.0);
        let input = sine(1000.0, 0.5, 2048);
        let mut processor = EffectProcessor::new(&Effect::new(boosted), RATE);
        let mut warm = input.clone();
        processor.process(&mut warm[..1024]);
        // Retuning keeps the filter history: the result differs from a cold
        // processor with the new settings.
        let retuned = channel(|bands| bands[3].db = 6.0);
        processor.update(&retuned);
        let mut continued = input[1024..1152].to_vec();
        processor.process(&mut continued);
        let mut cold = EffectProcessor::new(&Effect::new(retuned), RATE);
        let mut fresh = input[1024..1152].to_vec();
        cold.process(&mut fresh);
        assert_ne!(continued, fresh, "update must keep the biquad state");
        processor.update(&EffectKind::channel_eq());
        let mut out = input.clone();
        processor.process(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn channel_eq_update_ignores_another_type() {
        let mut processor = EffectProcessor::new(&Effect::new(EffectKind::channel_eq()), RATE);
        processor.update(&EffectKind::eq());
        processor.update(&channel(|bands| bands[3].db = 12.0));
        let input = sine(1000.0, 0.5, 512);
        let mut out = input.clone();
        processor.process(&mut out);
        assert_ne!(out, input, "same-type update applies");
        let mut other = EffectProcessor::new(&Effect::new(EffectKind::eq()), RATE);
        other.update(&channel(|bands| bands[3].db = 12.0));
        let mut out = input.clone();
        other.process(&mut out);
        assert_eq!(out, input, "another type is ignored");
    }
}
