// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::fmt;

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::IntegrationEntry;
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
///
/// The optional `separator` field carries the fold separator for a
/// [`ModifierKind::List`] entry; every other kind omits it. Callers construct
/// this type from entries that already passed compose-time separator
/// agreement (`ocx_lib::env::reconcile_list_separators`), so a `list` entry
/// reaching here never carries a bare `None` unless nothing in the
/// composition ever declared one.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// The separator a [`ModifierKind::List`] entry folds with. `None` on
    /// every other kind. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
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
    /// Display)` pairs from `AdmittedClaims`, differing only in the claim
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

/// One admitted integration contribution, attributed to the declaring
/// package.
///
/// `payload` is the interpolated payload — arbitrary JSON OCX does not
/// interpret. `package` is `Option` for the same reason
/// [`BinaryAttribution::package`] is: `None` means "attribution unknown",
/// never "no payload".
///
/// The field is `namespace`, not `name`: two of the three sibling arrays are
/// name claims that resolve on `PATH`, while an integration row is a keyed
/// payload. One row per (package, namespace) pair — an array longer than the
/// distinct-namespace count is the structural guarantee that nothing merged.
/// See `adr_package_integrations.md` C-014.
#[derive(Serialize)]
pub struct IntegrationAttribution {
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub payload: serde_json::Value,
}

impl IntegrationAttribution {
    /// Projects admitted `(identifier, entry)` pairs into the wire shape — the
    /// payload-carrying sibling of [`BinaryAttribution::from_pairs`].
    ///
    /// One row per input pair, in the admitted-set visit order compose
    /// established — never grouped by namespace, never collapsed for a single
    /// root (`adr_package_integrations.md` D2/D18).
    pub fn from_pairs(pairs: &[(ocx_lib::oci::PinnedIdentifier, IntegrationEntry)]) -> Vec<Self> {
        pairs
            .iter()
            .map(|(identifier, entry)| Self {
                namespace: entry.namespace.clone(),
                package: Some(identifier.to_string()),
                payload: entry.payload.clone(),
            })
            .collect()
    }
}

/// One advisory raised while composing a **deferred** tool, in wire shape.
///
/// `kind` is the machine discriminator — a consumer branches on it and never
/// on `message`, which is the human rendering of the same fact. `key` is
/// present only for the two variants that name an environment variable.
///
/// Advisories are warning-only and never fail a compose. They exist because a
/// deferred tool's declared metadata can describe something that will not
/// substitute cleanly until its content materializes; reaching a log alone
/// would make them unreadable to the tooling this product is a backend for.
#[derive(Serialize)]
pub struct LazyAdvisoryReport {
    pub kind: &'static str,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub message: String,
}

