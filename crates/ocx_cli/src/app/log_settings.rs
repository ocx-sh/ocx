// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::log;

use crate::options;

#[derive(Default, Debug, Clone)]
pub struct LogSettings {
    filter: Vec<String>,
    console_filter: Vec<String>,
    console_events: bool,
    console_level: Option<options::LogLevel>,
}

impl LogSettings {
    pub fn with_console_level(mut self, level: Option<options::LogLevel>) -> Self {
        self.console_level = level;
        self
    }

    pub fn init(self) -> anyhow::Result<()> {
        use tracing_subscriber::{layer::SubscriberExt, prelude::*, util::SubscriberInitExt};

        let indicatif_layer = tracing_indicatif::IndicatifLayer::new().with_progress_style(
            indicatif::ProgressStyle::with_template("{span_child_prefix}{spinner} {span_name}")
                .expect("valid indicatif template"),
        );
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
                .with_filter(self.common_filter("CONSOLE", self.console_filter.iter()))
        };

        tracing_subscriber::registry()
            .with(indicatif_layer)
            .with(fmt_layer)
            .init();
        Ok(())
    }

    fn common_filter<'a>(
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
                        log::error!("Failed to parse log filter directive: {error}");
                        return filter;
                    }
                };
                filter.add_directive(directive)
            })
    }
}
