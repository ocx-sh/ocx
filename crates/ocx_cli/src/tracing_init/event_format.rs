// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Renders events the way cargo and git do — `error: <msg>`, `warning: <msg>`, info bare — while
/// the filter stops at `info`; once debug or trace is enabled it delegates to `detailed`, keeping
/// the timestamp and level a developer reads.
pub struct EventFormat<F> {
    human: bool,
    detailed: F,
}

impl<F> EventFormat<F> {
    /// `max_level` is the filter's ceiling; `None` (unknown) counts as verbose.
    pub fn new(max_level: Option<LevelFilter>, detailed: F) -> Self {
        Self {
            human: max_level.is_some_and(|level| level <= LevelFilter::INFO),
            detailed,
        }
    }
}

impl<S, N, F> FormatEvent<S, N> for EventFormat<F>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        if !self.human {
            return self.detailed.format_event(ctx, writer, event);
        }
        let prefix = match *event.metadata().level() {
            Level::ERROR => Some(("error:", console::Style::new().red().bold())),
            Level::WARN => Some(("warning:", console::Style::new().yellow().bold())),
            _ => None,
        };
        if let Some((label, style)) = prefix {
            let style = style.force_styling(writer.has_ansi_escapes());
            write!(writer, "{} ", style.apply_to(label))?;
        }
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::format;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Everything `emit` logs under a subscriber with ceiling `level` and the given ANSI choice.
    fn render(level: LevelFilter, ansi: bool, emit: impl FnOnce()) -> String {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let detailed = format().compact().with_target(false);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(ansi)
            .with_max_level(level)
            .event_format(EventFormat::new(Some(level), detailed))
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        let bytes = buffer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8(bytes).expect("utf-8 log output")
    }

    #[test]
    fn default_level_prefixes_errors_and_warnings_without_timestamp_or_level() {
        let out = render(LevelFilter::INFO, false, || {
            tracing::error!("boom");
            tracing::warn!("careful");
            tracing::info!("note: fyi");
        });
        assert_eq!(out, "error: boom\nwarning: careful\nnote: fyi\n");
    }

    #[test]
    fn color_paints_only_the_prefix() {
        let out = render(LevelFilter::WARN, true, || {
            tracing::error!("boom");
            tracing::warn!("careful");
        });
        assert_eq!(
            out,
            "\u{1b}[31m\u{1b}[1merror:\u{1b}[0m boom\n\u{1b}[33m\u{1b}[1mwarning:\u{1b}[0m careful\n"
        );
    }

    #[test]
    fn debug_level_keeps_the_detailed_format() {
        let out = render(LevelFilter::DEBUG, false, || tracing::error!("boom"));
        let line = out.lines().next().expect("one line");
        let (timestamp, rest) = line.split_once(' ').expect("timestamp then the rest");
        assert!(
            timestamp.ends_with('Z') && timestamp.contains('T'),
            "RFC 3339 timestamp first: {line:?}"
        );
        assert_eq!(rest.trim_start(), "ERROR boom", "level word kept: {line:?}");
    }
}
