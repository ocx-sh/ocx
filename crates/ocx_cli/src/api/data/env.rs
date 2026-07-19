// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;

/// Origin of a resolved environment variable entry.
///
/// `Package` is the native origin — the entry came from the package's own
/// declared metadata; native entries carry `source = None`, so JSON omits the
/// field entirely. `Patch { rule, companion }` is a companion patch overlay
/// entry (`--show-patches`) carrying its provenance: `rule` is the descriptor
/// rule glob that admitted the companion for the base, and `companion` is the
/// companion identifier whose interface projection produced the entry.
///
/// JSON shape (internally tagged on a `kind` discriminator, lowercase):
///
/// ```json
/// "source": { "kind": "patch", "rule": "<glob>", "companion": "<companion-id>" }
/// ```
///
/// The keys are exactly `kind` (always `"patch"` for an overlay entry), `rule`,
/// and `companion`. A native entry has no `source` object at all (the field is
/// skipped). Pre-1.0 this replaces the Phase-4 `"source":"patch"` string with
/// the richer provenance object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EntrySource {
    // Never constructed — a native entry's `source` stays `None` so JSON omits
    // the field entirely. Kept as the explicit complement to `Patch` so the
    // taxonomy is total.
    #[allow(dead_code)]
    Package,
    Patch {
        rule: String,
        companion: String,
    },
}

impl fmt::Display for EntrySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntrySource::Package => write!(f, "package"),
            // Compact single-cell provenance for the plain `--show-patches` table.
            EntrySource::Patch { rule, companion } => write!(f, "{companion} (rule: {rule})"),
        }
    }
}

/// A single resolved environment variable entry, tagged with its modifier kind.
///
/// The optional `source` field is populated by the CLI when `--show-patches` is
/// enabled. It is `None` for package-native entries and
/// `Some(EntrySource::Patch { rule, companion })` for entries that came from a
/// companion overlay (carrying the rule + companion provenance). The field is
/// omitted from JSON output when absent.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// Origin annotation for `--show-patches`. `None` = package native entry;
    /// `Some(EntrySource::Patch { rule, companion })` = companion overlay entry
    /// carrying its provenance. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<EntrySource>,
}

