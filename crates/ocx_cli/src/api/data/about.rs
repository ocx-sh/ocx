// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;
use crate::app::build_info::Provenance;

/// System information about the ocx installation.
///
/// Plain format: colored logo with key-value pairs alongside.
///
/// JSON format: flat object with version + registry + platforms + libc +
/// shell + home plus optional `channel`, `commit`, `build`, `ci` blocks
/// merged from [`Provenance::current`]. The build-provenance fields are
/// absent on local `cargo build` without git, matching `ocx version
/// --format json` behaviour. `libc` is a JSON array of the detected libc
/// `os.features` tags (e.g. `["libc.glibc"]`, `["libc.glibc","libc.musl"]`),
/// empty when none were detected (non-Linux host, NixOS, or a failed probe).
#[derive(Serialize)]
pub struct About {
    pub version: String,
    pub registry: String,
    pub platforms: Vec<String>,
    /// Detected host libc `os.features` tags (e.g. `["libc.glibc"]`,
    /// `["libc.glibc","libc.musl"]`), empty when none detected (non-Linux,
    /// NixOS, failed probe). Reflects the same host detection the
    /// index-resolution path uses; a host may advertise multiple families.
    pub libc: Vec<String>,
    pub shell: Option<String>,
    pub home: String,
    #[serde(flatten)]
    pub provenance: Provenance,
}

impl About {
    pub fn new(
        version: String,
        registry: String,
        platforms: Vec<String>,
        libc: Vec<String>,
        shell: Option<String>,
        home: String,
    ) -> Self {
        Self {
            version,
            registry,
            platforms,
            libc,
            shell,
            home,
            provenance: Provenance::current(),
        }
    }

    /// Short commit summary for the info-table — `<short> (clean|dirty)`,
    /// or `None` when no git metadata was baked in.
    pub fn commit_summary(&self) -> Option<String> {
        let commit = self.provenance.commit.as_ref()?;
        let dirty = if commit.dirty { "dirty" } else { "clean" };
        Some(format!("{} ({dirty})", commit.short))
    }
}

impl Printable for About {
    fn print_plain(&self, _printer: &ocx_lib::cli::DataInterface) {
        // Plain format is handled directly by the command (logo rendering).
        // This is only called as a fallback.
        println!("Version:   {}", self.version);
        if let Some(commit) = self.commit_summary() {
            println!("Commit:    {commit}");
        }
        if let Some(channel) = self.provenance.channel {
            println!("Channel:   {channel}");
        }
        println!("Registry:  {}", self.registry);
        println!("Platforms: {}", self.platforms.join(", "));
        if !self.libc.is_empty() {
            println!("Libc:      {}", self.libc.join(", "));
        }
        println!("Shell:     {}", self.shell.as_deref().unwrap_or("n/a"));
        println!("Home:      {}", self.home);
    }
}

#[cfg(test)]
mod tests {
    use super::About;
    use crate::app::build_info::{CommitInfo, Provenance};

    fn make_about_with_provenance(provenance: Provenance) -> About {
        About {
            version: "1.0.0".to_owned(),
            registry: "registry.example.com".to_owned(),
            platforms: vec!["linux/amd64".to_owned()],
            libc: Vec::new(),
            shell: None,
            home: "/home/user/.ocx".to_owned(),
            provenance,
        }
    }

    /// The `libc` field is a JSON array of detected libc `os.features` tags —
    /// one entry, two entries (dual-libc host), or empty when undetected. The
    /// full `libc.*` tag is emitted (not the bare family name) so `about`
    /// matches the `version` host row and the resolver's wire form.
    #[test]
    fn libc_field_serialized_in_json() {
        let mut about = make_about_with_provenance(Provenance {
            channel: None,
            commit: None,
            build: None,
            ci: None,
        });
        about.libc = vec!["libc.glibc".to_owned()];
        let value = serde_json::to_value(&about).unwrap();
        assert_eq!(
            value.get("libc").and_then(|v| v.as_array()),
            Some(&vec![serde_json::Value::from("libc.glibc")])
        );

        about.libc = vec!["libc.glibc".to_owned(), "libc.musl".to_owned()];
        let value = serde_json::to_value(&about).unwrap();
        assert_eq!(
            value.get("libc").and_then(|v| v.as_array()),
            Some(&vec![
                serde_json::Value::from("libc.glibc"),
                serde_json::Value::from("libc.musl")
            ]),
            "dual-libc host must serialize both full tags as a JSON array"
        );

        about.libc = Vec::new();
        let value = serde_json::to_value(&about).unwrap();
        assert_eq!(
            value.get("libc").and_then(|v| v.as_array()),
            Some(&Vec::new()),
            "undetected libc must serialize as an empty array"
        );
    }

    /// `commit_summary` returns `None` when no git metadata is baked in.
    #[test]
    fn commit_summary_none_when_no_commit() {
        let about = make_about_with_provenance(Provenance {
            channel: None,
            commit: None,
            build: None,
            ci: None,
        });
        assert_eq!(about.commit_summary(), None);
    }

    /// `commit_summary` returns `"<short> (clean)"` for a clean commit.
    #[test]
    fn commit_summary_clean() {
        let about = make_about_with_provenance(Provenance {
            channel: None,
            commit: Some(CommitInfo {
                sha: "abcdef1234567890".to_owned(),
                short: "abcdef12".to_owned(),
                describe: "v1.0.0".to_owned(),
                dirty: false,
                timestamp: None,
            }),
            build: None,
            ci: None,
        });
        assert_eq!(about.commit_summary(), Some("abcdef12 (clean)".to_owned()));
    }

    /// `commit_summary` returns `"<short> (dirty)"` for a dirty commit.
    #[test]
    fn commit_summary_dirty() {
        let about = make_about_with_provenance(Provenance {
            channel: None,
            commit: Some(CommitInfo {
                sha: "abcdef1234567890".to_owned(),
                short: "abcdef12".to_owned(),
                describe: "v1.0.0-dirty".to_owned(),
                dirty: true,
                timestamp: None,
            }),
            build: None,
            ci: None,
        });
        assert_eq!(about.commit_summary(), Some("abcdef12 (dirty)".to_owned()));
    }

    /// `About::print_plain` does not panic; it is a smoke test only because
    /// constructing a full `DataInterface` is lightweight (Printer + color=false).
    /// See acceptance test `test_about_*` for full output verification.
    #[test]
    fn print_plain_smoke() {
        use crate::api::Printable as _;
        use ocx_lib::cli::{DataInterface, Printer};

        let about = make_about_with_provenance(Provenance {
            channel: None,
            commit: None,
            build: None,
            ci: None,
        });
        let di = DataInterface::new(Printer::new(false, false));
        // Must not panic.
        about.print_plain(&di);
    }
}
