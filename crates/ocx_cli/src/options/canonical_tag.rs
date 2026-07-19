// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether `ocx package push` also pushes a digest-named `sha256.<hex>` tag
/// for each pushed platform manifest.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--canonical-tag` / `--no-canonical-tag` flags. Canonical tagging is the
/// default: `--canonical-tag` is the affirmative form of the default,
/// `--no-canonical-tag` opts out. The two use POSIX last-wins semantics
/// (`overrides_with`), matching the `--verify` / `--no-verify` convention.
/// Resolve with [`CanonicalTag::enabled`].
#[derive(clap::Args, Clone, Debug, Default)]
pub struct CanonicalTag {
    /// Push a `sha256.<hex>` tag pointing at each pushed platform manifest
    /// (default).
    #[clap(long = "canonical-tag", overrides_with = "no_canonical_tag")]
    canonical_tag: bool,

    /// Skip the canonical tag push.
    #[clap(long = "no-canonical-tag", overrides_with = "canonical_tag")]
    no_canonical_tag: bool,
}

impl CanonicalTag {
    /// Resolve whether the canonical tag push is enabled. Default is on;
    /// only an explicit (last-wins) `--no-canonical-tag` turns it off.
    pub fn enabled(&self) -> bool {
        !self.no_canonical_tag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        canonical_tag: CanonicalTag,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").canonical_tag.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "canonical tag push must default on");
    }

    #[test]
    fn no_canonical_tag_disables() {
        assert!(!enabled(&["--no-canonical-tag"]));
    }

    #[test]
    fn explicit_canonical_tag_enables() {
        assert!(enabled(&["--canonical-tag"]));
    }

    #[test]
    fn last_wins() {
        assert!(
            !enabled(&["--canonical-tag", "--no-canonical-tag"]),
            "--no-canonical-tag wins when last"
        );
        assert!(
            enabled(&["--no-canonical-tag", "--canonical-tag"]),
            "--canonical-tag wins when last"
        );
    }
}
