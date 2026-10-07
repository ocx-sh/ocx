// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::ColorMode;
use ocx_console::progress::ProgressManager;

use super::{EventFormat, LogLevel, ProgressLogWriter};

/// Cargo-style lines up to `info`; above it, the compact format with timestamp and level.
fn event_format(
    filter: &tracing_subscriber::EnvFilter,
) -> EventFormat<tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Compact>> {
    let detailed = tracing_subscriber::fmt::format()
        .compact()
        .with_file(false)
        .with_target(false);
    EventFormat::new(filter.max_level_hint(), detailed)
}

/// Tracing subscriber configuration for this binary.
///
/// **With progress indicators** (auto-detected via stderr TTY):
/// ```ignore
/// LogSettings::default()
///     .with_console_level(log_level)
///     .init_with_progress(&progress)?;
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
    console_level: Option<LogLevel>,
    stderr_color: Option<bool>,
}

impl LogSettings {
    pub fn with_console_level(mut self, level: Option<LogLevel>) -> Self {
        self.console_level = level;
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

    /// Installs a plain fmt subscriber on stderr.
    ///
    /// # Errors
    /// When a global subscriber is already installed, or the filter env var is invalid.
    pub fn init(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let ansi = self.stderr_color.unwrap_or_else(|| ColorMode::Auto.config().stderr);
        let filter = self.build_env_filter("CONSOLE", std::iter::empty())?;
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(ansi)
            .event_format(event_format(&filter))
            .with_writer(std::io::stderr)
            .with_filter(filter);

        tracing_subscriber::registry()
            .with(fmt_layer)
            .try_init()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Installs a fmt subscriber writing through `progress`, so log lines never tear an active bar.
    ///
    /// # Errors
    /// As [`Self::init`].
    pub fn init_with_progress(
        self,
        progress: &ProgressManager,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let ansi = self.stderr_color.unwrap_or_else(|| ColorMode::Auto.config().stderr);
        let filter = self.build_env_filter("CONSOLE", std::iter::empty())?;
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(ansi)
            .event_format(event_format(&filter))
            .with_writer(ProgressLogWriter(progress.writer()))
            .with_filter(filter);

        tracing_subscriber::registry()
            .with(fmt_layer)
            .try_init()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Builds an `EnvFilter` from `OCX_LOG_{extra_name}` → `OCX_LOG` → `RUST_LOG` → the default level.
    ///
    /// # Errors
    /// When the resolved env var holds an invalid filter directive.
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
                        log::error!("failed to parse log filter directive: {error}");
                        return filter;
                    }
                };
                filter.add_directive(directive)
            }))
    }
}
