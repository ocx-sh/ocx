// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether a command also carries the artifacts anchored to each manifest —
/// signatures, SBOMs, attestations.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Referrers {
    /// Carry the signatures, SBOMs and attestations anchored to each manifest, plus the cosign `.sig`, `.att` and `.sbom` sidecar tags (default). A sidecar tag the target holds under a different manifest is reported, never overwritten, and the exit is 65. A target without the OCI 1.1 Referrers API exits 82.
    #[arg(long_help = "\
        Carry the signatures, SBOMs and attestations anchored to each manifest (default).\n\n\
        Requires the OCI 1.1 Referrers API at both ends; a registry without it fails rather than \
        silently moving an artifact whose signature stayed behind.\n\n\
        Cosign's `sha256-<hex>.sig`, `.att` and `.sbom` sidecar tags are ordinary tags, not \
        referrers, so they are probed by name and copied verbatim under the same tag with their \
        blobs. That sweep runs whether or not the target serves the Referrers API; a target \
        without it exits 82.")]
    #[clap(long = "referrers", overrides_with = "no_referrers")]
    referrers: bool,

    /// Leave the signatures, SBOMs and attestations behind.
    ///
    /// Promotes the package alone, sidecar tags included.
    #[clap(long = "no-referrers", overrides_with = "referrers")]
    no_referrers: bool,
}

impl Referrers {
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
        referrers: Referrers,
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
    fn explicit_referrers_enables() {
        assert!(enabled(&["--referrers"]));
    }

    #[test]
    fn last_wins() {
        assert!(
            !enabled(&["--referrers", "--no-referrers"]),
            "--no-referrers wins when last"
        );
        assert!(
            enabled(&["--no-referrers", "--referrers"]),
            "--referrers wins when last"
        );
    }
}
