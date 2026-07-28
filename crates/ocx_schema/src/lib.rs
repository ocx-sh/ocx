// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Library surface for the JSON-Schema generator binary.
//!
//! `main.rs` is a thin shell that delegates to [`schema_for`]. The library
//! layer exists so tests can exercise the generator output directly without
//! shelling out to the compiled binary.

use ocx_lib::Config;
use ocx_lib::package::metadata::authoring::AuthoringMetadata;
use ocx_lib::patch::PatchDescriptor;
use ocx_lib::project::{ProjectConfig, ProjectLock};
use ocx_lib::record::ExecutionRecord;
use schemars::generate::SchemaSettings;

/// Top-level `$comment` injected into the project-lock schema. Flags the
/// format as machine-generated and subject to evolution so consumers
/// (taplo, schema-store) surface a hint not to hand-author `ocx.lock`.
/// Mirrors the user-guide locking-subsection callout.
const PROJECT_LOCK_COMMENT: &str = "machine-generated; format may evolve across OCX versions — do not hand-edit";

/// Generate a JSON Schema for the given schema kind.
///
/// Returns `Some(json_string)` for known kinds and `None` for unknown kinds.
/// Known kinds: `metadata`, `config`, `project`, `project-lock`, `patch`,
/// `execution-record`.
///
/// The output JSON has its `$id` set to the canonical published URL
/// (`https://ocx.sh/schemas/<kind>/<version>.json`). Every schema is at
/// `v1.json` except `project-lock`, which is at `v3.json` (in lock-step with
/// `LockVersion::V3`). The `project-lock` schema additionally carries a
/// top-level `$comment` flagging the format as machine-generated.
///
/// The `patch` schema describes the JSON document authored for
/// `ocx patch publish --descriptor` (and carried in the `__ocx.patch`
/// OCI artifact layer); the `[patches]` config tier itself is covered by the
/// `config` schema.
///
/// The `execution-record` schema describes the pre-exec resolution record
/// written per tool launch. Its URL version and the record's own in-band
/// `schemaVersion` move in lockstep — the first incompatible change bumps
/// both. The `[records]` config section that designates the sink is covered by
/// the `config` schema, mirroring the `patch` / `[patches]` split above.
pub fn schema_for(kind: &str) -> Option<String> {
    match kind {
        // Per-layer strip/prefix layout lives in manifest layer-descriptor annotations
        // (`sh.ocx.layer.*`), not on `Bundle` — do not add a `layers` field here.
        //
        // The metadata schema describes the AUTHORING form (the sidecar a
        // publisher edits): a superset of the published wire form — dependency
        // digests optional, sidecar-only `platforms` fields allowed. Published
        // blobs remain a valid subset, so the same v1 URL keeps covering both.
        // ADR: adr_dependency_manifest_pinning.md.
        "metadata" => Some(generate_schema::<AuthoringMetadata>(
            "https://ocx.sh/schemas/metadata/v1.json",
            None,
        )),
        "config" => Some(generate_schema::<Config>("https://ocx.sh/schemas/config/v1.json", None)),
        "project" => Some(generate_schema::<ProjectConfig>(
            "https://ocx.sh/schemas/project/v1.json",
            None,
        )),
        "project-lock" => Some(generate_schema::<ProjectLock>(
            "https://ocx.sh/schemas/project-lock/v3.json",
            Some(PROJECT_LOCK_COMMENT),
        )),
        "patch" => Some(generate_schema::<PatchDescriptor>(
            "https://ocx.sh/schemas/patch/v1.json",
            None,
        )),
        "execution-record" => Some(generate_schema::<ExecutionRecord>(
            "https://ocx.sh/schemas/execution-record/v1.json",
            None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The metadata schema describes the authoring superset: dependency
    /// identifiers are digest-optional (plain `Identifier`, not
    /// `PinnedIdentifier`) and the sidecar-only `platforms` fields exist on
    /// both the bundle and each dependency.
    #[test]
    fn metadata_schema_is_the_authoring_superset() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");
        let defs = value.get("$defs").expect("schema has $defs");

        let dependency = defs.get("AuthoringDependency").expect("authoring dependency def");
        assert!(
            dependency.pointer("/properties/platforms").is_some(),
            "dependency must document the per-platform pin map"
        );
        // Digest-optional identifier: the plain Identifier type, and required
        // fields do not force a digest-bearing PinnedIdentifier.
        assert_eq!(
            dependency
                .pointer("/properties/identifier/$ref")
                .and_then(|v| v.as_str()),
            Some("#/$defs/Identifier"),
            "authoring dependency identifier must be the digest-optional Identifier"
        );
        assert!(
            defs.get("PinnedIdentifier").is_none(),
            "authoring schema must not define the digest-required PinnedIdentifier"
        );
    }

    /// `AuthoringBundle.binaries` is additive-optional (never `required`) and
    /// its resolved shape is the write contract only — `array<string>` with
    /// `uniqueItems`. The read-side `string | object` leniency
    /// (`BinaryElement`) is a Rust-only `Deserialize` affordance and must
    /// never leak into the published schema. See
    /// `adr_declared_binaries_metadata.md` §1 and the manual
    /// `impl JsonSchema for Binaries` in `binary.rs`.
    #[test]
    fn metadata_schema_pins_binaries_field_as_optional_string_array() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");
        let defs = value.get("$defs").expect("schema has $defs");

        let bundle = defs.get("AuthoringBundle").expect("authoring bundle def");
        let binaries_property = bundle
            .pointer("/properties/binaries")
            .expect("bundle must declare a `binaries` property");

        let required = bundle
            .get("required")
            .and_then(|r| r.as_array())
            .expect("bundle def has a `required` array");
        assert!(
            !required.iter().any(|v| v.as_str() == Some("binaries")),
            "`binaries` must be additive-optional, not required: {required:?}"
        );

        // `Option<Binaries>` schemas as `anyOf: [$ref, null]` — resolve the
        // `$ref` arm to the underlying `Binaries` def.
        let binaries_ref = binaries_property
            .get("anyOf")
            .and_then(|v| v.as_array())
            .and_then(|arms| arms.iter().find_map(|arm| arm.get("$ref").and_then(|r| r.as_str())))
            .expect("optional `binaries` property must carry a `$ref` arm in `anyOf`");
        let name = binaries_ref
            .strip_prefix("#/$defs/")
            .expect("`binaries` $ref must point into #/$defs/");
        let binaries_def = defs
            .get(name)
            .unwrap_or_else(|| panic!("$ref target {name} missing from $defs"));

        assert_eq!(
            binaries_def.get("type").and_then(|v| v.as_str()),
            Some("array"),
            "`binaries` must resolve to a JSON array"
        );
        assert_eq!(
            binaries_def.pointer("/items/type").and_then(|v| v.as_str()),
            Some("string"),
            "`binaries` items must be bare strings — no read-side object leniency in the published schema"
        );
        assert_eq!(
            binaries_def.get("uniqueItems").and_then(|v| v.as_bool()),
            Some(true),
            "`binaries` must declare `uniqueItems: true`"
        );

        assert!(
            defs.get("BinaryElement").is_none(),
            "the read-side string|object element union must never appear in the published schema"
        );
    }

    /// The execution-record schema is the published half of a two-channel
    /// version contract: this URL and the in-band `schemaVersion` move in
    /// lockstep, so the first incompatible change bumps both. Records are
    /// consumed by policy engines keyed on exact field names, so the camelCase
    /// wire spellings are pinned here — a blanket `rename_all` (or its removal)
    /// would rewrite them silently.
    #[test]
    fn execution_record_schema_pins_id_and_wire_key_spellings() {
        let schema = schema_for("execution-record").expect("execution-record schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");

        assert_eq!(
            value.get("$id").and_then(|v| v.as_str()),
            Some("https://ocx.sh/schemas/execution-record/v1.json"),
            "the published URL must move in lockstep with the in-band schemaVersion"
        );

        let required: Vec<&str> = value
            .get("required")
            .and_then(|r| r.as_array())
            .expect("schema has a `required` array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for key in ["schemaVersion", "kind", "recordedAt", "packages"] {
            assert!(
                required.contains(&key),
                "`{key}` must be a required top-level key: {required:?}"
            );
        }
    }

    /// `[records]` rides the `config` schema transitively — there is no separate
    /// arm for `RecordsOptions`. Its `system_locked` flag is runtime provenance
    /// set by the loader on `/etc/ocx/config.toml`, never authored, so it must
    /// not appear in the published schema and invite operators to write it.
    #[test]
    fn config_schema_documents_records_without_the_loader_only_lock_flag() {
        let schema = schema_for("config").expect("config schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");

        assert!(
            value.pointer("/properties/records").is_some(),
            "the config schema must document the [records] section"
        );

        let records = value
            .pointer("/$defs/RecordsOptions")
            .expect("config schema defines RecordsOptions");
        assert!(
            records.pointer("/properties/system_locked").is_none(),
            "`system_locked` is loader-set provenance and must stay out of the published schema"
        );
    }
}
