// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Number of entries between periodic debug log messages during archiving.
pub const LOG_INTERVAL: u64 = 100;

/// Returns a progress bar style that shows the span name, a bar, and a counter.
///
/// Renders as: `⠋ Bundling [=====>     ] 142/380 files`
#[cfg(feature = "progress")]
pub fn bar_style(unit: &str) -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::default_bar()
        .template(&format!(
            "{{span_child_prefix}}{{spinner}} {{span_name}} [{{bar:30}}] {{pos}}/{{len}} {unit}"
        ))
        .expect("valid progress template")
        .progress_chars("=> ")
}
