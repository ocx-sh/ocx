// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report payloads for the two `ocx index` shapes that emit one
//! (`adr_servable_index_snapshot.md` C-010, C-027).
//!
//! `ocx index update` has no stdout payload — the aggregated error on stderr is
//! its batch signal — so both types here are new surface rather than a shared
//! one gaining a field.
//!
//! Both carry names OCX did not author. `regenerate`'s three lists are
//! repository paths derived from a `p/` walk over a tree C-007 explicitly admits
//! may have been "written by another implementation", and
//! `index sync --dry-run`'s list is the key set of a foreign `c/index.json`,
//! which [`CatalogDocument::into_packages`] admits after validating
//! `format_version` and nothing about the keys. Both are printed to an
//! operator's terminal, so both are neutralized on the plain path — see
//! [`sanitize_for_terminal`].
//!
//! [`CatalogDocument::into_packages`]: ocx_lib::oci::index::CatalogDocument

use ocx_lib::cli::{Cell, DataInterface};
use ocx_lib::oci::index::RegenerateOutcome;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

// ── `ocx index regenerate` (C-010) ───────────────────────────────────────────

/// One registry's drift-repair outcome.
///
/// `roots` is what the `p/` walk found, and it is reported even when the three
/// lists are empty: a `0` there says the walk found an empty tree, which is a
/// different fact from "the catalog already matched".
#[derive(Serialize, schemars::JsonSchema)]
pub struct RegenerateEntry {
    pub registry: String,
    pub roots: usize,
    pub added: Vec<String>,
    pub corrected: Vec<String>,
    pub removed: Vec<String>,
}

impl From<RegenerateOutcome> for RegenerateEntry {
    fn from(outcome: RegenerateOutcome) -> Self {
        Self {
            registry: outcome.source,
            roots: outcome.roots,
            added: outcome.added,
            corrected: outcome.corrected,
            removed: outcome.removed,
        }
    }
}

impl RegenerateEntry {
    /// Whether this registry's catalog already matched the tree on disk.
    fn is_clean(&self) -> bool {
        self.added.is_empty() && self.corrected.is_empty() && self.removed.is_empty()
    }
}

/// What `ocx index regenerate <REGISTRY>...` changed, per registry.
///
/// Plain format: a four-column table (Registry | Roots | Change | Package), one
/// row per changed package plus one `none` row for a registry that changed
/// nothing, with Registry and Roots carried on the first row of each group.
/// Every registry therefore appears, which is what makes the per-registry
/// `roots` reportable in a mixed run. A run in which *every* registry was
/// already clean prints a single line instead — C-010's "a clean run says so in
/// one line".
///
/// JSON format: an array of `{ registry, roots, added, corrected, removed }`
/// objects in argument order.
pub struct RegenerateReport {
    registries: Vec<RegenerateEntry>,
}

impl RegenerateReport {
    pub fn new(registries: Vec<RegenerateEntry>) -> Self {
        Self { registries }
    }

    /// Whether every registry's catalog already matched its tree — the
    /// predicate [`Printable::print_plain`] selects C-010's "a clean run says so
    /// in one line" branch on.
    ///
    /// Named rather than inlined so a test can assert the branch's actual
    /// condition. Asserting `all(is_clean)` over a fixture built from empty
    /// `Vec`s instead reduces to `Vec::new().is_empty()` and holds however
    /// `print_plain` is wired — a round-2 reviewer inverted the selector and the
    /// suite stayed green.
    fn all_clean(&self) -> bool {
        self.registries.iter().all(RegenerateEntry::is_clean)
    }

