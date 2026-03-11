// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::LogLevel;

/// Tracing subscriber configuration shared across OCX binaries.
///
/// Supports the following environment variable cascade for log filtering:
/// `OCX_LOG_CONSOLE` → `OCX_LOG` → `RUST_LOG` → default level (WARN).
///
/// # Usage
///
/// **Simple (no progress bars)** — used by `ocx-mirror`:
/// ```ignore
/// LogSettings::default().with_console_level(log_level).init()?;
/// ```
///
/// **With custom layers** — used by `ocx` (adds `tracing-indicatif`):
/// ```ignore
/// let settings = LogSettings::default().with_console_level(log_level);
/// let filter = settings.build_env_filter();
/// // compose your own subscriber with the filter
/// ```
#[derive(Default, Debug, Clone)]
pub struct LogSettings {
    filter: Vec<String>,
    console_filter: Vec<String>,
    console_events: bool,
    console_level: Option<LogLevel>,
}

impl LogSettings {
    pub fn with_console_level(mut self, level: Option<LogLevel>) -> Self {
        self.console_level = level;
        self
    }

    pub fn with_console_events(mut self, enabled: bool) -> Self {
        self.console_events = enabled;
        self
    }

    pub fn with_filter(mut self, directive: String) -> Self {
        self.filter.push(directive);
        self
    }

    pub fn with_console_filter(mut self, directive: String) -> Self {
        self.console_filter.push(directive);
        self
    }

    /// Whether console span events are enabled.
    pub fn console_events(&self) -> bool {
        self.console_events
    }

    /// Initialize a simple tracing subscriber (fmt layer to stderr, no progress bars).
    ///
    /// Use this for tools that don't need `tracing-indicatif`. For tools that do,
    /// call [`build_env_filter`] and compose the subscriber manually.
    pub fn init(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let fmt_layer = tracing_subscriber::fmt::layer()
            .compact()
            .with_file(false)
            .with_target(false)
            .with_writer(std::io::stderr)
            .with_filter(self.build_env_filter("CONSOLE", std::iter::empty()));

        tracing_subscriber::registry().with(fmt_layer).init();
        Ok(())
    }

    /// Initialize a tracing subscriber with `tracing-indicatif` progress bar support.
    ///
    /// The `style` parameter sets the default progress style for spans.
    /// Requires the `progress` feature.
    #[cfg(feature = "progress")]
    pub fn init_with_indicatif(
        self,
        style: indicatif::ProgressStyle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let indicatif_layer = tracing_indicatif::IndicatifLayer::new().with_progress_style(style);
        let writer = indicatif_layer.get_stderr_writer();

        let fmt_layer = {
            let subscriber = tracing_subscriber::fmt::layer().compact();
            let subscriber = if self.console_events {
                subscriber.with_span_events(
                    tracing_subscriber::fmt::format::FmtSpan::NEW | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
                )
            } else {
                subscriber
            };
            subscriber
                .with_file(false)
                .with_target(false)
                .with_writer(writer)
                .with_filter(self.build_env_filter("CONSOLE", std::iter::empty()))
        };

        tracing_subscriber::registry()
            .with(indicatif_layer)
            .with(fmt_layer)
            .init();
        Ok(())
    }

    /// Build an `EnvFilter` using the OCX env var cascade.
    ///
    /// Checks in order: `OCX_LOG_{extra_name}` → `OCX_LOG` → `RUST_LOG` → default level.
    /// The `extra_filter` iterator allows adding additional filter directives.
    pub fn build_env_filter<'a>(
        &'a self,
        extra_name: &str,
        extra_filter: impl Iterator<Item = &'a String>,
    ) -> tracing_subscriber::filter::EnvFilter {
        let name_env = format!("OCX_LOG_{extra_name}");

        let builder = tracing_subscriber::EnvFilter::builder();
        let builder = {
            if std::env::var(&name_env).is_ok() {
                builder.with_env_var(name_env)
            } else if std::env::var("OCX_LOG").is_ok() {
                builder.with_env_var("OCX_LOG")
            } else if std::env::var("RUST_LOG").map(|v| v.is_empty()).unwrap_or(false) {
                builder.with_env_var("RUST_LOG")
            } else {
                builder
            }
        };

        let builder = {
            if self.filter.is_empty() {
                let log_level = self.console_level.map(tracing_subscriber::filter::LevelFilter::from);
                builder.with_default_directive(
                    log_level
                        .unwrap_or(tracing_subscriber::filter::LevelFilter::WARN)
                        .into(),
                )
            } else {
                builder
            }
        };

        let filter = if let Some(console_level) = self.console_level {
            let console_level: tracing_subscriber::filter::LevelFilter = console_level.into();
            builder
                .parse(console_level.to_string())
                .expect("Failed to initialize log filter!")
        } else {
            builder.from_env().expect("Failed to initialize log filter!")
        };

        self.filter
            .iter()
            .chain(extra_filter)
            .fold(filter, |filter, directive| {
                let directive = match directive.parse() {
                    Ok(directive) => directive,
                    Err(error) => {
                        crate::log::error!("Failed to parse log filter directive: {error}");
                        return filter;
                    }
                };
                filter.add_directive(directive)
            })
    }
}
