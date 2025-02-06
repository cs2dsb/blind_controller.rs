use core::str::FromStr;

use esp_println::println;
use log::{
    max_level, set_logger_racy, set_max_level_racy, trace, Level, LevelFilter, Log, Metadata,
    Record,
};

const NC: &str = "\u{001B}[0m";
const DIM: &str = "\u{001B}[2m";
const RED: &str = "\u{001B}[31m";
const GREEN: &str = "\u{001B}[32m";
const YELLOW: &str = "\u{001B}[33m";
const BLUE: &str = "\u{001B}[34m";
const CYAN: &str = "\u{001B}[35m";

pub fn setup() {
    /// Log level
    const LEVEL: Option<&'static str> = option_env!("ESP_LOG");

    // SAFETY:
    // This function must be called once at the beginning of execution.
    let result = unsafe { set_logger_racy(&EspPrintlnLogger) };

    if result.is_err() {
        // Could not set default logger.
        // There is nothing we can do; logging will not work.
        return;
    }

    if let Some(level) = LEVEL {
        let level = LevelFilter::from_str(level).unwrap_or(LevelFilter::Off);

        // SAFETY:
        // This function must be called once at the beginning of execution.
        unsafe { set_max_level_racy(level) };
    }

    trace!("Logger is ready");
}

/// Logger that prints messages to console
struct EspPrintlnLogger;

impl Log for EspPrintlnLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        if metadata.target().starts_with("esp_wifi") {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= max_level()
        }
    }

    fn log(&self, record: &Record) {
        let color = match record.level() {
            Level::Error => RED,
            Level::Warn => YELLOW,
            Level::Info => GREEN,
            Level::Debug => BLUE,
            Level::Trace => CYAN,
        };

        if self.enabled(record.metadata()) {
            println!(
                "{}{:>5} {}{}{}{}]{} {}",
                color,
                record.level(),
                NC,
                DIM,
                record.target(),
                DIM,
                NC,
                record.args()
            );
        }
    }

    fn flush(&self) {}
}