    /// The plain table's column-major rows, already neutralized.
    ///
    /// Split out of [`Printable::print_plain`] so the sanitization is assertable
    /// without capturing stdout — the rows are what reaches the terminal.
    fn plain_rows(&self) -> [Vec<String>; 4] {
        let mut rows: [Vec<String>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.registries {
            let mut changes: Vec<(&str, String)> = [
                ("added", &entry.added),
                ("corrected", &entry.corrected),
                ("removed", &entry.removed),
            ]
            .into_iter()
            .flat_map(|(change, names)| names.iter().map(move |name| (change, sanitize_for_terminal(name))))
            .collect();
            // A clean registry still gets a row. C-010 reports `roots` PER
            // registry, and in a mixed run the all-clean hint below does not
            // fire — so without this the clean registry and its root count
            // vanish from the report entirely while a dirty sibling is printed.
            if changes.is_empty() {
                changes.push(("none", "-".to_string()));
            }
            for (position, (change, package)) in changes.into_iter().enumerate() {
                // Registry and roots label the group, not every row — the value
                // is constant across a group and repeating it buries the
                // package names the report exists to show.
                let label = position == 0;
                rows[0].push(if label {
                    sanitize_for_terminal(&entry.registry)
                } else {
                    String::new()
                });
                rows[1].push(if label { entry.roots.to_string() } else { String::new() });
                rows[2].push(change.to_string());
                rows[3].push(package);
            }
        }
        rows
    }
}

impl Serialize for RegenerateReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.registries.serialize(serializer)
    }
}

