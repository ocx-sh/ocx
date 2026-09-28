// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether `ocx self setup` was explicitly told to skip modifying the user's
/// shell profiles and session PATH.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ModifyPath {
    /// Write the env shims but touch neither a shell profile nor the
    /// session PATH.
    ///
    /// Suppresses both PATH surfaces: the managed activation block in your
    /// shell profiles, and the session-level registration. Remembered in
    /// `[shell] modify_path`, so later runs keep honouring it, including the
    /// setup that `ocx self update` performs. Remove that key to resume
    /// writing both.
    ///
    /// https://ocx.sh/docs/reference/command-line#self-setup
    #[clap(long = "no-modify-path")]
    no_modify_path: bool,
}

impl ModifyPath {
    /// `Some(false)` for `--no-modify-path`, else `None`.
    ///
    /// Collapsing `None` to a `bool` lets absence outrank
    /// [`OCX_NO_MODIFY_PATH`](ocx_config::env::keys::OCX_NO_MODIFY_PATH) and `config.toml`.
    pub fn explicit(&self) -> Option<bool> {
        if self.no_modify_path { Some(false) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        modify_path: ModifyPath,
    }

    fn explicit(args: &[&str]) -> Option<bool> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").modify_path.explicit()
    }

    /// Red state (shown, then fixed): return `Some(false)` from the `else`
    /// arm of [`ModifyPath::explicit`] — the absent-flag case must not
    /// answer for a user who typed nothing, or `OCX_NO_MODIFY_PATH` and
    /// `config.toml` would never get a turn.
    #[test]
    fn explicit_is_none_when_no_flag_typed() {
        assert_eq!(explicit(&[]), None, "no flag must not answer for the user");
        assert_eq!(explicit(&["--no-modify-path"]), Some(false));
    }

    /// There is no `--modify-path` to combine it with — the pair-idiom
    /// `overrides_with` tests in `pinned.rs`/`consent.rs` do not apply here,
    /// since a second flag would violate C-043 in the first place.
    #[test]
    fn no_modify_path_is_a_bare_toggle_with_no_value() {
        let mut argv = vec!["harness"];
        argv.push("--no-modify-path=true");
        assert!(
            Harness::try_parse_from(argv).is_err(),
            "--no-modify-path must be rejected; it takes no value"
        );
    }
}
