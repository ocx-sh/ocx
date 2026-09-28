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
    /// Whether verification is enabled, ignoring `OCX_NO_VERIFY`.
    pub fn enabled(&self) -> bool {
        self.resolve(false)
    }

    /// Resolve against an env opt-out: either flag wins, and with neither the env decides.
    pub fn resolve(&self, env_opt_out: bool) -> bool {
        resolve_flag_over_env(self.verify, self.no_verify, env_opt_out)
    }
}

/// Whether `ocx package install` / `pull` verify a policy-covered package's
/// Sigstore signature: `--verify` / `--no-verify`, POSIX last-wins.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct SignatureVerify {
    /// Verify the package's Sigstore signature before installing (default).
    ///
    /// When a `[[trust.policy]]` covers the package, its keyless Sigstore
    /// signature is verified before the package is installed; a failure aborts
    /// the install fail-closed. Overrides an `OCX_NO_VERIFY` opt-out for this
    /// invocation.
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY`.
    #[clap(long = "no-verify", overrides_with = "verify")]
    no_verify: bool,
}

impl SignatureVerify {
    /// Resolve against the `OCX_NO_VERIFY` opt-out: either flag wins, and with neither the env decides.
    pub fn resolve(&self, env_opt_out: bool) -> bool {
        resolve_flag_over_env(self.verify, self.no_verify, env_opt_out)
    }
}

/// Resolve a paired `--verify` / `--no-verify` against an env opt-out, the flag winning.
fn resolve_flag_over_env(verify: bool, no_verify: bool, env_opt_out: bool) -> bool {
    if no_verify {
        false
    } else if verify {
        true
    } else {
        !env_opt_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

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

    fn resolve(args: &[&str], env_opt_out: bool) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
            .expect("parse")
            .verify
            .resolve(env_opt_out)
    }

    #[test]
    fn resolve_flag_wins_over_env() {
        // Explicit --no-verify turns off regardless of env.
        assert!(!resolve(&["--no-verify"], false));
        assert!(!resolve(&["--no-verify"], true));
        // Explicit --verify turns on, overriding an env opt-out.
        assert!(resolve(&["--verify"], true));
        assert!(resolve(&["--verify"], false));
    }

    #[test]
    fn resolve_env_decides_when_no_flag() {
        assert!(resolve(&[], false), "no flag + env off => verification on");
        assert!(!resolve(&[], true), "no flag + env opt-out => verification off");
    }
}
