// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use ocx_lib::cli::Cell;
use ocx_lib::managed_config::ManagedConfigPreview;
use serde::Serialize;

use crate::api::Printable;

/// The `[patches]` tier as it would resolve after the candidate merges —
/// defaults applied, so an omitted field is reported as the value that would
/// actually apply rather than as absent.
#[derive(Serialize)]
pub struct PatchesView {
    /// Registry hosting patch descriptors.
    pub registry: String,
    /// Path template for per-package patch repositories, placeholders intact.
    pub path_template: String,
    /// Whether an unavailable companion fails the launch.
    pub required: bool,
}

/// The machine's own `[managed]` tier posture.
///
/// Never the candidate's: a payload carrying a `[managed]` section is rejected
/// outright, so this answers "which tier would adopt this payload", not
/// "what does the payload declare".
#[derive(Serialize)]
pub struct ManagedView {
    /// The effective managed-config source (env override, else the seed).
    pub source: String,
    /// Whether an absent or mismatched snapshot fails commands closed.
    pub required: bool,
}

/// CLI report for `ocx config test` — a candidate managed-config payload
/// validated locally, plus the configuration adopting it would produce.
///
/// Plain format: key/value table; rows with no payload are suppressed, and
/// list-valued fields render one row per value.
///
/// JSON format: a fixed shape — every field is always present, with `null` or
/// `[]` where a tier is unconfigured, so a consumer can key on
/// `.valid`/`.unknown_keys` without probing for the field first.
#[derive(Serialize)]
pub struct ConfigTestData {
    /// The candidate file this report describes.
    pub(crate) candidate: String,
    /// Always `true`: an invalid payload fails the command (exit 78) and
    /// produces no report. Carried so a JSON consumer reads the verdict from
    /// the document rather than inferring it from the exit code.
    pub(crate) valid: bool,
    /// Effective `[registry] default`.
    pub(crate) registry_default: Option<String>,
    /// Effective `[registries.<name>]` keys, sorted.
    pub(crate) registries: Vec<String>,
    /// Effective `[mirrors."<host>"]` keys, sorted.
    pub(crate) mirrors: Vec<String>,
    /// Hosts this machine would contact over plain HTTP once the candidate is
    /// adopted, sorted — the candidate's own `[registries.<name>] insecure`
    /// entries unioned with this machine's `OCX_INSECURE_REGISTRIES`, less
    /// anything the system scope locked shut.
    ///
    /// The gate already computes this set to validate the payload's mirrors;
    /// reporting it is what makes "did my entry take effect?" a one-command
    /// question. A key that is not an exact `host[:port]` grants nothing and
    /// simply will not appear here.
    pub(crate) plain_http: Vec<String>,
    /// Effective `[patches]` tier, defaults applied.
    ///
    /// Same precedence every ocx invocation uses: the merged config tier when
    /// it declares `[patches]`, else the forwarded `OCX_PATCHES` env tier. A
    /// candidate declaring `[patches]` therefore outranks an ambient
    /// `OCX_PATCHES`, exactly as it would once adopted.
    pub(crate) patches: Option<PatchesView>,
    /// The machine's `[managed]` tier posture — see [`ManagedView`].
    pub(crate) managed: Option<ManagedView>,
    /// Dotted paths of keys the config schema ignores, sorted. Advisory: the
    /// loader ignores unknown keys by design, so these are equally typos and
    /// settings a newer ocx understands.
    ///
    /// Keys inside a `[mirrors]` entry are NOT covered: that table is parsed
    /// value-first from a raw TOML value, so nothing there is ever ignored by
    /// the schema. A misspelled mirror role is silently dropped and reported
    /// here as clean.
    pub(crate) unknown_keys: Vec<String>,
}

impl ConfigTestData {
    /// Projects a validated preview into the report shape.
    pub fn new(
        candidate: &Path,
        preview: ManagedConfigPreview,
        patches: Option<ocx_lib::ResolvedPatchConfig>,
        managed: Option<&ocx_lib::ResolvedManagedConfig>,
        plain_http: Vec<String>,
    ) -> Self {
        let effective = preview.effective;
        Self {
            candidate: candidate.display().to_string(),
            valid: true,
            registry_default: effective.resolved_default_registry().map(str::to_owned),
            registries: sorted_keys(effective.registries.as_ref()),
            mirrors: sorted_keys(effective.mirrors.as_ref()),
            plain_http: sorted(plain_http),
            patches: patches.map(|patches| PatchesView {
                registry: patches.registry,
                path_template: patches.path_template,
                required: patches.required,
            }),
            managed: managed.map(|managed| ManagedView {
                source: managed.source.to_string(),
                required: managed.required,
            }),
            unknown_keys: preview.unknown_keys,
        }
    }
}

