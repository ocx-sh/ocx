// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// The `--profile` / `--no-profile` tier of `ocx self setup`'s profile-target
/// resolution.
///
/// Flatten into `ocx self setup` with `#[clap(flatten)]` to add both flags.
/// The two use POSIX last-wins semantics (`overrides_with`) — combining them
/// is not an error (the git `--[no-]verify` idiom), the same shape as
/// [`super::Pinned`] and [`super::Consent`]. Resolve through
/// [`Profiles::explicit`] — never read either field at a call site.
///
/// # Why the return is `Option<Vec<PathBuf>>`, not `Vec<PathBuf>`
///
/// Absence and an explicitly empty list are different answers here, in the
/// same way `Consent::explicit`'s `None` differs from a `false` nobody typed:
/// omitting both flags means "auto-detect the usual profile files", which is
/// a resolution this type does not perform — that is the config rung's job.
/// `--no-profile` means "write no profile blocks at all", which is a
/// *decision*, not an absence of one. Collapsing `--no-profile` to `None`
/// would make it indistinguishable from typing nothing, and the setup writer
/// would fall through to auto-detection instead of honoring the refusal.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Profiles {
    /// Target an explicit profile file. Repeatable.
    ///
    /// Explicit targets are written with POSIX-fence semantics regardless of
    /// the file name. Omit to auto-detect the usual profile files for the
    /// current shell.
    ///
    /// https://ocx.sh/docs/reference/command-line#self-setup
    #[clap(long = "profile", value_name = "PATH", overrides_with = "no_profile")]
    profile: Vec<PathBuf>,

    /// Write no profile blocks at all.
    ///
    /// Suppresses only the profile-block surface; the env shims and, unless
    /// `--no-modify-path` is also given, the session-PATH registration are
    /// still written. Combine with an explicit `--profile` and the one typed
    /// last wins, POSIX style.
    ///
    /// https://ocx.sh/docs/reference/command-line#self-setup
    #[clap(long = "no-profile", overrides_with = "profile")]
    no_profile: bool,
}

impl Profiles {
    /// Resolves the paired flags to the CLI tier of the profile-target
    /// ladder.
    ///
    /// `None` means neither flag was given, so the config rung's
    /// auto-detection still applies. `Some(vec![])` is `--no-profile` having
    /// won — an explicit refusal, never `None`'s "nothing was said". POSIX
    /// last-wins with both flags: `overrides_with` guarantees at most one of
    /// `no_profile` and a non-empty `profile` survives parsing, so checking
    /// `no_profile` first is enough to read the winner off either field.
    #[must_use]
    pub fn explicit(&self) -> Option<Vec<PathBuf>> {
        if self.no_profile {
            Some(Vec::new())
        } else if self.profile.is_empty() {
            None
        } else {
            Some(self.profile.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        profiles: Profiles,
    }

    fn explicit(args: &[&str]) -> Option<Vec<PathBuf>> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").profiles.explicit()
    }

    /// Red state (shown, then fixed): return `Some(vec![])` from the `else`
    /// arm of [`Profiles::explicit`] — the no-flags case must not answer for
    /// a user who typed nothing, or the config rung's auto-detection would
    /// never get a turn.
    #[test]
    fn explicit_is_none_when_no_flag_typed() {
        assert_eq!(explicit(&[]), None, "no flag must not answer for the user");
    }

    /// `--profile` accumulates in the order given, with no `--no-profile` in
    /// play.
    #[test]
    fn repeated_profile_collects_every_path() {
        assert_eq!(
            explicit(&["--profile", "a", "--profile", "b"]),
            Some(vec![PathBuf::from("a"), PathBuf::from("b")])
        );
    }

    /// The single most important test in this file: `--no-profile` resolves
    /// to an explicitly empty list, and combining it with `--profile` still
    /// lands there when `--no-profile` is the one typed last — `Some(vec![])`
    /// must never collapse into `None`, since the two mean different things
    /// all the way down to the config file.
    ///
    /// Red state (shown, then fixed): make `--no-profile` clear to `None`
    /// instead of `Some(vec![])`.
    #[test]
    fn no_profile_is_empty_not_absent() {
        assert_eq!(explicit(&["--no-profile"]), Some(Vec::new()));
        assert_eq!(
            explicit(&["--profile", "a", "--no-profile"]),
            Some(Vec::new()),
            "--no-profile typed after --profile must still win"
        );
    }

    /// RUL-60's parse half, mirrored from `pinned.rs`: the pair is POSIX
    /// last-wins in both directions, not a usage error. Both orders, because
    /// a one-sided `overrides_with` passes the first and reds the second.
    #[test]
    fn the_last_flag_wins_in_either_order() {
        assert_eq!(
            explicit(&["--profile", "a", "--no-profile"]),
            Some(Vec::new()),
            "--no-profile last must win"
        );
        assert_eq!(
            explicit(&["--no-profile", "--profile", "a"]),
            Some(vec![PathBuf::from("a")]),
            "--profile last must win"
        );
    }

    /// Neither flag's grammar changes shape under the pair: `--no-profile`
    /// takes no value, `--profile` always takes exactly one path per
    /// occurrence.
    #[test]
    fn no_profile_takes_no_value() {
        assert!(
            Harness::try_parse_from(["harness", "--no-profile=true"]).is_err(),
            "--no-profile must be rejected; it is a bare toggle"
        );
        assert!(Harness::try_parse_from(["harness", "--no-profile"]).is_ok());
    }
}