/// A single admitted `binaries`/`entrypoints` claim, attributed to the
/// package that declared it.
///
/// `package` is `Option<String>` — `None` means "attribution unknown," never
/// "this package has zero binaries." With the current admission model
/// (`ocx_lib::package_manager::composer::compose`'s admitted-set closure),
/// `package` is populated for every entry; the `Option` typing leaves room
/// for a future no-clean-attribution source without a breaking schema
/// change. See `adr_declared_binaries_metadata.md` §4 Decision A.
#[derive(Serialize)]
pub struct BinaryAttribution {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

impl BinaryAttribution {
    /// Projects admitted `(identifier, claimed name)` pairs into the wire shape.
    ///
    /// Shared by `binaries` and `entrypoints` — both are `(PinnedIdentifier, T:
    /// Display)` pairs from `AdmittedBinaries`, differing only in the claim
    /// type. See `adr_declared_binaries_metadata.md` §4 Decision A.
    pub fn from_pairs<T: fmt::Display>(pairs: &[(ocx_lib::oci::PinnedIdentifier, T)]) -> Vec<Self> {
        pairs
            .iter()
            .map(|(identifier, name)| Self {
                name: name.to_string(),
                package: Some(identifier.to_string()),
            })
            .collect()
    }
}

/// Resolved environment variables for one or more packages, in declaration order.
///
/// Each entry carries its [`ModifierKind`] so callers can apply the correct operation:
/// - [`ModifierKind::Constant`] — replace any existing value for this key.
/// - [`ModifierKind::Path`]     — prepend to any existing value using the platform path separator.
///
/// An ordered list (rather than type-keyed maps) preserves declaration order, allows multiple
/// entries per key with different kinds, and naturally accommodates future modifier types.
///
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"[,
/// "source": {"kind": "patch", "rule": "...", "companion": "..."}]}, ...], "binaries":
/// [{"name": "...", "package": "..."}, ...], "entrypoints": [{"name": "...", "package":
/// "..."}, ...]}`.
/// The optional `"source"` object is present only when `--show-patches` is passed and only
/// for companion overlay entries; it is omitted for package-native entries. `binaries` and
/// `entrypoints` are the admitted-set claim attribution (`adr_declared_binaries_metadata.md`
/// §4) — always present as arrays, possibly empty. The `entries` envelope is the canonical
/// shape shared with `ci export` so consumers can branch on a single shape; `binaries` and
/// `entrypoints` are top-level siblings, not nested inside `entries`.
#[derive(Serialize)]
pub struct EnvVars {
    pub entries: Vec<EnvEntry>,
    pub binaries: Vec<BinaryAttribution>,
    pub entrypoints: Vec<BinaryAttribution>,
}

impl EnvVars {
    pub fn new(entries: Vec<EnvEntry>, binaries: Vec<BinaryAttribution>, entrypoints: Vec<BinaryAttribution>) -> Self {
        Self {
            entries,
            binaries,
            entrypoints,
        }
    }
}

/// Number of names spelled out in a hint line before collapsing the rest into
/// a trailing `...`. The hint is a glance, not the exhaustive list —
/// `--format json` is the full-list path (Decision C).
const HINT_NAME_PREVIEW: usize = 3;

/// Formats the `--format plain` availability hint for admitted binaries.
///
/// Per `adr_declared_binaries_metadata.md` §4 Decision C: the `entries` table
/// stays byte-stable (a `binaries` column would misrepresent a dataset with
/// no natural per-entry-row mapping); binary/entrypoint availability is a
/// separate hint line below the table, not a new column or a second table.
/// E.g. `"5 binaries available (cmake, ctest, cpack, ...); use --format json
/// for the full list"`.
fn binaries_hint(binaries: &[BinaryAttribution], entrypoints: &[BinaryAttribution]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(part) = summarize_claims("binary", "binaries", binaries) {
        parts.push(part);
    }
    if let Some(part) = summarize_claims("entrypoint", "entrypoints", entrypoints) {
        parts.push(part);
    }
    parts.push("use --format json for the full list".to_owned());
    parts.join("; ")
}

/// Summarizes one claim kind as `"N <label> available (a, b, c, ...)"`, or
/// `None` when `claims` is empty. `claims` order is the admitted-set visit
/// order compose already established — reused verbatim, no re-sort.
fn summarize_claims(singular: &str, plural: &str, claims: &[BinaryAttribution]) -> Option<String> {
    if claims.is_empty() {
        return None;
    }
    let label = if claims.len() == 1 { singular } else { plural };
    let preview: Vec<&str> = claims.iter().take(HINT_NAME_PREVIEW).map(|c| c.name.as_str()).collect();
    let mut names = preview.join(", ");
    if claims.len() > HINT_NAME_PREVIEW {
        names.push_str(", ...");
    }
    Some(format!("{} {label} available ({names})", claims.len()))
}

/// Whether any entry carries a companion patch overlay origin. Gates the
/// plain-table Source column — extracted so the decision is unit-testable
/// without capturing `DataInterface`'s stdout writes.
fn has_patch_entry(entries: &[EnvEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e.source, Some(EntrySource::Patch { .. })))
}

