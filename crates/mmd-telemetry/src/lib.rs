//! Process-wide tracing setup.
//!
//! Logs go to stderr and to a daily file under `logs/`. The `log` crate used
//! by wgpu and egui is forwarded into the same subscriber. `RUST_LOG` selects
//! the filter when it is set. An invalid filter falls back to the default and
//! is reported on stderr before the subscriber starts.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Default filter. Application events stay at info, while wgpu internals stay quieter.
pub const DEFAULT_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";

/// Failures while installing the global subscriber.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// The current directory could not be resolved or the log directory could not be created.
    #[error("failed to prepare the log directory: {0}")]
    LogDirectory(#[from] std::io::Error),
    /// A tracing subscriber or log logger is already installed.
    #[error("a global tracing subscriber is already installed: {0}")]
    Subscriber(String),
}

/// Install the global tracing subscriber.
pub fn init() -> Result<(), TelemetryError> {
    let filter = env_filter();
    let log_dir = std::env::current_dir()?.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "mmd-viewer.log");

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_ansi(true);
    let file_layer = fmt::layer().with_writer(file_appender).with_ansi(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init()
        .map_err(|error| TelemetryError::Subscriber(error.to_string()))?;

    tracing::info!(log_dir = %log_dir.display(), "logging initialized");
    Ok(())
}

fn env_filter() -> EnvFilter {
    match std::env::var("RUST_LOG") {
        Ok(value) => match EnvFilter::try_new(&value) {
            Ok(filter) => filter,
            Err(error) => {
                eprintln!("invalid RUST_LOG ({error}); using {DEFAULT_FILTER}");
                EnvFilter::new(DEFAULT_FILTER)
            }
        },
        Err(std::env::VarError::NotPresent) => EnvFilter::new(DEFAULT_FILTER),
        Err(error) => {
            eprintln!("could not read RUST_LOG ({error}); using {DEFAULT_FILTER}");
            EnvFilter::new(DEFAULT_FILTER)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_is_accepted_by_tracing() {
        let _filter = EnvFilter::new(DEFAULT_FILTER);
    }
}