impl Printable for RegenerateReport {
    fn print_plain(&self, printer: &DataInterface) {
        if self.all_clean() {
            let roots: usize = self.registries.iter().map(|entry| entry.roots).sum();
            printer.print_hint(&format!(
                "{roots} root document(s); every catalog already matched the tree, nothing written."
            ));
            return;
        }
        printer.print_table(
            &["Registry".into(), "Roots".into(), "Change".into(), "Package".into()],
            &self
                .plain_rows()
                .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

// ── `ocx index sync --dry-run` (C-027) ───────────────────────────────────────

/// One registry's enumerated package set.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CatalogPreviewEntry {
    pub registry: String,
    pub packages: Vec<String>,
}

impl CatalogPreviewEntry {
    /// Sorts `packages`. C-027's report row says "sorted", and the sort lives
    /// here rather than at the call site so the claim is a property of the
    /// payload the doc makes it about — a `CatalogIndex` arrives ordered but a
    /// registry's repository listing does not.
    ///
    /// This is the report's sort, and only the report's: the wet path does not
    /// ride on it. C-012's "the lowest input index wins" needs the same
    /// determinism and gets it from `enumerate_catalog`, which sorts before
    /// either path sees the vector. Moving this sort to the print site is
    /// therefore a safe refactor — it was not, while this was the only one.
    pub fn new(registry: String, mut packages: Vec<String>) -> Self {
        packages.sort();
        Self { registry, packages }
    }
}

/// What `ocx index sync --dry-run` would refresh.
///
/// `index sync` is the only refresh shape whose work set the operator cannot read
/// off the command line, which is the whole reason this payload exists while the
/// refresh itself still reports nothing.
///
/// Plain format: a two-column table (Registry | Package), one package per line,
/// sorted within each registry. An empty enumeration prints one line and still
/// exits 0.
///
/// JSON format: an array of `{ registry, packages }` objects in argument order.
pub struct CatalogPreview {
    registries: Vec<CatalogPreviewEntry>,
}

impl CatalogPreview {
    pub fn new(registries: Vec<CatalogPreviewEntry>) -> Self {
        Self { registries }
    }

    /// The plain table's column-major rows, already neutralized. See
    /// [`RegenerateReport::plain_rows`] for why this is not inlined.
    fn plain_rows(&self) -> [Vec<String>; 2] {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.registries {
            for package in &entry.packages {
                rows[0].push(sanitize_for_terminal(&entry.registry));
                rows[1].push(sanitize_for_terminal(package));
            }
        }
        rows
    }
}

impl Serialize for CatalogPreview {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.registries.serialize(serializer)
    }
}

impl Printable for CatalogPreview {
    fn print_plain(&self, printer: &DataInterface) {
        let rows = self.plain_rows();
        if rows[0].is_empty() {
            printer.print_hint("No packages listed; nothing would be refreshed.");
            return;
        }
        printer.print_table(
            &["Registry".into(), "Package".into()],
            &rows.map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

// The `Serialize` impl above is transparent, so the published schema is the
// inner type's. `registries` is written as a bare array.
impl schemars::JsonSchema for CatalogPreview {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CatalogPreview".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<CatalogPreviewEntry>>::json_schema(generator)
    }
}

// The `Serialize` impl above is transparent, so the published schema is the
// inner type's. `registries` is written as a bare array.
impl schemars::JsonSchema for RegenerateReport {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RegenerateReport".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<RegenerateEntry>>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    //! Specification tests for the two `ocx index` report payloads, written
    //! from `design_spec_servable_index_snapshot.md` C-010 and C-027 and from
    //! the CWE-150 finding two review panels routed to this work package. Each
    //! test names the clause it pins.

    use super::*;
    use crate::api::data::is_bidi_control;

    /// A name carrying every shape the finding measured: a raw ESC (the start
    /// of a CSI sequence), a newline, a NUL, and a right-to-left override.
    const HOSTILE: &str = "ns/\u{1b}[31mev\nil\u{0}\u{202e}gnp.exe";

    // ── CWE-150 — terminal neutralization at the print site ──────────────────

    #[test]
    fn sanitize_drops_control_and_bidi_characters() {
        let cleaned = sanitize_for_terminal(HOSTILE);
        assert!(
            !cleaned.chars().any(|c| c.is_control() || is_bidi_control(c)),
            "no control or bidi character may survive to the terminal; got {cleaned:?}"
        );
    }

    #[test]
    fn sanitize_leaves_an_ordinary_repository_path_untouched() {
        // The neutralization must be invisible for every name OCX itself
        // produces, or it would be silently rewriting the report's data.
        for name in ["kitware/cmake", "ocx/mirror", "a.b-c_d/e123", "ns/pkg"] {
            assert_eq!(sanitize_for_terminal(name), name, "{name} must pass through verbatim");
        }
    }

    #[test]
    fn regenerate_plain_rows_are_neutralized() {
        // C-010's plain output "names every changed package", and the names come
        // from a foreign tree. Asserted on the rows because they are exactly
        // what `print_table` writes to the terminal.
        let report = RegenerateReport::new(vec![RegenerateEntry {
            registry: HOSTILE.to_string(),
            roots: 1,
            added: vec![HOSTILE.to_string()],
            corrected: Vec::new(),
            removed: vec![HOSTILE.to_string()],
        }]);
        for column in report.plain_rows() {
            for cell in column {
                assert!(
                    !cell.chars().any(|c| c.is_control() || is_bidi_control(c)),
                    "regenerate row {cell:?} reached the terminal unneutralized"
                );
            }
        }
    }

    #[test]
    fn catalog_preview_plain_rows_are_neutralized() {
        // C-027 prints keys straight out of a foreign `c/index.json`, which
        // `CatalogDocument::into_packages` never validates.
        let preview = CatalogPreview::new(vec![CatalogPreviewEntry::new(
            HOSTILE.to_string(),
            vec![HOSTILE.to_string()],
        )]);
        for column in preview.plain_rows() {
            for cell in column {
                assert!(
                    !cell.chars().any(|c| c.is_control() || is_bidi_control(c)),
                    "dry-run row {cell:?} reached the terminal unneutralized"
                );
            }
        }
    }

    #[test]
    fn json_keeps_the_name_verbatim() {
        // The other half of "sanitize at the print site": `--format json` is not
        // a terminal, and a consumer diffing the report against the tree needs
        // the real key. `serde_json` escapes the C0 range by specification, so
        // the raw bytes never reach a terminal through this path either.
        let report = RegenerateReport::new(vec![RegenerateEntry {
            registry: "ocx.sh".to_string(),
            roots: 1,
            added: vec![HOSTILE.to_string()],
            corrected: Vec::new(),
            removed: Vec::new(),
        }]);
        let json = serde_json::to_string(&report).expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(
            parsed[0]["added"][0], HOSTILE,
            "JSON must carry the key verbatim, not the display form"
        );
        assert!(
            !json.contains('\u{1b}'),
            "serde_json must have escaped the ESC rather than emitting it raw"
        );
    }

    // ── C-010 — report shape ─────────────────────────────────────────────────

    #[test]
    fn regenerate_report_json_is_an_array_of_per_registry_objects() {
        let report = RegenerateReport::new(vec![
            RegenerateEntry {
                registry: "ocx.sh".to_string(),
                roots: 3,
                added: vec!["ns/added".to_string()],
                corrected: vec!["ns/corrected".to_string()],
                removed: vec!["ns/removed".to_string()],
            },
            RegenerateEntry {
                registry: "corp.example".to_string(),
                roots: 0,
                added: Vec::new(),
                corrected: Vec::new(),
                removed: Vec::new(),
            },
        ]);
        let parsed: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).expect("serializes")).expect("round-trips");

        assert_eq!(parsed[0]["registry"], "ocx.sh");
        assert_eq!(parsed[0]["roots"], 3);
        assert_eq!(parsed[0]["added"][0], "ns/added");
        assert_eq!(parsed[0]["corrected"][0], "ns/corrected");
        assert_eq!(parsed[0]["removed"][0], "ns/removed");
        // Argument order, not sorted — a per-registry failure is reported by
        // input index, so the report has to be readable against the same order.
        assert_eq!(parsed[1]["registry"], "corp.example");
    }