impl Printable for EnvVars {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        if has_patch_entry(&self.entries) {
            let mut rows: [Vec<String>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.key.clone());
                rows[1].push(entry.kind.to_string());
                rows[2].push(entry.value.clone());
                rows[3].push(entry.source.as_ref().map(|s| s.to_string()).unwrap_or_default());
            }
            printer.print_table(
                &["Key".into(), "Type".into(), "Value".into(), "Source".into()],
                &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
            );
        } else {
            let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.key.clone());
                rows[1].push(entry.kind.to_string());
                rows[2].push(entry.value.clone());
            }
            printer.print_table(
                &["Key".into(), "Type".into(), "Value".into()],
                &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
            );
        }

        // Decision C hint line — Single-Table Rule keeps the table above
        // byte-stable; availability is a separate line, only when there is
        // anything to announce.
        if !self.binaries.is_empty() || !self.entrypoints.is_empty() {
            printer.print_hint(&binaries_hint(&self.binaries, &self.entrypoints));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::cli::{DataInterface, Printer};

    fn entry(key: &str, source: Option<EntrySource>) -> EnvEntry {
        EnvEntry {
            key: key.to_owned(),
            value: "value".to_owned(),
            kind: ModifierKind::Constant,
            source,
        }
    }

    fn patch_source(rule: &str, companion: &str) -> EntrySource {
        EntrySource::Patch {
            rule: rule.to_owned(),
            companion: companion.to_owned(),
        }
    }

    #[test]
    fn patch_entry_source_names_rule_and_companion() {
        let vars = EnvVars::new(
            vec![entry(
                "SSL_CERT_FILE",
                Some(patch_source("*", "internal.corp/certs/ca-bundle:latest")),
            )],
            Vec::new(),
            Vec::new(),
        );
        let json = serde_json::to_string(&vars).expect("serializes");
        // The provenance object names BOTH the rule glob and the companion.
        assert!(
            json.contains(r#""kind":"patch""#),
            "source must be tagged patch: {json}"
        );
        assert!(json.contains(r#""rule":"*""#), "source must name the rule glob: {json}");
        assert!(
            json.contains(r#""companion":"internal.corp/certs/ca-bundle:latest""#),
            "source must name the companion: {json}"
        );
    }

    #[test]
    fn native_entry_omits_source_field() {
        let vars = EnvVars::new(vec![entry("KEY", None)], Vec::new(), Vec::new());
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            !json.contains("\"source\""),
            "native entry must omit source field, got {json}"
        );
    }

    #[test]
    fn plain_table_hides_source_column_when_all_entries_native() {
        let entries = vec![entry("A", None), entry("B", None)];
        assert!(!has_patch_entry(&entries));
    }

    #[test]
    fn plain_table_shows_source_column_when_any_entry_is_patch() {
        let entries = vec![entry("A", None), entry("B", Some(patch_source("*", "corp/ca:1")))];
        assert!(has_patch_entry(&entries));
    }

    #[test]
    fn entry_source_display_names_rule_and_companion() {
        assert_eq!(
            patch_source("ocx.sh/java:*", "corp/jdk-trust:1.0").to_string(),
            "corp/jdk-trust:1.0 (rule: ocx.sh/java:*)"
        );
        assert_eq!(EntrySource::Package.to_string(), "package");
    }

    // ── binaries / entrypoints envelope (adr_declared_binaries_metadata.md §4) ──

    fn attribution(name: &str, package: Option<&str>) -> BinaryAttribution {
        BinaryAttribution {
            name: name.to_owned(),
            package: package.map(str::to_owned),
        }
    }

    #[test]
    fn envelope_carries_binaries_and_entrypoints_as_top_level_sibling_arrays() {
        let vars = EnvVars::new(
            Vec::new(),
            vec![attribution("cmake", Some("ocx.sh/cmake:3.28@sha256:aaaa"))],
            vec![attribution("fmt", Some("ocx.sh/cmake:3.28@sha256:aaaa"))],
        );
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            json.contains(r#""binaries":[{"#),
            "binaries must be a top-level sibling array, not nested inside entries: {json}"
        );
        assert!(
            json.contains(r#""entrypoints":[{"#),
            "entrypoints must be a top-level sibling array, not nested inside entries: {json}"
        );
        assert!(json.contains(r#""name":"cmake""#));
        assert!(json.contains(r#""package":"ocx.sh/cmake:3.28@sha256:aaaa""#));
    }

    #[test]
    fn envelope_binaries_and_entrypoints_present_as_empty_arrays_never_omitted() {
        let vars = EnvVars::new(Vec::new(), Vec::new(), Vec::new());
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            json.contains(r#""binaries":[]"#),
            "binaries must be present as an empty array, never omitted: {json}"
        );
        assert!(
            json.contains(r#""entrypoints":[]"#),
            "entrypoints must be present as an empty array, never omitted: {json}"
        );
    }

    #[test]
    fn binary_attribution_omits_package_field_when_none() {
        let json = serde_json::to_string(&attribution("cmake", None)).expect("serializes");
        assert_eq!(
            json, r#"{"name":"cmake"}"#,
            "None package means attribution unknown, not zero binaries — the key must be omitted entirely"
        );
    }

    #[test]
    fn binary_attribution_includes_package_field_when_some() {
        let json = serde_json::to_string(&attribution("cmake", Some("ocx.sh/cmake:3.28"))).expect("serializes");
        assert_eq!(json, r#"{"name":"cmake","package":"ocx.sh/cmake:3.28"}"#);
    }

    // ── plain-mode: table stays byte-stable when empty, hint gated on non-empty ──
    //
    // `print_plain` cannot be captured/inspected byte-for-byte here (no
    // injectable writer on `Printer` — see `has_patch_entry`'s doc comment
    // for the same constraint), so these three exercise only the *decision*
    // (renders without panicking, empty or non-empty). The hint's actual text
    // is pinned byte-for-byte by the `binaries_hint`/`summarize_claims` unit
    // tests below, which are pure functions with no `Printer` dependency.

    #[test]
    fn plain_table_renders_without_panicking_when_binaries_and_entrypoints_are_empty() {
        let vars = EnvVars::new(vec![entry("KEY", None)], Vec::new(), Vec::new());
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    #[test]
    fn plain_table_emits_hint_when_binaries_non_empty() {
        let vars = EnvVars::new(Vec::new(), vec![attribution("cmake", None)], Vec::new());
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    #[test]
    fn plain_table_emits_hint_when_entrypoints_non_empty() {
        let vars = EnvVars::new(Vec::new(), Vec::new(), vec![attribution("fmt", None)]);
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    // ── binaries_hint / summarize_claims — Decision C hint-line format ────

    fn attr(name: &str) -> BinaryAttribution {
        attribution(name, None)
    }

    #[test]
    fn hint_formats_count_and_preview_names() {
        let hint = binaries_hint(&[attr("cmake"), attr("ctest")], &[]);
        assert_eq!(
            hint,
            "2 binaries available (cmake, ctest); use --format json for the full list"
        );
    }

    #[test]
    fn hint_uses_singular_label_for_one_binary() {
        let hint = binaries_hint(&[attr("cmake")], &[]);
        assert_eq!(hint, "1 binary available (cmake); use --format json for the full list");
    }

    #[test]
    fn hint_truncates_preview_with_ellipsis_beyond_three_names() {
        let claims = vec![attr("a"), attr("b"), attr("c"), attr("d")];
        let hint = binaries_hint(&claims, &[]);
        assert_eq!(
            hint,
            "4 binaries available (a, b, c, ...); use --format json for the full list"
        );
    }

    /// Exactly `HINT_NAME_PREVIEW` (3) names must NOT carry the `, ...`
    /// suffix — truncation only kicks in strictly beyond the preview count.
    #[test]
    fn hint_omits_ellipsis_at_exactly_three_names() {
        let claims = vec![attr("a"), attr("b"), attr("c")];
        let hint = binaries_hint(&claims, &[]);
        assert_eq!(
            hint,
            "3 binaries available (a, b, c); use --format json for the full list"
        );
    }

    #[test]
    fn hint_combines_binaries_and_entrypoints_as_separate_clauses() {
        let hint = binaries_hint(&[attr("cmake")], &[attr("fmt")]);
        assert_eq!(
            hint,
            "1 binary available (cmake); 1 entrypoint available (fmt); use --format json for the full list"
        );
    }

    #[test]
    fn hint_omits_binaries_clause_when_only_entrypoints_present() {
        let hint = binaries_hint(&[], &[attr("fmt"), attr("cmake")]);
        assert_eq!(
            hint,
            "2 entrypoints available (fmt, cmake); use --format json for the full list"
        );
    }

    #[test]
    fn hint_is_ascii() {
        let hint = binaries_hint(
            &[attr("cmake"), attr("ctest"), attr("cpack"), attr("ccmake")],
            &[attr("fmt")],
        );
        assert!(hint.is_ascii(), "hint must be ASCII (help-text ASCII gate): {hint}");
    }
}
