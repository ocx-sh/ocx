# Research: Interface contract tooling — clap grammar export, env registry, schema lint and diff

## Metadata

**Date:** 2026-10-03
**Domain:** cli
**Triggered by:** OCX interface contract ADR — tooling for grammar export, env enforcement, schema lint/diff
**Expires:** 2027-04-03

Method: web, crates.io and GitHub API research plus local experiments outside the repo (clap 4.6,
schemars 1.2.2, clap_usage 5.0.0, usage-cli 6.12.0, jsoncompat 0.4.2, json-schema-diff 0.1.8,
oasdiff 1.33.0, rustc 1.95). **[measured]** = run here; **[unverified]** = not run.

## Direct Answer

1. **Grammar export.** Walk `clap::Command` yourself into a small OCX-owned model, emit it as
   `usage` KDL (via the `usage-lib` types), and use `usage diff` as the backward-compat gate.
   `usage` is the only option with a real breaking-change differ (`usage diff`, exit 1 on
   breaking, JSON output). But `clap_usage` 5.0.0 silently drops **env bindings**, `requires`
   relations and enum-variant help, so OCX must patch `env` onto the spec itself. clispec.dev and
   OpenCLI are not grammar formats you can adopt today. clap_mangen, clap-markdown and
   clap_complete are renderers, not interchange formats.
2. **Env registry.** A clippy ban works. `disallowed-methods` and `disallowed-macros` are
   resolved by definition, so aliases, re-exports, local and external macro expansions and
   function-pointer references are all caught **[measured]**. Pair it with one registry type
   (uv/ty `EnvVars` pattern) and a generated-doc check. No crate on crates.io covers the whole job.
3. **Schema lint.** Do both: a schemars `Transform` at generation time to normalise shapes, and a
   strict draft 2020-12 meta-schema (`$dynamicRef`) as the fail-closed validator. Spectral is
   viable but adds a Node toolchain for little. A meta-schema ban on `anyOf` and type arrays was
   proven red and green **[measured]**.
4. **Schema diff.** The 2026-09-25 verdict needs a partial update. getsentry/json-schema-diff is
   still not adequate. Two usable options now exist: **oasdiff on schemas wrapped in a stub
   OpenAPI 3.1 document** (mature, direction-aware, handles `oneOf`/`anyOf`) and
   **jsoncompat** (JSON-Schema-native, serializer/deserializer roles, self-declared alpha).
   Neither is a drop-in "mature JSON Schema differ", so the conclusion "none mature enough as a
   standalone JSON Schema differ" stands; the wrapper route is the update.

## Findings

### 1. Exporting the clap grammar as a machine-readable spec

#### What clap exposes (the ceiling for every exporter)

Public getters in clap_builder 4.6.x: `Arg::get_env`, `get_default_values`,
`get_possible_values`, `is_global_set`, `is_hide_set`, `get_value_parser().type_id()`,
`get_num_args`, `get_visible_aliases`; `Command::get_subcommands`, `is_hide_set`,
`get_visible_aliases`. Gaps **[measured by grep of clap_builder 4.6.0]**:

- No `deprecated` concept on `Arg` or `Command` (no `fn deprecated` / `get_deprecated`). OCX's
  deprecated-spelling windows are hidden variants, so "deprecated" must come from OCX's own
  `deprecated.rs` `RENAMED` table, not from clap.
