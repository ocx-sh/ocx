// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether and how `ocx package create` scans the content tree for
/// interface-surface executables to fill or verify the `binaries` claim.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--bin-scan` / `--no-bin-scan` flags. The two use POSIX last-wins
/// semantics (`overrides_with`) — combining the flags is not an error (git
/// `--[no-]verify` idiom), same shape as [`super::Pull`]. Unlike `Pull`,
/// this resolves to a **tri-state** [`BinScanMode`] via [`BinScan::mode`]
/// rather than `enabled(default: bool)`: "auto" and "off" are not a
/// default/override pair, they are three distinct behaviors — see
/// `adr_declared_binaries_metadata.md` §2.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct BinScan {
    /// Scan the content tree for executables the package puts on `PATH`,
    /// verifying a declared `binaries` claim or filling an absent one.
    ///
    /// Requires `--metadata`/`-m`: there is nothing to check or fill
    /// without a metadata sidecar; omitting it is a usage error (exit 64).
    /// When `binaries` is undeclared, behaves
    /// like the default and fills it from the scan. When `binaries` is
    /// declared, verifies it against the scan and fails (exit 65) if a
    /// scanned executable is missing from the declared list, or a declared
    /// name exists on disk but is not executable. See
    /// https://ocx.sh/docs/reference/metadata#executables for the field's
    /// full semantics.
    #[clap(long = "bin-scan", overrides_with = "no_bin_scan")]
    bin_scan: bool,

    /// Skip the executable scan; the `binaries` field passes through
    /// unchanged, whether declared or absent.
    ///
    /// Use this to skip scan cost, or when content a later `push` layer
    /// adds makes a create-time scan meaningless. See
    /// https://ocx.sh/docs/reference/metadata#executables for what
    /// `binaries` means.
    #[clap(long = "no-bin-scan", overrides_with = "bin_scan")]
    no_bin_scan: bool,
}

/// Resolved scan behavior for `ocx package create`.
///
/// See `adr_declared_binaries_metadata.md` §2 mode table for the full
/// fill/verify/pass-through matrix against each authoring-field state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinScanMode {
    /// Neither flag given: scan and fill an absent `binaries` claim; pass a
    /// declared claim through verbatim (no verification).
    Auto,
    /// `--bin-scan`: scan; verify a declared claim one-directionally against
    /// the scan result, or fill an absent one exactly like `Auto`.
    Verify,
    /// `--no-bin-scan`: never scan; the `binaries` field passes through
    /// verbatim regardless of its state.
    Off,
}

impl BinScan {
    /// Resolves the paired flags to a [`BinScanMode`]. Neither flag yields
    /// `Auto`; `--bin-scan` yields `Verify`; `--no-bin-scan` yields `Off`.
    /// POSIX last-wins with both flags — `overrides_with` guarantees at most
    /// one of `bin_scan`/`no_bin_scan` is `true` after parsing.
    pub fn mode(&self) -> BinScanMode {
        if self.bin_scan {
            BinScanMode::Verify
        } else if self.no_bin_scan {
            BinScanMode::Off
        } else {
            BinScanMode::Auto
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
        bin_scan: BinScan,
    }

    fn mode(args: &[&str]) -> BinScanMode {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").bin_scan.mode()
    }

    /// Neither flag → `Auto`.
    #[test]
    fn no_flags_yield_auto() {
        assert_eq!(mode(&[]), BinScanMode::Auto);
    }

    /// `--bin-scan` alone → `Verify`.
    #[test]
    fn explicit_bin_scan_yields_verify() {
        assert_eq!(mode(&["--bin-scan"]), BinScanMode::Verify);
    }

    /// `--no-bin-scan` alone → `Off`.
    #[test]
    fn explicit_no_bin_scan_yields_off() {
        assert_eq!(mode(&["--no-bin-scan"]), BinScanMode::Off);
    }

    /// POSIX last-wins with both flags.
    #[test]
    fn last_wins() {
        assert_eq!(
            mode(&["--bin-scan", "--no-bin-scan"]),
            BinScanMode::Off,
            "--no-bin-scan wins when last"
        );
        assert_eq!(
            mode(&["--no-bin-scan", "--bin-scan"]),
            BinScanMode::Verify,
            "--bin-scan wins when last"
        );
    }
}
