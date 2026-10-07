# Research: Machine-readable CLI output — envelopes, versioning, nullability, enum evolution

## Metadata

**Date:** 2026-10-03
**Domain:** cli
**Triggered by:** OCX interface contract ADR — envelope/versioning/compat conventions
**Expires:** 2027-04-03

Evidence grades: **[F]** fetched from the cited page this session; **[S]** from a search snippet only; **[M]** from author memory, not re-verified (treat as a lead, check before quoting in the ADR).

## Direct Answer

1. **Envelope.** Mature tools do not wrap every command in one `{schema_version, command, data}` envelope. They version the *document* (terraform `format_version`, cargo `version`, pip report `version`, PEP 691 `meta.api-version`) or the *object* (kubernetes `apiVersion`/`kind`). Streams tag each *message* (`type`, `reason`). A uniform wrapper is rare in CLIs; it is common in HTTP APIs (Stripe `object`, k8s `Status`).
2. **Granularity.** Per-document (per command/schema) versions are the norm; one global number appears only where one tool emits one document family (cargo metadata). Everybody uses a "ignore unknown fields, reject unknown major" contract, so additive change costs nothing and breaking change bumps major.
3. **Null vs absent.** No ecosystem is clean. The lowest-ambiguity convention for codegen is: **optional = omitted, never `null`; `null` only where "explicitly empty" carries meaning and then declared nullable in the schema.** Kubernetes (pointer+`omitempty`), proto3 JSON (serializers must not emit null), and Stripe/GitHub (omit or null per field, documented) all lean this way, with exceptions.
4. **Enums.** Every ecosystem's default decoder fails on an unknown enum value (Swift synthesized Codable, Jackson, serde, proto3 JSON string parsing). Survival is opt-in per language. The only universal fix is contractual: declare "new values are additive, consumers must tolerate" (cargo, GitHub, AIP-180) and make generators emit an unknown/other case.
5. **Tagged unions.** Internally tagged with a string-const discriminator is the best cross-language bet: it is what OpenAPI `discriminator`, serde `tag=`, Jackson `@JsonTypeInfo(property)`, and k8s `kind` all encode. Name it `type` (dominant in OpenAPI/serde/Stripe/terraform); avoid `@type` (needs escaping in codegen identifiers) and avoid `kind` if `kind` already means "resource type" elsewhere.

## Findings

### 1. Success/error envelopes

| Tool | Shape | Version signal | Errors |
|---|---|---|---|
| terraform `-json` stream | JSON Lines, each msg `{"@level","@message","type",...}` | first message `type:"version"` with `ui:"1.0"` [F] | on **stdout** as `diagnostic` messages; `@level:"error"` [F] |
| terraform `show -json` | single doc, bare payload | top-level `format_version` ("1.x") [F] | n/a (non-zero exit) |
| cargo metadata | single doc | `"version": 1`; caller passes `--format-version` [F] | stderr text, non-zero exit [M] |
| cargo `--message-format=json` | JSON Lines, tag `reason` | none in message [F] | `compiler-message` lines on stdout [F] |
| kubectl `-o json` / k8s API | every object `{apiVersion, kind,...}`; lists are `kind: List` + `items` [S] | `apiVersion` per object [F] | API returns a `Status` object `{status, message, reason, details}` [F]; kubectl itself prints text to stderr [M] |
| docker inspect | bare array of objects | none [S] | stderr text [M] |
| gh `--json fields` | bare payload; caller picks fields [F] | none; no stability promise documented [F] | docs silent [F]; observed behaviour: stderr text, `--json` only on success [M] |
| pip `--report` | single doc | `"version": "1"`, "tools must check this field"; changes only for incompatible change [F] | n/a |
| PyPI PEP 691 | `{"meta":{"api-version":"1.x"},...}` + content-type `...v1+json` | both [F] | HTTP status |
| pylock.toml (PEP 751) | file | `lock-version`; warn on unknown keys within supported major, error on unsupported major [F] | n/a |
| pulumi `--json` / engine events | bare payload / event stream | none found in docs [S] | plugin noise forced to stderr to keep `--json` clean [S] |
| npm/pnpm `--json` | bare payload | none [M] | JSON error object on stderr in recent npm [M] |
| buf `--error-format=json` | one JSON object per diagnostic line | none [S] | stderr by definition (`--error-format` = "build errors printed to stderr") [S] |
| HTTP APIs (RFC 9457) | `{type,title,status,detail,instance}` + extensions; clients MUST ignore unknown extensions [F] | `type` URI identifies the problem class | body, `application/problem+json` |

