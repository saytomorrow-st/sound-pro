//! Sampler voice: playhead, envelope, and linear interpolation (RFC-001 §3).
//!
//! A [`SamplerVoice`] owns everything needed to read one sample region into
//! the mix: the playhead position, the playback ratio, the amplitude envelope,
//! and the fast fade used for voice stealing and group cuts. Like
//! [`crate::synth::Voice`], all state advances against **absolute sample
//! positions**, so the rendered output is independent of the quantum size.
//!
//! This module is pure: no allocation, no locking, no I/O, no `unsafe`.

use crate::sampler::region::AmpEnvelope;

/// Fade-out length for stealing and `off_by` fast cuts, ≈ 5 ms at 48 kHz
/// (RFC-001 §3.1).
pub const STEAL_FADE_SAMPLES: u32 = 240;

/// Sentinel for "no region assigned": the voice is idle.
pub const NO_REGION: u16 = u16::MAX;

/// One sample-playback voice.
///
/// `pos` is a `f64` read position in source frames; `ratio` is the number of
/// source frames consumed per output frame. Both are computed once at note-on,
/// so the per-sample loop is two multiplies and an add.
#[derive(Debug, Clone, Copy)]
pub struct SamplerVoice {
    /// Region index, or [`NO_REGION`] when idle.
    pub region: u16,
    /// MIDI note that started the voice.
    pub note: u8,
    /// MIDI velocity that started the voice.
    pub velocity: u8,
    /// Read position in source frames.
    pub pos: f64,
    /// Source frames per output frame.
    pub ratio: f64,
    /// Peak amplitude after velocity and region gain (before envelope).
    pub peak: f32,
    /// Left/right pan multipliers, precomputed at note-on.
    pub pan_l: f32,
    /// Right-channel pan multiplier.
    pub pan_r: f32,
    /// Absolute output sample the voice started at.
    pub started_at: u64,
    /// Absolute output sample the note-off landed on, if any.
    pub released_at: Option<u64>,
    /// Level at the moment release began.
    pub release_level: f32,
    /// Absolute sample the fast fade began (stealing / `off_by` fast).
    pub fade_start: u64,
    /// Level at the moment the fast fade began.
    pub fade_from: f32,
    /// Whether a fast fade is currently active.
    pub fading: bool,
    /// Whether this voice loops (`loop_continuous` / `loop_sustain`).
    pub loop_enabled: bool,
    /// Loop start in source frames (wrap-back point).
    pub loop_start: f64,
    /// Loop end in source frames (wrap-forward point).
    pub loop_end: f64,
    /// Last playable source frame (exclusive); `pos` past it ends the voice.
    pub end_frame: f64,
    /// Envelope for this voice, in samples.
    pub envelope: AmpEnvelope,
    /// Level at the last rendered sample, for stealing order.
    pub last_gain: f32,
}

