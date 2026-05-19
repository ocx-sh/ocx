// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::progress::Spinner;

/// Update the task spinner's message to show the active stage.
pub fn set_stage(spinner: &Spinner, label: &str, version: &str, platform: &impl std::fmt::Display) {
    spinner.set_message(format!("{version} {platform} — {label}"));
}
