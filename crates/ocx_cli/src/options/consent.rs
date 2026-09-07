// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to record a shell-activation consent stamp for the project a
/// command creates.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--consent` / `--no-consent` flags. The two use POSIX last-wins semantics
/// (`overrides_with`) — combining them is not an error (git `--[no-]verify`
/// idiom). Resolve with [`Consent::enabled`], passing the command's default;
/// `ocx init` passes `true`, because creating an `ocx.toml` in a directory is
/// at least as deliberate a gesture as the `ocx add` that already stamps one.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Consent {
    /// Record a consent stamp for the new project (the default)
    ///
    /// The stamp is what lets a shell prompt in this directory apply the
    /// project's tools and environment; without one the project is inert.
    /// It is the same stamp `ocx add`, `ocx lock`, `ocx pull` and `ocx exec`
    /// write as a side effect, and `ocx shell revoke` takes back.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    #[clap(long = "consent", overrides_with = "no_consent")]
    consent: bool,

    /// Create the project without consenting to its shell activation
    ///
    /// Consent later with `ocx shell allow`, or by running any mutating
    /// command in the directory.
    #[clap(long = "no-consent", overrides_with = "consent")]
    no_consent: bool,
}

impl Consent {
    /// Resolve whether a consent stamp is recorded. `default` is the command's
    /// behavior when neither flag is given; an explicit (last-wins) flag
    /// overrides it.
    pub fn enabled(&self, default: bool) -> bool {
        if self.consent {
            true
        } else if self.no_consent {
            false
        } else {
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        consent: Consent,
    }

    fn enabled(args: &[&str], default: bool) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").consent.enabled(default)
    }

    /// Neither flag → the command's default decides, both ways.
    #[test]
    fn no_flags_yield_default() {
        assert!(enabled(&[], true), "the consenting default must hold without flags");
        assert!(!enabled(&[], false), "a non-consenting default must hold without flags");
    }

    /// `--consent` alone → on regardless of default.
    #[test]
    fn explicit_consent_enables() {
        assert!(enabled(&["--consent"], true));
        assert!(enabled(&["--consent"], false));
    }

    /// `--no-consent` alone → off regardless of default.
    #[test]
    fn explicit_no_consent_disables() {
        assert!(!enabled(&["--no-consent"], true));
        assert!(!enabled(&["--no-consent"], false));
    }

    /// POSIX last-wins with both flags, under both defaults.
    #[test]
    fn last_wins() {
        for default in [true, false] {
            assert!(
                !enabled(&["--consent", "--no-consent"], default),
                "--no-consent wins when last (default {default})"
            );
            assert!(
                enabled(&["--no-consent", "--consent"], default),
                "--consent wins when last (default {default})"
            );
        }
    }
}
