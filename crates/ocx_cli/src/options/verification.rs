// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci::verify::VerificationMode;

/// Whether an SBOM read demands a verifiable signature.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired `--verify`
/// / `--no-verify` flags. Tri-state, not a boolean with a default: with
/// neither flag given the mode is resolved from the invocation itself —
/// identity flags or a matching `[[trust.policy]]` mean verification was asked
/// for, and their absence means there is nothing to verify against. So
/// "neither flag" is a third outcome and not a synonym for either, which is
/// why this returns an `Option` rather than a `bool` (the shape
/// [`super::BinScan`] uses for the same reason).
///
/// The two flags last-win (`overrides_with`, the `git --[no-]verify` idiom),
/// like every other paired toggle here — combining them is not an error, and
/// the later one decides. That matters when one of them is injected from
/// outside the command line by a wrapper or an alias: a conflict would leave
/// the caller no way to override it back.
///
/// `--no-verify` **does** conflict with the certificate flags, which is a
/// different thing: those are not the other half of a pair, and supplying an
/// identity while refusing to check it is contradictory rather than
/// overridden.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Verification {
    /// Require a verified signature; refuse unsigned attachments.
    ///
    /// The default when identity flags are given, or when a [trust.policy]
    /// covers the package. Without either, there is nothing to verify
    /// against and this is a usage error.
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// List documents without verifying anything.
    ///
    /// Reads signed and unsigned SBOMs alike and marks every one unverified.
    /// No signer identity is reported, because none was checked. Cannot be
    /// combined with the certificate flags.
    #[clap(
        long = "no-verify",
        overrides_with = "verify",
        conflicts_with_all = ["certificate_identity", "certificate_oidc_issuer"]
    )]
    no_verify: bool,
}

impl Verification {
    /// The mode the flags name, or `None` when neither was given and the
    /// invocation's identity sources decide.
    ///
    /// At most one arm is reachable: `overrides_with` resets the flag it
    /// overrode to `false`, so the pair cannot both be set however many times
    /// they appear. The match order is therefore not a tie-break.
    pub fn requested(&self) -> Option<VerificationMode> {
        match (self.verify, self.no_verify) {
            (true, _) => Some(VerificationMode::Demand),
            (_, true) => Some(VerificationMode::Permissive),
            _ => None,
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
        verification: Verification,
        #[clap(long = "certificate-identity", requires = "certificate_oidc_issuer")]
        certificate_identity: Option<String>,
        #[clap(long = "certificate-oidc-issuer", requires = "certificate_identity")]
        certificate_oidc_issuer: Option<String>,
    }

    fn requested(args: &[&str]) -> Option<VerificationMode> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").verification.requested()
    }

    /// Neither flag is a third state: the caller resolves it from the
    /// identity sources, and must not read it as either flag's default.
    #[test]
    fn no_flag_defers_to_the_caller() {
        assert_eq!(requested(&[]), None);
    }

    #[test]
    fn each_flag_names_its_mode() {
        assert_eq!(requested(&["--verify"]), Some(VerificationMode::Demand));
        assert_eq!(requested(&["--no-verify"]), Some(VerificationMode::Permissive));
    }

    /// POSIX last-wins, both orders — the house convention for every paired
    /// toggle, so a wrapper that injects one flag can always be overridden on
    /// the command line.
    ///
    /// Asserted in both directions on purpose: a single `overrides_with`
    /// attribute makes only one direction work, and the half that is missing
    /// then silently reports the *earlier* flag.
    #[test]
    fn the_two_flags_last_win() {
        assert_eq!(
            requested(&["--verify", "--no-verify"]),
            Some(VerificationMode::Permissive),
            "--no-verify wins when last",
        );
        assert_eq!(
            requested(&["--no-verify", "--verify"]),
            Some(VerificationMode::Demand),
            "--verify wins when last",
        );
    }

    /// Supplying an identity while refusing to check it is contradictory.
    #[test]
    fn no_verify_conflicts_with_the_certificate_flags() {
        let parsed = Harness::try_parse_from([
            "harness",
            "--no-verify",
            "--certificate-identity",
            "me@example.com",
            "--certificate-oidc-issuer",
            "https://example.com",
        ]);
        assert!(parsed.is_err(), "--no-verify with an identity must be a usage error");
    }

    /// The identity flags alone still parse — that is the demand default.
    #[test]
    fn the_certificate_flags_alone_are_accepted() {
        assert_eq!(
            requested(&[
                "--certificate-identity",
                "me@example.com",
                "--certificate-oidc-issuer",
                "https://example.com"
            ]),
            None,
            "identity flags name no mode themselves; the caller reads them",
        );
    }
}
