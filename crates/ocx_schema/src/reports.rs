// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The published `--format json` report contract.
//!
//! Every type in [`ocx::api::data`] that implements `Printable` is a root of
//! the JSON a command writes to stdout. This module emits one document
//! describing all of them, generated from the same definitions the CLI
//! serializes — so an SDK can pin its parsers against the wire format instead
//! of against a fixture somebody typed by hand.
//!
//! # Why the raw schemars output is post-processed
//!
//! schemars derives its `required` set from the Rust type, and serde derives
//! the emitted keys from its attributes. For `Option<T>` the two disagree, and
//! the disagreement is the whole contract a parser needs:
//!
//! | Rust | serde emits | raw schemars | corrected |
//! |---|---|---|---|
//! | `T` | always, non-null | required | unchanged |
//! | `Option<T>` | always, `null` when `None` | **not required**, nullable | **required**, nullable |
//! | `Option<T>` + `skip_serializing_if` | absent when `None`, never null | not required, **nullable** | not required, **non-null** |
//! | `U` + `skip_serializing_if` | absent when empty | **required** | not required |
//!
//! schemars renders rows two and three identically, so the raw output cannot
//! tell a key that is *sometimes absent* from one that is *always present and
//! sometimes null* — the exact distinction a parser gets wrong silently. Every
//! `skip_serializing_if` field therefore carries a
//! `#[schemars(extend("x-ocx-absent-when-none" = true))]` marker; [`normalize`]
//! consumes it, fixes `required` and nullability in both directions, and strips
//! the marker from the published document. `reports_roots_are_complete` and the
//! `every_skip_serializing_if_field_is_marked` test in `ocx` keep the markers
//! and this root list from drifting away from the source.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

/// Canonical published URL of the report contract.
pub const REPORTS_ID: &str = "https://ocx.sh/schemas/reports/v1.json";

const REPORTS_COMMENT: &str =
    "machine-generated from the CLI's own report types; `reports` maps each --format json root to its definition";

/// Field marker meaning "serde omits this key instead of writing `null`".
const ABSENT_WHEN_NONE: &str = "x-ocx-absent-when-none";

/// Every `--format json` root, in `impl Printable for …` order per module.
///
/// A plain path is published under its own last segment. A generic root has no
/// single name to take, so it is spelled `<type> as "<name>"` and published
/// under the name given — one entry per instantiation the CLI actually prints.
macro_rules! report_roots {
    ($generator:expr, $($path:ty $(as $alias:literal)?),* $(,)?) => {{
        let mut roots = Map::new();
        $(
            #[allow(unused_mut, unused_assignments)]
            let mut name = stringify!($path).rsplit("::").next().expect("a path has a last segment").trim();
            $(name = $alias;)?
            let schema = $generator.subschema_for::<$path>();
            roots.insert(
                name.to_owned(),
                serde_json::to_value(&schema).expect("a schemars Schema is always serializable"),
            );
        )*
        roots
    }};
}

