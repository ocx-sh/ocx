// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Where `ocx vendor` and `ocx package vendor` write, and how much of it.
#[derive(clap::Args, Debug, Clone)]
pub struct VendorOutput {
    // Both commands flatten this after `--platform`; order 0 lists `--output` first
    // (it wins the tie with `--platform` by name). Change it only with the help text of
    // `ocx vendor` and `ocx package vendor` in view.
    /// Directory to write; absent or empty.
    #[arg(short = 'o', long = "output", value_name = "DIR", required = true, display_order = 0)]
    pub output: PathBuf,

    /// Write `manifest.json` only: no package content and no launcher.
    #[arg(long = "manifest-only")]
    pub manifest_only: bool,

    /// Create no symlink under the directory.
    #[arg(long = "omit-links")]
    pub omit_links: bool,
}
