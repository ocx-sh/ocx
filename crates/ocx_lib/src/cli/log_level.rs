// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Log level for controlling the verbosity of logging output.
///
/// Implements `clap_builder::ValueEnum` for use as a CLI flag (`--log-level`).
#[derive(Clone, Copy, Debug)]
pub enum LogLevel {
    /// Log everything, including very detailed information typically only useful for debugging
    Trace,
    /// Log detailed information, typically of interest only when diagnosing problems.
    Debug,
    /// Log informational messages, warnings, and errors
    Info,
    /// Only log warnings and errors
    Warn,
    /// Only log errors
    Error,
    /// Disable all logging output
    Off,
}

impl clap_builder::ValueEnum for LogLevel {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Trace, Self::Debug, Self::Info, Self::Warn, Self::Error, Self::Off]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Trace => PossibleValue::new("trace"),
            Self::Debug => PossibleValue::new("debug"),
            Self::Info => PossibleValue::new("info"),
            Self::Warn => PossibleValue::new("warn"),
            Self::Error => PossibleValue::new("error"),
            Self::Off => PossibleValue::new("off"),
        })
    }
}

impl From<LogLevel> for tracing_subscriber::filter::LevelFilter {
    fn from(val: LogLevel) -> Self {
        match val {
            LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
            LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
            LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
            LogLevel::Warn => tracing_subscriber::filter::LevelFilter::WARN,
            LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
            LogLevel::Off => tracing_subscriber::filter::LevelFilter::OFF,
        }
    }
}
