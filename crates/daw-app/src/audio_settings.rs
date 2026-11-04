//! Audio device settings chosen in the window (SPEC-033).
//!
//! Plain data plus a tiny `key = value` file format, so it is testable
//! without a sound card. The device glue in [`crate::realtime`] reads it.

use std::path::{Path, PathBuf};

use crate::logging::LogLevel;
use audio_core::Interpolation;

/// Buffer sizes offered in the settings window, in frames.
pub const BUFFER_SIZES: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

/// Which devices to open and how large the device buffer should be.
///
/// `None` means "system default" for every field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioSettings {
    /// Output device name.
    pub output: Option<String>,
    /// Input (microphone) device name.
    pub input: Option<String>,
    /// Device buffer size in frames.
    pub buffer_frames: Option<u32>,
    /// Sampler interpolation quality (SPEC-068); linear by default.
    pub interpolation: Interpolation,
    /// Log verbosity (info by default).
    pub log_level: LogLevel,
}

/// The settings-file word for `interpolation`.
#[must_use]
pub const fn interpolation_name(interpolation: Interpolation) -> &'static str {
    match interpolation {
        Interpolation::Linear => "linear",
        Interpolation::Hermite4 => "hermite",
    }
}

impl AudioSettings {
    /// Parses the settings file text. Unknown keys and bad lines are ignored,
    /// so a damaged file falls back to defaults instead of failing.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "output" => settings.output = Some(value.to_owned()),
                "input" => settings.input = Some(value.to_owned()),
                "buffer" => {
                    settings.buffer_frames =
                        value.parse().ok().filter(|n| BUFFER_SIZES.contains(n));
                }
                "log_level" => {
                    settings.log_level = LogLevel::parse(value).unwrap_or_default();
                }
                "interpolation" => {
                    settings.interpolation = match value {
                        "hermite" => Interpolation::Hermite4,
                        _ => Interpolation::Linear,
                    };
                }
                _ => {}
            }
        }
        settings
    }

    /// The settings file text; [`AudioSettings::parse`] reads it back.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::from("# Sound Pro audio settings\n");
        if let Some(output) = &self.output {
            text.push_str(&format!("output = {}\n", one_line(output)));
        }
        if let Some(input) = &self.input {
            text.push_str(&format!("input = {}\n", one_line(input)));
        }
        if let Some(frames) = self.buffer_frames {
            text.push_str(&format!("buffer = {frames}\n"));
        }
        if self.log_level != LogLevel::default() {
            text.push_str(&format!("log_level = {}\n", self.log_level.name()));
        }
        if self.interpolation != Interpolation::default() {
            let name = interpolation_name(self.interpolation);
            text.push_str(&format!("interpolation = {name}\n"));
        }
        text
    }

    /// Reads `path`; a missing or unreadable file gives defaults.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |t| Self::parse(&t))
    }

    /// Writes the settings to `path`, creating its folder.
    ///
    /// # Errors
    ///
    /// The I/O error from creating the folder or writing the file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_text())
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// The config directory: `$XDG_CONFIG_HOME/sound-pro`,
/// `~/.config/sound-pro`, or `%APPDATA%\sound-pro`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("sound-pro"))
}

/// Default settings file: `$XDG_CONFIG_HOME/sound-pro/audio.conf`,
/// `~/.config/sound-pro/audio.conf`, or `%APPDATA%\sound-pro\audio.conf`.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("audio.conf"))
}

/// Marker file recording that the first-run dialog has been shown (SPEC-066).
#[must_use]
pub fn first_run_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("first-run-done"))
}

/// Whether the first-run dialog has already been acknowledged (SPEC-066).
#[must_use]
pub fn first_run_done() -> bool {
    first_run_path().is_some_and(|p| p.exists())
}

/// Marks the first-run dialog as acknowledged, creating the config folder.
///
/// # Errors
///
/// The I/O error from creating the folder or writing the marker.
pub fn mark_first_run_done() -> std::io::Result<()> {
    let path = first_run_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, b"1")
}

/// Buffer size to ask the device for (SPEC-033).
///
/// `requested` is clamped into the device's `range` (min, max) when the
/// device reports one; `None` keeps the device default.
#[must_use]
pub fn choose_buffer(requested: Option<u32>, range: Option<(u32, u32)>) -> Option<u32> {
    let frames = requested?;
    Some(match range {
        Some((min, max)) if min <= max => frames.clamp(min, max),
        _ => frames,
    })
}

/// Picks the device named `wanted` from `names`, or `None` for the default.
///
/// Returns the index of the match; a name that is no longer present (the
/// device was unplugged) falls back to the default.
#[must_use]
pub fn find_device(names: &[String], wanted: Option<&str>) -> Option<usize> {
    let wanted = wanted?;
    names.iter().position(|n| n == wanted)
}

/// Latency of one device buffer, in milliseconds.
#[must_use]
pub fn buffer_ms(frames: u32, rate: u32) -> f64 {
    if rate == 0 {
        return 0.0;
    }
    f64::from(frames) * 1000.0 / f64::from(rate)
}
