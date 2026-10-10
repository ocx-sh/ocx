// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;

use serde::Serialize;

use crate::api::Printable;
use crate::app::build_info::Provenance;

/// Version information reported by `ocx version`.
///
/// `version` is always present. Every other key appears only when its source
/// data was available at build time, so a tarball-source, no-CI build emits
/// `version` alone. `cargo_pkg_version` appears only when it differs from
/// `version` (dev-deploy builds overriding it via `__OCX_BUILD_VERSION`).
///
/// ```json
/// {
///   "version":            "0.3.2-dev+20260528143045",
///   "cargo_pkg_version":  "0.3.1",
///   "channel":            "dev",
///   "commit":             { ... },
///   "build":              { ... },
///   "ci":                 { ... },
///   "contract":           { "errors": 1, "commands": { ... }, "reports": { ... } }
/// }
/// ```
#[derive(Serialize, schemars::JsonSchema)]
pub struct VersionData {
    /// The version ocx reports for itself.
    // Always present: `ocx self update` parses this key to compare against the latest tag.
    version: String,
    /// The crate version, when it differs from `version`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cargo_pkg_version: Option<String>,
    #[serde(flatten)]
    provenance: Provenance,
    /// The machine-interface versions this binary speaks.
    contract: ContractVersions,
}

/// The version of every gated machine document, so a caller can refuse a command before running it.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ContractVersions {
    /// The error document's `schema_version`.
    errors: u32,
    /// Each command's contract version, keyed by its path below `ocx`, space-separated.
    commands: BTreeMap<String, u32>,
    /// Each `--format json` root's `schema_version`, keyed by the root name the command grammar uses.
    reports: BTreeMap<String, u32>,
}

impl ContractVersions {
    fn current() -> Self {
        Self {
            errors: crate::error_document::ERRORS_SCHEMA_VERSION,
            commands: crate::command::CONTRACT
                .iter()
                .map(|(path, version, _)| (path.join(" "), *version))
                .collect(),
            reports: crate::api::report_versions()
                .into_iter()
                .map(|(name, version)| (name.to_owned(), version))
                .collect(),
        }
    }
}

impl VersionData {
    /// Payload with every build-time provenance field that is available.
    pub fn enriched(version: impl Into<String>, cargo_pkg_version: impl Into<String>) -> Self {
        let version = version.into();
        let cargo_pkg = cargo_pkg_version.into();
        let cargo_pkg_version = (cargo_pkg != version).then_some(cargo_pkg);
        Self {
            version,
            cargo_pkg_version,
            provenance: Provenance::current(),
            contract: ContractVersions::current(),
        }
    }
}

impl Printable for VersionData {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "VersionData";

    fn print_plain(&self, _data: &ocx_console::DataInterface) {
        // Bare version only: scripts parse this stdout as one semver token.
        println!("{}", self.version);
    }
}

/// Verbose rendering of [`VersionData`]: build provenance beside the version.
///
/// JSON is the inner `VersionData` unchanged.
pub struct VerboseVersionData(pub VersionData);

impl Printable for VerboseVersionData {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "VerboseVersionData";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        let inner = &self.0;

        let mut header = format!("{} {}", theme.label("ocx"), theme.tag(&inner.version));
        let mut extras: Vec<String> = Vec::new();
        if let Some(cargo) = &inner.cargo_pkg_version {
            extras.push(format!("cargo: {}", theme.tag(cargo)));
        }
        if let Some(channel) = inner.provenance.channel {
            extras.push(format!("channel: {}", theme.tag(channel)));
        }
        if !extras.is_empty() {
            header.push_str(&format!(" ({})", extras.join(", ")));
        }
        println!("{header}");

        // Plain-only: never add the host to the JSON shape `ocx self update` parses.
        if let Some(platform) = ocx_oci::Platform::current() {
            // Not `Display`: its `+features` suffix would duplicate the libc parenthetical.
            let base = platform.segments().join("/");
            let tags = ocx_oci::cached_libc_labels();
            let host = if tags.is_empty() {
                base
            } else {
                let joined = tags.join(", ");
                format!("{base} {}", theme.aside(format!("({joined})")))
            };
            println!("{}    {host}", theme.label("host:"));
        }

        if let Some(commit) = &inner.provenance.commit {
            let dirty_text = if commit.dirty { "dirty" } else { "clean" };
            let timestamp = commit
                .timestamp
                .as_deref()
                .map(|ts| theme.aside(format!(" - {ts}")))
                .unwrap_or_default();
            println!(
                "{}   {} {}{}",
                theme.label("commit:"),
                theme.digest(&commit.short),
                theme.aside(format!("({dirty_text})")),
                timestamp,
            );
        }

        if let Some(build) = &inner.provenance.build {
            println!(
                "{}    {} {}",
                theme.label("built:"),
                build.timestamp,
                theme.aside(format!("({})", build.profile)),
            );
            println!("{}   {}", theme.label("target:"), theme.tag(&build.target));
            println!("{}    {}", theme.label("rustc:"), theme.tag(&build.rustc));
        }

        if let Some(ci) = &inner.provenance.ci {
            println!("{}       {}", theme.label("ci:"), theme.aside(ci.run_url.clone()));
        }
    }
}