impl SamplerVoice {
    /// A silent, unassigned voice.
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            region: NO_REGION,
            note: 0,
            velocity: 0,
            pos: 0.0,
            ratio: 1.0,
            peak: 0.0,
            pan_l: 1.0,
            pan_r: 1.0,
            started_at: 0,
            released_at: None,
            release_level: 0.0,
            fade_start: 0,
            fade_from: 0.0,
            fading: false,
            loop_enabled: false,
            loop_start: 0.0,
            loop_end: 0.0,
            end_frame: 0.0,
            envelope: AmpEnvelope {
                attack_samples: 0,
                hold_samples: 0,
                decay_samples: 0,
                sustain: 1.0,
                release_samples: 0,
            },
            last_gain: 0.0,
        }
    }

    /// Whether the voice has nothing left to play.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.region == NO_REGION
    }

    /// Level the voice reached at its last rendered sample.
    #[must_use]
    pub const fn gain(&self) -> f32 {
        self.last_gain
    }

    /// Starts the voice on `region` at absolute sample `at`.
    ///
    /// `peak` is the full pre-envelope amplitude (velocity · region gain ·
    /// pan); the envelope shapes it from there. `pos` is the source read
    /// position (region `offset`), `ratio` the playhead step.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        region: u16,
        note: u8,
        velocity: u8,
        at: u64,
        pos: f64,
        ratio: f64,
        peak: f32,
        pan_l: f32,
        pan_r: f32,
        loop_enabled: bool,
        loop_start: f64,
        loop_end: f64,
        end_frame: f64,
        envelope: AmpEnvelope,
    ) {
        self.region = region;
        self.note = note;
        self.velocity = velocity;
        self.pos = pos;
        self.ratio = ratio;
        self.peak = peak;
        self.pan_l = pan_l;
        self.pan_r = pan_r;
        self.started_at = at;
        self.released_at = None;
        self.release_level = 0.0;
        self.fade_start = at;
        self.fade_from = 0.0;
        self.fading = false;
        self.loop_enabled = loop_enabled;
        self.loop_start = loop_start;
        self.loop_end = loop_end;
        self.end_frame = end_frame;
        self.envelope = envelope;
        self.last_gain = 0.0;
    }

    /// Marks the note released from `at`.
    ///
    /// Like [`crate::synth::Voice::release`], the release level is taken from
    /// the envelope at `at`, not the last rendered sample, so a note-off at a
    /// quantum boundary anchors correctly for the whole pool.
    pub fn release(&mut self, at: u64) {
        if self.released_at.is_some() {
            return;
        }
        self.release_level = self.level_at(at);
        self.released_at = Some(at);
    }

    /// Starts a fast fade-out from `at` (stealing or `off_by` fast).
    ///
    /// The fade is a linear ramp to zero over [`STEAL_FADE_SAMPLES`] samples,
    /// multiplied on top of the normal envelope.
    pub fn fast_fade(&mut self, at: u64) {
        if self.fading {
            return;
        }
        self.fade_from = self.level_at(at);
        self.fade_start = at;
        self.fading = true;
    }

    /// Envelope level at absolute output sample `at`, before the fast fade.
    pub fn level_at(&self, at: u64) -> f32 {
        if at < self.started_at {
            return 0.0;
        }
        let elapsed = at - self.started_at;
        let base = if let Some(released_at) = self.released_at {
            if at >= released_at {
                release_ramp(
                    self.release_level,
                    at - released_at,
                    self.envelope.release_samples,
                )
            } else {
                adsr_level(elapsed, self.envelope)
            }
        } else {
            adsr_level(elapsed, self.envelope)
        };
        if self.fading && at >= self.fade_start {
            let into_fade = at - self.fade_start;
            if into_fade >= u64::from(STEAL_FADE_SAMPLES) {
                return 0.0;
            }
            #[allow(clippy::cast_precision_loss)]
            let done = into_fade as f32 / STEAL_FADE_SAMPLES as f32;
            // `fade_from` was captured at the moment the fade began, so it
            // already includes the envelope level; the fade is a ramp from
            // that frozen level to zero, independent of the live envelope.
            return self.fade_from * (1.0 - done);
        }
        base
    }

    /// Full gain (envelope × fast fade × peak) at absolute sample `at`.
    ///
    /// The engine uses this to both advance `last_gain` and scale the sample.
    #[inline]
    pub fn gain_at(&self, at: u64) -> f32 {
        self.level_at(at) * self.peak
    }

    /// Advances the playhead by one output frame, applying loop wrap.
    ///
    /// Returns `true` while the voice still has source to play. When looping is
    /// disabled and `pos` passes [`Self::end_frame`], returns `false` and the
    /// voice is exhausted. The engine stops rendering it.
    #[inline]
    pub fn advance(&mut self) -> bool {
        self.pos += self.ratio;
        if self.loop_enabled && self.loop_end > self.loop_start && self.pos >= self.loop_end {
            let span = self.loop_end - self.loop_start;
            self.pos = self.loop_start + (self.pos - self.loop_end) % span;
        }
        self.pos < self.end_frame
    }
}

/// ADSR level (attack → hold → decay → sustain) at `elapsed` samples since
/// note-on, for an [`AmpEnvelope`]. `sustain` is a fraction of peak.
fn adsr_level(elapsed: u64, env: AmpEnvelope) -> f32 {
    let attack = u64::from(env.attack_samples);
    if elapsed < attack {
        if attack == 0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss)]
        return elapsed as f32 / attack as f32;
    }
    let hold = attack + u64::from(env.hold_samples);
    if elapsed < hold {
        return 1.0;
    }
    let decay = u64::from(env.decay_samples);
    let into_decay = elapsed - hold;
    if into_decay < decay {
        if decay == 0 {
            return env.sustain;
        }
        #[allow(clippy::cast_precision_loss)]
        let done = into_decay as f32 / decay as f32;
        return 1.0 + (env.sustain - 1.0) * done;
    }
    env.sustain
}

