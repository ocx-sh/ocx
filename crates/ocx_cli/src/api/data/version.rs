// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;
use crate::app::build_info::Provenance;

/// Version information reported by `ocx version`.
///
/// # Plain format
///
/// Bare version string (e.g. `0.3.1`), preserving the
/// pre-enrichment contract that scripts can `ocx version` and parse
/// stdout as a single semver token.
///
/// Verbose rendering is handled by the `VerboseVersionData` wrapper:
/// a multi-line summary (version, commit + dirty flag, build time,
/// target, rustc, CI run URL). Suppressed lines for absent fields so a
/// locally-built binary doesn't show empty `ci:` rows.
///
/// # JSON format
///
/// The `version` key is the always-present contract that the
/// `query_installed_version` subprocess parser in
/// `ocx_lib/src/package_manager/tasks/update_check.rs`
/// reads when comparing the locally-installed version to the latest
/// remote tag during `ocx self update`. Every other top-level key is
/// optional and gated on whether the source data was available at build
/// time, so a tarball-source / no-CI build emits only `version` and a
/// dev-deploy CI build emits the full schema.
///
/// Concrete shape (every field except `version` is optional):
///
/// ```json
/// {
///   "version":            "0.3.2-dev+20260528143045",
///   "cargo_pkg_version":  "0.3.1",
///   "channel":            "dev",
///   "commit":             { ... },
///   "build":              { ... },
///   "ci":                 { ... }
/// }
/// ```
///
/// `cargo_pkg_version` is suppressed when it would be identical to
/// `version` — only meaningful for dev-deploy builds where the embedded
/// version is overridden via `__OCX_BUILD_VERSION`.
#[derive(Serialize)]
pub struct VersionData {
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cargo_pkg_version: Option<String>,
    #[serde(flatten)]
    provenance: Provenance,
}

impl VersionData {
    /// Enriched payload: every available build-time provenance field is
    /// populated. JSON output always includes the populated subset;
    /// plain-text output is bare (a single version token on stdout).
    ///
    /// `cargo_pkg_version` is folded into the payload only when it
    /// differs from the effective `version` — meaningful only for
    /// dev-deploy / `__OCX_BUILD_VERSION` overrides.
    pub fn enriched(version: impl Into<String>, cargo_pkg_version: impl Into<String>) -> Self {
        let version = version.into();
        let cargo_pkg = cargo_pkg_version.into();
        let cargo_pkg_version = (cargo_pkg != version).then_some(cargo_pkg);
        Self {
            version,
            cargo_pkg_version,
            provenance: Provenance::current(),
        }
    }
}

impl Printable for VersionData {
    fn print_plain(&self, _data: &ocx_lib::cli::DataInterface) {
        println!("{}", self.version);
    }
}

/// Verbose rendering of [`VersionData`] — multi-line labelled-value
/// summary showing build provenance alongside the version token.
///
/// Plain format: `ocx <version>` header with optional cargo/channel
/// qualifiers, followed by commit, build, and CI rows for the fields
/// that were baked into the binary at build time.
///
/// JSON format: delegates to the inner `VersionData` — identical wire
/// shape whether verbose or not.
pub struct VerboseVersionData(pub VersionData);

impl Printable for VerboseVersionData {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let theme = data.theme();
        let inner = &self.0;

        // ── Header: "ocx <version> (cargo: …, channel: …)" ──────────
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

        // ── Commit row ──────────────────────────────────────────────
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

        // ── Build block ─────────────────────────────────────────────
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

        // ── CI block ────────────────────────────────────────────────
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

#[cfg(test)]
mod tests {
    use super::{VerboseVersionData, VersionData};
    use ocx_lib::cli::{DataInterface, Printer};

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
}
