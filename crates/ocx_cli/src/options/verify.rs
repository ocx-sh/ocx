// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether `ocx login` verifies credentials against the registry before
/// storing them: `--verify` / `--no-verify`, POSIX last-wins, on by default.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Verify {
    /// Verify the operation against the registry before committing it (default).
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// Skip verification.
    #[clap(long = "no-verify", overrides_with = "verify")]
    no_verify: bool,
}

impl Verify {
    /// Whether verification is enabled; `ocx login` reads no `OCX_NO_VERIFY`.
    pub fn enabled(&self) -> bool {
        explicit_flag(self.verify, self.no_verify).unwrap_or(true)
    }
}

/// Whether an `ocx index` verb checks the signed catalog of the sources it reads:
/// `--verify` / `--no-verify`, POSIX last-wins.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct CatalogVerify {
    /// Check the signed index catalog even when `OCX_NO_VERIFY` is set.
    ///
    /// A source whose registry has an index signer list (the built-in one for `ocx.sh`) is checked against
    /// it. The flag wins over `OCX_NO_VERIFY` for this run.
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// Skip the signed index catalog check for this run
    ///
    /// Equivalent env var: `OCX_NO_VERIFY`. The flag wins over the env var. The bypass prints one warning and
    /// leaves the recorded expiry bound untouched.
    #[clap(long = "no-verify", overrides_with = "verify")]
    no_verify: bool,
}

impl CatalogVerify {
    /// What the user typed: `Some(true)` for `--verify`, `Some(false)` for `--no-verify`, `None` for neither,
    /// so `OCX_NO_VERIFY` still decides.
    pub fn explicit(&self) -> Option<bool> {
        explicit_flag(self.verify, self.no_verify)
    }
}

/// Whether `ocx package install` / `pull` verify a policy-covered package's
/// Sigstore signature and check the signed index catalog: `--verify` / `--no-verify`, POSIX last-wins.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct SignatureVerify {
    /// Verify the package's Sigstore signature before installing (default).
    #[arg(long_help = "\
        Verify the package's Sigstore signature before installing (default).\n\n\
        When the package's index entry lists `publishers`, or a `[[trust.policy]]` covers it, its \
        Sigstore signature is verified before the package is installed; both apply, and a failure \
        aborts the install fail-closed. Also checks the signed index catalog of every index source. \
        Overrides an `OCX_NO_VERIFY` opt-out for this invocation.\n\n\
        The check runs after the manifest digest resolves and before any layer downloads or state \
        is written. A package the index lists no publishers for and no `[[trust.policy]]` covers is \
        not verified. A failed check exits 65 (tampered bundle), 77 (identity or issuer mismatch), \
        78 (trust-root or policy problem) or 79 (no signature found). Under `--offline` the check \
        reuses local trust material and fails closed with 81 (`PolicyBlocked`) when there is none.")]
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY`.
    ///
    /// The flag wins over the env var, and the bypass prints one warning per
    /// invocation. It skips the signed index catalog check too. Commands
    /// without the flag honour the env var only; `ocx index` verbs that read a
    /// source take their own pair for the catalog check.
    #[clap(long = "no-verify", overrides_with = "verify")]
    no_verify: bool,
}

impl SignatureVerify {
    /// What the user typed: `Some(true)` for `--verify`, `Some(false)` for `--no-verify`, `None` for neither.
    pub fn explicit(&self) -> Option<bool> {
        explicit_flag(self.verify, self.no_verify)
    }
}

/// The typed answer of a paired `--verify` / `--no-verify`; `overrides_with` leaves at most one set.
fn explicit_flag(verify: bool, no_verify: bool) -> Option<bool> {
    if no_verify { Some(false) } else { verify.then_some(true) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct CatalogHarness {
        #[clap(flatten)]
        verify: CatalogVerify,
    }

    fn catalog_explicit(args: &[&str]) -> Option<bool> {
        let mut argv = vec!["h"];
        argv.extend_from_slice(args);
        CatalogHarness::try_parse_from(argv).expect("parse").verify.explicit()
    }

    #[test]
    fn catalog_verify_states_what_was_typed_and_last_flag_wins() {
        assert_eq!(catalog_explicit(&[]), None);
        assert_eq!(catalog_explicit(&["--verify"]), Some(true));
        assert_eq!(catalog_explicit(&["--no-verify"]), Some(false));
        assert_eq!(catalog_explicit(&["--no-verify", "--verify"]), Some(true));
        assert_eq!(catalog_explicit(&["--verify", "--no-verify"]), Some(false));
    }

    #[test]
    fn catalog_verify_help_is_short_and_names_the_catalog_not_the_signer_list() {
        use clap::CommandFactory as _;
        let command = CatalogHarness::command();
        for name in ["verify", "no_verify"] {
            let help = command
                .get_arguments()
                .find(|arg| arg.get_id() == name)
                .and_then(clap::Arg::get_help)
                .map(ToString::to_string)
                .expect("the flag carries help");
            assert!(help.len() <= 70, "{name} short help is {} chars: {help}", help.len());
            assert!(!help.contains("signer list"), "{help}");
            assert!(help.contains("index catalog"), "{help}");
        }
    }

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        verify: Verify,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").verify.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "verification must default on");
    }

    #[test]
    fn no_verify_disables() {
        assert!(!enabled(&["--no-verify"]));
    }

    #[test]
    fn explicit_verify_enables() {
        assert!(enabled(&["--verify"]));
    }

    #[test]
    fn last_wins() {
        assert!(!enabled(&["--verify", "--no-verify"]), "--no-verify wins when last");
        assert!(enabled(&["--no-verify", "--verify"]), "--verify wins when last");
    }
}
