//! Live parameter channel (SPEC-006).
//!
//! [`CommandConsumer`](crate::CommandConsumer) carries *structure* changes to
//! the audio thread; parameters carry *values*. A parameter is a single
//! smoothed `f32` shared through an `Arc`, so the control side writes with
//! [`ParamHandle::set`] and the audio thread reads with [`ParamHandle::next`]
//! once per quantum. Both are wait-free: two atomics per quantum, no queue, no
//! allocation, no lock.
//!
//! ```text
//! UI thread                          audio thread (node.process)
//! ─────────                          ──────────────────────────
//! level.set(0.25)  ──atomic────►     let level = self.level.next(frames, sr);
//! ```
//!
//! See `docs/PARAMS.md` for why this is atomics rather than another `rtrb`
//! ring, and `docs/REALTIME.md` for the surrounding contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Marker stored in `left` by `set`: ramp armed, length not sized yet.
const ARMED: f32 = -1.0;

/// Upper bound on `smoothing_ms` accepted by [`ParamSpec::new`].
pub const MAX_SMOOTHING_MS: u32 = 10_000;

/// Identifier of a parameter, unique inside one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

/// Errors returned when describing or registering a parameter.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParamError {
    /// The range is empty, inverted, or not finite.
    #[error("invalid parameter range [{min}, {max}]")]
    InvalidRange {
        /// Lower bound as given.
        min: f32,
        /// Upper bound as given.
        max: f32,
    },
    /// The default value is outside the range.
    #[error("default {default} is outside [{min}, {max}]")]
    DefaultOutOfRange {
        /// Rejected default.
        default: f32,
        /// Lower bound.
        min: f32,
        /// Upper bound.
        max: f32,
    },
    /// Smoothing time exceeds [`MAX_SMOOTHING_MS`].
    #[error("smoothing of {ms} ms exceeds the maximum of {max} ms")]
    SmoothingTooLong {
        /// Requested smoothing.
        ms: u32,
        /// Accepted maximum.
        max: u32,
    },
    /// Another parameter with the same id is already registered.
    #[error("parameter {id:?} is already registered")]
    DuplicateId {
        /// Colliding id.
        id: ParamId,
    },
    /// The id is not registered in this graph.
    #[error("parameter {id:?} is not registered")]
    UnknownParam {
        /// Unknown id.
        id: ParamId,
    },
    /// The graph reserved no room for another parameter.
    #[error("parameter registry capacity of {max} exhausted")]
    RegistryFull {
        /// Capacity reserved by the graph.
        max: usize,
    },
}

/// Plain-data description of a parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSpec {
    /// Unique id inside the graph.
    pub id: ParamId,
    /// Human-readable name shown by the UI.
    pub name: &'static str,
    /// Lower bound applied to every `set`.
    pub min: f32,
    /// Upper bound applied to every `set`.
    pub max: f32,
    /// Value used at construction.
    pub default: f32,
    /// Ramp length in milliseconds; `0` means "jump at the quantum border".
    pub smoothing_ms: u32,
}

impl ParamSpec {
    /// Describes a smoothed parameter.
    ///
    /// # Errors
    ///
    /// * [`ParamError::InvalidRange`] if `min`/`max` are not finite or
    ///   `min > max`
    /// * [`ParamError::DefaultOutOfRange`] if `default` is not finite or is
    ///   outside `[min, max]`
    /// * [`ParamError::SmoothingTooLong`] if `smoothing_ms > MAX_SMOOTHING_MS`
    pub fn new(
        id: ParamId,
        name: &'static str,
        min: f32,
        max: f32,
        default: f32,
        smoothing_ms: u32,
    ) -> Result<Self, ParamError> {
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(ParamError::InvalidRange { min, max });
        }
        if !default.is_finite() || default < min || default > max {
            return Err(ParamError::DefaultOutOfRange { default, min, max });
        }
        if smoothing_ms > MAX_SMOOTHING_MS {
            return Err(ParamError::SmoothingTooLong {
                ms: smoothing_ms,
                max: MAX_SMOOTHING_MS,
            });
        }
        Ok(Self {
            id,
            name,
            min,
            max,
            default,
            smoothing_ms,
        })
    }

    /// Describes a parameter that jumps to its new value at the quantum
    /// border instead of ramping.
    ///
    /// # Errors
    ///
    /// Same range and default errors as [`ParamSpec::new`].
    pub fn unsmoothed(
        id: ParamId,
        name: &'static str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Result<Self, ParamError> {
        Self::new(id, name, min, max, default, 0)
    }
}