/// Linear release ramp from `from` down to zero over `total` samples.
fn release_ramp(from: f32, elapsed: u64, total: u32) -> f32 {
    if total == 0 {
        return 0.0;
    }
    if elapsed >= u64::from(total) {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let done = elapsed as f32 / total as f32;
    from * (1.0 - done)
}

/// Reads one interpolated mono sample from `i16` data at `pos`.
///
/// Linear interpolation between neighbouring frames; clamps `pos` to
/// `[0, len-1]`. Returns `0.0` for an empty buffer and the single sample for a
/// one-frame buffer.
#[inline]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub fn read_linear_i16(data: &[i16], pos: f64, len: usize) -> f32 {
    if len == 0 {
        return 0.0;
    }
    if len == 1 {
        return f32::from(data[0]);
    }
    let clamped = pos.clamp(0.0, (len - 1) as f64);
    let i = clamped as usize;
    let frac = (clamped - i as f64) as f32;
    let a = f32::from(data[i]);
    let b = f32::from(data[(i + 1).min(len - 1)]);
    a + (b - a) * frac
}

/// Reads one interpolated stereo frame from interleaved `i16` data at `pos`,
/// returning `(left, right)`. `frames` is the frame count (so
/// `data.len() == frames * 2`).
#[inline]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub fn read_linear_stereo(data: &[i16], pos: f64, frames: usize) -> (f32, f32) {
    if frames == 0 {
        return (0.0, 0.0);
    }
    let last = frames - 1;
    let clamped = pos.clamp(0.0, last as f64);
    let i = clamped as usize;
    let frac = (clamped - i as f64) as f32;
    let j = (i + 1).min(last);
    let al = f32::from(data[i * 2]);
    let ar = f32::from(data[i * 2 + 1]);
    let bl = f32::from(data[j * 2]);
    let br = f32::from(data[j * 2 + 1]);
    (al + (bl - al) * frac, ar + (br - ar) * frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(attack: u32, hold: u32, decay: u32, sustain: f32, release: u32) -> AmpEnvelope {
        AmpEnvelope {
            attack_samples: attack,
            hold_samples: hold,
            decay_samples: decay,
            sustain,
            release_samples: release,
        }
    }

    #[test]
    fn adsr_reaches_sustain_after_decay() {
        let e = env(10, 5, 10, 0.5, 20);
        assert_eq!(adsr_level(0, e), 0.0);
        assert!((adsr_level(5, e) - 0.5).abs() < 1e-6);
        assert_eq!(adsr_level(10, e), 1.0);
        // Halfway through decay (hold ends at 15, decay ends at 25): halfway
        // between peak (1.0) and sustain (0.5) is 0.75.
        assert!((adsr_level(20, e) - 0.75).abs() < 1e-6);
        assert_eq!(adsr_level(25, e), 0.5);
        assert_eq!(adsr_level(1000, e), 0.5);
    }

    #[test]
    fn zero_attack_jumps_to_peak() {
        let e = env(0, 0, 0, 1.0, 0);
        assert_eq!(adsr_level(0, e), 1.0);
    }

    #[test]
    fn release_ramp_goes_to_zero() {
        assert_eq!(release_ramp(0.8, 0, 10), 0.8);
        assert!((release_ramp(0.8, 5, 10) - 0.4).abs() < 1e-6);
        assert_eq!(release_ramp(0.8, 10, 10), 0.0);
        assert_eq!(release_ramp(0.8, 0, 0), 0.0);
    }

    #[test]
    fn linear_read_interpolates() {
        let data = [0i16, 1000, 2000, 3000];
        assert_eq!(read_linear_i16(&data, 0.0, 4), 0.0);
        assert!((read_linear_i16(&data, 0.5, 4) - 500.0).abs() < 1e-3);
        assert!((read_linear_i16(&data, 1.5, 4) - 1500.0).abs() < 1e-3);
        assert_eq!(read_linear_i16(&data, 3.0, 4), 3000.0);
        assert_eq!(read_linear_i16(&data, 999.0, 4), 3000.0);
        assert_eq!(read_linear_i16(&[], 0.0, 0), 0.0);
        assert_eq!(read_linear_i16(&[7], 0.0, 1), 7.0);
    }
}