/// A report never inherits the order its input happened to arrive in.
fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort_unstable();
    values
}

/// Sorted keys of an optional config table — the order a `HashMap` yields is
/// not one a report may inherit.
fn sorted_keys<V>(table: Option<&std::collections::HashMap<String, V>>) -> Vec<String> {
    let mut keys: Vec<String> = table.map(|table| table.keys().cloned().collect()).unwrap_or_default();
    keys.sort_unstable();
    keys
}

impl Printable for ConfigTestData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let mut fields: Vec<Cell> = Vec::new();
        let mut values: Vec<Cell> = Vec::new();
        let mut row = |field: Cell, value: String| {
            fields.push(field);
            values.push(Cell::from(value));
        };

        // No `Valid` row: reaching plain output at all IS the verdict (an
        // invalid payload exits 78 with no report). JSON keeps the field for a
        // consumer reading the document rather than the exit code.
        row("Candidate".into(), self.candidate.clone());
        if let Some(registry_default) = &self.registry_default {
            row("Default registry".into(), registry_default.clone());
        }
        // One row per value: a joined list would grow the column without bound
        // (plain pads to the widest cell and never truncates).
        for name in &self.registries {
            row("Registries".into(), name.clone());
        }
        for host in &self.mirrors {
            row("Mirrors".into(), host.clone());
        }
        for host in &self.plain_http {
            row("Plain HTTP".into(), host.clone());
        }
        if let Some(patches) = &self.patches {
            row("Patch registry".into(), patches.registry.clone());
            row("Patch path".into(), patches.path_template.clone());
            row("Patch required".into(), patches.required.to_string());
        }
        if let Some(managed) = &self.managed {
            row("Managed source".into(), managed.source.clone());
            row("Managed required".into(), managed.required.to_string());
        }
        for key in &self.unknown_keys {
            row("Unknown keys".into(), key.clone());
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_of(payload: &str) -> ManagedConfigPreview {
        ocx_lib::managed_config::preview_managed_config(
            payload.as_bytes(),
            ocx_lib::Config::default(),
            &ocx_lib::Config::default(),
        )
        .expect("a well-formed candidate previews")
    }

    fn report(payload: &str, env_insecure: &[String]) -> ConfigTestData {
        let preview = preview_of(payload);
        let plain_http = ocx_lib::insecure_hosts(&preview.effective, env_insecure);
        ConfigTestData::new(
            std::path::Path::new("/tmp/candidate.toml"),
            preview,
            None,
            None,
            plain_http,
        )
    }

    /// The candidate's OWN `[registries]` half must reach the plain-HTTP set —
    /// that is the whole point of previewing a payload before a fleet adopts
    /// it, and both existing acceptance cases drive the env half only.
    ///
    /// Asserted against the same payload minus the entry, with the environment
    /// empty in both: without the pair, an inherited `OCX_INSECURE_REGISTRIES`
    /// would make the green indistinguishable from the entry doing nothing.
    #[test]
    fn the_candidates_own_insecure_entry_reaches_the_reported_plain_http_set() {
        let declared = report("[registries.\"mirror.corp:5000\"]\ninsecure = true\n", &[]);
        assert_eq!(declared.plain_http, vec!["mirror.corp:5000".to_string()]);

        let undeclared = report("[registries.\"mirror.corp:5000\"]\n", &[]);
        assert!(
            undeclared.plain_http.is_empty(),
            "an entry that never states `insecure` licenses nothing"
        );
    }

    /// A key that is not an exact `host[:port]` grants nothing at the transport,
    /// and the report is where that becomes visible before it surfaces as an
    /// opaque TLS error mid-install. Both rows list it verbatim — it IS a config
    /// entry, and the set is exactly what the transport will compare against — so
    /// the signal is seeing `private` where `private.corp:5000` was meant, not the
    /// two rows disagreeing.
    #[test]
    fn a_wrongly_spelled_key_is_reported_verbatim_so_the_operator_can_see_it() {
        let data = report("[registries.private]\ninsecure = true\n", &[]);

        assert_eq!(data.registries, vec!["private".to_string()]);
        assert_eq!(
            data.plain_http,
            vec!["private".to_string()],
            "the set is the key verbatim — the report shows exactly what the transport will compare"
        );
    }

    /// The machine's environment composes with the candidate, and the report
    /// is sorted rather than inheriting the order the two sources arrived in.
    #[test]
    fn the_reported_set_unions_the_environment_and_is_sorted() {
        let data = report(
            "[registries.\"zeta.corp:5000\"]\ninsecure = true\n",
            &["alpha.corp".to_string()],
        );

        assert_eq!(
            data.plain_http,
            vec!["alpha.corp".to_string(), "zeta.corp:5000".to_string()]
        );
    }
}
