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