/// Shared, smoothed parameter value.
///
/// It is an `Arc`, so cloning it is a reference-count bump: do that in node
/// constructors and on the control thread only. Nodes keep one handle and call
/// [`ParamHandle::next`] from `process`, which never clones.
#[derive(Debug, Clone)]
pub struct ParamHandle(Arc<ParamInner>);

#[derive(Debug)]
struct ParamInner {
    spec: ParamSpec,
    /// Ramp target as raw `f32` bits; four bytes make a torn read impossible.
    target: AtomicU32,
    /// Value the current ramp started from. Only the consumer writes it.
    from: AtomicU32,
    /// Remaining ramp length in samples, as `f32` bits.
    ///
    /// A negative value means "armed by `set`, waiting for the audio thread to
    /// size the ramp from the sample rate"; `-1.0` is negative, finite and
    /// arithmetic on it stays finite, so the consumer's realtime path has no
    /// `NaN` special case.
    left: AtomicU32,
    /// Full ramp length in samples for the current ramp, as `f32` bits.
    span: AtomicU32,
}

fn load(atomic: &AtomicU32) -> f32 {
    f32::from_bits(atomic.load(Ordering::Acquire))
}

fn store(atomic: &AtomicU32, value: f32) {
    atomic.store(value.to_bits(), Ordering::Release);
}

impl ParamHandle {
    /// Creates a parameter on its default value. Control thread only.
    #[must_use]
    pub fn new(spec: ParamSpec) -> Self {
        let zero = 0.0f32.to_bits();
        Self(Arc::new(ParamInner {
            spec,
            target: AtomicU32::new(spec.default.to_bits()),
            from: AtomicU32::new(spec.default.to_bits()),
            left: AtomicU32::new(zero),
            span: AtomicU32::new(zero),
        }))
    }

    /// Description this parameter was created with.
    #[must_use]
    pub fn spec(&self) -> ParamSpec {
        self.0.spec
    }

    /// Current target (the value the ramp is heading to). Control thread.
    #[must_use]
    pub fn value(&self) -> f32 {
        load(&self.0.target)
    }

    /// Requests a new value. Control thread only.
    ///
    /// The value is clamped into `[min, max]` and `NaN` is ignored; setting the
    /// value that is already the target changes nothing, so a repeated UI
    /// update cannot restart a ramp.
    pub fn set(&self, value: f32) {
        if value.is_nan() {
            return;
        }
        let ParamSpec { min, max, .. } = self.0.spec;
        let value = value.clamp(min, max);
        if value.to_bits() == self.value().to_bits() {
            return;
        }
        // Start from wherever the previous ramp had got to, so a second move
        // mid-ramp does not jump back to the previous target. Order matters:
        // `current()` reads the old target, so the new one is stored last.
        let from = self.current();
        store(&self.0.from, from);
        store(&self.0.target, value);
        // The ramp length depends on the sample rate, which is only known on
        // the audio thread; arm the ramp and let `next` size it.
        store(&self.0.left, ARMED);
    }

