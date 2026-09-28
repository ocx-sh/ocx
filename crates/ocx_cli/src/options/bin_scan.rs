// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether and how `ocx package create` scans the content tree for
/// interface-surface executables to fill or verify the `binaries` claim.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct BinScan {
    /// Scan the content tree for executables the package puts on `PATH`,
    /// verifying a declared `binaries` claim or filling an absent one.
    ///
    /// Requires `--metadata`/`-m`, without which there is nothing to check or
    /// fill (exit 64). An undeclared `binaries` is filled from the scan, as by
    /// default; a declared one is verified against it, failing (exit 65) if a
    /// scanned executable is missing from the list or a declared name exists
    /// but is not executable. See https://ocx.sh/docs/reference/metadata#executables.
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

/// Resolved scan behavior for `ocx package create` (matrix: `adr_declared_binaries_metadata.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinScanMode {
    /// Neither flag: fill an absent `binaries` claim; pass a declared one through unverified.
    Auto,
    /// `--bin-scan`: verify a declared claim one-directionally against the scan, or fill an absent one.
    Verify,
    /// `--no-bin-scan`: never scan; `binaries` passes through verbatim.
    Off,
}

impl BinScan {
    /// Resolves the paired flags to a [`BinScanMode`].
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
