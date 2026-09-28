// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether a command also writes a digest-named `__ocx.keep.<algorithm>-<hex>`
/// tag for each platform manifest it publishes.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct KeepTag {
    /// Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest
    /// published (default).
    #[clap(long = "keep-tag", overrides_with = "no_keep_tag")]
    keep_tag: bool,

    /// Skip the keep tag.
    #[clap(long = "no-keep-tag", overrides_with = "keep_tag")]
    no_keep_tag: bool,
}

impl KeepTag {
    /// Whether the keep tag is written; on unless `--no-keep-tag` won.
    pub fn enabled(&self) -> bool {
        !self.no_keep_tag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        keep_tag: KeepTag,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").keep_tag.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "keep tag push must default on");
    }

    #[test]
    fn no_keep_tag_disables() {
        assert!(!enabled(&["--no-keep-tag"]));
    }

    #[test]
    fn explicit_keep_tag_enables() {
        assert!(enabled(&["--keep-tag"]));
    }

    #[test]
    fn last_wins() {
        assert!(
            !enabled(&["--keep-tag", "--no-keep-tag"]),
            "--no-keep-tag wins when last"
        );
        assert!(enabled(&["--no-keep-tag", "--keep-tag"]), "--keep-tag wins when last");
    }
}
