// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Library surface for the JSON-Schema generator binary.
//!
//! `main.rs` is a thin shell that delegates to [`schema_for`]. The library
//! layer exists so tests can exercise the generator output directly without
//! shelling out to the compiled binary.

use ocx_lib::Config;
use ocx_lib::package::metadata::Metadata;
use ocx_lib::profile::ProfileManifest;
use ocx_lib::project::{ProjectConfig, ProjectLock};
use schemars::generate::SchemaSettings;

/// Top-level `$comment` injected into the project-lock schema. Flags the
/// format as machine-generated and subject to evolution so consumers
/// (taplo, schema-store) surface a hint not to hand-author `ocx.lock`.
/// Mirrors the user-guide locking-subsection callout.
const PROJECT_LOCK_COMMENT: &str = "machine-generated; format may evolve across OCX versions — do not hand-edit";

/// Generate a JSON Schema for the given schema kind.
///
/// Returns `Some(json_string)` for known kinds and `None` for unknown kinds.
/// Known kinds: `metadata`, `profile`, `config`, `project`, `project-lock`.
///
/// The output JSON has its `$id` set to the canonical published URL
/// (`https://ocx.sh/schemas/<kind>/v1.json`). The `project-lock` schema
/// additionally carries a top-level `$comment` flagging the format as
/// machine-generated.
pub fn schema_for(kind: &str) -> Option<String> {
    match kind {
        "metadata" => Some(generate_schema::<Metadata>(
            "https://ocx.sh/schemas/metadata/v1.json",
            None,
        )),
        "profile" => Some(generate_schema::<ProfileManifest>(
            "https://ocx.sh/schemas/profile/v1.json",
            None,
        )),
        "config" => Some(generate_schema::<Config>("https://ocx.sh/schemas/config/v1.json", None)),
        "project" => Some(generate_schema::<ProjectConfig>(
            "https://ocx.sh/schemas/project/v1.json",
            None,
        )),
        "project-lock" => Some(generate_schema::<ProjectLock>(
            "https://ocx.sh/schemas/project-lock/v1.json",
            Some(PROJECT_LOCK_COMMENT),
        )),
        _ => None,
    }
}

fn generate_schema<T: schemars::JsonSchema>(id: &str, comment: Option<&str>) -> String {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());

    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();

    // SAFETY: schemars' `RootSchema` is a derived struct of `serde_json`-
    // friendly types (objects, arrays, scalars) — `to_value` cannot fail
    // for it. Any failure here would be a bug in schemars, not user input.
    let mut value =
        serde_json::to_value(&schema).expect("schemars RootSchema is always serializable to serde_json::Value");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("$id".to_owned(), serde_json::Value::String(id.to_owned()));
        if let Some(c) = comment {
            obj.insert("$comment".to_owned(), serde_json::Value::String(c.to_owned()));
        }
    }

    // SAFETY: `value` is an in-memory `serde_json::Value` we just built;
    // pretty-printing it cannot fail (no I/O, no fallible serializers).
    serde_json::to_string_pretty(&value).expect("serde_json::Value is always serializable to a JSON string")
}