    /// Advances the ramp by one quantum and returns the value for it.
    ///
    /// Realtime-safe: two atomic loads, two atomic stores, no allocation, no
    /// locking, no panics. `step_frames` is the quantum length in frames and
    /// `sample_rate` comes from [`crate::ProcessContext`].
    pub fn next(&self, step_frames: usize, sample_rate: u32) -> f32 {
        let spec = self.0.spec;
        #[allow(clippy::cast_precision_loss)]
        let step = step_frames as f32;
        #[allow(clippy::cast_precision_loss)]
        let a = spec.smoothing_ms as f32 * sample_rate as f32 / 1000.0;
        // Non-positive or NaN: no smoothing possible.
        if a.is_nan() || a <= 0.0 {
            // Linear-time jump: the new value is live for the whole quantum.
            return self.value();
        }
        let target = self.value();
        let mut left = load(&self.0.left);
        if left < 0.0 {
            // `set` armed a ramp and only the audio thread knows the rate.
            store(&self.0.span, a);
            left = a;
        }
        self.advance(a, left, target, step)
    }

    /// Moves the ramp by one quantum and reports the value for it.
    ///
    /// The returned value is the ramp position *before* this quantum, so the
    /// first call after `set` returns exactly `from`.
    fn advance(&self, span: f32, left: f32, target: f32, step: f32) -> f32 {
        let f0 = load(&self.0.from);
        let value = f0 + (target - f0) * (1.0 - left / span);
        let left = (left - step).max(0.0);
        store(&self.0.left, left);
        value
    }

