// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing::Span;

/// Update the current span's message to show the active stage.
pub fn set_stage(span: &Span, label: &str, version: &str, platform: &impl std::fmt::Display) {
    span.pb_set_message(&format!("{version} {platform} — {label}"));
}
