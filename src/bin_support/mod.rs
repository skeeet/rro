//! Shared diagnostics for the two command-line executables.
use anyhow::Context;
use std::fmt;
use std::io::{self, Write};

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            diagnostic(format_args!("warning: {}", record.args()));
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

pub(super) fn initialize_logging() -> anyhow::Result<()> {
    log::set_logger(&LOGGER).context("cannot initialize logging")?;
    log::set_max_level(log::LevelFilter::Warn);
    Ok(())
}

pub(super) fn diagnostic(message: fmt::Arguments<'_>) {
    let mut stderr = io::stderr().lock();
    // A closed diagnostic stream must not replace the conversion's actual result.
    let _ = stderr.write_fmt(message);
    let _ = stderr.write_all(b"\n");
}