/// Generate the report contract as pretty-printed JSON.
pub fn reports_schema() -> String {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());
    let mut generator = settings.into_generator();

    let roots = report_roots![
        generator,
        ocx::api::data::about::About,
        ocx::api::data::announce::AnnounceReport,
        ocx::api::data::attestation::AttestationReport,
        ocx::api::data::catalog::Catalog,
        ocx::api::data::clean::Clean,
        ocx::api::data::config_setup::ConfigSetupData,
        ocx::api::data::config_test::ConfigTestData,
        ocx::api::data::config_update::ConfigUpdateData,
        ocx::api::data::deps::Dependencies,
        ocx::api::data::deps::DependenciesTrace,
        ocx::api::data::deps::FlatDependencies,
        ocx::api::data::env::EnvVars,
        ocx::api::data::index::CatalogPreview,
        ocx::api::data::index::RegenerateReport,
        ocx::api::data::install::Installs,
        ocx::api::data::lock::LockReport,
        ocx::api::data::login::LoginResult,
        ocx::api::data::login::LogoutResult,
        ocx::api::data::package_cascade_check::PackageCascadeCheck,
        ocx::api::data::package_cascade_repair::PackageCascadeRepair,
        ocx::api::data::package_copy::CopyReport,
        ocx::api::data::package_description::PackageDescription,
        ocx::api::data::package_description::PackageDescriptions,
        ocx::api::data::package_inspect::InspectReport,
        ocx::api::data::package_inspect::PackageInspect,
        ocx::api::data::patch_freeze::PatchFreezeReport,
        ocx::api::data::patch_publish::PatchPublishReport,
        ocx::api::data::patch_sync::PatchSyncReport,
        ocx::api::data::patch_test::PatchTestReport,
        ocx::api::data::patch_why::PatchWhyReport,
        ocx::api::data::paths::LocatedPaths,
        ocx::api::data::paths::Paths,
        ocx::api::data::pull_dry_run::PullDryRun,
        ocx::api::data::push::PushReport,
        ocx::api::data::removed::Removed,
        ocx::api::data::sbom::SbomListingReport,
        ocx::api::data::script_run::ScriptRunReport,
        ocx::api::data::self_setup::SelfSetupData,
        ocx::api::data::self_update::SelfUpdateData,
        ocx::api::data::self_update::UpdateCheckData,
        ocx::api::data::shell_state::ShellStateReport,
        ocx::api::data::shell_state::VerboseShellState,
        ocx::api::data::signature::SignatureReport,
        ocx::api::data::status::StatusReport,
        // `SweepReport<R>` has a generic `Printable` impl; the CLI prints exactly
        // these two instantiations, from `package sign` and `package attest`.
        ocx::api::data::sweep::SweepReport<ocx::api::data::signature::SignatureReport>
            as "SweepReport<SignatureReport>",
        ocx::api::data::sweep::SweepReport<ocx::api::data::attestation::AttestationReport>
            as "SweepReport<AttestationReport>",
        ocx::api::data::tag::Tags,
        ocx::api::data::verification::VerificationReport,
        ocx::api::data::version::VerboseVersionData,
        ocx::api::data::version::VersionData,
        ocx::api::data::warmed_paths::WarmedPaths,
    ];

    let mut defs = Value::Object(generator.take_definitions(true));
    normalize(&mut defs);

    let mut document = Map::new();
    document.insert(
        "$schema".to_owned(),
        Value::String("https://json-schema.org/draft/2020-12/schema".to_owned()),
    );
    document.insert("$id".to_owned(), Value::String(REPORTS_ID.to_owned()));
    document.insert("$comment".to_owned(), Value::String(REPORTS_COMMENT.to_owned()));
    document.insert("reports".to_owned(), Value::Object(roots));
    document.insert("$defs".to_owned(), defs);

    serde_json::to_string_pretty(&Value::Object(document))
        .expect("a serde_json::Value is always serializable to a JSON string")
}

/// Rewrite `required` and nullability to match what serde actually emits.
///
/// Walks the whole document: any object carrying `properties` has its own
/// `required` corrected, then every child is visited so nested and inline
/// object schemas are corrected too.
fn normalize(node: &mut Value) {
    match node {
        Value::Array(items) => {
            for item in items {
                normalize(item);
            }
        }
        Value::Object(map) => {
            if map.contains_key("properties") {
                correct_required(map);
            }
            for value in map.values_mut() {
                normalize(value);
            }
        }
        _ => {}
    }
}

