// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether an export also carries the artifacts anchored to each manifest: signatures, SBOMs, attestations.
///
/// `options::Referrers` names the Referrers API and cosign sidecar tags of a registry copy, which an export has
/// no use for, so export flattens this struct, whose help describes the directory.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ExportReferrers {
    /// Carry the signatures, SBOMs and attestations of each exported manifest (default)
    #[arg(
        long = "referrers",
        overrides_with = "no_referrers",
        long_help = "\
        Carry the signatures, SBOMs and attestations of each exported manifest (default).\n\n\
        A trust policy that covers a package can then verify the import with no network access."
    )]
    referrers: bool,

    /// Leave them out; a trust policy then cannot verify the import offline
    #[arg(
        long = "no-referrers",
        overrides_with = "referrers",
        long_help = "\
        Leave the signatures, SBOMs and attestations out.\n\n\
        A package a trust policy covers then fails to verify when it is imported, because the import has no \
        network to fetch them from."
    )]
    no_referrers: bool,
}

impl ExportReferrers {
    /// Whether referrers travel; on unless `--no-referrers` won.
    pub fn enabled(&self) -> bool {
        !self.no_referrers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        referrers: ExportReferrers,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").referrers.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "referrers must travel by default");
    }

    #[test]
    fn no_referrers_disables() {
        assert!(!enabled(&["--no-referrers"]));
    }

    #[test]
    fn last_wins() {
        assert!(!enabled(&["--referrers", "--no-referrers"]));
        assert!(enabled(&["--no-referrers", "--referrers"]));
    }
}
