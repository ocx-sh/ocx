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
use schemars::generate::SchemaSettings;

/// Top-level `$comment` injected into the project-lock schema. Flags the
/// format as machine-generated and subject to evolution so consumers
/// (taplo, schema-store) surface a hint not to hand-author `ocx.lock`.
/// Mirrors the user-guide locking-subsection callout.
const PROJECT_LOCK_COMMENT: &str = "machine-generated; format may evolve across OCX versions — do not hand-edit";

/// Generate a JSON Schema for the given schema kind.
///
/// Returns `Some(json_string)` for known kinds and `None` for unknown kinds.
/// Known kinds: `metadata`, `config`, `project`, `project-lock`, `patch`.
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
pub fn schema_for(kind: &str) -> Option<String> {
    match kind {
        // Per-layer strip/prefix layout lives in manifest layer-descriptor annotations
        // (`sh.ocx.layer.*`), not on `Bundle` — do not add a `layers` field here.
        //
        // The metadata schema describes the AUTHORING form (the sidecar a
        // publisher edits): the published wire form with one relaxation —
        // dependency digests are optional, because `ocx package create`
        // resolves them. Published blobs are therefore a valid subset and the
        // same v1 URL keeps covering both. The platform a bundle was built for
        // is not in either form: it lives in the OCI image index, and between
        // create and push in an unschema'd build receipt.
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

    /// The metadata schema is the published wire form plus exactly one
    /// relaxation: dependency identifiers are digest-optional (plain
    /// `Identifier`, not `PinnedIdentifier`). Nothing build-time appears —
    /// notably no bundle `platform`, which lives in the build receipt.
    #[test]
    fn metadata_schema_is_published_plus_optional_digest() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");
        let defs = value.get("$defs").expect("schema has $defs");

        let bundle = defs.get("AuthoringBundle").expect("authoring bundle def");
        assert!(
            bundle.pointer("/properties/platform").is_none(),
            "the bundle must declare no platform — it is a build fact, not metadata"
        );

        let dependency = defs.get("AuthoringDependency").expect("authoring dependency def");
        assert!(
            dependency.pointer("/properties/platforms").is_none(),
            "the per-platform pin map is gone; a dependency carries one digest"
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

    /// `Modifier::Unknown` is a read-side fallback for a `type` this binary does
    /// not know. It must not reach the published schema: the schema is the
    /// **write** contract, and an `Unknown` arm would advertise a modifier that
    /// resolves to nothing — telling authors a shape ocx will refuse at
    /// `ValidMetadata` is valid. Kept out by `#[schemars(skip)]`, which this
    /// pins: the `oneOf` stays exactly the executable types.
    #[test]
    fn metadata_schema_omits_the_unknown_modifier_fallback() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");

        let arms = value
            .pointer("/$defs/Var/oneOf")
            .and_then(|v| v.as_array())
            .expect("Var must flatten the modifier into a oneOf");
        let tags: Vec<&str> = arms
            .iter()
            .filter_map(|arm| arm.pointer("/properties/type/const").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            tags,
            vec!["path", "constant", "list"],
            "the published modifier vocabulary must be exactly the executable types"
        );

        assert!(
            !schema.contains("type_name"),
            "the Unknown variant's tag-capture field must never appear in the published schema"
        );
    }

    /// `Integrations` (C-002) needs no manual `JsonSchema` impl — the plain
    /// `#[derive(schemars::JsonSchema)]` on the `BTreeMap<String, Value>`
    /// newtype already emits the exact contract the ADR requires:
    /// `{"type":"object","additionalProperties":true}`, no `properties`, no
    /// `propertyNames` restriction (the namespace grammar is enforced at
    /// `ValidMetadata`, never by the schema). This pins that shape so a future
    /// schemars upgrade or a change to the newtype that drifted from it is
    /// caught here rather than silently publishing a stricter schema than the
    /// parser accepts. `integrations` itself is additive-optional, the same
    /// as `binaries` — see
    /// `metadata_schema_pins_binaries_field_as_optional_string_array` above.
    #[test]
    fn metadata_schema_pins_integrations_as_an_open_object_map() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");
        let defs = value.get("$defs").expect("schema has $defs");

        let bundle = defs.get("AuthoringBundle").expect("authoring bundle def");
        let integrations_property = bundle
            .pointer("/properties/integrations")
            .expect("bundle must declare a `integrations` property");

        let required = bundle
            .get("required")
            .and_then(|r| r.as_array())
            .expect("bundle def has a `required` array");
        assert!(
            !required.iter().any(|v| v.as_str() == Some("integrations")),
            "`integrations` must be additive-optional, not required: {required:?}"
        );

        let name = integrations_property
            .get("$ref")
            .and_then(|r| r.as_str())
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .expect("`integrations` must $ref its own def");
        let integrations_def = defs
            .get(name)
            .unwrap_or_else(|| panic!("$ref target {name} missing from $defs"));

        assert_eq!(
            integrations_def.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "`Integrations` must resolve to a JSON object"
        );
        assert_eq!(
            integrations_def.get("additionalProperties").and_then(|v| v.as_bool()),
            Some(true),
            "`Integrations` must accept any namespace key with any value — the namespace \
             grammar and size caps are enforced at `ValidMetadata`, never expressed in the schema"
        );
        assert!(
            integrations_def.get("properties").is_none(),
            "`Integrations` must declare no fixed `properties` — every key is a publisher namespace"
        );
    }

    /// `separator` is required on the wire for a `list` entry: no human is
    /// present when metadata is read, and a wrongly assumed separator fails
    /// silently in the consuming tool. The Rust field is `Option` only so the
    /// refusal can come from `ValidMetadata` (naming the variable) instead of
    /// serde, so `#[schemars(required)]` is the only thing keeping the write
    /// contract honest — and it is silently dropped by a
    /// `skip_serializing_if` on the same field.
    #[test]
    fn metadata_schema_requires_a_separator_on_list_entries() {
        let schema = schema_for("metadata").expect("metadata schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");
        let defs = value.get("$defs").expect("schema has $defs");

        let list_arm = value
            .pointer("/$defs/Var/oneOf")
            .and_then(|v| v.as_array())
            .expect("Var must flatten the modifier into a oneOf")
            .iter()
            .find(|arm| arm.pointer("/properties/type/const").and_then(|v| v.as_str()) == Some("list"))
            .expect("the published vocabulary must carry a `list` arm");

        let name = list_arm
            .get("$ref")
            .and_then(|r| r.as_str())
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .expect("the `list` arm must $ref its modifier def");
        let list_def = defs
            .get(name)
            .unwrap_or_else(|| panic!("$ref target {name} missing from $defs"));

        let required = list_def
            .get("required")
            .and_then(|r| r.as_array())
            .expect("the list def has a `required` array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("separator")),
            "`separator` must be required for a list entry: {required:?}"
        );
        assert_eq!(
            list_def.pointer("/properties/separator/type").and_then(|v| v.as_str()),
            Some("string"),
            "`separator` must be a plain string — a nullable arm would readmit the omission"
        );
    }

    /// Follow an `Option<T>` field's schema to the `$defs` name it wraps.
    ///
    /// schemars renders `Option<T>` as `anyOf: [{$ref}, {type: null}]`, so the
    /// `$ref` is one level in. Panics with the property name when the shape is
    /// anything else — a silently-`None` walker would make every assertion
    /// downstream of it vacuous.
    fn optional_ref_target(property: &serde_json::Value, label: &str) -> String {
        property
            .pointer("/anyOf/0/$ref")
            .or_else(|| property.get("$ref"))
            .and_then(|reference| reference.as_str())
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .unwrap_or_else(|| panic!("{label} must $ref a definition, got: {property}"))
            .to_string()
    }

    /// C-035: the `config` schema is generated by no test today, so a broken
    /// `ShellConfig` `JsonSchema` compiles clean and passes `task verify`. This
    /// closes that — `website/src/public/schemas/` is gitignored and PR CI
    /// builds `metadata/v1.json` only, so nothing else calls
    /// `schema_for("config")`.
    ///
    /// The asserted branch is `shell.consent.namespaces`, walked link by link,
    /// ending at `ScopeSpec`'s **hand-written** `oneOf` — a derive over the
    /// normalized Rust type would emit `anyOf` and could not express
    /// "one of `include`/`exclude` is required", so an editor bound to the
    /// schema would show no error for a table ocx exits 78 on.
    ///
    /// Red state: `#[schemars(skip)]` on `ShellConsent::namespaces` and the
    /// `namespaces` walk below fails by name; point the field at a derived
    /// schema and the `oneOf` assertion fails instead.
    #[test]
    fn config_schema_publishes_the_shell_consent_namespaces_branch() {
        let schema = schema_for("config").expect("config schema exists");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema parses");

        let shell = optional_ref_target(
            value
                .pointer("/properties/shell")
                .expect("the config root declares `shell`"),
            "shell",
        );
        let consent = optional_ref_target(
            value
                .pointer(&format!("/$defs/{shell}/properties/consent"))
                .expect("[shell] declares `consent`"),
            "shell.consent",
        );
        let namespaces = optional_ref_target(
            value
                .pointer(&format!("/$defs/{consent}/properties/namespaces"))
                .expect("[shell.consent] declares `namespaces`"),
            "shell.consent.namespaces",
        );

        let spec = value
            .pointer(&format!("/$defs/{namespaces}"))
            .unwrap_or_else(|| panic!("$defs.{namespaces} missing"));
        assert!(
            spec.get("anyOf").is_none(),
            "the union must be the hand-written `oneOf`, not a derive's `anyOf`: {spec}"
        );
        let arms = spec
            .get("oneOf")
            .and_then(|arms| arms.as_array())
            .unwrap_or_else(|| panic!("$defs.{namespaces} must publish the string-or-table `oneOf`: {spec}"));
        assert_eq!(arms.len(), 2, "exactly the bare-string and table arms: {spec}");
        assert_eq!(
            arms[0].get("type").and_then(|t| t.as_str()),
            Some("string"),
            "the bare-string shorthand is the arm a derive silently drops"
        );
        assert!(
            arms[1]
                .get("anyOf")
                .and_then(|required| required.as_array())
                .is_some_and(|required| required
                    .iter()
                    .any(|arm| { arm.pointer("/required/0").and_then(|key| key.as_str()) == Some("include") })),
            "the table arm must require `include` or `exclude`; without it `namespaces = {{}}` reads clean in an \
             editor and exits 78 in ocx: {spec}"
        );

        // The constitution deviation is published, not merely enforced in
        // Rust: an editor must red-underline an unknown key in the consent
        // table, which is the whole point of refusing it at parse.
        assert_eq!(
            value.pointer(&format!("/$defs/{consent}/additionalProperties")),
            Some(&serde_json::Value::Bool(false)),
            "[shell.consent] refuses unknown keys, and the schema must say so"
        );

        // Runtime provenance is not config. A `#[serde(skip)]` field leaking
        // into the schema would advertise a key that can never be written.
        let shell_properties = value
            .pointer(&format!("/$defs/{shell}/properties"))
            .and_then(|properties| properties.as_object())
            .expect("[shell] has properties");
        for runtime_only in ["hook_tier", "completions_tier", "consent_strip_reason"] {
            assert!(
                !shell_properties.contains_key(runtime_only),
                "`{runtime_only}` is runtime provenance and must never be published as a config key"
            );
        }
    }
}