Observations:

- **Terraform is the closest analogue to what OCX does** (CLI driven by wrappers; terraform-exec / terraform-json). It mixes two designs: per-document `format_version` for snapshot documents and a leading `version` message for streams. Wrapper libraries pin to Terraform releases: terraform-exec's changelog records tfjson updates per Terraform release and warns that a newer Terraform "may produce unexpected results" with an older library [S]. Lesson: even with `format_version`, consumers still lag; the version field makes failure loud, not absent.
- **Where do errors go?** Split by design: terraform puts diagnostics in the stdout stream (stream is the whole truth); cargo/buf/gh/npm keep stdout = success payload and stderr = errors. A consumer that parses stdout and gets an empty/partial stdout plus exit code != 0 is the common contract. Putting errors in a stdout envelope means a consumer must parse before it knows success, which defeats exit codes; terraform gets away with it because the stream is already message-typed.
- **Runtime version detection:** a field in the payload (terraform, cargo, pip, PEP 691, lock-version, k8s apiVersion), a flag the caller passes (cargo `--format-version`), or the media type (PEP 691). Only payload fields survive being piped to a file, which matters for tools that cache or log output.
- **JSON Lines** (jsonlines.org [F]): UTF-8, `\n`-terminated, each line a complete JSON value, no blank lines, `.jsonl`, media type `application/jsonl` unofficial. Streams cannot carry a document-level version except as a first message (terraform) or a per-line tag (cargo `reason`, terraform `type`). Per-line tagging is the de-facto discriminator for streams.
- **Uniform wrapper `{schema_version, command, data}`:** no surveyed CLI does exactly this. Closest: k8s `{apiVersion, kind}` on every object (version + type of the payload, not the command). For: one place to read version, trivial generic client parsing, room for sibling fields (`warnings`, `error`). Against: adds a level of nesting every consumer must unwrap; `command` is redundant with how the consumer invoked it; one wrapper schema version either changes for all commands at once (coarse) or is a lie; codegen generates a generic `Envelope<T>` per command (generics are weak in Go pre-1.18 style, Java Jackson `TypeReference`, Swift needs `Decodable` generics; OpenAPI generators flatten generics into per-command wrappers anyway).
- **Bare payload per command:** gh, docker, npm. Simplest for `jq`; no version; breakage discovered at runtime. Docker's inspect drift (fields omitted when empty between releases; inconsistent timestamp formats in one document [S]) is the cautionary tale.
- **Payload with version inside:** terraform, cargo, pip. Version is next to the thing it versions, no wrapper tax, but each document type needs the field, and "bare array" outputs (like `docker inspect`) cannot carry it without becoming an object. Note k8s' lists got `kind: List` precisely so a collection has somewhere to live.

### 2. Versioning granularity

