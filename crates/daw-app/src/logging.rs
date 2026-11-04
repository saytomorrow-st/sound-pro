//! File logger: `<data_dir>/sound-pro/logs/sound-pro.log` with one rotated
//! generation (`.log.1`). Control threads only — never call from the audio path.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate when the log file reaches this size.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Verbosity; a message is written when its level is <= the configured one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum LogLevel {
    /// Nothing is written.
    Off = 0,
    /// Errors only.
    Error = 1,
    /// Errors and warnings.
    Warn = 2,
    /// Normal operation (default).
    #[default]
    Info = 3,
    /// Detailed diagnostics.
    Debug = 4,
    /// Everything.
    Trace = 5,
}

impl LogLevel {
    /// All levels in menu order.
    pub const ALL: [Self; 6] = [
        Self::Off,
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    /// Lower-case word used in settings and `SOUND_PRO_LOG`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// Parses a level word, case-insensitively.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|l| l.name() == t)
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Off,
            1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            _ => Self::Trace,
        }
    }
}

static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);
static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

struct Sink {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    max_bytes: u64,
}

impl Sink {
    fn open(path: PathBuf, max_bytes: u64) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        let written = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            path,
            file,
            written,
            max_bytes,
        }
    }

    fn write_line(&mut self, line: &str) {
        if self.written >= self.max_bytes {
            self.rotate();
        }
        if let Some(file) = &mut self.file
            && file.write_all(line.as_bytes()).is_ok()
        {
            self.written += line.len() as u64;
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, rotated_path(&self.path));
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.written = 0;
    }
}

fn rotated_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// `<data_dir>/sound-pro/logs/sound-pro.log`.
#[must_use]
pub fn log_file_path() -> Option<PathBuf> {
    crate::bank_library::banks_dir()
        .and_then(|b| b.parent().map(|p| p.join("logs").join("sound-pro.log")))
}

/// The logs folder.
#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    log_file_path().and_then(|p| p.parent().map(PathBuf::from))
}

/// Level from `SOUND_PRO_LOG`, if set and valid.
#[must_use]
pub fn env_level() -> Option<LogLevel> {
    std::env::var("SOUND_PRO_LOG")
        .ok()
        .and_then(|v| LogLevel::parse(&v))
}

/// Changes the level immediately.
pub fn set_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// The current level.
#[must_use]
pub fn level() -> LogLevel {
    LogLevel::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// Opens the log file, sets the level (`SOUND_PRO_LOG` wins over `configured`),
/// installs the panic hook and logs the startup banner. Idempotent.
pub fn init(configured: LogLevel) {
    set_level(env_level().unwrap_or(configured));
    if let Some(path) = log_file_path() {
        init_at(path, MAX_LOG_BYTES);
    }
}

fn init_at(path: PathBuf, max_bytes: u64) {
    if SINK.set(Mutex::new(Sink::open(path, max_bytes))).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        write(LogLevel::Error, format_args!("PANIC: {info}\n{bt}"));
        previous(info);
    }));
    write(
        LogLevel::Info,
        format_args!(
            "=== Sound Pro {} started, log level {} ===",
            env!("CARGO_PKG_VERSION"),
            level().name()
        ),
    );
}

/// Writes one record (also to stderr). Prefer the `log_*!` macros.
pub fn write(lvl: LogLevel, args: fmt::Arguments<'_>) {
    if lvl == LogLevel::Off || lvl > level() {
        return;
    }
    let line = format!("{} {:<5} {args}\n", timestamp(), lvl.name().to_uppercase());
    eprint!("{line}");
    if let Some(sink) = SINK.get()
        && let Ok(mut s) = sink.lock()
    {
        s.write_line(&line);
    }
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Logs at error level.
#[macro_export]
macro_rules! log_error { ($($a:tt)*) => { $crate::logging::write($crate::logging::LogLevel::Error, format_args!($($a)*)) }; }
/// Logs at warn level.
#[macro_export]
macro_rules! log_warn { ($($a:tt)*) => { $crate::logging::write($crate::logging::LogLevel::Warn, format_args!($($a)*)) }; }
/// Logs at info level.
#[macro_export]
macro_rules! log_info { ($($a:tt)*) => { $crate::logging::write($crate::logging::LogLevel::Info, format_args!($($a)*)) }; }
/// Logs at debug level.
#[macro_export]
macro_rules! log_debug { ($($a:tt)*) => { $crate::logging::write($crate::logging::LogLevel::Debug, format_args!($($a)*)) }; }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        assert_eq!(LogLevel::parse("DEBUG"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse(" off "), Some(LogLevel::Off));
        assert_eq!(LogLevel::parse("loud"), None);
        assert_eq!(LogLevel::default(), LogLevel::Info);
        for l in LogLevel::ALL {
            assert_eq!(LogLevel::parse(l.name()), Some(l));
        }
    }

    #[test]
    fn rotates_at_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("t.log");
        let mut sink = Sink::open(path.clone(), 100);
        for i in 0..20 {
            sink.write_line(&format!("line number {i:04} xxxxxxxxxxxx\n"));
        }
        assert!(rotated_path(&path).exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 100 + 40);
    }

    #[test]
    fn timestamp_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 20);
        assert!(t.starts_with("20"));
    }
}
