// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci;

/// Shared `--platform` / `-p` argument for commands that resolve against a
/// single platform.
///
/// Flatten into a command struct with `#[clap(flatten)]` to add the standard
/// `-p/--platform` argument. Absent is the signal for "use the current host
/// platform" — callers use `conventions::platform_or_default` to apply that
/// default.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct PlatformOption {
    /// Target platform to resolve packages against.
    ///
    /// The value is `os/arch[/variant][+feature[,feature...]]`,
    /// for example `linux/amd64`, `linux/arm64`, or `linux/amd64+libc.glibc`.
    /// The optional `+feature` suffix filters by `os.features`: OCX selects
    /// the manifest whose features are a subset of the value you pass, so
    /// `+libc.glibc` or `+libc.musl` forces a specific libc variant. Defaults
    /// to the auto-detected host platform. Details:
    /// <https://ocx.sh/docs/authoring/multi-platform>
    #[clap(short = 'p', long = "platform", value_name = "PLATFORM")]
    pub platform: Option<oci::Platform>,
}
