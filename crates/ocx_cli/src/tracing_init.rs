// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Installing this binary's tracing subscriber.

use ocx_console::progress::{LogWriter, LogWriterHandle};

mod event_format;
mod log_level;
mod log_settings;

pub use event_format::EventFormat;
pub use log_level::LogLevel;
pub use log_settings::LogSettings;

/// Adapts the console's bar-suspending stderr writer to `tracing_subscriber`.
pub struct ProgressLogWriter(pub LogWriter);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ProgressLogWriter {
    type Writer = LogWriterHandle;

    fn make_writer(&'a self) -> Self::Writer {
        self.0.handle()
    }
}
