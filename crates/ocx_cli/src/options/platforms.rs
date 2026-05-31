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
    ///
    /// Each value is `os/arch[/variant][+feature[+feature...]]`, for example
    /// `linux/amd64`, `linux/arm64`, or `linux/amd64+libc.glibc`. The optional
    /// `+feature` suffix filters by `os.features`: OCX selects the manifest
    /// whose features are a subset of the value you pass, so `+libc.glibc` or
    /// `+libc.musl` forces a specific libc variant. Repeat the flag or pass a
    /// comma-separated list for several platforms. Defaults to the
    /// auto-detected host platform - see the
    /// [multi-platform packages](https://ocx.sh/docs/authoring/multi-platform)
    /// documentation for details.
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