- No readers for `requires`, `requires_if`, `default_value_if`, `default_missing_value`
  (confirmed by usage's own docs). Exporters cannot see these.
- `value_parser().type_id()` Debug-prints the Rust type path (`u32`, `gram::Fmt`,
  `std::path::PathBuf`). That is a refactor-unstable string. Map it to a closed vocabulary
  (integer, string, path, bool, enum) and fail on an unmapped type.

Env bindings need clap's `env` feature to be readable. The workspace `clap` dependency enables
only `derive` and `color`. OCX also reads env at ~200 sites outside clap (`ocx_util::env`,
`ocx_config`), so clap's `env` covers only flag-bound vars either way.

#### Option A: jdx `usage` (KDL spec) with `clap_usage`

| Signal | Evidence |
|---|---|
| Maturity | `usage-cli` 6.12.0 released 2026-09-28; 1.0k stars; 1,500+ commits; pushed 2026-10-03; MIT |
| Rust bridge | `clap_usage` 5.0.0 (2026-08-04), ~134k total downloads, 94k recent |
| Spec versioning | `min_usage_version`; spec has `env`, `hide`, `deprecated`, `global`, `choices`, `default`, `aliases`, `var`, `count`, `negate`, `surface` |
| Differ | `usage diff [-f json] [-b] OLD NEW`: classifies breaking / compatible / metadata, exit 1 on breaking |

Measured with a clap app (global enum flag with `env`, hidden flag, hidden subcommand, visible
alias, `requires`, default, `Vec` positional) through `clap_usage::spec` **[measured]**:

- Captured: subcommands, long/short flags, defaults, `choices` (value enums), `global`, `hide`
  (flag and command), aliases, variadic positionals, version.
- **Dropped: `env` bindings** (`OCX_FORMAT`, `OCX_JOBS` absent from the KDL), `requires`,
  enum-variant help text, and scalar types (`u32` becomes `arg <JOBS>`; usage has no scalar
  type system beyond `choices`).
- `usage-lib` 5.1.0 `SpecFlag.env` and `SpecFlag.deprecated` exist but the clap converter never
  fills them. The docs advertise `spec_with_report()` / `is_lossless()` for flagging losses;
  the published `clap_usage` 5.0.0 exports only `spec` and `generate`, so that API is
  unreleased on crates.io.

`usage diff` on hand-written specs **[measured]**: detected `choice-removed`,
`flag-no-longer-global`, `env-removed`, `default-changed`, `flag-removed`, `cmd-removed`,
`arg-now-required` as breaking, and `unhidden` as metadata. Exit code 1. JSON output stable
(`category`, `code`, `message`, `location`). It does not see types because the spec has none.

Against: single maintainer, fast-moving majors (4.0.0 2026-07-25, 5.0.0 2026-08-04), lossy clap bridge.

#### Option B: clispec.dev

Not a grammar format: six design principles plus a `schema` command emitting behaviour metadata
(`effects`, `cardinality`, `output_kind`, `pagination`, `errors` with exit codes) against
`clispec.dev/schema/v0.3.json`. v0.3 is an explicit candidate (August 2026: "do not ship 0.3
expecting it to keep validating"). No flags or arguments, no differ. Possibly complementary for
exit-code vocabulary.

#### Option C: OpenCLI (two unrelated projects share the name)

- **openclispec.org v1.0.0** (Apache-2.0): commands tree, parameters with `in: flag`, aliases,
  `scope: inherited`, `environment`, JSON Schema types and enums. Per an independent summary: 9 stars,
  two contributors, created July 2025, no reference generator, conformance suite or Rust tooling.
- **Spectre.Console OpenCLI** (dotnet, `--help-dump-opencli` since 0.52.0, 2025-10): draft; lists
  "detecting CLI API changes" as a use case, but no differ found and no Rust tooling.

Richest type model on paper, weakest ecosystem. Not recommended as the contract format.

#### Option D: clap_mangen, clap-markdown, clap_complete

| Crate | Version / date | Measured output |
|---|---|---|
| clap_mangen | 0.3.3, 2026-08-12 | defaults, enum variants with help, aliases; **no env, no hidden items, no global marker** |
| clap-markdown | 0.1.5, 2025-05-02 | same gaps; stale for 17 months |
| clap_complete | 4.6.11, 2026-09-15 | shell scripts only; the `Generator` trait receives `&Command`, the same input as a hand walk |

Lossy, prose-shaped renderers: diffing them is text diffing. Good as docs, wrong as the contract.

#### Option E: walk `clap::Command` and emit custom JSON

A ~15-line walker emitted name, hidden, aliases, global, env, defaults, possible values, required,
parser type and num_args per command and arg **[measured]**: complete with respect to clap's
getters, and OCX controls the schema (types, env, deprecation, since). Cost: OCX also owns the
differ. About 12 rule codes (see above) is small, but it is code to maintain and brushes against
"Don't own non-domain code".

#### Option F: others

docopt-like (parsing DSL, no types or env) and cobra `doc.Gen*Tree` (Go only, no compat tooling) are dismissed.

#### Capture matrix

| Capability | usage via clap_usage | usage via custom walker | OCX JSON walker | clap_mangen / markdown | OpenCLI | clispec |
|---|---|---|---|---|---|---|
| subcommands, args | yes | yes | yes | yes | yes | no |
| value enums | yes (`choices`) | yes | yes | yes | yes | no |
| defaults | yes | yes | yes | yes | yes | no |
| env bindings | **no** | yes (patch `env`) | yes | no | yes | no |
| hidden | yes | yes | yes | no | spec only | no |
| deprecated | no (not in clap) | yes, from `RENAMED` | yes | no | spec only | no |
| global args | yes | yes | yes | no | yes | no |
| scalar types | no | no | yes (mapped) | partial | yes | no |
| backward-compat diff | **`usage diff`** | **`usage diff`** | custom | text diff | none found | none |

### 2. One declared registry for `std::env::var` reads

#### clippy `disallowed-methods` and `disallowed-macros`

Experiment: clippy 1.95 (rustc 1.95.0), `clippy.toml` banning `std::env::var`, `var_os`,
`vars`, `vars_os` and macros `std::option_env`, `std::env` **[measured]**:

| Call shape | Flagged |
|---|---|
| direct `std::env::var(..)` | yes |
| module alias `use std::env as e; e::var_os(..)` | yes |
| re-export alias `pub use std::env::var as getvar; getvar(..)` | yes |
| inside a local `macro_rules!` (flagged at the macro body) | yes |
| inside an **external crate's** `#[macro_export]` macro | yes (flagged at the call site) |
| `std::env::vars()` | yes |
| `option_env!(..)` and `env!(..)` | yes (`disallowed-macros`) |
| path reference without call: `std::env::var::<&str>`, `.map(std::env::var_os::<&str>)` | yes |
| `#[allow(clippy::disallowed_methods)]` on one statement | suppressed |
| `#[expect(clippy::disallowed_methods, reason = "..")]` | suppressed, and no unfulfilled warning |

Resolution is by definition (`DefId`), so spelling does not matter. Not covered: `set_var` /
`remove_var` (add them to the list), `libc::getenv`, `std::process::Command` inheriting the
parent environment, and dependencies reading env internally (clap `env`, `dirs`). `allow-invalid
= true` makes an entry tolerant of a missing path. `clippy.toml` is workspace-wide: there is no
per-path scoping, so the registry module carries the `#![allow]` and tests either use the
registry's seam or an `#[expect]`. `build.rs` is also linted and reads env legitimately
(`crates/ocx_cli/build.rs` is one).

**Bazel (rules_rust `rust_clippy`).** rules_rust documents `build
--@rules_rust//rust/settings:clippy.toml=//:clippy.toml` (file must be named `clippy.toml` or
`.clippy.toml`). OCX's `.bazelrc` already registers `rust_clippy_aspect`, and it sets
`clippy_output_diagnostics=true`, which caps lints at warn; `scripts/lint_ratchet.py` then fails
any key above its baseline. `disallowed_methods` is warn-by-default (style group), so a
baseline of zero for that key makes it a hard gate in the Bazel lane. **[unverified]**: Bazel was
not run here; the first implementation step is a red/green proof on a seeded violation. rules_rust
reads no Cargo `[lints]` table, so keep the policy in `clippy.toml`.

Prior art: uv's `clippy.toml` bans only `dotenvy::var`/`vars`, not `std::env::var` (fetched), so OCX would go further.

#### Typed env declaration crates

| Crate | Version / last release | Fit |
|---|---|---|
| envy | 0.4.2, 2021-01 | serde-from-env into a struct; stale; no docs surface |
| figment | 0.10.19, 2024-05 | layered config providers; heavy; not a registry |
| confique | 0.4.0 (crates.io 2025-10-27; docs.rs page says 2026-08-16) | `#[config(env = "KEY")]` per field, doc comments to config templates; config-file oriented, no CLI integration |
| twelf 0.15.0 (2024-03), envconfig 0.11.1 (2025-12) | | layered config / derive-from-env; no docs surface |
| clap `env` feature | 4.6.7, 2026-09 | per-flag only, shown in `--help`, readable by `get_env`; cannot declare env without a flag |

All are *consumption* libraries: they parse values into config. None bans stray reads, covers
variables OCX **writes** to child processes, or verifies documentation. Adopting one would move
the problem rather than remove it.

#### Prior art that fits: declare names as documented constants and generate the reference

- **astral-sh/uv**: `EnvVars` with doc comments on each const, attribute macros `attr_added_in`,
  `attr_hidden`, `attr_env_var_pattern` (parameterised names like `UV_INDEX_{name}_USERNAME`),
  and `cargo dev generate-all --mode check` regenerating `docs/reference/environment.md` and
  failing CI on a diff. **Gap:** the generator does not validate that every var has docs or a
  version; missing docs render as empty text.
- **astral-sh/ty**: `ty_static::EnvVars` (0.0.16, 2026-10-01), same pattern; docs.rs shows 50%
  documented coverage, which confirms the missing-doc gap.
- OCX already has the seeds: `ocx_util::env::keys` (canonical `OCX_*` names with doc comments)
  and a large vocabulary in `ocx_config`. Consolidating into one registry is a move, not a new
  concept.

#### Verifying every declared var is documented

No off-the-shelf tool found. The workable shape: (1) registry rows carry `name`, `doc`, `since`,
`visibility` (public / internal / test) and a `kind` (read, written to children, both);
(2) a generator renders `website/src/docs/reference/environment.md` from the rows and a check
mode diffs it in `task verify`; (3) a unit test fails when a non-hidden row has an empty doc
(closing uv's gap); (4) the clippy ban covers the other direction (no read outside the
registry). Red/green both provable locally.

### 3. Linting generated JSON Schema for codegen-friendliness

#### What schemars 1.2.2 emits **[measured, draft 2020-12, default settings]**

| Rust | Emitted |
|---|---|
| `Option<String>` | `"type": ["string","null"]` |
| `Option<Struct>` | `"anyOf": [{"$ref": ...}, {"type":"null"}]` — **a second nullable form** |
| `#[serde(untagged)]` enum | `"anyOf": [...]` in `$defs` |
| `#[serde(tag="kind")]` enum | `"oneOf"` of objects, each with `"kind": {"const": "A"}` and `kind` in `required` |
| unit-variant enum | `"type":"string","enum":[..]` |
| struct reuse | `$defs/Name` with `$ref`; two fields of the same type share one def |
| `skip_serializing_if` field, default contract | stays in `required` for a non-Option (`Vec`) — wrong for output |
| `skip_serializing_if` on `Option<T>` | not in `required`, but still allows `null` |
| `HashMap<String,u64>` | `additionalProperties` schema |

So one Rust codebase yields three constructs codegen tools handle unevenly: `anyOf` (untagged and
`Option<$ref>`), `oneOf` with `const` discriminators, and a type-array nullable.

**Output types must use the serialize contract.** `SchemaSettings::draft2020_12().for_serialize()`
removes `skip_serializing_if` fields from `required` **[measured]**; the default (deserialize)
contract does not. This is the largest correctness lever for OCX's `--format json` types, and
it is cheap. `option_add_null_type` no longer exists in 1.x (compile error); nullable shape is
now controlled by transforms (`AddNullable` for OpenAPI 3.0 style) and by custom ones.

#### Generation-time enforcement and normalisation (schemars transforms)

`SchemaSettings::with_transform(t)` runs a `Transform` over every generated subschema
(`fn(&mut Schema)` or a struct implementing `Transform`; `transform_subschemas` recurses;
`RecursiveTransform` wraps a non-recursive one). Per-type `#[schemars(transform = f)]` also
exists. Measured: a transform stamped a marker on every `anyOf` node, including the one inside
a nested `Option<Struct>` **[measured]**.

- **Normalise:** rewrite `anyOf: [X, {type:null}]` to one canonical nullable form. This is a
  small, local transform (the shape is already decided), chosen once for the whole contract.
- **Enforce:** panic or collect an error on `anyOf`, type arrays, or an untagged enum from the
  generator transform, so the generator itself is the first lint.
- Limit: transforms see the schema, not the Rust type, so an intended `untagged` enum looks like an
  accident; ban `#[serde(untagged)]` separately (syn check) if wanted.

#### Validators for the emitted documents

| Option | Evidence | Verdict |
|---|---|---|
| **Strict meta-schema** (draft 2020-12 plus `not anyOf`, `type` not an array, via `$dynamicAnchor: meta`) | Measured with Python `jsonschema` + `jsonschema-specifications` (offline registry): flagged `anyOf` at top level, nested in `properties`, and `type: [..]` inside `$defs`; passed a clean schema **[measured]** | Best fit: standard, declarative, language-neutral, shows both red and green |
| Spectral custom ruleset | Active (6.17.0 on 2026-10-01, 3.2k stars); JSONPath `given: $..anyOf`; built for OpenAPI/AsyncAPI, Node toolchain | Works for any JSON, but a new runtime for a handful of rules |
| Small custom linter (Rust or Python) | ~40 lines walking the JSON | Fine for rules a meta-schema cannot express (e.g. every `$ref` target is shared, no duplicated inline struct) |

Reuse rules ("repeated inline, should be a `$def`") are not meta-schema material: keep `inline_subschemas = false` (the default) or a tiny custom check.

### 4. Breaking-change diffing of JSON Schema (state at 2026-10-03)

Earlier assessment (2026-09-25): none mature enough. Re-check:

| Tool | State | Measured result on the same 2-schema pair (enum value removed, nullable dropped, nested `integer` to `string`) |
|---|---|---|
| getsentry/json-schema-diff 0.1.8 | Last release 2026-01-29 (previous 0.1.7 was 2023-06); 32 stars; README: "best-effort ... obviously breaking changes"; draft-07 partial | Listed every change with `is_breaking`, but is **direction-blind**: treats nullable removal and enum removal as breaking (right for an input schema, wrong for output); no `anyOf` modelling stated |
| atlassian json-schema-diff (npm) 1.0.0 | 2025-11-21; draft-07; set-theoretic added/removed | Not run; GSoC issue calls it incomplete |
| **jsoncompat 0.4.2** (ostrowr) | Rust, MIT, 17 stars, releases 2026-04 to 2026-08, Python + WASM bindings, draft 2020-12 + OpenAPI 3.1; self-declared **alpha**, "can miss incompatible changes or report false positives" | `--role serializer|deserializer|both`; correctly flagged the nested type change for serializers and the enum narrowing for deserializers, following `$ref`. Reported **one finding per run** in my cases **[measured]** |
| **oasdiff 1.33.0** on a JSON Schema wrapped as an OpenAPI 3.1 `200` response | Released 2026-10-01, 1.4k stars, weekly releases, Go | Direction-correct and mature **[measured]**: `response-property-type-changed` (error), `...became-not-nullable` and `...enum-value-removed` (info), `response-property-one-of-added` (error), `...one-of-removed` and `...any-of-removed` (info). Request-side schemas use `requestBody` for the opposite direction |
| GSoC 2026 checker ([community#984](https://github.com/json-schema-org/community/issues/984)); Confluent registry checker | Accepted 2026-01-30, no contributor yet; Confluent is Java and registry-bound | No official tool yet; not usable standalone |
| Several 2026 "json-schema-diff" repos (Julksaa, solaciuz, koriannsq share one identical description; errantsolutions, NickCirv are stdlib-only tools) | Unvetted | Treat as unvetted; the identical text across three accounts is a red flag |

**Verdict.** Confirmed in part: still no mature *standalone* JSON Schema differ with a vendor
behind it. New since 2026-09-25: oasdiff gates OCX's schemas when embedded as response/request
bodies of a stub OpenAPI 3.1 document (OCX owns a ~20-line wrapper that rewrites `$defs` refs to
`components/schemas`; messages read "API GET /x"), and jsoncompat is the native option to track.

## Trade-offs

| Decision | Take | Give up |
|---|---|---|
| `usage` KDL as the grammar contract | A maintained spec and a ready, correct differ | Single-maintainer dependency; lossy clap bridge needs an OCX patch pass; no scalar types |
| Own JSON model for the grammar | Full fidelity (types, env, deprecated, since) | Own the differ (small, but not free) |
| clippy ban plus registry | Compile-time (lint-time) guarantee, covers aliases and macros | Needs `#[allow]` in the registry and tests; Bazel proof still owed |
| oasdiff wrapper | Mature, direction-aware, weekly releases | Wrapper generator, OpenAPI-flavoured messages, Go binary in the toolchain |

## Trends

- `usage` is moving fast (two majors in July and August 2026) and now ships `diff` and `lint`; watch
  for `spec_with_report` reaching crates.io and an `env` fill in the clap bridge.
- CLI specs are drifting toward AI-agent use (clispec, OpenCLI, `usage mcp`); expect draft churn.
- JSON Schema compatibility checking is an acknowledged gap (GSoC 2026, C++ effort aiming at a
  `buf breaking` equivalent); not before 2027.
- Astral's `EnvVars` plus generated reference is the Rust idiom for env docs.

## Recommendation

1. **Grammar.** Build the exporter by walking `clap::Command` (OCX owns the walk), emit `usage`
   KDL through `usage-lib` types, fill `env`, `deprecated` (from `RENAMED`) and a mapped type as
   KDL properties or `x-` metadata, and gate with `usage diff -b` against the last released
   spec. Pin `usage-cli` in `ocx.toml`. Do not use `clap_usage` output unpatched: it silently
   loses env. Defer clispec and OpenCLI. If the `usage` dependency proves unstable, the same walk
   feeds a plain JSON model and a 12-rule differ.
2. **Env.** Add the `clippy.toml` ban (`std::env::{var, var_os, vars, vars_os, set_var,
   remove_var}`, macros `std::env`, `std::option_env`) with the registry module as the one
   `#![allow]`, and a uv-style registry with a generated `environment.md` check plus an
   empty-doc failure. Prove it red and green once under Bazel before relying on it; do not adopt
   envy/figment/confique for this.
3. **Schema lint.** Switch output types to `for_serialize()`, add one normalising transform
   (single nullable form), and validate emitted schemas against a strict meta-schema in the T0
   lint tier. Skip Spectral. Keep a tiny custom check for reuse rules only if wanted.
4. **Schema diff.** Use oasdiff over a wrapper document in `task verify:scoped` for the output
   contracts, and run jsoncompat as a non-blocking second opinion. Revisit in 2027-04 or when
   jsoncompat leaves alpha or the GSoC checker ships.

Owed before the ADR commits: (a) Bazel `rust_clippy` red/green proof, (b) a golden test that an
enum or `env` edit turns `usage diff` red, (c) oasdiff on OCX's real tagged enums.

## Sources

- usage: [spec](https://usage.jdx.dev), [clap integration](https://usage.jdx.dev/spec/integrations/clap),
  [migrating from clap](https://usage.jdx.dev/rust/migrating-from-clap), [usage diff](https://usage.jdx.dev/cli/reference/diff),
  [repo](https://github.com/jdx/usage) (v6.12.0, 2026-09-28), [clap_usage](https://crates.io/crates/clap_usage) (5.0.0)
- [CLI Spec](https://clispec.dev); [OpenCLI](https://openclispec.org), [summary](https://standards.apievangelist.com/store/opencli-specification/),
  [Spectre.Console OpenCLI](https://spectreconsole.net/cli/opencli), [open-cli](https://github.com/spectreconsole/open-cli)
- crates.io API (retrieved 2026-10-03): clap 4.6.7, clap_complete 4.6.11, clap_mangen 0.3.3, clap-markdown 0.1.5,
  schemars 1.2.2, envy, figment, confique, twelf, envconfig, json-schema-diff 0.1.8, jsoncompat 0.4.2
- [clippy configuration](https://doc.rust-lang.org/clippy/lint_configuration.html),
  [rules_rust clippy docs](https://android.googlesource.com/platform/external/bazelbuild-rules_rust/+/4b3492a3/docs/rust_clippy.md)
- uv/ty: [env_vars.rs](https://github.com/astral-sh/uv/blob/main/crates/uv-static/src/env_vars.rs),
  [generator](https://github.com/astral-sh/uv/blob/main/crates/uv-dev/src/generate_env_vars_reference.rs),
  [clippy.toml](https://github.com/astral-sh/uv/blob/main/clippy.toml), [ty_static](https://docs.rs/ty_static/latest/ty_static/struct.EnvVars.html);
  [confique](https://docs.rs/confique/latest/confique/)
- [schemars](https://github.com/gresau/schemars) (Context7 `/gresau/schemars`; 1.2.2 source), [Spectral](https://github.com/stoplightio/spectral) (6.17.0)
- Diff tools: [getsentry](https://github.com/getsentry/json-schema-diff), [atlassian](https://www.npmjs.com/package/json-schema-diff),
  [jsoncompat](https://github.com/ostrowr/jsoncompat), [oasdiff](https://github.com/oasdiff/oasdiff) (v1.33.0),
  [Confluent](https://docs.confluent.io/platform/current/schema-registry/fundamentals/schema-evolution.html)
- Local experiments (scratchpad, not committed): every item marked [measured] above.
