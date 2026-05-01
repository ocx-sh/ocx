// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci;

/// Shared `--platform` / `-p` argument group for commands that accept a platform filter.
///
/// Flatten into a command struct with `#[clap(flatten)]` to add the standard
/// `-p/--platform` argument. When the user supplies no platforms the empty
/// `Vec` is the signal; callers use `conventions::platforms_or_default` to
/// expand it to the current host platform.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Platforms {
    /// Target platforms to consider when resolving packages.
    #[clap(
        short = 'p',
        long = "platform",
        value_delimiter = ',',
        value_name = "PLATFORM",
        num_args = 1
    )]
    pub platforms: Vec<oci::Platform>,
}

impl Platforms {
    /// Consume the flag and return the inner `Vec<oci::Platform>`.
    #[allow(dead_code)]
    pub fn into_vec(self) -> Vec<oci::Platform> {
        self.platforms
    }

    /// Borrow the inner platform slice.
    pub fn as_slice(&self) -> &[oci::Platform] {
        &self.platforms
    }
}
