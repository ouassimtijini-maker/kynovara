//! Journal minimal (sortie d'erreur standard, horodaté en UTC).

use kynovara_core::time::{format_utc, now_secs};
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// Niveau de détail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    pub fn from_name(s: &str) -> Option<Level> {
        match s {
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERREUR",
            Level::Warn => "ALERTE",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

pub fn log(level: Level, target: &str, args: fmt::Arguments<'_>) {
    if enabled(level) {
        eprintln!("{} {:6} [{}] {}", format_utc(now_secs()), level.label(), target, args);
    }
}

#[macro_export]
macro_rules! error {
    ($target:expr, $($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Error, $target, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($target:expr, $($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Warn, $target, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! info {
    ($target:expr, $($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Info, $target, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($target:expr, $($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Debug, $target, format_args!($($arg)*)) };
}
