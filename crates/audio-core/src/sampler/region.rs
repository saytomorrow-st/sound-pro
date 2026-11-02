//! Resolved regions and key→region lookup (RFC-001 §4, §5).
//!
//! The SFZ parser ([`crate::sampler::sfz`]) produces an un-resolved
//! [`sfz::Region`] whose fields are all `Option`. This module turns that into
//! a fully-resolved [`SampleRegion`] with defaults and range checks applied,
//! and indexes it in a [`RegionTable`] so a note-on finds its region in O(1)
//! by note number plus a short linear scan over that note's candidates.
//!
//! Nothing here touches the filesystem or the audio thread; table construction
//! happens once at bank/engine setup.

use crate::sampler::sfz::{LoopMode, OffMode, Region};

/// Maximum regions considered per MIDI note (RFC-001 §4: extra regions are
/// dropped at load time with a warning).
pub const MAX_PER_KEY: usize = 64;

/// Number of MIDI notes (0..=127).
pub const MIDI_NOTES: usize = 128;

/// A fully-resolved sample region, ready to play.
///
/// All `Option` fields of [`Region`] have been resolved to concrete values,
/// defaults applied, and ranges validated. The engine indexes and reads these
/// on the audio thread; they are immutable after construction.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleRegion {
    /// Index into the bank's sample array.
    pub sample_index: usize,
    /// Channel count of the sample (`1` or `2`).
    pub channels: u8,
    /// Root note for pitch reference (`pitch_keycenter`), default 60.
    pub root: u8,
    /// Inclusive key range (`lokey`/`hikey`), default `0..=127`.
    pub key_lo: u8,
    /// Upper inclusive key bound (`hikey`).
    pub key_hi: u8,
    /// Inclusive velocity range (`lovel`/`hivel`), default `1..=127`.
    pub vel_lo: u8,
    /// Upper inclusive velocity bound (`hivel`).
    pub vel_hi: u8,
    /// Fine tune in cents (`tune`) plus coarse transpose (`transpose`) in
    /// semitones, pre-multiplied so `transpose` adds 100 cents per semitone.
    pub tune_cents: i32,
    /// Linear gain `10^(volume/20)`, applied after the envelope.
    pub gain: f32,
    /// Velocity tracking `0.0..=1.0` (`amp_veltrack/100`), default 1.0.
    pub vel_track: f32,
    /// Stereo pan `-1.0..=1.0` (`pan/100`), default 0.0.
    pub pan: f32,
    /// Amplitude envelope, in samples (see [`AmpEnvelope`]).
    pub envelope: AmpEnvelope,
    /// Group this region belongs to (`group`).
    pub group: Option<i32>,
    /// Group cut when this region triggers (`off_by`).
    pub off_by: Option<i32>,
    /// How the cut group's tail behaves (`off_mode`).
    pub off_mode: OffMode,
    /// Loop behaviour.
    pub loop_mode: LoopMode,
    /// Loop start, in sample frames.
    pub loop_start: u32,
    /// Loop end, in sample frames (`0` means "to sample end").
    pub loop_end: u32,
    /// Playback window (`offset`/`end`); `end == 0` means "until sample end".
    pub offset: u32,
    /// Playback end, in sample frames (`0` means "to sample end").
    pub end: u32,
}

impl Default for SampleRegion {
    fn default() -> Self {
        Self {
            sample_index: 0,
            channels: 1,
            root: 60,
            key_lo: 0,
            key_hi: 127,
            vel_lo: 1,
            vel_hi: 127,
            tune_cents: 0,
            gain: 1.0,
            vel_track: 1.0,
            pan: 0.0,
            envelope: AmpEnvelope::default(),
            group: None,
            off_by: None,
            off_mode: OffMode::default(),
            loop_mode: LoopMode::default(),
            loop_start: 0,
            loop_end: 0,
            offset: 0,
            end: 0,
        }
    }
}