/// Apply the two corrections to one object schema's `required` list.
fn correct_required(map: &mut Map<String, Value>) {
    let mut required: Vec<String> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    let Some(Value::Object(properties)) = map.get_mut("properties") else {
        return;
    };
    let order: Vec<String> = properties.keys().cloned().collect();

    for (key, property) in properties.iter_mut() {
        let absent_when_none = property
            .get(ABSENT_WHEN_NONE)
            .and_then(Value::as_bool)
            .unwrap_or_default();
        if absent_when_none {
            if let Value::Object(object) = property {
                object.remove(ABSENT_WHEN_NONE);
            }
            // serde omits the key entirely rather than writing `null`, so the
            // nullable rendering schemars produced for `Option<T>` is wrong.
            strip_null(property);
            required.retain(|name| name != key);
        } else if is_nullable(property) && !required.iter().any(|name| name == key) {
            // A plain `Option<T>` is always written — as `null` when `None`.
            required.push(key.clone());
        }
    }

    required.sort_by_key(|name| order.iter().position(|field| field == name).unwrap_or(usize::MAX));
    if required.is_empty() {
        map.remove("required");
    } else {
        map.insert(
            "required".to_owned(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
}

/// Report whether a property schema admits `null`.
fn is_nullable(property: &Value) -> bool {
    if let Some(Value::Array(types)) = property.get("type") {
        return types.iter().any(|value| value.as_str() == Some("null"));
    }
    ["anyOf", "oneOf"].iter().any(|key| {
        property
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(is_null_branch))
    })
}

/// Remove the `null` alternative a property schema admits, if any.
fn strip_null(property: &mut Value) {
    let Value::Object(map) = property else {
        return;
    };
    if let Some(Value::Array(types)) = map.get_mut("type") {
        types.retain(|value| value.as_str() != Some("null"));
        if let [only] = types.as_slice() {
            let only = only.clone();
            map.insert("type".to_owned(), only);
        }
    }
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = map.get_mut(key) else {
            continue;
        };
        branches.retain(|branch| !is_null_branch(branch));
        // A one-branch anyOf is just that branch; inline it so the published
        // shape reads the same as a field that was never optional.
        if let [only] = branches.as_slice() {
            let only = only.clone();
            map.remove(key);
            if let Value::Object(fields) = only {
                for (name, value) in fields {
                    map.insert(name, value);
                }
            }
        }
    }
}

/// Report whether a schema branch is the bare `null` type.
fn is_null_branch(branch: &Value) -> bool {
    branch.get("type").and_then(Value::as_str) == Some("null")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `api::data`, resolved from this crate's manifest directory.
    fn data_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_cli/src/api/data")
    }

    /// Every `.rs` under `api::data`, with its `#[cfg(test)]` half removed.
    fn data_sources() -> Vec<(String, String)> {
        let mut files: Vec<_> = std::fs::read_dir(data_dir())
            .expect("api::data is a directory in this workspace")
            .filter_map(|entry| {
                let path = entry.expect("a readable directory entry").path();
                (path.extension()? == "rs").then(|| {
                    let name = path.file_name()?.to_string_lossy().into_owned();
                    let body = std::fs::read_to_string(&path).ok()?;
                    Some((name, body.split("#[cfg(test)]").next()?.to_owned()))
                })?
            })
            .collect();
        files.sort();
        files
    }

    /// A root that is printable but unlisted is a command whose JSON nobody
    /// published — the failure mode is silent, so it is asserted rather than
    /// trusted to review.
    #[test]
    fn every_printable_root_is_published() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let published: Vec<&str> = document["reports"]
            .as_object()
            .expect("`reports` is an object")
            .keys()
            .map(String::as_str)
            .collect();

        let mut missing = Vec::new();
        for (file, body) in data_sources() {
            for line in body.lines() {
                // `impl<R: Serialize> Printable for SweepReport<R>` counts too:
                // a generic root is still a printed document, and matching only
                // the plain form is how it went unpublished the first time.
                let Some((_, tail)) = line.split_once("Printable for ") else {
                    continue;
                };
                let name = tail.split(['<', ' ', '{']).next().unwrap_or_default().trim();
                if name.is_empty() {
                    continue;
                }
                // A generic root is published once per instantiation, under a
                // name that starts with the base type.
                if !published
                    .iter()
                    .any(|root| *root == name || root.starts_with(&format!("{name}<")))
                {
                    missing.push(format!("{name} ({file})"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "printable report roots missing from `report_roots!`: {missing:?}"
        );
    }

    /// The marker is what tells `absent` from `null`. A `skip_serializing_if`
    /// field without one is published as always-present-and-nullable, which is
    /// the single most expensive thing this contract can get wrong.
    #[test]
    fn every_skip_serializing_if_field_carries_the_marker() {
        let mut unmarked = Vec::new();
        for (file, body) in data_sources() {
            let lines: Vec<&str> = body.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if !line.contains("skip_serializing_if") {
                    continue;
                }
                let marked = lines[index + 1..]
                    .iter()
                    .take_while(|next| next.trim_start().starts_with('#'))
                    .any(|next| next.contains(ABSENT_WHEN_NONE));
                if !marked {
                    unmarked.push(format!("{file}:{}", index + 1));
                }
            }
        }
        assert!(
            unmarked.is_empty(),
            "`skip_serializing_if` fields missing #[schemars(extend(\"{ABSENT_WHEN_NONE}\" = true))]: {unmarked:?}"
        );
    }

    /// The two corrections, pinned on real fields rather than on a fixture.
    ///
    /// `DryRunEntry::path` is a plain `Option<PathBuf>` — always written, `null`
    /// when absent. `EnvEntry::source` is `skip_serializing_if` — omitted
    /// entirely, never `null`. schemars renders both identically; a parser that
    /// believes it has broken on both, and the SDK's `pull --dry-run` parser
    /// once did.
    #[test]
    fn the_two_option_shapes_are_published_differently() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let defs = &document["$defs"];

        let entry = &defs["DryRunEntry"];
        assert!(
            entry["required"]
                .as_array()
                .expect("DryRunEntry has required fields")
                .iter()
                .any(|name| name == "path"),
            "a plain Option<T> is always written, so it is required"
        );
        assert_eq!(
            entry["properties"]["path"]["type"],
            serde_json::json!(["string", "null"])
        );

        let env = &defs["EnvEntry"];
        assert!(
            !env["required"]
                .as_array()
                .expect("EnvEntry has required fields")
                .iter()
                .any(|name| name == "source"),
            "a skip_serializing_if field is absent, not null, so it is not required"
        );
        assert!(
            env["properties"]["source"].get("anyOf").is_none(),
            "and its null alternative is stripped: {}",
            env["properties"]["source"]
        );
    }

    /// The marker is an internal handshake between the derive and `normalize`;
    /// publishing it would invite consumers to depend on it.
    #[test]
    fn the_marker_never_reaches_the_published_document() {
        assert!(!reports_schema().contains(ABSENT_WHEN_NONE));
    }
}
