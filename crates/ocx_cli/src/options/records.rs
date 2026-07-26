// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::record::RecordsOptions;

/// Where to write the execution record for this invocation.
///
/// Flatten into a launching command with `#[clap(flatten)]`. Resolve through
/// [`Records::options`] — never read the raw fields at a call site, so the
/// config / environment / flag fold stays the single precedence rule.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Records {
    /// Directory to write this invocation's execution record into.
    ///
    /// Overrides `[records] dir` and `OCX_RECORDS_DIR`. Without any of the
    /// three, no record is written. A record captures the resolved package
    /// closure and executable immediately before the tool starts. See
    /// https://ocx.sh/docs/reference/execution-records for the record format.
    #[clap(long = "records-dir", value_name = "DIRECTORY")]
    dir: Option<std::path::PathBuf>,

    /// Filename template for the record, e.g. `{time}-{host}-{pid}.json`.
    ///
    /// Placeholders: `{time}`, `{host}`, `{pid}`, `{rand}`. Defaults to
    /// `{time}-{pid}-{rand}.json`. The template must contain at least one of
    /// `{time}`, `{pid}` or `{rand}`; an unknown placeholder or a template that
    /// cannot vary exits 78. Overrides `[records] name` and `OCX_RECORDS_NAME`.
    /// See
    /// https://ocx.sh/docs/reference/execution-records#execution-records-filename
    /// for the full grammar.
    #[clap(long = "records-name", value_name = "TEMPLATE")]
    name: Option<String>,
}

impl Records {
    /// This tier's contribution to the records fold.
    ///
    /// `required` is absent by construction: the fail posture is settable from a
    /// config file only, so that a mistyped `--records-dir` warns rather than
    /// killing a build nobody set a policy for.
    pub fn options(&self) -> RecordsOptions {
        RecordsOptions {
            dir: self.dir.clone(),
            name: self.name.clone(),
            required: None,
            system_locked: false,
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
        records: Records,
    }

    fn options(args: &[&str]) -> RecordsOptions {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").records.options()
    }

    /// Neither flag -> this tier contributes nothing, so a lower tier decides.
    #[test]
    fn no_flags_contribute_nothing() {
        let resolved = options(&[]);
        assert!(resolved.dir.is_none());
        assert!(resolved.name.is_none());
    }

    /// Both flags reach the fold under the names the config tier uses.
    #[test]
    fn flags_populate_dir_and_name() {
        let resolved = options(&["--records-dir", "/var/log/ocx", "--records-name", "{pid}-{rand}.json"]);
        assert_eq!(resolved.dir.as_deref(), Some(std::path::Path::new("/var/log/ocx")));
        assert_eq!(resolved.name.as_deref(), Some("{pid}-{rand}.json"));
    }

    /// The fail posture is config-file-only, so this tier must never assert it —
    /// a mistyped `--records-dir` warns rather than killing the build.
    #[test]
    fn cli_tier_never_sets_required_or_lock() {
        let resolved = options(&["--records-dir", "/var/log/ocx"]);
        assert!(resolved.required.is_none(), "`required` is not settable from a flag");
        assert!(!resolved.system_locked, "only the SYSTEM config scope may clamp");
    }
}