/// Amplitude envelope, pre-converted to sample counts (RFC-001 §2, §3).
///
/// Times are in output samples at the engine's sample rate; the conversion
/// from seconds happens in [`resolve`] so the audio thread only compares
/// integers. `sustain` is a fraction of peak in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmpEnvelope {
    /// Attack time in samples.
    pub attack_samples: u32,
    /// Hold time in samples (level held at peak).
    pub hold_samples: u32,
    /// Decay time in samples (peak → sustain).
    pub decay_samples: u32,
    /// Sustain level, `0.0..=1.0` of peak.
    pub sustain: f32,
    /// Release time in samples.
    pub release_samples: u32,
}

impl Default for AmpEnvelope {
    /// A neutral envelope: zero attack/hold/decay/release with full sustain,
    /// so a sample plays at constant level until its note-off.
    fn default() -> Self {
        Self {
            attack_samples: 0,
            hold_samples: 0,
            decay_samples: 0,
            sustain: 1.0,
            release_samples: 0,
        }
    }
}

/// Converts seconds to samples, clamped against wrap (RFC-001 §3).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn seconds_to_samples(seconds: f32, rate: u32) -> u32 {
    (seconds.max(0.0) * rate as f32)
        .round()
        .min(u32::MAX as f32) as u32
}

/// Resolves a parsed [`Region`] into a [`SampleRegion`].
///
/// `rate` is the output sample rate, used to convert envelope times to
/// samples. `sample_index` and `channels` come from the caller (the bank), not
/// from the parser. `None` is returned when the region cannot be played:
/// key/velocity range inverted, or a zero channel count.
///
/// # Panics
///
/// This is a setup-time function; it does not run on the audio thread and may
/// assume the caller has already validated `rate > 0`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn resolve(
    region: &Region,
    rate: u32,
    sample_index: usize,
    channels: u8,
) -> Option<SampleRegion> {
    let key_lo = region.lokey.unwrap_or(0);
    let key_hi = region.hikey.unwrap_or(127);
    if key_lo > key_hi {
        return None;
    }
    let vel_lo = region.lovel.unwrap_or(1);
    let vel_hi = region.hivel.unwrap_or(127);
    if vel_lo > vel_hi {
        return None;
    }
    if channels == 0 {
        return None;
    }

    // `transpose` is semitones; fold it into cents so the playhead ratio needs
    // a single `tune/1200` term (RFC-001 §3.2).
    let tune_cents = region.tune.unwrap_or(0) + region.transpose.unwrap_or(0) * 100;

    // SFZ v1 amplitude: `gain = 10^(volume/20)`, velocity curve handled at
    // note-on (`vel_track`). `volume` in dB.
    let gain = 10.0_f32.powf(region.volume.unwrap_or(0.0) / 20.0);

    // `amp_veltrack` is 0..=100, default 100; stored as 0.0..=1.0.
    let vel_track = region.amp_veltrack.unwrap_or(100.0).clamp(0.0, 100.0) / 100.0;

    // `pan` is -100..=100, stored as -1.0..=1.0.
    let pan = region.pan.unwrap_or(0.0).clamp(-100.0, 100.0) / 100.0;

    let envelope = AmpEnvelope {
        attack_samples: seconds_to_samples(region.ampeg_attack.unwrap_or(0.0), rate),
        hold_samples: seconds_to_samples(region.ampeg_hold.unwrap_or(0.0), rate),
        decay_samples: seconds_to_samples(region.ampeg_decay.unwrap_or(0.0), rate),
        sustain: region.ampeg_sustain.unwrap_or(100.0).clamp(0.0, 100.0) / 100.0,
        release_samples: seconds_to_samples(region.ampeg_release.unwrap_or(0.0), rate),
    };

    let loop_mode = region.loop_mode.unwrap_or_default();
    let loop_start = region.loop_start.unwrap_or(0);
    let loop_end = region.loop_end.unwrap_or(0);

    Some(SampleRegion {
        sample_index,
        channels,
        root: region.pitch_keycenter.unwrap_or(60),
        key_lo,
        key_hi,
        vel_lo,
        vel_hi,
        tune_cents,
        gain,
        vel_track,
        pan,
        envelope,
        group: region.group,
        off_by: region.off_by,
        off_mode: region.off_mode.unwrap_or_default(),
        loop_mode,
        loop_start,
        loop_end,
        offset: region.offset.unwrap_or(0),
        end: region.end.unwrap_or(0),
    })
}

