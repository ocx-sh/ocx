use clap::ValueEnum;

/// Log level for controlling the verbosity of logging output.
#[derive(Clone, Copy, Debug, ValueEnum)]
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
