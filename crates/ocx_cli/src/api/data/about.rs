// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_util::fs::path::AbsolutePath;
use serde::Serialize;

use crate::api::Printable;
use crate::app::build_info::Provenance;

/// System information about the ocx installation.
///
/// The build-provenance blocks are absent on a local `cargo build` without
/// git, as in `ocx version --format json`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct About {
    /// The version ocx reports for itself.
    pub version: String,
    /// The default registry a bare identifier resolves against.
    #[schemars(with = "ocx_oci::RegistryHost")]
    pub registry: String,
    /// The host platform as ocx matches it, `os.features` included.
    pub platforms: Vec<ocx_oci::Platform>,
    /// The host's full `os.features` (a superset of `libc`); a package is
    /// runnable when its offered features are a subset of these.
    pub features: Vec<String>,
    /// Detected host libc `os.features` tags (e.g. `["libc.glibc"]`,
    /// `["libc.glibc","libc.musl"]`), empty when none detected (non-Linux,
    /// NixOS, failed probe). Reflects the same host detection the
    /// index-resolution path uses; a host may advertise multiple families.
    pub libc: Vec<String>,
    /// The shell ocx detected; absent when it detected none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// The ocx home directory.
    pub home: AbsolutePath,
    #[serde(flatten)]
    pub provenance: Provenance,
    /// Platforms as bare `os/arch`: the plain output shows the libc on its own row.
    #[serde(skip)]
    pub plain_platforms: Vec<String>,
}

impl About {
    pub fn new(
        version: String,
        registry: String,
        host_platform: &ocx_oci::Platform,
        libc: Vec<String>,
        shell: Option<String>,
        home: AbsolutePath,
    ) -> Self {
        Self {
            version,
            registry,
            platforms: vec![host_platform.clone()],
            features: host_platform.os_features().to_vec(),
            libc,
            shell,
            home,
            provenance: Provenance::current(),
            plain_platforms: vec![host_platform.segments().join("/")],
        }
    }

    /// `<short> (clean|dirty)`, or `None` when no git metadata was baked in.
    pub fn commit_summary(&self) -> Option<String> {
        let commit = self.provenance.commit.as_ref()?;
        let dirty = if commit.dirty { "dirty" } else { "clean" };
        Some(format!("{} ({dirty})", commit.short))
    }
}

impl Printable for About {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "About";

    fn print_plain(&self, _printer: &ocx_console::DataInterface) {
        // Fallback only: the command renders the plain form itself, with the logo.
        println!("Version:   {}", self.version);
        if let Some(commit) = self.commit_summary() {
            println!("Commit:    {commit}");
        }
        if let Some(channel) = self.provenance.channel {
            println!("Channel:   {channel}");
        }
        println!("Registry:  {}", self.registry);
        println!("Platforms: {}", self.plain_platforms.join(", "));
        if !self.libc.is_empty() {
            println!("Libc:      {}", self.libc.join(", "));
        }
        println!("Shell:     {}", self.shell.as_deref().unwrap_or("n/a"));
        println!("Home:      {}", self.home.as_path().display());
    }
}

#[cfg(test)]
mod tests {
    use super::About;
    use ocx_util::fs::path::AbsolutePath;

    use crate::app::build_info::{CommitInfo, Provenance};

    fn make_about_with_provenance(provenance: Provenance) -> About {
        About {
            version: "1.0.0".to_owned(),
            registry: "registry.example.com".to_owned(),
            platforms: vec!["linux/amd64".parse().expect("platform")],
            features: Vec::new(),
            libc: Vec::new(),
            shell: None,
            home: AbsolutePath::new(std::env::temp_dir()).expect("absolute temp dir"),
            provenance,
            plain_platforms: vec!["linux/amd64".to_owned()],
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

    /// `features` serializes as an array; the bare plain-only platforms never reach JSON.
    #[test]
    fn features_field_serialized_and_plain_platforms_skipped() {
        let mut about = make_about_with_provenance(Provenance {
            channel: None,
            commit: None,
            build: None,
            ci: None,
        });
        about.platforms = vec!["linux/amd64+libc.glibc,libc.musl".parse().expect("platform")];
        about.features = vec!["libc.glibc".to_owned(), "libc.musl".to_owned()];
        let value = serde_json::to_value(&about).unwrap();
        assert_eq!(value["platforms"][0]["os"], "linux");
        assert_eq!(value["platforms"][0]["architecture"], "amd64");
        assert_eq!(
            value["platforms"][0]["os.features"],
            serde_json::json!(["libc.glibc", "libc.musl"])
        );
        assert_eq!(value["features"], serde_json::json!(["libc.glibc", "libc.musl"]));
        assert!(
            value.get("plain_platforms").is_none(),
            "plain-only field leaked: {value}"
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
        use ocx_console::{DataInterface, Printer};

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
