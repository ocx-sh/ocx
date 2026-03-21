// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

use tracing_indicatif::span_ext::IndicatifSpanExt;

/// Number of entries between periodic debug log messages during archiving.
pub const LOG_INTERVAL: u64 = 100;

/// Sets the progress bar message on a span to a quoted package name.
///
/// Renders as ` 'cmake:3.28'` in the `{msg}` template variable (leading space
/// so templates can use `{span_name}{msg}` without a trailing space when empty).
fn set_span_package(span: &tracing::Span, package: &impl std::fmt::Display) {
    span.pb_set_message(&format!(" '{package}'"));
}

/// Creates a labeled spinner span for use with `.instrument()`.
///
/// Convenience wrapper around [`ProgressBar::spinner`] + [`ProgressBar::into_span`]
/// for async tasks that need a spinner span without a progress bar.
///
/// ```ignore
/// self.find(&packages[0], platforms)
///     .instrument(spinner_span(info_span!("Finding"), &packages[0]))
///     .await
/// ```
pub fn spinner_span(span: tracing::Span, package: &impl std::fmt::Display) -> tracing::Span {
    ProgressBar::spinner(span, package).into_span()
}

/// A progress bar backed by a tracing-indicatif span.
///
/// Wraps a [`tracing::Span`] and configures it as a progress bar on
/// construction. Created via [`bytes()`](Self::bytes) for byte-transfer
/// bars or [`files()`](Self::files) for file-count bars.
///
/// # Byte bars
///
/// ```ignore
/// let bar = ProgressBar::bytes(info_span!("Downloading", package = %id), total, &id);
/// let on_progress = bar.callback();
/// let _guard = bar.enter();
/// transport.pull_blob_to_file(&image, &digest, &path, total, on_progress).await?;
/// ```
///
/// # File bars
///
/// ```ignore
/// let bar = ProgressBar::files(info_span!("Bundling"), count);
/// let _guard = bar.enter();
/// // `pb_inc(1)` calls inside archive code work via `Span::current()`.
/// archive.add_dir_all("", &source).await?;
/// ```
pub struct ProgressBar {
    span: tracing::Span,
}

impl ProgressBar {
    /// Creates a spinner with a package label but no progress bar.
    ///
    /// Use for operations where the total work is unknown or instant
    /// (find, select, deselect, uninstall, extract).
    ///
    /// Renders as: `⠋ Deselecting 'cmake:3.28'`
    pub fn spinner(span: tracing::Span, package: &impl std::fmt::Display) -> Self {
        set_span_package(&span, package);
        Self { span }
    }

    /// Creates a byte-transfer progress bar with a package label.
    ///
    /// Renders as: `⠋ Downloading 'cmake:3.28' [=====>  ] 12.5 MB/45.2 MB`
    pub fn bytes(span: tracing::Span, total: u64, package: &impl std::fmt::Display) -> Self {
        set_span_package(&span, package);
        span.pb_set_length(total);
        span.pb_set_style(&Style::Bytes.into());
        Self { span }
    }

    /// Creates a file-count progress bar.
    ///
    /// Renders as: `⠋ Bundling [=====>     ] 142/380 files`
    ///
    /// Callers increment position via `pb_inc(1)` on `tracing::Span::current()`
    /// from within the entered span.
    pub fn files(span: tracing::Span, count: u64) -> Self {
        span.pb_set_length(count);
        span.pb_set_style(&Style::Files.into());
        Self { span }
    }

    /// Returns a callback that sets the bar position.
    ///
    /// Suitable for passing to transport methods as a progress callback.
    pub fn callback(&self) -> Arc<dyn Fn(u64) + Send + Sync> {
        let span = self.span.clone();
        Arc::new(move |bytes: u64| {
            span.pb_set_position(bytes);
        })
    }

    /// Enters the span, returning a guard that exits on drop.
    pub fn enter(&self) -> tracing::span::Entered<'_> {
        self.span.enter()
    }

    /// Consumes the bar and returns the inner span for `.instrument()`.
    pub fn into_span(self) -> tracing::Span {
        self.span
    }
}

impl From<tracing::Span> for ProgressBar {
    /// Wraps a span without configuring a bar style.
    ///
    /// Useful as a fallback when the total count is unknown.
    fn from(span: tracing::Span) -> Self {
        Self { span }
    }
}

const PROGRESS_CHARS: &str = "=> ";

/// Progress bar style for tracing-indicatif spans.
enum Style {
    /// File-count bar: `⠋ Bundling [=====>     ] 142/380 files`
    Files,
    /// Byte-transfer bar: `⠋ Downloading 'cmake:3.28' [=====>  ] 12.5 MB/45.2 MB`
    Bytes,
}

impl From<Style> for indicatif::ProgressStyle {
    fn from(style: Style) -> Self {
        let (template, chars) = match style {
            Style::Files => (
                "{span_child_prefix}{span_name} [{bar:30}] {pos}/{len} files".to_string(),
                PROGRESS_CHARS,
            ),
            Style::Bytes => (
                "{span_child_prefix}{span_name}{msg} [{bar:30}] {bytes}/{total_bytes}".to_string(),
                PROGRESS_CHARS,
            ),
        };
        indicatif::ProgressStyle::default_bar()
            .template(&template)
            .expect("valid progress template")
            .progress_chars(chars)
    }
}