    /// Value at the current point of the ramp, without advancing it.
    ///
    /// This is the smoothed value the next quantum would play, i.e. the `from`
    /// of a ramp that `set` starts now.
    fn current(&self) -> f32 {
        let left = load(&self.0.left);
        let target = self.value();
        if left < 0.0 {
            // Ramp armed by `set` but not advanced by the audio thread yet: it
            // still sits exactly on `from`.
            return load(&self.0.from);
        }
        let from = load(&self.0.from);
        let span = load(&self.0.span);
        if left > 0.0 {
            from + (target - from) * (1.0 - left / span)
        } else {
            target
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(min: f32, max: f32, default: f32, smoothing_ms: u32) -> ParamSpec {
        ParamSpec::new(ParamId(0), "level", min, max, default, smoothing_ms).unwrap()
    }

    #[test]
    fn spec_rejects_empty_and_inverted_ranges() {
        assert!(matches!(
            ParamSpec::new(ParamId(0), "p", 2.0, 1.0, 1.0, 0),
            Err(ParamError::InvalidRange { .. })
        ));
        assert!(matches!(
            ParamSpec::new(ParamId(0), "p", f32::NAN, 1.0, 1.0, 0),
            Err(ParamError::InvalidRange { .. })
        ));
        assert!(matches!(
            ParamSpec::new(ParamId(0), "p", 0.0, f32::INFINITY, 1.0, 0),
            Err(ParamError::InvalidRange { .. })
        ));
    }

    #[test]
    fn spec_accepts_a_constant_range() {
        let s = ParamSpec::new(ParamId(0), "p", 0.5, 0.5, 0.5, 0).unwrap();
        let p = ParamHandle::new(s);
        p.set(0.9);
        assert!((p.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn spec_rejects_default_out_of_range() {
        assert!(matches!(
            ParamSpec::new(ParamId(0), "p", 0.0, 1.0, 1.5, 0),
            Err(ParamError::DefaultOutOfRange { .. })
        ));
    }

    #[test]
    fn spec_rejects_oversized_smoothing() {
        assert_eq!(
            ParamSpec::new(ParamId(0), "p", 0.0, 1.0, 0.5, MAX_SMOOTHING_MS + 1),
            Err(ParamError::SmoothingTooLong {
                ms: MAX_SMOOTHING_MS + 1,
                max: MAX_SMOOTHING_MS
            })
        );
    }

    #[test]
    fn unsmoothed_has_zero_smoothing() {
        let s = ParamSpec::unsmoothed(ParamId(0), "p", 0.0, 1.0, 0.5).unwrap();
        assert_eq!(s.smoothing_ms, 0);
    }

    #[test]
    fn new_param_reports_its_default_and_spec() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.25, 10));
        assert!((p.value() - 0.25).abs() < f32::EPSILON);
        assert_eq!(p.spec().id, ParamId(0));
        assert_eq!(p.spec().name, "level");
    }

    #[test]
    fn set_ignores_nan() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.5, 0));
        p.set(f32::NAN);
        assert!((p.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn set_clamps_to_the_upper_bound() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.5, 0));
        p.set(1e9);
        assert!((p.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn set_same_value_does_not_restart_the_ramp() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 1000));
        p.set(1.0);
        let sr = 1000;
        // 1000 ms at 1 kHz = 1000 samples; 10 frames per call.
        let first = p.next(10, sr);
        assert!(
            (first - 0.0).abs() < 1e-6,
            "first value after set is `from`"
        );
        let second = p.next(10, sr);
        // Setting the target that is already stored must not re-arm the ramp
        // and rewind it to `from`.
        let left_before = load(&p.0.left);
        p.set(1.0);
        assert!(
            (load(&p.0.left) - left_before).abs() < f32::EPSILON,
            "same-value set rewound the ramp: {} -> {}",
            left_before,
            load(&p.0.left)
        );
        let third = p.next(10, sr);
        assert!(second > first, "ramp is moving");
        assert!(
            (third - second - (second - first)).abs() < 1e-6,
            "ramp kept its slope: {first} -> {second} -> {third}"
        );
    }

    #[test]
    fn unsmoothed_next_returns_target_immediately() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 0));
        p.set(1.0);
        assert!((p.next(480, 48_000) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn zero_step_frames_does_not_advance_the_ramp() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 100));
        p.set(1.0);
        p.next(48, 48_000);
        let a = p.next(0, 48_000);
        let b = p.next(0, 48_000);
        assert!((a - b).abs() < f32::EPSILON);
    }

    #[test]
    fn a_ramp_shorter_than_the_quantum_finishes_without_overshoot() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 1));
        p.set(1.0);
        // 1 ms at 1 kHz = 1 sample, but the quantum is 480 frames: one step
        // already passes the end of the ramp, and the following quantum is
        // exactly the target with no overshoot.
        assert!((p.next(480, 1000) - 0.0).abs() < f32::EPSILON);
        assert!((p.next(480, 1000) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn restarting_mid_ramp_starts_from_the_current_ramp_value() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 1000));
        p.set(1.0);
        // The quantum after `set` still plays 0, the next one 0.25.
        assert!((p.next(250, 1000) - 0.0).abs() < 1e-6);
        let quarter = p.next(250, 1000);
        assert!((quarter - 0.25).abs() < 1e-6, "got {quarter}");
        p.set(0.0);
        // The ramp toward 0 starts from the live ramp position (0.5, halfway
        // through the 1000-sample ramp), not from the old target of 1.0.
        let first = p.next(250, 1000);
        let at_set = 0.5;
        assert!(
            (first - at_set).abs() < 1e-6,
            "ramp starts from the live value (0.5), not from the old target 1.0: {first}"
        );
        // The value still moves toward the new target instead of jumping.
        let second = p.next(250, 1000);
        assert!(
            second < first,
            "ramp must head toward 0: {second} !< {first}"
        );
    }

    #[test]
    fn ramp_length_follows_the_sample_rate() {
        let p = ParamHandle::new(spec(0.0, 1.0, 0.0, 10));
        p.set(1.0);
        // 10 ms at 48 kHz = 480 samples: the first 480-frame quantum is still
        // 0, the second one is already at the target.
        assert!((p.next(480, 48_000) - 0.0).abs() < 1e-6);
        let last = p.next(480, 48_000);
        assert!((last - 1.0).abs() < 1e-6, "got {last}");
        // At 96 kHz the same 10 ms is 960 samples: the second quantum is only
        // halfway through the ramp.
        let q = ParamHandle::new(spec(0.0, 1.0, 0.0, 10));
        q.set(1.0);
        assert!((q.next(480, 96_000) - 0.0).abs() < 1e-6);
        let half = q.next(480, 96_000);
        assert!((half - 0.5).abs() < 1e-6, "got {half}");
    }
}