    #[test]
    fn a_clean_registry_survives_a_mixed_run() {
        // C-010 reports `roots` PER registry. The all-clean hint only fires when
        // EVERY registry is clean, so a clean registry beside a dirty one has
        // only the table to appear in — and a rows-per-changed-package shape
        // dropped it entirely, root count included.
        let report = RegenerateReport::new(vec![
            RegenerateEntry {
                registry: "dirty.example".to_string(),
                roots: 2,
                added: vec!["ns/a".to_string()],
                corrected: Vec::new(),
                removed: Vec::new(),
            },
            RegenerateEntry {
                registry: "clean.example".to_string(),
                roots: 99,
                added: Vec::new(),
                corrected: Vec::new(),
                removed: Vec::new(),
            },
        ]);
        let rows = report.plain_rows();
        assert_eq!(
            rows[0],
            ["dirty.example", "clean.example"],
            "both registries must appear, not just the one that drifted"
        );
        assert_eq!(rows[1], ["2", "99"], "each registry reports its own root count");
        assert_eq!(rows[2], ["added", "none"], "a clean registry says so in its own row");
    }

    #[test]
    fn the_one_line_branch_is_selected_by_cleanliness_in_both_directions() {
        // C-010: "a clean run says so in one line". `all_clean` IS the
        // expression `print_plain` branches on, so asserting it both ways pins
        // the decision itself. Its predecessor asserted
        // `all(is_clean)` over a fixture of empty `Vec`s — true by construction
        // of the fixture, independent of every line of production code, and a
        // round-2 reviewer inverted the selector with the suite still green.
        let clean = RegenerateReport::new(vec![RegenerateEntry {
            registry: "ocx.sh".to_string(),
            roots: 42,
            added: Vec::new(),
            corrected: Vec::new(),
            removed: Vec::new(),
        }]);
        assert!(
            clean.all_clean(),
            "an all-clean run must take print_plain's one-line hint branch, not the table"
        );

        // The mixed case: one drifted registry beside a clean one must take the
        // table, or the drift is reported as "nothing written".
        let mixed = RegenerateReport::new(vec![
            RegenerateEntry {
                registry: "dirty.example".to_string(),
                roots: 2,
                added: vec!["ns/a".to_string()],
                corrected: Vec::new(),
                removed: Vec::new(),
            },
            RegenerateEntry {
                registry: "clean.example".to_string(),
                roots: 99,
                added: Vec::new(),
                corrected: Vec::new(),
                removed: Vec::new(),
            },
        ]);
        assert!(
            !mixed.all_clean(),
            "one drifted registry must force the table, whatever its siblings did"
        );

        // And the direction `print_plain` reads it in. The two assertions above
        // pin `all_clean` itself, but both still hold if the branch is inverted
        // to `if !self.all_clean()` — which is precisely the mutation that beat
        // the predecessor. Structural because selecting a branch is only
        // observable through stdout, and `DataInterface` has no capture seam.
        let body = include_str!("index.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half");
        assert!(
            body.contains("if self.all_clean() {"),
            "print_plain must select the one-line hint ON cleanliness, not on its negation"
        );
        // Swapping the two branch BODIES inverts C-010 by the other route, with
        // the condition literal untouched. The hint belongs to the taken branch,
        // so it must come first in the source.
        let branch = body.find("if self.all_clean() {").expect("asserted above");
        let hint = body[branch..]
            .find("print_hint")
            .expect("the one-line branch is a hint");
        let table = body[branch..]
            .find("print_table")
            .expect("the drifted branch is a table");
        assert!(
            hint < table,
            "the one-line hint must be the body of the all_clean branch, not the table"
        );
    }