impl serde::Serialize for VerboseVersionData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl schemars::JsonSchema for VerboseVersionData {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VerboseVersionData".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <VersionData>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::{VerboseVersionData, VersionData};
    use ocx_console::{DataInterface, Printer};

    /// Enriched payload still carries the canonical `version` key — the
    /// self-update parser must keep working.
    ///
    /// Pins the wire format so `ocx --format json version` callers can
    /// rely on the JSON shape. The subprocess-based version source in
    /// `update_check.rs::query_installed_version` parses this exact key
    /// out of the payload.
    #[test]
    fn enriched_payload_keeps_version_key() {
        let data = VersionData::enriched("0.3.1", "0.3.1");
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value.get("version").and_then(|v| v.as_str()), Some("0.3.1"));
    }

    /// `VersionData::enriched` accepts both `String` and `&str` and
    /// produces the same JSON wire output for byte-equal inputs.
    #[test]
    fn enriched_accepts_str_and_string() {
        let from_str = serde_json::to_value(VersionData::enriched("1.0.0", "1.0.0")).unwrap();
        let from_string =
            serde_json::to_value(VersionData::enriched("1.0.0".to_string(), "1.0.0".to_string())).unwrap();
        assert_eq!(from_str, from_string);
    }

    /// `cargo_pkg_version` is suppressed when identical to `version` —
    /// only dev-deploy / override paths surface a distinct Cargo.toml
    /// base.
    #[test]
    fn cargo_pkg_version_suppressed_when_equal() {
        let data = VersionData::enriched("0.3.1", "0.3.1");
        let value = serde_json::to_value(&data).unwrap();
        assert!(value.get("cargo_pkg_version").is_none());
    }

    /// `cargo_pkg_version` surfaces when distinct from the effective
    /// version (the dev-deploy / `__OCX_BUILD_VERSION` override path).
    #[test]
    fn cargo_pkg_version_surfaces_when_overridden() {
        let data = VersionData::enriched("0.3.2-dev+20260528143045", "0.3.1");
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value.get("cargo_pkg_version").and_then(|v| v.as_str()), Some("0.3.1"));
    }

    /// `VerboseVersionData::print_plain` does not panic, emits the version
    /// token, and produces no ANSI bytes when colour is disabled.
    #[test]
    fn verbose_print_plain_smoke() {
        use crate::api::Printable as _;

        let data = VersionData::enriched("1.2.3", "1.2.3");
        let verbose = VerboseVersionData(data);

        // Verify JSON shape is identical to plain VersionData
        let json = serde_json::to_value(&verbose).unwrap();
        assert_eq!(json.get("version").and_then(|v| v.as_str()), Some("1.2.3"));

        // Verify print_plain does not panic with color disabled
        let di = DataInterface::new(Printer::new(false, false));
        verbose.print_plain(&di);

        // Verify the version token appears in JSON (no ANSI bytes in
        // key values when colour is off)
        let version_str = json.get("version").and_then(|v| v.as_str()).unwrap();
        assert!(
            !version_str.contains('\x1b'),
            "version must contain no ANSI when color disabled"
        );
    }

    /// The verbose plain-text render adds a `host:` row (os/arch + detected
    /// libc), but the JSON wire shape must stay identical to plain
    /// `VersionData` — no `libc` or `host` key leaks in. The
    /// `query_installed_version` subprocess parser in
    /// `ocx_package_manager::tasks::update_check` only ever reads
    /// `version` out of this payload; a stray key would still be harmless to
    /// that parser today, but its absence is the documented self-update
    /// wire contract and must not silently drift.
    #[test]
    fn verbose_json_wire_shape_has_no_libc_or_host_keys() {
        let data = VersionData::enriched("1.2.3", "1.2.3");
        let verbose = VerboseVersionData(data);

        let value = serde_json::to_value(&verbose).unwrap();
        let object = value.as_object().expect("wire shape must be a JSON object");

        assert!(
            !object.contains_key("libc"),
            "JSON wire shape must not carry a libc key"
        );
        assert!(
            !object.contains_key("host"),
            "JSON wire shape must not carry a host key"
        );
        for key in object.keys() {
            assert!(
                [
                    "version",
                    "cargo_pkg_version",
                    "channel",
                    "commit",
                    "build",
                    "ci",
                    "contract"
                ]
                .contains(&key.as_str()),
                "unexpected top-level key {key:?} in verbose version JSON wire shape"
            );
        }
    }

    #[test]
    fn contract_names_every_command_and_root_with_its_version() {
        let value = serde_json::to_value(VersionData::enriched("1.0.0", "1.0.0")).unwrap();
        let contract = &value["contract"];
        assert_eq!(contract["errors"], crate::error_document::ERRORS_SCHEMA_VERSION);
        assert_eq!(contract["commands"]["package sign"], 1);
        assert_eq!(
            contract["commands"].as_object().map(serde_json::Map::len),
            Some(crate::command::CONTRACT.len())
        );
        assert_eq!(contract["reports"]["SignatureReport"], 2);
        assert_eq!(contract["reports"]["SweepReport<AttestationReport>"], 2);
        assert_eq!(contract["reports"]["VersionData"], 1);
    }
}
