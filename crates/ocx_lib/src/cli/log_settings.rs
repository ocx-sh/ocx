// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::LogLevel;

/// Tracing subscriber configuration shared across OCX binaries.
///
/// Supports the following environment variable cascade for log filtering:
/// `OCX_LOG_CONSOLE` → `OCX_LOG` → `RUST_LOG` → default level (INFO).
///
/// # Usage
///
/// **With progress indicators** (auto-detected via stderr TTY):
/// ```ignore
/// LogSettings::default()
///     .with_console_level(log_level)
///     .init_progress(style)?;
/// ```
///
/// **Plain** (no progress bars):
/// ```ignore
/// LogSettings::default().with_console_level(log_level).init()?;
/// ```
#[derive(Default, Debug, Clone)]
pub struct LogSettings {
    filter: Vec<String>,
    console_filter: Vec<String>,
    console_events: bool,
    console_level: Option<LogLevel>,
    stderr_color: Option<bool>,
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

    pub fn with_stderr_color(mut self, enabled: bool) -> Self {
        self.stderr_color = Some(enabled);
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
    ///
    /// Returns an error if a global subscriber is already installed (safe to call
    /// `.ok()` on at sites where double-init is expected, e.g. plugin dispatch).
    pub fn init(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let ansi = self
            .stderr_color
            .unwrap_or_else(|| super::ColorMode::Auto.config().stderr);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(ansi)
            .with_file(false)
            .with_target(false)
            .with_writer(std::io::stderr)
            .with_filter(self.build_env_filter("CONSOLE", std::iter::empty())?);

        tracing_subscriber::registry()
            .with(fmt_layer)
            .try_init()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Initialize a tracing subscriber whose fmt layer writes through the
    /// given span-free [`ProgressManager`](crate::cli::progress::ProgressManager).
    ///
    /// Log lines are flushed inside `MultiProgress::suspend` so they never
    /// tear active progress bars. A disabled manager writes straight to
    /// stderr (the non-TTY path), so callers do not branch on TTY state —
    /// the manager already encodes it. There is no `tracing-indicatif`
    /// layer: progress is driven by RAII guards, not spans
    /// (ADR adr_progress_architecture).
    pub fn init_with_progress(
        self,
        progress: &crate::cli::progress::ProgressManager,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let ansi = self
            .stderr_color
            .unwrap_or_else(|| super::ColorMode::Auto.config().stderr);
        let fmt_layer = {
            let subscriber = tracing_subscriber::fmt::layer().compact().with_ansi(ansi);
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
                .with_writer(progress.writer())
                .with_filter(self.build_env_filter("CONSOLE", std::iter::empty())?)
        };

        tracing_subscriber::registry()
            .with(fmt_layer)
            .try_init()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Build an `EnvFilter` using the OCX env var cascade.
    ///
    /// Checks in order: `OCX_LOG_{extra_name}` → `OCX_LOG` → `RUST_LOG` → default level.
    /// The `extra_filter` iterator allows adding additional filter directives.
    ///
    /// # Errors
    ///
    /// Returns an error if the resolved env var contains an invalid filter directive.
    pub fn build_env_filter<'a>(
        &'a self,
        extra_name: &str,
        extra_filter: impl Iterator<Item = &'a String>,
    ) -> Result<tracing_subscriber::filter::EnvFilter, Box<dyn std::error::Error + Send + Sync>> {
        let name_env = format!("OCX_LOG_{extra_name}");

        let builder = tracing_subscriber::EnvFilter::builder();
        let builder = {
            if std::env::var(&name_env).is_ok() {
                builder.with_env_var(name_env)
            } else if std::env::var("OCX_LOG").is_ok() {
                builder.with_env_var("OCX_LOG")
            } else if std::env::var("RUST_LOG").map(|v| !v.is_empty()).unwrap_or(false) {
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
                        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO)
                        .into(),
                )
            } else {
                builder
            }
        };

        let filter = if let Some(console_level) = self.console_level {
            let console_level: tracing_subscriber::filter::LevelFilter = console_level.into();
            builder.parse(console_level.to_string())?
        } else {
            builder.from_env()?
        };

        Ok(self
            .filter
            .iter()
            .chain(extra_filter)
            .fold(filter, |filter, directive| {
                let directive = match directive.parse() {
                    Ok(directive) => directive,
                    Err(error) => {
                        crate::log::error!("failed to parse log filter directive: {error}");
                        return filter;
                    }
                };
                filter.add_directive(directive)
            }))
    }
}