/// Key→region index for O(1) note-on lookup (RFC-001 §4).
///
/// Built once at load time. Each MIDI note maps to a `Vec<u16>` of region
/// indices in source order; at note-on the engine scans only that note's list
/// (at most [`MAX_PER_KEY`] entries) and picks the first whose velocity range
/// matches.
#[derive(Debug)]
pub struct RegionTable {
    /// Region indices per MIDI note, in source order.
    pub by_key: [Vec<u16>; MIDI_NOTES],
    /// Regions dropped because a note already had [`MAX_PER_KEY`] candidates.
    pub truncated_regions: u32,
}

impl Default for RegionTable {
    fn default() -> Self {
        Self {
            by_key: std::array::from_fn(|_| Vec::new()),
            truncated_regions: 0,
        }
    }
}

impl RegionTable {
    /// Builds the table from resolved regions.
    ///
    /// Regions whose `sample_index` is out of range for `sample_count` are
    /// skipped (they reference a missing sample). Extra regions beyond
    /// [`MAX_PER_KEY`] for a single note are dropped and counted in
    /// [`Self::truncated_regions`] (RFC-001 §4).
    #[must_use]
    pub fn build(regions: &[SampleRegion], sample_count: usize) -> Self {
        let mut table = Self::default();
        for (index, region) in regions.iter().enumerate() {
            if region.sample_index >= sample_count {
                continue;
            }
            if index > u16::MAX as usize {
                break;
            }
            #[allow(clippy::cast_possible_truncation)]
            let idx = index as u16;
            for key in region.key_lo..=region.key_hi {
                let bucket = &mut table.by_key[key as usize];
                if bucket.len() < MAX_PER_KEY {
                    bucket.push(idx);
                } else {
                    table.truncated_regions = table.truncated_regions.saturating_add(1);
                }
            }
        }
        table
    }

    /// Finds the first region index matching `(key, velocity)`, or `None`.
    ///
    /// Source order is preserved, so the first match is the most specific
    /// region (earlier in the `.sfz`). Realtime-safe: a short linear scan, no
    /// allocation, no panic.
    #[must_use]
    pub fn find(&self, regions: &[SampleRegion], key: u8, velocity: u8) -> Option<usize> {
        for &index in &self.by_key[key as usize] {
            let region = &regions[index as usize];
            if velocity >= region.vel_lo && velocity <= region.vel_hi {
                return Some(index as usize);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region_with(
        key_lo: u8,
        key_hi: u8,
        vel_lo: u8,
        vel_hi: u8,
        sample_index: usize,
    ) -> SampleRegion {
        SampleRegion {
            sample_index,
            key_lo,
            key_hi,
            vel_lo,
            vel_hi,
            ..SampleRegion::default()
        }
    }

    #[test]
    fn table_finds_first_velocity_match_in_source_order() {
        let regions = vec![
            region_with(60, 60, 1, 64, 0),
            region_with(60, 60, 65, 127, 1),
        ];
        let table = RegionTable::build(&regions, 2);
        assert_eq!(table.find(&regions, 60, 64), Some(0));
        assert_eq!(table.find(&regions, 60, 65), Some(1));
        assert_eq!(table.find(&regions, 59, 100), None);
        assert_eq!(table.find(&regions, 60, 0), None);
    }

    #[test]
    fn out_of_range_sample_is_skipped() {
        let regions = vec![region_with(60, 60, 1, 127, 5)];
        let table = RegionTable::build(&regions, 2);
        assert_eq!(table.find(&regions, 60, 100), None);
    }

    #[test]
    fn truncated_regions_is_counted() {
        let mut regions = Vec::new();
        for i in 0..(MAX_PER_KEY + 2) {
            regions.push(region_with(60, 60, 1, 127, i));
        }
        let table = RegionTable::build(&regions, regions.len());
        assert_eq!(table.by_key[60].len(), MAX_PER_KEY);
        assert_eq!(table.truncated_regions, 2);
    }
}
