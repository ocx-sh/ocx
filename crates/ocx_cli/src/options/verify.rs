// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to verify credentials against the registry before storing them
/// (the `ocx login` credential ping).
///
/// Flatten into `login` with `#[clap(flatten)]` to add the paired `--verify` /
/// `--no-verify` flags. Verification is the default: `--verify` is the
/// affirmative form, `--no-verify` opts out. The two use POSIX last-wins
/// semantics (`overrides_with`), matching the `--pull` / `--no-pull`
/// convention. Env-ignorant — resolve with [`Verify::enabled`]. For the
/// install/pull Sigstore-signature gate, see [`SignatureVerify`].
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
    /// Resolve whether verification is enabled. Default is on; only an explicit
    /// (last-wins) `--no-verify` turns it off.
    ///
    /// Login is env-ignorant, so this resolves as if there is no `OCX_NO_VERIFY`
    /// opt-out — equivalent to `resolve(false)`.
    pub fn enabled(&self) -> bool {
        self.resolve(false)
    }

    /// Resolve verification against an env-var opt-out, with the flag winning
    /// over the env. Used by install/pull where `OCX_NO_VERIFY` mirrors
    /// `--no-verify`:
    ///
    /// - explicit `--no-verify` → `false` (off, regardless of env)
    /// - explicit `--verify` → `true` (on, overriding an env opt-out)
    /// - neither flag → `!env_opt_out` (the env decides)
    pub fn resolve(&self, env_opt_out: bool) -> bool {
        resolve_flag_over_env(self.verify, self.no_verify, env_opt_out)
    }
}

/// Whether to verify the package's Sigstore signature before installing it
/// (the policy-gated auto-verify gate on `ocx package install` / `pull`).
///
/// Flatten into `install` / `pull` with `#[clap(flatten)]` to add the paired
/// `--verify` / `--no-verify` flags. When a `[[trust.policy]]` covers the
/// package, its keyless Sigstore signature is verified before the package is
/// installed and a failure aborts fail-closed. The flag wins over the
/// `OCX_NO_VERIFY` environment variable; POSIX last-wins between the two forms
/// (`overrides_with`). Resolve with [`SignatureVerify::resolve`]. Distinct from
/// [`Verify`], which is the env-ignorant `ocx login` credential ping.
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
    /// Resolve signature verification against the `OCX_NO_VERIFY` env opt-out,
    /// with the flag winning over the env:
    ///
    /// - explicit `--no-verify` → `false` (off, regardless of env)
    /// - explicit `--verify` → `true` (on, overriding an env opt-out)
    /// - neither flag → `!env_opt_out` (the env decides)
    pub fn resolve(&self, env_opt_out: bool) -> bool {
        resolve_flag_over_env(self.verify, self.no_verify, env_opt_out)
    }
}

/// Resolve a paired `--verify` / `--no-verify` against an env opt-out, with the
/// flag winning over the env. Shared by [`Verify::resolve`] and
/// [`SignatureVerify::resolve`] so the flag-over-env precedence lives once.
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
