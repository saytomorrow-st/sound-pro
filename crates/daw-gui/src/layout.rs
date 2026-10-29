//! Window layout in the style of a classic DAW main window (SPEC-034).
//!
//! Which side panels are shown and what the bottom area holds. Pure state, so
//! the toggles are tested without a display.

/// What the bottom area of the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottom {
    /// Channel strips of every track, bus and the master.
    Mixer,
    /// Piano roll of the open MIDI clip.
    Editor,
    /// Quick controls of the selected track.
    SmartControls,
}

/// Visible panels of the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Sound library on the far left.
    pub library: bool,
    /// Inspector of the selected track on the left.
    pub inspector: bool,
    /// File browser on the right.
    pub browser: bool,
    /// Bottom area, `None` when hidden.
    pub bottom: Option<Bottom>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            library: false,
            inspector: true,
            browser: false,
            bottom: None,
        }
    }
}

impl Layout {
    /// Shows `panel` in the bottom area, or hides the area if it already
    /// shows `panel`.
    pub fn toggle_bottom(&mut self, panel: Bottom) {
        self.bottom = if self.bottom == Some(panel) {
            None
        } else {
            Some(panel)
        };
    }

    /// Opening a clip in the editor shows the editor area.
    pub fn show_editor(&mut self) {
        self.bottom = Some(Bottom::Editor);
    }
}

/// Ticks in one sixteenth note on the position display.
pub const TICKS_PER_SIXTEENTH: u64 = 240;

/// Position display fields `bar beat sixteenth tick`, each counted from 1
/// except the tick (from 1 too, `1..=240`). `beats_per_bar` of zero counts
/// as 4; a bad tempo counts as 120 BPM.
#[must_use]
pub fn lcd_position(samples: u64, rate: u32, tempo_bpm: f64, beats_per_bar: u32) -> [u64; 4] {
    let per_bar = u64::from(if beats_per_bar == 0 { 4 } else { beats_per_bar });
    let tempo = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
        tempo_bpm
    } else {
        crate::DEFAULT_TEMPO
    };
    let per_beat = 4 * TICKS_PER_SIXTEENTH;
    #[allow(clippy::cast_precision_loss)]
    let beats = samples as f64 / f64::from(rate.max(1)) * tempo / 60.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ticks = (beats * per_beat as f64 + 1e-6).floor() as u64;
    let beat = ticks / per_beat;
    let in_beat = ticks % per_beat;
    [
        beat / per_bar + 1,
        beat % per_bar + 1,
        in_beat / TICKS_PER_SIXTEENTH + 1,
        in_beat % TICKS_PER_SIXTEENTH + 1,
    ]
}

/// Fader value as decibels: `-∞` for silence, one decimal otherwise.
#[must_use]
pub fn db_label(gain: f32) -> String {
    if !gain.is_finite() || gain <= 0.0 {
        return "-∞".to_owned();
    }
    let db = 20.0 * gain.log10();
    if db.abs() < 0.05 {
        "0.0".to_owned()
    } else {
        format!("{db:.1}")
    }
}

/// Pan knob value: `C` in the middle, `L50` / `R50` to the sides.
#[must_use]
pub fn pan_label(pan: f32) -> String {
    let pan = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    #[allow(clippy::cast_possible_truncation)]
    let amount = (pan.abs() * 64.0).round() as i32;
    match amount {
        0 => "C".to_owned(),
        _ if pan < 0.0 => format!("L{amount}"),
        _ => format!("R{amount}"),
    }
}

/// Number of colours in the track palette.
pub const PALETTE_SIZE: u8 = 8;

/// Palette slot of a track: its own colour, or one picked by position so new
/// tracks are colourful like in a classic DAW (SPEC-036).
#[must_use]
pub fn display_color(color: Option<u8>, index: usize) -> u8 {
    match color {
        Some(c) if c < PALETTE_SIZE => c,
        #[allow(clippy::cast_possible_truncation)]
        _ => (index % usize::from(PALETTE_SIZE)) as u8,
    }
}

/// Colours of a region drawn on the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionColors {
    /// Title strip with the region name, darker than the body.
    pub header: [u8; 3],
    /// Region body, the track colour.
    pub body: [u8; 3],
    /// Waveform or notes inside, a dark tint of the track colour.
    pub detail: [u8; 3],
}

/// Multiplies each channel by `factor` (`< 1` darker); `> 1` mixes towards
/// white instead, so `2.0` is white and `0.0` is black.
#[must_use]
pub fn shade(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    let factor = if factor.is_finite() {
        factor.clamp(0.0, 2.0)
    } else {
        1.0
    };
    rgb.map(|c| {
        let c = f32::from(c);
        let v = if factor <= 1.0 {
            c * factor
        } else {
            c + (255.0 - c) * (factor - 1.0)
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = v.round().clamp(0.0, 255.0) as u8;
        v
    })
}

/// Region colours from the track colour; muted tracks are grey.
#[must_use]
pub fn region_colors(base: [u8; 3], muted: bool) -> RegionColors {
    let body = if muted { [86, 86, 86] } else { base };
    RegionColors {
        header: shade(body, 0.72),
        body,
        detail: shade(body, 0.38),
    }
}

/// Lowest level on the fader scale, in dB; below it the fader is at the
/// bottom (silence).
pub const FADER_FLOOR_DB: f32 = -60.0;

/// Fader travel `0..=1` for a gain `0..=1`: equal steps in dB from
/// [`FADER_FLOOR_DB`] to 0 dB, so -6 dB sits high on the fader.
#[must_use]
pub fn fader_position(gain: f32) -> f32 {
    if !gain.is_finite() || gain <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * gain.min(1.0).log10();
    ((db - FADER_FLOOR_DB) / -FADER_FLOOR_DB).clamp(0.0, 1.0)
}

/// Gain for a fader travel; inverse of [`fader_position`]. The bottom is
/// silence.
#[must_use]
pub fn fader_gain(position: f32) -> f32 {
    if !position.is_finite() || position <= 0.0 {
        return 0.0;
    }
    let db = FADER_FLOOR_DB * (1.0 - position.min(1.0));
    10f32.powf(db / 20.0)
}

/// Pan knob pointer angle in radians from straight up: `-135°` for hard
/// left, `+135°` for hard right.
#[must_use]
pub fn pan_angle(pan: f32) -> f32 {
    let pan = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    pan * 135f32.to_radians()
}

/// Horizontal scroll offset that keeps a point of the timeline still while
/// the zoom changes (SPEC-037).
///
/// `anchor` is the point's distance from the left edge of the visible area,
/// `offset` the scroll offset before the change. The result is never negative.
#[must_use]
pub fn zoom_offset(old_zoom: f32, new_zoom: f32, offset: f32, anchor: f32) -> f32 {
    if !(old_zoom.is_finite() && new_zoom.is_finite() && old_zoom > 0.0 && new_zoom > 0.0) {
        return offset.max(0.0);
    }
    ((offset + anchor) * new_zoom / old_zoom - anchor).max(0.0)
}

/// Anchor of a zoom made with the slider (SPEC-037): the playhead when it is
/// on screen, the middle of the visible area otherwise.
#[must_use]
pub fn slider_anchor(playhead_x: f32, offset: f32, view_width: f32) -> f32 {
    let x = playhead_x - offset;
    if (0.0..=view_width).contains(&x) {
        x
    } else {
        view_width / 2.0
    }
}