- **terraform:** documented as semver-lite on a string: minor bump = additive, "ignore any object properties with unrecognized names"; major = breaking, "reject any input which reports an unsupported major version" [F]. Separate version spaces per document family (`ui` for the stream, `format_version` for plan/state) [F].
- **cargo:** single integer, `1` is "the only possible value"; explicitly *not* a breaking change: adding fields, **adding new values to enum-like fields**, changing opaque representations (e.g., package-ID format) [F]. The version has effectively never moved, i.e. the contract is "additive forever, bump never". Message stream has no version, tagged by `reason` only [F].
- **pip report:** integer-as-string; bumps only on removing mandatory fields or changing semantics/type of existing fields; consumers "must check" [F].
- **PEP 691:** `Major.Minor`; only major in the content type; minor for additive; unknown keys must be ignored [F]. Cleanest written statement of the pattern.
- **pylock:** warn on unknown keys inside a supported major, error on unsupported major [F].
- **GitHub REST:** breaking = removing/renaming fields, changing types, removing enum values, new required params; non-breaking = adding response fields and **adding enum values** [F]. Dated versions (`X-GitHub-Api-Version`) for the breaking class only.
- **Stripe:** monthly releases contain only backward-compatible changes; named major releases hold the breaking ones [S]. Backward compatible includes new response properties, reordered properties, changed opaque string formats, new event types [S]. Statically typed SDKs (Java, Go, .NET) pin the API version at generation time and Stripe advises against overriding it because "response objects may not match the strong types" [F, German page]; this is the direct analogue of OCX's generated SDKs: the SDK version *is* the schema version.
- **k8s:** group/version in `apiVersion` (`v1alpha1 -> v1beta1 -> v1`), per object type, with conversion between versions [F].
- **Pre-1.0 practice:** cargo, terraform's JSON, pip's report and pylock all publish a stable-from-day-one version number (`1`, `"1.0"`) and then do not touch it for years; none ships a `0.x` format version. The pre-1.0 freedom lives in the *product* version, not the format number. Starting at `1` while announcing "breaks allowed pre-1.0 via changelog" is consistent with cargo's behaviour (changes under `1` that are formally compatible).
- **One version vs many:** terraform has several (stream `ui`, document `format_version`); cargo has one because it emits one document. Per-schema versioning costs a field per schema and a compat matrix; single global version couples unrelated commands, so an additive change to one command forces every consumer's version check to see a new number. Codegen angle: SDKs generated from per-command schemas want per-schema `$id`s anyway.

### 3. Optional (omitted) vs present-but-null

What the ecosystems and generators do:

- **Kubernetes:** non-slice/map optional fields are pointers with `omitempty`, so unset is omitted; to distinguish unset from empty collection use a pointer to the slice/map [F]. Unions: all member fields Optional [F].
- **proto3 JSON:** parsers accept `null` for any field as "unset"; serializers "should not emit null"; fields with default value and no presence are omitted [F]. AIP-149/203: `optional` presence label vs `OPTIONAL` behavior are not the same thing [F].
- **Go encoding/json:** `omitempty` omits nil pointers/empty slices but not zero structs; Go 1.24 added `omitzero` because `omitempty` misbehaves for `time.Time` [F]. Go can only tell "absent" from "null" with pointers or custom types; `*T` + `omitempty` collapses null and absent on decode.
- **typify (Rust):** non-required property => `Option<T>` + `#[serde(default)]`; types with natural defaults (`Vec`) get only `#[serde(default)]` to avoid `Option<Vec<T>>`. `anyOf` is "one of the weaker areas" (modelled as structs with optional flattened members) [F]. `oneOf` maps to serde enums [F]. Consequence: a schema that is `oneOf` with `const` tags is the safe input.
- **schemars (Rust schema source):** `Option<T>` becomes `anyOf: [T, {"type":"null"}]` [F]; untagged enums become `anyOf` [F]. So a Rust-authored schema is nullable-by-`anyOf` unless `skip_serializing_if = "Option::is_none"` is paired with schema settings to drop the null arm. This is a known mismatch: serde omits, schema still says nullable. [M for the pairing advice.]
- **Codegen consumers:** quicktype historically collapsed `oneOf` of objects into one class with all fields optional [S] (fix in progress, quicktype PR 3012 [S]); openapi-generator Java has `legacyDiscriminatorBehavior` default true, needing `false` for `oneOf`/`anyOf` discriminator mappings [F]; datamodel-code-generator and json-schema-to-typescript: not verified this session [M] (both support `oneOf` unions; check discriminator handling and `nullable`/`type:[...,"null"]` before the ADR).
- **Swift Codable:** `decodeIfPresent` treats absent and null identically; synthesized Codable only for `Optional`; no way to distinguish absent from null without custom code [M].
- **Jackson:** `null` and absent both map to Java null by default; distinguishing needs `JsonNullable`. `FAIL_ON_UNKNOWN_PROPERTIES` is **enabled by default** [F], so a Jackson client breaks on additive fields unless the generated SDK disables it. This is the single most important per-language default for the "additive is free" promise.
- **What big APIs pick:** Stripe emits `null` for "present but empty" on many resource fields (e.g. expandable/nullable fields) and omits unexpanded ones [M]; GitHub emits `null` pervasively (`"closed_at": null`) [M]; Kubernetes omits. The null-heavy style is a legacy of dynamic-language consumers; typed-language SDKs then need `Option<Option<T>>`-style gymnastics for PATCH, but not for read-only CLI output.
- **Convention that minimises ambiguity for a read-only CLI:** (a) omit optional fields when unset; (b) never emit `null` for "not applicable"; (c) if "known-empty" differs from "unknown", model it as an explicit value (`[]`, `""`, or an enum like `"none"`), not null; (d) arrays and objects always present when the schema says required, even if empty; (e) schema marks omitted-able fields as not in `required` and does **not** add `null` to the type. Every generator above handles (e) best: `Option<T>`/`T?`/`*T`/`Optional`, one meaning.
- **Counter-evidence:** terraform `format_version` doc treats unknown and null values as "rendered as absent or null" [F], i.e. even terraform leaves that door open; consumers must be written tolerant of both regardless of the OCX convention. State this as "consumers MUST accept null wherever the field is optional" while "producers MUST NOT emit it".

