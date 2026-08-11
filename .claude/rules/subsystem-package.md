---
paths:
  - crates/ocx_lib/src/package/**
  - crates/ocx_lib/src/package.rs
---

# Package Subsystem

Package metadata, env resolution, bundling, cascade publishing, version semantics at `crates/ocx_lib/src/package/`.

## Design Rationale

Tagged enum metadata (`Metadata::Bundle`) supports future format versions, no break existing packages. Cascade = publisher convention (tag naming), not registry-enforced — keeps registry layer generic, enables rich version semantics. Env var templates use `${installPath}` substitution so packages declare env needs declaratively. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `metadata.rs` | Root `Metadata` enum (tagged: currently `Bundle` only); re-exports `Entrypoint`, `EntrypointName`, `Entrypoints`, `ValidMetadata`, `validate_for_publish` — the strict publish-time gate `ocx package create`/`push` call (D14) |
| `metadata/bundle.rs` | `Version` enum (single variant `V1`; `serde_repr` rejects unknown on deser); `Bundle` struct (version + strip_components + env + dependencies + entrypoints) |
| `metadata/dependency.rs` | `Dependency` struct (`identifier: oci::PinnedIdentifier` + `visibility` + `name`); `DependencyName` newtype; `Dependencies` validated collection — `MAX_DEPENDENCIES = 256` lives here (the form `ocx package push`'s pre-push gate fans one registry GET per dependency over — SSRF/DoS bound); `DependencyError` variants |
| `metadata/authoring.rs` | `AuthoringMetadata`/`AuthoringBundle` — sidecar (authoring) superset of `Metadata`. The sole delta is that a dependency identifier's digest is optional (`AuthoringDependency`); everything else, including the platform, is byte-identical, so a published `metadata.json` is itself valid authoring input. `to_published()` projects it straight through — no platform field, no per-dependency pin map, nothing else stripped. `AuthoringError` (65). ADR `adr_dependency_manifest_pinning.md`, `adr_platform_model_unification.md` (D5). The platform a bundle targets is not metadata at all — it lives in the OCI image index and, between `create` and `push`/`test`, in the build receipt `crates/ocx_cli/src/build_receipt.rs` writes beside the bundle (a build artifact, no schema, never published; `create` writes it with or without `-m`, recording the declared `--platform` and `--identifier`, and `push`/`test` read it lazily as a silent fallback for a flag they were not given) |
| `metadata/authoring/dependency.rs` | `AuthoringDependency { identifier: oci::Identifier (digest optional), visibility, name }` — no `platforms` map, no `pin_for`; `is_pinned()` / `pinned() -> Option<PinnedIdentifier>` read the identifier's own digest directly, `to_published()` is a straight pass-through once pinned (`AuthoringError::UnpinnedDependency` otherwise); `AuthoringDependencies` validated collection, same `MAX_DEPENDENCIES` cap and duplicate-repository/duplicate-name checks as the published `Dependencies` |
| `dependency_pinning.rs` | `pin_dependencies(metadata, index, declared_platform)` — the `ocx package create` compile step: resolves tag-only deps to platform MANIFEST digests via `Index::fetch_candidates(Resolve)` (never index digests — GC hazard) for the single declared platform (D5 — no bundle-level target set) via `select_best`, then `pin()` collapses the winner to a single digest bare on the identifier — the one pin shape, for every `declared_platform` including `any`; `reject_digest_pins_in_any_target` runs a separate pass over already-pinned deps too (an `any`-target bundle prohibits a pre-existing direct digest pin — create has no registry evidence for one it did not resolve itself); `DependencyPinningError` (NotFound 79 / DataError 65 / transparent index cause → 81) |
| `metadata/entrypoint.rs` | `Entrypoint` value struct — optional `command: Option<EntrypointName>` (dispatch target when ≠ invocable name; absent = name dispatched directly); `args: Vec<String>` field (fixed leading args prepended before user args, `${installPath}` only, `${deps.*}` rejected); `args()` accessor returns `&[String]`; `Entrypoints::dispatch_command(name)` resolves command, consumed by `ocx launcher exec`; `Entrypoints::get(name) -> Option<&Entrypoint>` returns the full entry; `EntrypointName` slug newtype; `Entrypoints` wraps `BTreeMap<EntrypointName, Entrypoint>` with custom `MapAccess` `Deserialize` that rejects duplicate keys (overrides `serde_json` last-wins default); iter yields `(&EntrypointName, &Entrypoint)`; `EntrypointError` variants |
| `metadata/binary.rs` | `BinaryName` newtype (bare, case-preserving, looser grammar than `EntrypointName` — admits `python3.13`, `c++`, `MSBuild`; `TryFrom<String>`/`TryFrom<&str>`, `MAX_LEN = slug::SLUG_MAX_LEN`); `Binaries` — sorted/unique `BTreeSet<BinaryName>` wrapper, derived `Serialize` (bare-string array), custom `Deserialize` (untagged `string \| object` element union → `TryFrom<BTreeSet<BinaryName>>`, case-fold-collision check), manual `impl schemars::JsonSchema` (write-contract-only: plain string array); `BinaryError` (`thiserror`, `#[non_exhaustive]`) — `Empty`, `InvalidCharacter`, `Whitespace`, `LeadingDash`, `LeadingOrTrailingDot`, `TooLong`, `ReservedWindowsDeviceName`, `CaseFoldCollision`. `Bundle.binaries: Option<Binaries>` — `Option`, not the `Entrypoints`/`Dependencies` `#[serde(default)]` + `X::is_empty` pattern: `None` (undeclared) and `Some(empty)` (publisher asserts zero) are deliberately distinct wire states. ADR `adr_declared_binaries_metadata.md`. |
| `metadata/integrations.rs` | `Integrations` newtype (`BTreeMap<String, serde_json::Value>` — namespace → opaque payload, lexicographic iteration); namespace-key grammar (non-empty, ≤128 bytes, no control/bidi/whitespace characters — Unicode-scoped, refused at `ValidMetadata`, never by serde) + size caps (`MAX_INTEGRATION_NAMESPACE_BYTES` 8 KiB, `MAX_INTEGRATIONS_BYTES` 32 KiB, both raise-only — the read path can only ever loosen); `resolve(resolver)` walks every string LEAF through `TemplateResolver` (object keys, numbers, booleans, null pass through verbatim) into `IntegrationEntry { namespace, payload }`. `Bundle.integrations: Integrations` — `#[serde(default)]` + `is_empty` skip, so absent and empty are the same wire state (unlike `binaries`'s deliberate `Option` tri-state). OCX never merges, validates, or interprets a payload's contents — only the container (key grammar, size, its own `${...}` tokens: an unrecognized one is refused, exit 65, the same as every other metadata surface; `$${` is the escape for a literal `${`). Composition/attribution across packages is `composer.rs`'s concern, not this module's — see `subsystem-package-manager.md`. ADR `adr_package_integrations.md`. |
| `bin_scan.rs` | `scan_directory_files(dir)` — the one content-tree directory walk (regular files + followed metadata, missing dir and dangling symlink yield nothing, every other I/O failure propagates), shared with `libc_lint.rs`; each caller applies its own filter over it, never a flag passed into it. `scan_interface_binaries(content_root, metadata, platform)` — the `ocx package create` compile step (sibling of `dependency_pinning.rs`) that scans install-path-rooted (`${installPath}` bare and `${self.installPath}` alias — the two are one referent, not two spellings a scan treats differently), interface-visible `Path` env vars' target directories for executables (Unix exec-bit vs. Windows extension allowlist `BIN_SCAN_WINDOWS_EXTENSIONS`), returning a plain `BTreeSet<BinaryName>`; `verify_declared_binaries` — one-directional diff against a declared `Binaries` claim; `ScanMode` (`Auto`/`Verify`/`Off` — lib-local mirror of `ocx_cli::options::BinScanMode`, duplicated per lib-hosts-substance/CLI-thin convention); `resolve_binaries(content_root, metadata, platform, mode)` — the full fill/verify/pass-through orchestration; `BinScanError` (`ClassifyExitCode` → `DataError` 65) — `UndeclaredBinary`, `DeclaredNotExecutable`, `Binary(#[from] BinaryError)`, `Scan(#[from] crate::Error)`. ADR `adr_declared_binaries_metadata.md` §2. |
| `libc_lint.rs` | `checks_declared_libc(platform)` — the ONE implementation of the lint's scope rule (Linux + `any`; every other concrete target is out of scope). `check_declared_libc` early-returns on it and the CLI gates its `--no-libc-lint` bypass warning on it, so a bypass never claims something went unverified on a platform that was never going to be checked. `check_declared_libc(content_root, metadata, platform)` — the third `ocx package create` compile step (sibling of `bin_scan.rs` / `dependency_pinning.rs`): reads the ELF `PT_INTERP` of every file on an interface `PATH` dir (walked by `bin_scan::scan_directory_files`, the shared walk, with no filter applied — the binaries scan's exec-bit/name-grammar predicates would hide files whose loader still matters). Scope resolution stays this module's own (`resolve_scan_scope`), deliberately NOT `bin_scan`'s: the two ask different questions of the same metadata and refuses a Linux `--platform` whose `os.features` do not cover the libc family the binaries need. Linux targets only (macOS has one libc; OCX defines no `libc.*` tag for the Windows CRTs). Fail-closed: absent ELF magic is a positive "not in scope", but an ELF that will not parse or names an unattributable loader errors rather than passing as "needs nothing". `LibcLintError` (`ClassifyExitCode` → `DataError` 65, the same code the resolve-time mirror image `SelectResult::FeatureMismatch` maps to; `Read` → `IoError` 74). ELF parsing delegates to the `elf` crate, the same reader behind `oci::host_capabilities`. |
| `metadata/env.rs` | `Env` struct (array of Var); `EnvBuilder` |
| `metadata/env/var.rs` | `Var` with flattened modifier (key + Path, Constant or List) |
| `metadata/env/path.rs` | Path modifier: prepended to existing values, `${installPath}` template |
| `metadata/env/constant.rs` | Constant modifier: replaces existing values, `${installPath}` template |
| `metadata/env/list.rs` | List modifier: appended to existing values with earlier occurrences removed, `${installPath}` template; `separator` REQUIRED on the wire (refused at `ValidMetadata`, not by serde, so the message names the var), defaulted to `" "` on `ocx.toml` / `--env`. `separator_is_valid` (non-empty, no `=`) + `is_separator_edged` are the shared predicates each surface folds into its own typed error (65 / 78 / 64) |
| `metadata/env/entry.rs` | `Entry { key, value, kind, separator }`: resolved env-var binding produced by [`EnvResolver`]; `separator` is a plain field with no invariant (`pub` fields, many struct-literal sites) settled at compose time by `env::reconcile_list_separators` |
| `metadata/env/modifier.rs` | `Modifier` enum (Path/Constant/List + read-only `Unknown` fallback) + `ModifierKind` stripped enum (derives `JsonSchema` so schemas `$ref` one vocabulary) |
| `metadata/env/dep_context.rs` | `DependencyContext` enum (`Full(Arc<InstallInfo>)` / `PathOnly`) for `${deps.NAME.*}` interpolation |
| `metadata/env/resolver.rs` | `EnvResolver`: per-var resolver — `resolve(&Var, self_env: &SelfEnvScope<Entry>) → crate::Result<Option<Entry>>` runs every filesystem/shape assertion; `resolve_without_emit_assertions(&Var, self_env: &SelfEnvScope<Entry>) → crate::Result<Option<Entry>>` resolves the identical value with those assertions suppressed (D8 — a value nobody emits must still resolve, so a crossing var can reference a non-crossing earlier one via `${self.env.KEY}`); surface gating (`has_interface()` / `has_private()`) happens upstream at `composer.rs`, which now resolves every declared var and gates only which resolved entries reach `entries`, not whether resolution runs |
| `metadata/slug.rs` | `SLUG_PATTERN` regex + `SLUG_MAX_LEN` constant shared by `DependencyName` and `EntrypointName` validation |
| `metadata/template.rs` | `TemplateResolver` — resolves the four-body token grammar (`${installPath}`/`${self.installPath}` alias, `${self.env.KEY}`, `${deps.NAME.installPath}`, each optional `:native`/`:posix`) via the `scanner`/`render` submodules below; `Usage` enum (`Environment`/`EntryPointArgs`) + `AllowedTokens { deps: bool, self_env: bool }` capability gate (scan-then-gate, ADR `adr_interpolation_token_grammar.md` D9); `first_disallowed_token` helper identifies the first token a `Usage` does not permit; `TemplateError` variants — `UnknownToken { token, hint: UnknownTokenHint }`, `UnknownField`, `UnknownModifier`, `UndefinedSelfEnvRef`, `AmbiguousSelfEnvRef`, `DisallowedToken`, etc. — carry the D13 three-branch diagnostic (`UnknownTokenHint::{SuggestedRoot, Escape, SupportedBodies}`) |
| `metadata/template/scanner.rs` | `scan()` — the one recogniser for `${…}` tokens: single-pass, left-to-right, first-match-wins (R1 escape `$${` → literal `${`, R2 token, R3 literal fallback); `Segment`/`Token`/`TokenShape` types; the closed four-body set, each fully validated by the time it exists |
| `metadata/template/render.rs` | `RenderModifier` (`Native`/`Posix`) + `Host` (`Windows`/`Unix`, `Host::current()`); pure `render(value, modifier, host) -> Cow<'_, str>` — the `:native`/`:posix` rendering axis, distinct from the wire `type` axis (`env::modifier::Modifier`) |
| `metadata/validation.rs` | Two-layer split (D14): `ValidMetadata::try_from` runs on every ingress path and only asserts structural readability (`validate_env_modifier_types`, `validate_env_list_entries`); `validate_for_publish(metadata)` is the strict gate `ocx package create`/`push` call on top of it — calls `ValidMetadata::try_from` then `validate_env_tokens` (every `${deps.NAME.installPath}` resolves to a declared, non-ambiguous dep) and `validate_entrypoint_args` (refuses token classes `Usage::EntryPointArgs` does not permit) |
| `metadata/visibility.rs` | `Visibility` two-axis struct (`private` + `interface` booleans); four constants `SEALED`/`PRIVATE`/`INTERFACE`/`PUBLIC`; `through_edge`, `merge` algebra; `has_interface()` / `has_private()` accessors; `deserialize_entry_visibility` + `entry_visibility_schema` for `Var.visibility` field restriction |
| `bundle.rs` | `BundleBuilder`: tar archive creation with configurable compression |
| `cascade.rs` | Cascade algebra: `decompose()`, `cascade()`, `resolve_cascade_tags()`, `push_with_cascade()` |
| `cascade/graph.rs` | `ocx package cascade check`/`repair` pure fold + diff core — folds published versions into expected alias state, diffs against observed tags, plans registry rewrites; no I/O |
| `cascade/gather.rs` | Concurrent, bounded registry tag fetch (plus, for a logical identifier, the live index root) feeding the fold above |
| `cascade/apply.rs` | Batched, concurrent index PUTs applying a computed repair plan to the registry |
| `tag.rs` | `Tag` enum: Latest, Internal(InternalTag), Version, Canonical, Other |
| `version.rs` | `Version` struct: semver-inspired with build + prerelease, rolling tag support |
| `install_info.rs` | `InstallInfo`: identifier + metadata + content path |
| `description.rs` | Package description metadata (title, description, keywords, README, logo) |

## Metadata Schema

Rust types = **single source of truth**. JSON Schema generated by `ocx_schema` crate.

```
Metadata (tagged enum: "type" = "bundle")
  └─ Bundle { version: V1, strip_components: Option<u8>, env: Env, dependencies: Dependencies, entrypoints: Entrypoints, binaries: Option<Binaries>, integrations: Integrations }
       └─ Env { variables: Vec<Var> }
            └─ Var { key: String, visibility: Visibility, modifier: Modifier (flattened) }
                 ├─ Path { required: bool, value: String }
                 ├─ Constant { value: String }
                 └─ List { separator: Option<String>, value: String }
```

`binaries` is `Option<Binaries>` (not `Entrypoints`'s `#[serde(default)]` + `is_empty` skip)
because `None` (undeclared) and `Some(empty)` (publisher asserts zero interface executables)
carry different meaning on the wire. See `metadata/binary.rs` in the Module Map above.

`integrations` follows the `Entrypoints`/`Dependencies` shape instead — `#[serde(default)]` +
`Integrations::is_empty` skip — so absent and empty are the same wire state, deliberately not
a third tri-state field. Namespace values are `serde_json::Value`: any JSON, uninterpreted. See
`metadata/integrations.rs` in the Module Map above.

`Visibility` is a struct of two named booleans — `private` (self-axis) and `interface`
(consumer-axis) — with associated constants `Visibility::SEALED` (`{false, false}`),
`PRIVATE`, `INTERFACE`, `PUBLIC` (`{true, true}`). Custom serde keeps the wire format
byte-identical with the four named strings.

Two accessors drive surface emission:
- `has_interface()` → `true` for `PUBLIC` and `INTERFACE`; used by the composer to gate TC
  entries and env-var entries for the interface surface (`--self` off, default exec).
- `has_private()` → `true` for `PUBLIC` and `PRIVATE`; used for the private surface (`--self`).

Inductive TC composition uses `through_edge(child_eff: Visibility)`: if `child_eff.interface`
is false, result is `SEALED`; otherwise the edge passes through unchanged. Diamond dedup uses
`merge` (OR per axis). No `intersects` method exists.

`Var.visibility` uses `Visibility` directly but restricts the valid wire values to
`["private", "public", "interface"]` via `#[serde(deserialize_with = "deserialize_entry_visibility")]`
and `#[schemars(schema_with = "entry_visibility_schema")]`. `"sealed"` is rejected at parse
(dead config — ADR Tension 4). Default is `Visibility::PRIVATE`.
- Interface surface (`--self` off): emits vars where `var.visibility.has_interface()` is true.
- Private surface (`--self` on): emits vars where `var.visibility.has_private()` is true.

The canonical — and only — runtime surface build is
`composer::compose(roots, store, self_view)` in `package_manager/composer.rs`, which iterates
each root's pre-built TC flat, gates by surface, and emits only a dep's interface-tagged vars
when that dep crosses an edge. Entries reach the process env through
`env::Env::apply_entries`; there is deliberately no second, ungated fold path on `Env` itself.

Every `Serialize`/`Deserialize` struct needs `#[derive(schemars::JsonSchema)]`.

## Environment Variable Types

| Type | Behavior | Template | Example |
|------|----------|----------|---------|
| `path` | Prepended to existing value (like PATH) | `${installPath}` | `PATH=${installPath}/bin` |
| `constant` | Replaces existing value | `${installPath}` | `JAVA_HOME=${installPath}` |
| `list` | Appended to existing value, joined by `separator`, earlier occurrences of the same contribution removed | `${installPath}` | `GODEBUG=gctrace=1` with `separator: ","` |

The `list` fold is pinned by `adr_env_modifier_types.md` D1 and implemented once in
`utility::list::append_unique` (wrap in separator → replace every `sep+value+sep` with `sep`
to a fixpoint → strip wrapper → append). Every shell snippet must implement the same
algorithm, or the in-process env and the exported text disagree on separator-bearing values.

Resolution flow: `EnvResolver::new(content_path, &dep_contexts).resolve(var, self_env) → Option<Entry>`
resolves a single `Var` to a concrete `Entry { key, value, kind, separator }`. For a `list`
var it re-checks the separator edge on the *resolved* value — a parse gate only ever sees the
authored template. The composer resolves **every** declared var in declaration order,
crossing or not (D8, resolve-then-gate) — a crossing var through `resolve`, a non-crossing
one through `resolve_without_emit_assertions` (identical value, filesystem/shape assertions
suppressed) — so that a crossing var can reference an earlier non-crossing one via
`${self.env.KEY}`. Surface gating (the var's `Visibility` against `has_interface()` /
`has_private()`) decides only whether the resolved entry is pushed onto the emitted surface,
not whether resolution runs. Surface gating is the composer's responsibility, not the
resolver's.

## BundleBuilder

```rust
BundleBuilder::from_path(path)
    .with_compression(CompressionOptions)  // inferred from extension
    .create(output_path)                   // async, atomic (temp + rename)
```

If source = directory: adds all files to archive root (no top-level dir). `BundleBuilder` itself emits no progress; the caller wraps `create()` in a `ProgressManager` spinner (e.g. `package create`). ADR: `adr_progress_architecture.md`.

## Cascade Logic

Cascade = **publisher convention** (not registry-enforced). `--cascade` automates derived tag updates.

- `cascade(version, others)` → returns versions to cascade to + `latest` eligibility
- Pre-releases without build: no cascade, never eligible for `latest`
- Pre-releases with build: cascade only to parent pre-release
- Regular versions: cascade up to major, eligible for `latest` if no blockers above
- `resolve_cascade_tags()`: platform-aware; checks each level's blockers for platform membership
- `push_with_cascade()`: pushes primary tag, then merges platform into cascade tags sequentially

## Version & Tag Parsing

`Version` struct: major (required), optional minor, patch, prerelease, build.

- `is_rolling()` — true if no build tag (all rolling: major, minor, patch, prerelease)
- `parent()` — version without innermost component (build→prerelease→patch→minor→major)
- Ordering: major.minor.patch by component; prerelease < release; build sorts lexicographically
- Build separator: `+` parses but normalizes to `_` in output (OCI forbids `+`)

`Tag` enum: `Latest`, `Internal(InternalTag)`, `Version(Version)`, `Canonical(String)`, `Other(String)`.

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`. Full `task verify` = final gate before commit.