    #[test]
    fn a_registrys_rows_label_the_group_once() {
        // Registry and roots identify the group; repeating them on every row
        // would bury the package column the report exists for.
        let report = RegenerateReport::new(vec![RegenerateEntry {
            registry: "ocx.sh".to_string(),
            roots: 7,
            added: vec!["ns/a".to_string(), "ns/b".to_string()],
            corrected: Vec::new(),
            removed: vec!["ns/c".to_string()],
        }]);
        let rows = report.plain_rows();
        assert_eq!(rows[3], ["ns/a", "ns/b", "ns/c"], "every changed package is named");
        assert_eq!(rows[0], ["ocx.sh", "", ""], "the registry labels the first row only");
        assert_eq!(rows[1], ["7", "", ""], "roots labels the first row only");
        assert_eq!(
            rows[2],
            ["added", "added", "removed"],
            "each row names which list it came from"
        );
    }

    // ── C-027 — dry-run report shape ─────────────────────────────────────────

    #[test]
    fn catalog_preview_json_is_registry_keyed_objects() {
        let preview = CatalogPreview::new(vec![CatalogPreviewEntry::new(
            "ocx.sh".to_string(),
            vec!["ns/one".to_string(), "ns/two".to_string()],
        )]);
        let parsed: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&preview).expect("serializes")).expect("round-trips");

        assert_eq!(parsed[0]["registry"], "ocx.sh");
        assert_eq!(parsed[0]["packages"][0], "ns/one");
        assert_eq!(parsed[0]["packages"][1], "ns/two");
    }

    #[test]
    fn catalog_preview_prints_one_package_per_row() {
        let preview = CatalogPreview::new(vec![
            CatalogPreviewEntry::new("ocx.sh".to_string(), vec!["ns/one".to_string(), "ns/two".to_string()]),
            CatalogPreviewEntry::new("corp.example".to_string(), vec!["team/tool".to_string()]),
        ]);
        let rows = preview.plain_rows();
        assert_eq!(rows[0], ["ocx.sh", "ocx.sh", "corp.example"]);
        assert_eq!(rows[1], ["ns/one", "ns/two", "team/tool"]);
    }

    #[test]
    fn catalog_preview_sorts_each_registrys_packages() {
        // C-027's report row says "sorted". Fed pre-sorted input the row test
        // above cannot see the difference, so this feeds it unsorted — a
        // registry's repository listing arrives in whatever order it arrives.
        let preview = CatalogPreview::new(vec![CatalogPreviewEntry::new(
            "ocx.sh".to_string(),
            vec!["ns/zulu".to_string(), "ns/alpha".to_string(), "ns/mike".to_string()],
        )]);
        assert_eq!(preview.plain_rows()[1], ["ns/alpha", "ns/mike", "ns/zulu"]);
    }

    #[test]
    fn an_empty_enumeration_produces_no_rows() {
        // C-027: exit 0 on a clean enumeration "even when the set is empty".
        // The payload's job is to have something printable for that case.
        let preview = CatalogPreview::new(vec![CatalogPreviewEntry::new("ocx.sh".to_string(), Vec::new())]);
        assert!(preview.plain_rows()[0].is_empty());
    }
}
