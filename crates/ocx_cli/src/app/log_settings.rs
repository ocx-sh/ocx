// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub use ocx_lib::cli::LogSettings;
use ocx_lib::cli::indicatif::ProgressStyle;

pub fn init_with_indicatif(settings: LogSettings) -> anyhow::Result<()> {
    let style =
        ProgressStyle::with_template("{span_child_prefix}{spinner} {span_name}").expect("valid indicatif template");
    settings
        .init_with_indicatif(style)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}