impl LazyAdvisoryReport {
    /// Projects the library's advisories into the wire shape, preserving the
    /// order the composer raised them in.
    pub fn from_advisories(advisories: &[ocx_lib::package_manager::LazyAdvisory]) -> Vec<Self> {
        use ocx_lib::package_manager::LazyAdvisory;
        advisories
            .iter()
            .map(|advisory| {
                let (kind, package, key) = match advisory {
                    LazyAdvisory::InstallPathRootedNonPathVar { package, key } => {
                        ("install-path-rooted-non-path-var", package, Some(key.clone()))
                    }
                    LazyAdvisory::UndeclaredBinaries { package } => ("undeclared-binaries", package, None),
                    LazyAdvisory::CombinedPathValue { package, key } => {
                        ("combined-path-value", package, Some(key.clone()))
                    }
                };
                Self {
                    kind,
                    package: package.to_string(),
                    key,
                    message: advisory.to_string(),
                }
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
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"|"list"[,
/// "separator": "..."][, "source": {"kind": "patch", "rule": "...", "companion": "..."}]}, ...],
/// "binaries": [{"name": "...", "package": "..."}, ...], "entrypoints": [{"name": "...", "package":
/// "..."}, ...], "integrations": [{"namespace": "...", "package": "...", "payload": {...}}, ...]}`.
/// The optional `"separator"` field is present only for a `"type":"list"` entry — every other
/// kind omits it. The optional `"source"` object is present only when `--show-patches` is passed
/// and only for companion overlay entries; it is omitted for package-native entries. `binaries`
/// and `entrypoints` are the admitted-set claim attribution (`adr_declared_binaries_metadata.md`
/// §4) — always present as arrays, possibly empty. `integrations` is the third such array
/// (`adr_package_integrations.md` C-014), never collapsed for a single root. The `entries`
/// envelope is the canonical shape shared with `ci export` so consumers can branch on a single
/// shape; `binaries`, `entrypoints` and `integrations` are top-level siblings, not nested
/// inside `entries`. `advisories` is the fourth such sibling — always present, empty unless a
/// deferred tool raised something; warning-only, and a consumer branches on its `kind`, never
/// on `message`.
#[derive(Serialize)]
pub struct EnvVars {
    pub entries: Vec<EnvEntry>,
    pub binaries: Vec<BinaryAttribution>,
    pub entrypoints: Vec<BinaryAttribution>,
    pub integrations: Vec<IntegrationAttribution>,
    /// Advisories raised for the **deferred** tools in this composition —
    /// always present, empty whenever nothing was deferred. Warning-only.
    pub advisories: Vec<LazyAdvisoryReport>,
}

impl EnvVars {
    pub fn new(
        entries: Vec<EnvEntry>,
        binaries: Vec<BinaryAttribution>,
        entrypoints: Vec<BinaryAttribution>,
        integrations: Vec<IntegrationAttribution>,
    ) -> Self {
        Self {
            entries,
            binaries,
            entrypoints,
            integrations,
            advisories: Vec::new(),
        }
    }

    /// Attaches the deferred-composition advisories, returning `self` for
    /// chaining after [`new`](Self::new).
    ///
    /// A separate step rather than a fourth constructor argument: only a
    /// command that composes lazily has any to attach, and every other call
    /// site says so by not calling this.
    #[must_use]
    pub fn with_advisories(mut self, advisories: Vec<LazyAdvisoryReport>) -> Self {
        self.advisories = advisories;
        self
    }
}

/// Number of names spelled out in a hint line before collapsing the rest into
/// a trailing `...`. The hint is a glance, not the exhaustive list —
/// `--format json` is the full-list path (Decision C).
const HINT_NAME_PREVIEW: usize = 3;

/// Formats the `--format plain` availability hint for the three claim arrays.
///
/// Per `adr_declared_binaries_metadata.md` §4 Decision C: the `entries` table
/// stays byte-stable (a `binaries` column would misrepresent a dataset with
/// no natural per-entry-row mapping); binary/entrypoint availability is a
/// separate hint line below the table, not a new column or a second table.
/// `adr_package_integrations.md` C-015/D15 adds the integrations clause on
/// the same reasoning, and for the same reason names only the NAMESPACE keys —
/// an opaque payload has no per-entry-row mapping either, and plain output
/// never renders one.
///
/// Clause order is binaries, entrypoints, integrations, then the trailing
/// `use --format json for the full list`. E.g. `"5 binaries available (cmake,
/// ctest, cpack, ...); 2 integration namespaces (com.jetbrains,
/// com.microsoft.vscode); use --format json for the full list"`.
fn availability_hint(
    binaries: &[BinaryAttribution],
    entrypoints: &[BinaryAttribution],
    integrations: &[IntegrationAttribution],
) -> String {
    let binary_names: Vec<&str> = binaries.iter().map(|c| c.name.as_str()).collect();
    let entrypoint_names: Vec<&str> = entrypoints.iter().map(|c| c.name.as_str()).collect();
    // The integrations array carries one row per (package, namespace) pair,
    // so two packages declaring one namespace yield two rows. The clause counts
    // and names NAMESPACES, so it dedupes — the no-merge guarantee (D2) lives in
    // the JSON array, and a hint reading "2 integration namespaces (x, x)"
    // would be false on its own terms.
    //
    // `seen` gates membership in O(1); `namespaces` is the parallel
    // first-seen-order list the hint must preserve (D2/D18 visit order) — a
    // HashSet alone cannot express "which one was seen first". The loop runs
    // over the AGGREGATE rows across every admitted package in the closure,
    // not one package's declarations, so there is no small per-package bound
    // to lean on for an O(n^2) scan here.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut namespaces: Vec<&str> = Vec::new();
    for row in integrations {
        if seen.insert(row.namespace.as_str()) {
            namespaces.push(row.namespace.as_str());
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(part) = summarize_claims("binary available", "binaries available", &binary_names) {
        parts.push(part);
    }
    if let Some(part) = summarize_claims("entrypoint available", "entrypoints available", &entrypoint_names) {
        parts.push(part);
    }
    if let Some(part) = summarize_claims("integration namespace", "integration namespaces", &namespaces) {
        parts.push(part);
    }
    parts.push("use --format json for the full list".to_owned());
    parts.join("; ")
}

/// Summarizes one claim kind as `"N <label> (a, b, c, ...)"`, or `None` when
/// `names` is empty. `names` order is the admitted-set visit order compose
/// already established — reused verbatim, no re-sort. The label carries its own
/// `available` where the claim kind reads that way, so the integrations
/// clause is `"2 integration namespaces (...)"`.
fn summarize_claims(singular: &str, plural: &str, names: &[&str]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let label = if names.len() == 1 { singular } else { plural };
    let mut preview = names[..names.len().min(HINT_NAME_PREVIEW)].join(", ");
    if names.len() > HINT_NAME_PREVIEW {
        preview.push_str(", ...");
    }
    Some(format!("{} {label} ({preview})", names.len()))
}

/// Whether any entry carries a companion patch overlay origin. Gates the
/// plain-table Source column — extracted so the decision is unit-testable
/// without capturing `DataInterface`'s stdout writes.
fn has_patch_entry(entries: &[EnvEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e.source, Some(EntrySource::Patch { .. })))
}

/// Whether the Decision C hint line has anything to announce. Extracted so
/// the three-way gate is unit-testable without capturing `Printer`'s direct
/// stdout writes — same rationale as `has_patch_entry`.
fn has_availability_hint(
    binaries: &[BinaryAttribution],
    entrypoints: &[BinaryAttribution],
    integrations: &[IntegrationAttribution],
) -> bool {
    !binaries.is_empty() || !entrypoints.is_empty() || !integrations.is_empty()
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
        if has_availability_hint(&self.binaries, &self.entrypoints, &self.integrations) {
            printer.print_hint(&availability_hint(
                &self.binaries,
                &self.entrypoints,
                &self.integrations,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::cli::{DataInterface, Printer};
    use ocx_lib::oci;

    fn entry(key: &str, source: Option<EntrySource>) -> EnvEntry {
        EnvEntry {
            key: key.to_owned(),
            value: "value".to_owned(),
            kind: ModifierKind::Constant,
            separator: None,
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
        let vars = EnvVars::new(vec![entry("KEY", None)], Vec::new(), Vec::new(), Vec::new());
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            !json.contains("\"source\""),
            "native entry must omit source field, got {json}"
        );
    }

    // ── separator field (W-10) ─────────────────────────────────────────────

    #[test]
    fn list_entry_serializes_type_and_separator() {
        let vars = EnvVars::new(
            vec![EnvEntry {
                key: "GODEBUG".to_owned(),
                value: "gctrace=1".to_owned(),
                kind: ModifierKind::List,
                separator: Some(",".to_owned()),
                source: None,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(json.contains(r#""type":"list""#), "kind must serialize as list: {json}");
        assert!(json.contains(r#""separator":",""#), "separator must be present: {json}");
    }

    #[test]
    fn path_and_constant_entries_omit_the_separator_field() {
        let vars = EnvVars::new(
            vec![
                entry("PATH", None), // built via the Constant fixture helper above
                EnvEntry {
                    key: "PATH2".to_owned(),
                    value: "/opt/bin".to_owned(),
                    kind: ModifierKind::Path,
                    separator: None,
                    source: None,
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            !json.contains("\"separator\""),
            "a non-list entry must omit separator entirely: {json}"
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
            Vec::new(),
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
        let vars = EnvVars::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            json.contains(r#""binaries":[]"#),
            "binaries must be present as an empty array, never omitted: {json}"
        );
        assert!(
            json.contains(r#""entrypoints":[]"#),
            "entrypoints must be present as an empty array, never omitted: {json}"
        );
        assert!(
            json.contains(r#""integrations":[]"#),
            "integrations must be present as an empty array, never omitted: {json}"
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

    // ── IntegrationAttribution (C-014) ──────────────────────────────────
    //
    // These construct the struct literal directly (all fields `pub`) to test
    // the derived `Serialize` shape, same as `attribution()` above for
    // `BinaryAttribution`. `from_pairs` — the projection helper — is tested
    // separately below.

    fn integration(namespace: &str, package: Option<&str>, payload: serde_json::Value) -> IntegrationAttribution {
        IntegrationAttribution {
            namespace: namespace.to_owned(),
            package: package.map(str::to_owned),
            payload,
        }
    }

    #[test]
    fn integration_attribution_key_order_is_namespace_package_payload() {
        let json = serde_json::to_string(&integration(
            "com.example",
            Some("ocx.sh/cmake:3.28"),
            serde_json::json!({"a": 1}),
        ))
        .expect("serializes");
        assert_eq!(
            json,
            r#"{"namespace":"com.example","package":"ocx.sh/cmake:3.28","payload":{"a":1}}"#
        );
    }

    #[test]
    fn integration_attribution_omits_package_field_when_none() {
        let json =
            serde_json::to_string(&integration("com.example", None, serde_json::json!("v"))).expect("serializes");
        assert_eq!(json, r#"{"namespace":"com.example","payload":"v"}"#);
    }

    /// A pinned identifier for the `from_pairs` fixtures below — the `env.rs`
    /// sibling of `package_inspect::tests::pinned`.
    fn pinned(repo: &str, hex_char: char) -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry(repo, "ocx.sh")
            .clone_with_digest(oci::Digest::Sha256(hex_char.to_string().repeat(64)));
        oci::PinnedIdentifier::try_from(id).expect("digest-bearing identifier is always pinnable")
    }

    #[test]
    fn from_pairs_projects_namespace_package_and_value_per_input_pair() {
        let pairs = vec![
            (
                pinned("cmake", 'a'),
                IntegrationEntry {
                    namespace: "com.jetbrains".to_owned(),
                    payload: serde_json::json!({"ide": "clion"}),
                },
            ),
            (
                pinned("ninja", 'b'),
                IntegrationEntry {
                    namespace: "com.microsoft.vscode".to_owned(),
                    payload: serde_json::json!("enabled"),
                },
            ),
        ];

        let rows = IntegrationAttribution::from_pairs(&pairs);

        assert_eq!(rows.len(), 2, "one row per input pair");
        assert_eq!(
            rows[0].namespace, "com.jetbrains",
            "namespace sourced from the entry half"
        );
        assert_eq!(
            rows[0].package,
            Some(pinned("cmake", 'a').to_string()),
            "package sourced from the identifier half"
        );
        assert_eq!(rows[0].payload, serde_json::json!({"ide": "clion"}));
        assert_eq!(rows[1].namespace, "com.microsoft.vscode");
        assert_eq!(rows[1].package, Some(pinned("ninja", 'b').to_string()));
        assert_eq!(rows[1].payload, serde_json::json!("enabled"));
    }

    // ── plain-mode: table stays byte-stable when empty, hint gated on non-empty ──
    //
    // `print_plain` cannot be captured/inspected byte-for-byte here (no
    // injectable writer on `Printer` — see `has_patch_entry`'s doc comment
    // for the same constraint), so each test below asserts the real *decision*
    // via `has_availability_hint` directly, then also drives `print_plain` as a
    // no-panic smoke check on the same fixture. The hint's actual text is
    // pinned byte-for-byte by the `availability_hint`/`summarize_claims` unit
    // tests below, which are pure functions with no `Printer` dependency.

    #[test]
    fn plain_table_renders_without_panicking_when_binaries_and_entrypoints_are_empty() {
        let vars = EnvVars::new(vec![entry("KEY", None)], Vec::new(), Vec::new(), Vec::new());
        assert!(
            !has_availability_hint(&vars.binaries, &vars.entrypoints, &vars.integrations),
            "nothing to announce when all three claim arrays are empty"
        );
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    #[test]
    fn plain_table_emits_hint_when_binaries_non_empty() {
        let vars = EnvVars::new(Vec::new(), vec![attribution("cmake", None)], Vec::new(), Vec::new());
        assert!(
            has_availability_hint(&vars.binaries, &vars.entrypoints, &vars.integrations),
            "non-empty binaries alone must trigger the hint gate"
        );
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    #[test]
    fn plain_table_emits_hint_when_entrypoints_non_empty() {
        let vars = EnvVars::new(Vec::new(), Vec::new(), vec![attribution("fmt", None)], Vec::new());
        assert!(
            has_availability_hint(&vars.binaries, &vars.entrypoints, &vars.integrations),
            "non-empty entrypoints alone must trigger the hint gate"
        );
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    /// The third gate arm — a claim array `print_plain` never checked before
    /// integrations existed. Both `binaries` and `entrypoints` stay empty
    /// here, so a regression that drops `|| !self.integrations.is_empty()`
    /// from the gate turns this red while every acceptance fixture (which
    /// auto-fills `binaries`) stays green.
    #[test]
    fn plain_table_emits_hint_when_integrations_non_empty() {
        let vars = EnvVars::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![integration("com.jetbrains", None, serde_json::json!({}))],
        );
        assert!(
            has_availability_hint(&vars.binaries, &vars.entrypoints, &vars.integrations),
            "non-empty integrations alone must trigger the hint gate"
        );
        let hint = availability_hint(&vars.binaries, &vars.entrypoints, &vars.integrations);
        assert!(
            hint.contains("com.jetbrains"),
            "the hint must name the integration namespace: {hint}"
        );
        let printer = DataInterface::new(Printer::new(false, false));
        vars.print_plain(&printer);
    }

    // ── availability_hint / summarize_claims — Decision C hint-line format ────

    fn attr(name: &str) -> BinaryAttribution {
        attribution(name, None)
    }

    #[test]
    fn hint_formats_count_and_preview_names() {
        let hint = availability_hint(&[attr("cmake"), attr("ctest")], &[], &[]);
        assert_eq!(
            hint,
            "2 binaries available (cmake, ctest); use --format json for the full list"
        );
    }

    #[test]
    fn hint_uses_singular_label_for_one_binary() {
        let hint = availability_hint(&[attr("cmake")], &[], &[]);
        assert_eq!(hint, "1 binary available (cmake); use --format json for the full list");
    }

    #[test]
    fn hint_truncates_preview_with_ellipsis_beyond_three_names() {
        let claims = vec![attr("a"), attr("b"), attr("c"), attr("d")];
        let hint = availability_hint(&claims, &[], &[]);
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
        let hint = availability_hint(&claims, &[], &[]);
        assert_eq!(
            hint,
            "3 binaries available (a, b, c); use --format json for the full list"
        );
    }

    #[test]
    fn hint_combines_binaries_and_entrypoints_as_separate_clauses() {
        let hint = availability_hint(&[attr("cmake")], &[attr("fmt")], &[]);
        assert_eq!(
            hint,
            "1 binary available (cmake); 1 entrypoint available (fmt); use --format json for the full list"
        );
    }

    #[test]
    fn hint_omits_binaries_clause_when_only_entrypoints_present() {
        let hint = availability_hint(&[], &[attr("fmt"), attr("cmake")], &[]);
        assert_eq!(
            hint,
            "2 entrypoints available (fmt, cmake); use --format json for the full list"
        );
    }

    /// `availability_hint` prints namespace keys verbatim, and the namespace
    /// grammar deliberately admits non-ASCII (`com.微软` is legal —
    /// `package::metadata::integrations` documents it, and it is pinned
    /// green in `ocx_lib`'s own namespace-validation tests). Only the STATIC
    /// portions OCX itself writes — the count/label clauses and the trailing
    /// `use --format json for the full list` sentence — are guaranteed ASCII;
    /// a non-ASCII namespace must render unrefused, not fail this check.
    #[test]
    fn hint_static_portions_are_ascii_even_when_a_namespace_is_not() {
        let namespace = "com.微软";
        let hint = availability_hint(
            &[attr("cmake"), attr("ctest")],
            &[attr("fmt")],
            &[integration(namespace, Some("ocx.sh/cmake:3.28"), serde_json::json!({}))],
        );
        assert!(
            hint.contains(namespace),
            "a non-ASCII namespace must render verbatim, not be refused or mangled: {hint}"
        );
        let static_portion = hint.replace(namespace, "");
        assert!(
            static_portion.is_ascii(),
            "every part OCX itself writes (count/label clauses, trailing sentence) must stay ASCII: {static_portion}"
        );
    }

    // ── availability_hint — integrations clause (adr_package_integrations.md C-015/D15) ──

    #[test]
    fn hint_combines_binaries_entrypoints_and_integrations_in_declared_order() {
        let hint = availability_hint(
            &[attr("cmake")],
            &[attr("fmt")],
            &[integration("com.a", Some("ocx.sh/cmake:3.28"), serde_json::json!({}))],
        );
        assert_eq!(
            hint,
            "1 binary available (cmake); 1 entrypoint available (fmt); \
             1 integration namespace (com.a); use --format json for the full list"
        );
    }

    #[test]
    fn hint_uses_plural_label_and_truncates_integration_namespaces_beyond_three() {
        let rows = vec![
            integration("com.a", Some("pkg-a"), serde_json::json!({})),
            integration("com.b", Some("pkg-b"), serde_json::json!({})),
            integration("com.c", Some("pkg-c"), serde_json::json!({})),
            integration("com.d", Some("pkg-d"), serde_json::json!({})),
        ];
        let hint = availability_hint(&[], &[], &rows);
        assert_eq!(
            hint,
            "4 integration namespaces (com.a, com.b, com.c, ...); use --format json for the full list"
        );
    }

    /// Two rows sharing one namespace (S-008: the JSON array legitimately
    /// carries both, one per declaring package) must still count as ONE
    /// namespace in the hint — the hint dedupes what the array does not.
    #[test]
    fn hint_dedupes_repeated_namespace_declared_by_two_packages() {
        let rows = vec![
            integration("com.a", Some("pkg-a"), serde_json::json!({})),
            integration("com.a", Some("pkg-b"), serde_json::json!({})),
        ];
        let hint = availability_hint(&[], &[], &rows);
        assert_eq!(
            hint,
            "1 integration namespace (com.a); use --format json for the full list"
        );
    }
}