### 4. Enum evolution

- **Contractual:** cargo says new enum-like values are not breaking [F]; GitHub: adding enum values non-breaking, removing breaking [F]; AIP-180: response enums "expected to receive new values should document this", and "user code may not handle new values gracefully" [F]; k8s: enumerations are CamelCase strings, with transition support when adding [F]. Stripe lists new event types, not explicitly enum values, as additive [S].
- **Rust:** generated enums should be `#[non_exhaustive]`. Fern's Rust generator (v0.37.0) emits `#[non_exhaustive]` plus a final `__Unknown(serde_json::Value)` `#[serde(untagged)]` variant for discriminated unions, losing `Eq`/`Hash` [F]. For plain string enums, `#[serde(other)]` on a unit variant works only for unit variants and does not retain the unknown string; extending it to capture the tag is an open serde enhancement [S]. Net: forward-compatible Rust needs a custom `Unknown(String)` variant via `#[serde(untagged)]`/custom deserializer, not `other`.
- **Swift:** synthesized Codable throws on unknown case and fails the whole top-level response [S]. Standard fix: enum with an `unknown(String)` case and custom `init(from:)`, or `RawRepresentable` struct wrapper. openapi-generator's Swift has `enumUnknownDefaultCase` on several generators (`unknown_default_open_api` case, default false) [F, Java page].
- **Java Jackson:** unknown enum value throws by default; `READ_UNKNOWN_ENUM_VALUES_AS_NULL` (default off) or `READ_UNKNOWN_ENUM_VALUES_USING_DEFAULT_VALUE` + `@JsonEnumDefaultValue` (default off; without a designated default it still throws) [F].
- **Go:** enums are `string` typed constants: unknown values decode fine, `switch` just falls to default. No generator work needed; most robust. Cost: no compile-time exhaustiveness [M].
- **TypeScript:** string-literal unions are erased at runtime so nothing throws; the type is a lie for unknown values. json-schema-to-typescript emits unions [M]; exhaustive `switch` with `never` check breaks silently at runtime unless a default branch exists. Idiom for open enums: `"a" | "b" | (string & {})`.
- **Python:** `enum.Enum` raises `ValueError` on unknown; pydantic (datamodel-code-generator output) raises `ValidationError` for `Literal`/`Enum` fields. Open enums need `str` fields or a `_missing_` hook [M].
- **protobuf:** proto3/editions use **open** enums: unknown numeric values are stored; proto2 closed enums drop them into unknown fields and reorder repeated values [F]. Conformance is uneven (C#, Go treat all as open; Dart all closed; Java, C++ non-conformant) [F]. JSON parsing: accepts names or ints [F]; unknown-name behaviour not stated in the page fetched. Recommendation in the guide: zero value `*_UNSPECIFIED` [M].
- **Stripe/GitHub SDKs:** Stripe's statically typed SDKs pin the API version at generation (above [F]); they type enums as strings with documented value lists [M]. octokit generates string unions [M].
- **Practical rule that works in all six languages:** (a) the schema types an extensible enum as `string` with `enum` as *documentation/`examples`/`x-` extension* **or** as `anyOf: [{enum:[...]}, {type:"string"}]`; (b) each generated language gets an open form (Rust newtype-or-Unknown, Swift `unknown(String)`, Java `@JsonEnumDefaultValue`, Go string, TS `string & {}`, Python `str`). Closed `enum` in the schema forces generators to emit closed types, which is exactly the Jackson/Swift/serde/pydantic failure mode.
- **Exit-code analogue:** exit codes and error `kind` slugs are enums in practice; cargo's rule is the template.

### 5. Tagged-union encoding

- **Serde representations** [F]: externally `{"Request":{...}}` (default; no discriminator field), internally `{"type":"Request",...}` ("popular in Java libraries"; struct/newtype-of-struct/unit variants only, no tuple variants), adjacently `{"t":..,"c":..}` ("common in Haskell"), untagged (try each).
- **Internally tagged** is what OpenAPI `discriminator` (propertyName + optional mapping) models, what Jackson `@JsonTypeInfo(use=NAME, include=PROPERTY)` models, and what Go/C#/Java wrapper generators key on [F, speakeasy]: TS/Python get native unions, Go/C#/Java get wrapper classes with a discriminator enum; unknown-variant behaviour is *not* documented by Speakeasy [F]. Weak spots: quicktype's historic oneOf flattening [S]; openapi-generator Java needs `legacyDiscriminatorBehavior=false` [F]; Jackson fails unknown subtype ids unless `defaultImpl`/`FAIL_ON_INVALID_SUBTYPE` configured [M].
- **Adjacently tagged** (`{"type":..,"data":..}`) maps to oneOf-of-two-field-objects: expressible everywhere, no collisions between tag and payload field names, handles tuple-like payloads, costs a nesting level. Preferred when payload variants might legitimately carry a field called `type`.
- **Externally tagged** has no discriminator for OpenAPI `discriminator` to point at; generators treat it as untyped `oneOf` with required-property disambiguation, which is the weakest case in typify (`anyOf` weak) and quicktype [F/S]. Avoid for cross-language SDKs.
- **Untagged** relies on structural matching (Jackson `Id.DEDUCTION` needs "structurally distinct" property sets [F]); fragile as variants grow. Avoid.
- **Discriminator name:** `type` dominates: terraform stream `type` [F], serde examples [F], OpenAPI examples, RFC 9457 `type` (URI-valued) [F], Stripe `type` on events/payment methods [M]. `kind` is Kubernetes' (resource type, paired with `apiVersion`) [F]. `reason` is cargo's [F]. `@type` is JSON-LD / protobuf `Any`; the `@` breaks identifier-derived field names in Swift/Go/Java generators (requires explicit rename on every language) and is what Speakeasy had to special-case for `$login`-style keys [F]. `@level`/`@message` in terraform are the same hazard, tolerated only because terraform-json hand-writes its structs.
- **Unknown-variant behaviour per language** is the real differentiator, not the encoding: Fern-Rust shows the workable pattern (`#[non_exhaustive]` + `__Unknown(Value)`) [F]; OCX should specify "consumers MUST preserve and ignore unknown `type` values" at the contract level.

## Trade-offs

| Decision | Option | For | Against |
|---|---|---|---|
| Envelope | Uniform wrapper | one version read; room for `warnings`; generic clients | nesting tax; `command` redundant; one coarse version; generics poorly generated (Go, Java, Swift) |
| | Bare payload | simplest `jq`; matches gh/docker/npm | no version, no place for warnings, arrays cannot carry metadata; docker-style silent drift |
| | Payload + inline version (`schema_version` or `format_version`) | cargo/terraform/pip precedent; per-schema; no wrapper | every root must be an object; field repeated in each schema |
| Errors | stderr text + exit code only | universal, matches cargo/gh | machine consumers get unstructured text |
| | JSON error on stderr, exit code | stdout stays "success payload or empty"; consumers branch on exit code first | two streams to read; interleaving with progress |
| | JSON error envelope on stdout | single parse path (terraform) | consumer cannot trust shape before parsing; breaks "stdout = data" for `jq` pipes |
| Version grain | one global | single check | any change bumps for all; false precision |
| | per schema | accurate; matches `$id`/SDK types | more fields, compat matrix |
| Absent vs null | omit optional | one meaning; best codegen (`Option<T>`) | consumers still must tolerate null (terraform, proto3 JSON) |
| | emit null | stable key set for `jq`; matches GitHub | nullable + optional ambiguity in every generator; schemars `anyOf null` noise |
| Enums | closed in schema | exhaustiveness, nicer types | Jackson/Swift/serde/pydantic hard-fail on new value; makes every addition breaking |
| | open string + documented values | additive forever (cargo/GitHub) | weaker typing, TS `string & {}` idiom |
| Tag | internal `type` | OpenAPI/Jackson/serde/typify align | payload may not have a field named `type`; serde forbids tuple variants |
| | adjacent `type`+`data` | no key collisions; all payload shapes | extra nesting; less native to OpenAPI discriminator |

## Trends

- Per-document inline version plus "ignore unknown, reject unknown major" (terraform 2018-, PEP 691 2022, pylock 2025, installation report) is converging as the Python/HashiCorp norm; cargo's stance ("additive including enum values, never bump") is the Rust norm.
- Agent/tooling consumers raise the stakes: generic "CLI JSON conventions" write-ups now recommend a JSON error object on stderr and semantic exit codes (0 ok, 1 generic, 2 auth, 3 not found, ...) [S, low authority]; RFC 9457 problem details is the standard reusable error shape.
- SDK generators are catching up on forward compatibility: Fern's Rust generator ships non-exhaustive + unknown variants by default (June 2026) [F]; openapi-generator has opt-in `enumUnknownDefaultCase` [F]; quicktype is fixing `oneOf` object unions (2026) [S]; Go added `omitzero` in 1.24 [F]; Jackson still defaults to failing on unknown properties [F].
- Serde is considering capturing unknown tags in `#[serde(other)]` non-unit variants [S], not yet shipped, so hand-rolled `Unknown` is still required.

## Recommendation

Opinionated, for OCX (Rust producer, six generated SDKs, pre-1.0, interfaces announced via commit subject):

1. **No uniform command wrapper.** Each command's stdout is a bare JSON object (never a bare array: wrap collections as `{"items":[...]}`) whose schema has its own `$id`. Every root object carries `"format_version": 1` (integer, terraform/cargo style, one field name everywhere) as its first field, so `jq` and script consumers can version-check without an SDK. Reject `{schema_version, command, data}`: it adds nesting and a redundant `command`, and codegen turns it into per-command generics.
2. **Version policy:** per-schema integer, starts at `1`, **not** `0`. Written contract (copy PEP 691/terraform): consumers ignore unknown fields; additive changes (new fields, new enum values, new union variants) never bump; a bump means removed/renamed field, type or semantics change, or a removed enum value. Pre-1.0 breaks still go through the commit subject, and then bump the integer. Do not bump the global OCX version for a format change; do not use semver strings (nobody uses the minor).
3. **Errors:** stdout carries only success payload (empty on failure); the exit code is the first branch. When `--format json` is set, emit one RFC 9457-shaped object on **stderr** (`type` as a stable slug aligned with `ocx_exit`'s `error.detail`, `title`, `detail`, plus `exit_code`), flagged by the same `format_version`. Reject stdout error envelopes: they force consumers to parse before deciding and break `jq` pipelines. Document the interleaving rule if progress also uses stderr (progress must be off in JSON mode).
4. **Streams (if any):** JSON Lines, per-line `type` discriminator, first line `{"type":"version","format_version":1}` (terraform precedent).
5. **Null policy:** producers omit unset optionals; never emit `null`; empty collections are always present when in `required`; schema does not add `null` to types (set schemars/serde `skip_serializing_if` and strip the `anyOf null` arm in a post-processing step, and gate it with a schema test). Consumers MUST accept `null` wherever optional.
6. **Enums:** declare all output enums *open*. In the schema, use `type: string` with documented values (`examples` or an `x-ocx-known-values` extension), or `anyOf [enum, string]`; every SDK gets an open form. State the rule in the interface ADR: adding a value is non-breaking, removing/renaming is breaking. Keep exit codes and error slugs under the same rule.
7. **Unions:** internally tagged, discriminator named `type`, string `const` in each variant, `oneOf` + OpenAPI-style `discriminator.propertyName` hint. Forbid variant fields named `type`. Struct/map variants only (serde forbids tuple variants internally tagged). Each generated language needs an unknown-variant fallback; specify "consumers MUST ignore unknown `type` values" in the contract. Use adjacent tagging only for a payload that cannot be an object.
8. **Before the ADR lands, run one spike** generating all six SDKs from a single schema with an open enum, a tagged union, and an optional field, and read the output: datamodel-code-generator, json-schema-to-typescript, Swift and Jackson discriminator handling were **not** verified here [M]. Concretely check: Jackson `FAIL_ON_UNKNOWN_PROPERTIES` setting in the generated config; Swift unknown-case handling; typify with `oneOf`+`const`; quicktype `oneOf` flattening.

## Sources

- Terraform machine-readable UI: https://developer.hashicorp.com/terraform/internals/machine-readable-ui
- Terraform JSON output format: https://developer.hashicorp.com/terraform/internals/json-format
- terraform-exec changelog (search): https://github.com/hashicorp/terraform-exec/blob/main/CHANGELOG.md
- cargo metadata: https://doc.rust-lang.org/cargo/commands/cargo-metadata.html
- cargo external tools / JSON messages: https://doc.rust-lang.org/cargo/reference/external-tools.html
- gh formatting: https://cli.github.com/manual/gh_help_formatting
- Kubernetes API concepts: https://kubernetes.io/docs/reference/using-api/api-concepts/
- Kubernetes API conventions: https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md
- PEP 691: https://peps.python.org/pep-0691/
- pylock.toml: https://packaging.python.org/en/latest/specifications/pylock-toml/
- pip installation report: https://pip.pypa.io/en/stable/reference/installation-report/
- Google AIP-180 backwards compatibility: https://google.aip.dev/180
- Google AIP-203 field behavior: https://google.aip.dev/203
- Google AIP-146 generic fields: https://google.aip.dev/146
- GitHub REST API versions (breaking vs non-breaking): https://docs.github.com/en/rest/about-the-rest-api/api-versions
- Stripe API upgrades: https://stripe.com/docs/upgrades (German render fetched; backward-compat list via search snippet)
- RFC 9457 problem details: https://www.rfc-editor.org/rfc/rfc9457.html
- Protobuf enum behavior: https://protobuf.dev/programming-guides/enum/
- Protobuf JSON mapping: https://protobuf.dev/programming-guides/json/
- Serde enum representations: https://serde.rs/enum-representations.html
- Serde variant attributes: https://serde.rs/variant-attrs.html
- Serde tagged-enum `other` discussions (search): https://github.com/serde-rs/serde/issues/2231 , https://github.com/serde-rs/serde/pull/2569
- typify README: https://github.com/oxidecomputer/typify/blob/main/README.md
- schemars docs: https://docs.rs/schemars/latest/schemars/
- openapi-generator Java options: https://openapi-generator.tech/docs/generators/java/
- Speakeasy oneOf SDK handling: https://www.speakeasy.com/docs/sdks/customize/data-model/oneof-schemas.md
- quicktype oneOf fix (search): https://github.com/glideapps/quicktype/pull/3012
- Fern Rust generator, forward-compatible unions: https://buildwithfern.com/learn/sdks/generators/rust/changelog/2026/6/1
- Jackson DeserializationFeature: https://fasterxml.github.io/jackson-databind/javadoc/2.12/com/fasterxml/jackson/databind/DeserializationFeature.html
- Swift Codable unknown-enum discussion (search): https://forums.swift.org/t/codable-synthesis-and-decoding-unavailable-values/64926
- Go 1.24 `omitzero`: https://go.dev/doc/go1.24
- JSON Lines: https://jsonlines.org/
- buf convert / error-format (search): https://buf.build/docs/reference/cli/buf/convert/
- Docker CLI JSON output drift (search): https://git.coopcloud.tech/toolshed/docker-cli/commit/23bd746c43a2d191a0faad16cbc393402c59ade0

Not verified (no usable page fetched): pulumi `--json`/automation API stability, npm/pnpm `--json` error routing, gh `--json` error behaviour, uv JSON output, datamodel-code-generator and json-schema-to-typescript discriminator/nullable handling, Swift Codable details, Stripe/GitHub SDK enum typing. Items marked [M] above fall in this set.
