//! First-run welcome flow (SPEC-…): a small, UI-free state machine plus the
//! helpers that format download progress. Kept here, outside the GUI, so the
//! "should show" logic and the persisted step are testable without a window.

/// The first-run welcome flow, persisted in the settings file.
///
/// [`WelcomeStep::Welcome`] shows the choice dialog, [`WelcomeStep::Download`]
/// shows the "loading core sounds" step (after «Полный режим»), and
/// [`WelcomeStep::Done`] means the flow never appears again. The wire value is
/// the stage's ordinal (`welcome = 0|1|2`); a missing or unrecognised key
/// defaults to [`WelcomeStep::Welcome`], so an existing settings file shows the
/// dialog exactly once after upgrading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WelcomeStep {
    /// Show the «Добро пожаловать» choice dialog.
    #[default]
    Welcome,
    /// «Полный режим» was chosen; show the download step.
    Download,
    /// Completed; do not show the flow again.
    Done,
}

impl WelcomeStep {
    /// Advances to the next stage; [`WelcomeStep::Done`] is a fixed point.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Welcome => Self::Download,
            Self::Download | Self::Done => Self::Done,
        }
    }

    /// Whether the whole flow is finished and should never appear again.
    #[must_use]
    pub const fn is_done(self) -> bool {
        matches!(self, Self::Done)
    }

    /// The settings-file word for this stage (`0`, `1`, or `2`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Welcome => "0",
            Self::Download => "1",
            Self::Done => "2",
        }
    }

    /// Parses a settings-file value, defaulting unknown input to
    /// [`WelcomeStep::Welcome`].
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "1" => Self::Download,
            "2" => Self::Done,
            _ => Self::Welcome,
        }
    }
}

/// Progress fraction in `0.0..=1.0`, or `0.0` when `total` is zero (never
/// `NaN` or division by zero).
#[must_use]
pub fn progress_fraction(received: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (received as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

/// Formats `bytes` as decimal megabytes with one decimal place and a comma
/// separator, e.g. `412_378_726` → `"412,4 МБ"`.
#[must_use]
pub fn format_megabytes(bytes: u64) -> String {
    format_number(bytes as f64 / 1_000_000.0, 1, "МБ")
}

/// Formats `bytes` as decimal gigabytes with two decimal places and a comma
/// separator, e.g. `1_690_000_000` → `"1,69 ГБ"`.
#[must_use]
pub fn format_gigabytes(bytes: u64) -> String {
    format_number(bytes as f64 / 1_000_000_000.0, 2, "ГБ")
}

/// The «X МБ из Y ГБ» caption shown under the download progress bar.
#[must_use]
pub fn download_progress_text(received: u64, total: u64) -> String {
    format!(
        "{} из {}",
        format_megabytes(received),
        format_gigabytes(total)
    )
}

fn format_number(value: f64, decimals: usize, unit: &str) -> String {
    // Round to the requested precision, then render the decimal point as a
    // comma (Russian locale convention, matching the rest of the UI).
    let scale = 10_f64.powi(decimals as i32);
    let rounded = (value * scale).round() / scale;
    let s = format!("{rounded:.decimals$}");
    format!("{} {unit}", s.replacen('.', ",", 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_step_is_welcome() {
        assert_eq!(WelcomeStep::default(), WelcomeStep::Welcome);
        assert!(!WelcomeStep::default().is_done());
    }

    #[test]
    fn step_advances_welcome_to_download_to_done() {
        assert_eq!(WelcomeStep::Welcome.next(), WelcomeStep::Download);
        assert_eq!(WelcomeStep::Download.next(), WelcomeStep::Done);
        assert_eq!(WelcomeStep::Done.next(), WelcomeStep::Done);
        assert!(!WelcomeStep::Welcome.is_done());
        assert!(!WelcomeStep::Download.is_done());
        assert!(WelcomeStep::Done.is_done());
    }

    #[test]
    fn step_roundtrips_through_name_and_parse() {
        for step in [
            WelcomeStep::Welcome,
            WelcomeStep::Download,
            WelcomeStep::Done,
        ] {
            assert_eq!(WelcomeStep::parse(step.name()), step);
        }
        // Unknown / missing values fall back to the default.
        assert_eq!(WelcomeStep::parse("bogus"), WelcomeStep::Welcome);
        assert_eq!(WelcomeStep::parse(""), WelcomeStep::Welcome);
        assert_eq!(WelcomeStep::parse("9"), WelcomeStep::Welcome);
    }

    #[test]
    fn fraction_is_zero_when_total_is_zero() {
        assert_eq!(progress_fraction(0, 0), 0.0);
        assert_eq!(progress_fraction(1_000, 0), 0.0);
        assert!(!progress_fraction(0, 0).is_nan());
    }

    #[test]
    fn fraction_ranges_zero_to_one() {
        assert_eq!(progress_fraction(0, 1_000), 0.0);
        assert_eq!(progress_fraction(1_000, 1_000), 1.0);
        assert_eq!(progress_fraction(2_000, 1_000), 1.0);
        let half = progress_fraction(500, 1_000);
        assert!((half - 0.5).abs() < 0.001);
    }

    #[test]
    fn formats_sizes_with_comma_and_unit() {
        assert_eq!(format_megabytes(393_300_000), "393,3 МБ");
        assert_eq!(format_gigabytes(1_690_000_000), "1,69 ГБ");
        assert_eq!(
            download_progress_text(393_300_000, 1_690_000_000),
            "393,3 МБ из 1,69 ГБ"
        );
    }

    #[test]
    fn formats_zero_and_rounds_up() {
        assert_eq!(format_megabytes(0), "0,0 МБ");
        // 0.05 GB stays "0,05" at two decimals.
        assert_eq!(format_gigabytes(50_000_000), "0,05 ГБ");
        // 1.005 GB rounds up to 1.01.
        assert_eq!(format_gigabytes(1_006_000_000), "1,01 ГБ");
    }
}
