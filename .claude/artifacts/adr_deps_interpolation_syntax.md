# ADR: `${deps.NAME.installPath}` Interpolation in Env Metadata

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** Michael Herwig
**Issue:** #32
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** data, api
**Supersedes:** N/A
**Superseded By:** N/A

---

## Context

OCX package metadata declares environment variables that are exported when a package is installed or executed. The current system supports a single interpolation token, `${installPath}`, which is replaced with the package's own content directory at resolution time. This is sufficient for self-referential env declarations such as `PATH=${installPath}/bin`.

Issue #32 introduces cross-dependency path references: a package must be able to reference a declared dependency's install path in its own env declarations. The motivating use case is sysroots and tool-chain integration — a `cmake` bundle that needs to point `CMAKE_PREFIX_PATH` at an installed `gcc` sysroot, or a JVM-based tool that must set `JAVA_HOME` to a co-installed Java runtime.

The new syntax is `${deps.NAME.installPath}`. `NAME` is a short identifier, not the full OCI reference, because the full reference contains characters that are invalid inside a `${...}` token and is impractical to type.

This is a **One-Way Door Medium** decision. Once packages in the public OCX registry use the `alias` field or the `${deps.NAME.*}` syntax, the format must be maintained indefinitely. Migration of deployed packages is expensive — publishers would need to re-push. The schema change must therefore be correct on first shipping.

---

## Decision Drivers

- **Backward compatibility**: Packages without `alias` or `${deps.*}` tokens must continue to work without change.
- **Detectability at author time**: Invalid interpolation names should fail at `package create` (earliest actionable gate), not at consumer install time.
- **Collision safety**: Two dependencies from different registries can have the same repository basename. The system must detect and reject this ambiguity.
- **Minimal schema surface**: OCX's project memory (`project_breaking_compat_next_version.md`) notes no optional fields with fallback paths — but `alias` is additive and opt-in, which is distinct from an optional field that patches over a schema gap.
- **Precedent alignment**: Cargo `links` field is the strongest prior art. Explicit alias is the recommended pattern.

---

## Industry Context & Research

**Research artifact:** [`.claude/artifacts/research_env_interpolation_patterns.md`](./research_env_interpolation_patterns.md)

**Trending approaches:** pkg-config `${prefix}`, Cargo `DEP_NAME_KEY`, Nix derivation interpolation, mise Tera templates. All successful systems use a short, author-declared key rather than a derived one.

**Key insight:** The interpolation key is always an explicit declaration by the package author (Cargo `links = "openssl"`), not auto-derived from the package name. Eager validation at authoring time (Nix model) is strongly preferred over lazy runtime discovery. Start with `installPath` only — `${deps.python.installPath}/bin/python3` covers the primary use cases.

---

## Considered Options

### Decision 1: Name Resolution Strategy

#### Option A: Repository Basename (automatic, no schema change)

`NAME` is always derived from the final component of the repository path. `ocx.sh/toolchains/gcc:13` → key `gcc`. No `alias` field needed.

| Pros | Cons |
|------|------|
| Zero schema change | Basename collision is silent until `package create` tries to build the map — error is late |
| Simple mental model | Two deps from different registries or orgs sharing a basename have no resolution path |
| Matches mise/Homebrew naming conventions | Ambiguity is unresolvable without a schema change later — forces a Two-Way Door back to an alias |

#### Option B: Explicit `alias` Field with Basename Fallback (recommended)

Add `alias: Option<String>` to `Dependency`. When `alias` is `Some`, it is the interpolation key. When `None`, the repository basename is the fallback key. At `package create`, if two deps share the same resolved key (whether from explicit alias or basename), error with a clear collision message requiring explicit aliases to disambiguate.

| Pros | Cons |
|------|------|
| Collision always resolvable — author adds `alias` | Minor schema change (one optional field) |
| Strongest precedent (Cargo `links` field) | Basename fallback means collision is a deferred error, not a static one |
| Author intent is explicit in metadata | Adds cognitive burden when aliases are required |
| Additive — backward-compatible, no migration needed | |
| One-Way Door closed correctly — no forced schema change later | |

#### Option C: Full Explicit Key Required (no fallback)

`alias` is required on every dependency. No basename fallback.

| Pros | Cons |
|------|------|
| No ambiguity at any point | Breaking change for packages without `alias` once the feature is used |
| Maximally explicit | Most publishers have single-registry deps with unique basenames — forced verbosity |

**Decision 1 outcome**: Option B. Basename fallback covers the common case (single registry, unique basenames). Explicit `alias` resolves every collision without an emergency schema revision. Matches the research recommendation and Cargo precedent.

---

### Decision 2: Validation Gates

#### Option A: Validate Only at Resolve Time (lazy)

`${deps.NAME.*}` tokens in env values are resolved when `export_env` runs at install/exec time. Unknown NAME → error at that point.

| Pros | Cons |
|------|------|
| No new logic in `package create` | Publisher does not learn about the error until a consumer installs |
| Simpler initial implementation | Error is far from the authoring action — poor UX for publishers |

#### Option B: Validate at `package create` + Resolve Time (dual-gate, recommended)

Gate 1 (`package create`): parse env values for `${deps.*}` tokens; cross-reference against declared deps. Error for unknown NAME or basename collision.

Gate 2 (resolve time in `Accumulator::resolve_var`): when the dep is in declared deps but the symlink is missing from the store → error with actionable message.

| Pros | Cons |
|------|------|
| Publisher learns at authoring time (Nix model) | `package create` must parse env tokens — new logic required |
| Two-layer defense: schema error vs runtime missing | Resolve-time error can still occur (dep not installed, corrupted store) |
| Consistent with existing OCX cycle detection at create time | |

**Decision 2 outcome**: Option B. OCX already validates dep graph cycles at `package create`. Adding env token validation follows the same pattern.

---

### Decision 3: Scope of Interpolatable Properties

#### Option A: `installPath` Only, Hardcoded String Replacement

The only property is `installPath`. Parser does a literal `value.replace("${deps.NAME.installPath}", ...)`. Adding `version` later requires a new replacement pass, a new map parameter through the call chain, and updates to every signature.

| Pros | Cons |
|------|------|
| Simplest to implement | Future property addition is a refactor, not an extension |
| Minimal surface area | Every property needs its own parameter map — data flow balloons |
|  | Violates open/closed principle |

#### Option B: Extensible via `DepView` Struct (recommended)

Today: only `installPath`. But the resolver carries a `DepView` struct per dep that bundles all queryable properties; the token parser dispatches on the property name. Adding a future property (`version`, `binPath`) means adding a field to `DepView` and a match arm to the dispatcher — not a signature change.

```rust
pub struct DepView {
    pub identifier: PinnedIdentifier,   // carries version, registry, repository
    pub install_path: PathBuf,
    // Future: pub bin_path: Option<PathBuf>, ...
}

impl DepView {
    pub fn install_path(&self) -> &Path { &self.install_path }
    pub fn version(&self) -> &Version { self.identifier.version() }  // derived, future
}
```

Token parser dispatches:
```rust
match property {
    "installPath" => Ok(dep_view.install_path()),
    // future: "version" => Ok(&dep_view.version().to_string()),
    _ => Err(UnknownDepInterpolationProperty { name, property }),
}
```

| Pros | Cons |
|------|------|
| Adding future properties is additive — no signature churn | One additional domain type (`DepView`) |
| Data flow is structured around the domain concept | |
| OCI domain types (`PinnedIdentifier`) stay intact, not flattened to `PathBuf` | |
| Enables future properties derivable from identifier (version, registry, repository) | |

**Decision 3 outcome**: Option B. Even though only `installPath` is exposed today, the extensibility seam (`DepView` struct + property dispatch) costs almost nothing to add now and makes every future property an additive change. Option A's "simpler" claim breaks the moment a second property is needed — and the user feedback explicitly requests this shape.

---

### Decision 4: Scoping — Direct Deps vs Transitive Closure

#### Option A: Direct Deps Only (recommended)

`${deps.NAME.installPath}` can only reference dependencies DIRECTLY declared in the package's metadata. Transitive deps of deps are NOT queryable. If a publisher wants to reference a transitive dep's path, they must elevate it to a direct dep.

| Pros | Cons |
|------|------|
| Explicit dependency surface — no action-at-a-distance | Publishers must explicitly declare deps they reference |
| Consistent with visibility model (env propagation follows declared deps) | |
| Changes to a transitive dep do not silently change the consumer's semantics | |
| Mirrors Cargo `links` (only immediate dependencies get `DEP_NAME_KEY`) | |

#### Option B: Transitive Closure Queryable

Any dep in `ResolvedPackage.dependencies` (transitive closure) is queryable via `${deps.*}`.

| Pros | Cons |
|------|------|
| More ergonomic for complex dep graphs | Action-at-a-distance — consumer sees deps-of-deps without declaring them |
|  | Collision detection must span the transitive closure (nondeterministic per install) |
|  | Breaks the visibility model — `sealed` deps of deps would leak |

**Decision 4 outcome**: Option A. The dep map for each package's env resolution is built from THAT package's own directly declared `Dependencies`, not from `ResolvedPackage.dependencies`. This keeps scoping explicit and matches the existing visibility-driven env propagation model. `package create` validation rejects tokens that reference non-direct deps at author time.

---

## Decision Outcome

**Chosen Options:** 1→B, 2→B, 3→B, 4→A

**Rationale:** Explicit `alias` with basename fallback (1-B) is the One-Way Door decision — it closes correctly without requiring a forced schema revision later. Dual-gate validation (2-B) matches OCX's existing early-error philosophy and mirrors Nix's eager model. `DepView` struct with property dispatch (3-B) keeps the initial surface minimal while making future properties additive. Direct-deps-only scoping (4-A) keeps the dependency surface explicit and consistent with the existing visibility model.

### Consequences

**Positive:**
- Publishers can declare cross-dependency env vars without shell script workarounds.
- Basename collision is detected at author time, not consumer install time.
- Schema is additive and backward-compatible — existing packages continue to work.
- The alias field establishes a stable identifier namespace that can be referenced from other metadata fields in future.

**Negative:**
- `package create` gains a new validation pass over env token strings.
- `Accumulator` signature changes — callers (`Exporter`, `export_env`) must supply a `HashMap<String, DepView>`.
- `Dependency` struct grows one field — JSON Schema and docs must be regenerated and updated.
- One new domain type (`DepView`) is introduced; modest additional surface area justified by the extensibility requirement.
- Packages that use `${deps.NAME.installPath}` tokens will silently produce literal string env var values on OCX clients prior to this release. The `alias` field itself is backward-compatible (older clients ignore unknown fields), but the token substitution is silently wrong on older clients rather than failing loudly. Mitigation: document minimum OCX version in the publishing guide; a future metadata `min_ocx_version` field is a separate consideration.

**Risks:**
- Basename fallback means collision errors surface only when a package has two deps with the same repository leaf. Test coverage must include this case explicitly. Mitigation: add a `Dependencies::interpolation_key_map()` method that builds the key map and returns `Err` on collision — call this in both `package create` validation and `Accumulator` construction.

---

## Technical Details

### Schema Change: `Dependency` Struct

Add `alias: Option<String>` to `crates/ocx_lib/src/package/metadata/dependency.rs`:

```
Dependency {
    identifier: PinnedIdentifier,      // existing
    visibility: Visibility,            // existing, serde(default)
    alias: Option<String>,             // NEW, serde(skip_serializing_if = "Option::is_none")
}
```

Serde representation:
- Field name: `alias` (snake_case, matches existing field naming)
- Serialization: `#[serde(skip_serializing_if = "Option::is_none")]` — absent from JSON when `None`, so existing packages without the field remain valid
- Deserialization: missing field → `None` (same pattern as `visibility` with `serde(default)`)
- JSON Schema: `#[derive(schemars::JsonSchema)]` on the updated struct generates `alias` as an optional string property

Example serialized form:

```json
{
  "identifier": "ocx.sh/toolchains/gcc:13@sha256:...",
  "visibility": "sealed",
  "alias": "gcc"
}
```

```json
{
  "identifier": "ocx.sh/toolchains/python:3.12@sha256:...",
  "visibility": "sealed"
}
```

### Name Resolution: Key Map Construction

A free function (not a method on `Dependency`) constructs the interpolation key map from a `Dependencies` list:

```
fn build_dep_key_map(deps: &Dependencies) -> Result<HashMap<String, usize>, DepKeyCollisionError>
```

`DepKeyCollisionError` carries OCI domain types, not flattened strings, so the error preserves identity information for downstream formatting and for the three-layer error chain:

```rust
#[derive(Debug, thiserror::Error)]
pub struct DepKeyCollisionError {
    pub key: String,
    pub first_identifier: PinnedIdentifier,
    pub second_identifier: PinnedIdentifier,
}
```

Rules:
1. For each dep in declaration order: key = `dep.alias.as_deref().unwrap_or(repository_basename(dep.identifier.repository()))`
2. `repository_basename` extracts the final `/`-delimited segment: `"toolchains/gcc"` → `"gcc"`, `"gcc"` → `"gcc"`
3. If two deps map to the same key: return `Err(DepKeyCollisionError { key, first_identifier, second_identifier })` — both identifiers preserve full `PinnedIdentifier` fidelity (registry, repository, version, digest)
4. Return `HashMap<String, usize>` mapping key → index into the deps vec

This function is called in two places: `package create` validation and `Accumulator::new` (indirectly via the dep map that the caller threads in).

**Important distinction from existing `Dependencies` uniqueness enforcement:** `Dependencies::new()` enforces uniqueness on the `(registry, repository)` composite key — a cross-registry basename collision (e.g., `ocx.sh/gcc` and `ghcr.io/gcc`) passes that check and is a valid `Dependencies` list. `interpolation_key_map()` is a separate, orthogonal check on the derived short-name namespace. The two uniqueness invariants guard different namespaces.

### Dependency View (Extensibility Seam)

`DepView` is the single domain object passed through the env resolution chain to represent a queryable direct dep. It carries OCI domain types, not flattened primitives, so future interpolation properties are additive field-and-arm changes rather than signature rewrites.

```rust
pub struct DepView {
    pub identifier: PinnedIdentifier,   // registry, repository, version, digest
    pub install_path: PathBuf,
    // Future: pub bin_path: Option<PathBuf>, ...
}

impl DepView {
    pub fn install_path(&self) -> &Path { &self.install_path }
    // Future: pub fn version(&self) -> &Version { self.identifier.version() }
}
```

The token parser dispatches on the property segment, so adding a second property is a match arm, not a signature change:

```rust
match property {
    "installPath" => Ok(dep_view.install_path()),
    // future: "version" => Ok(dep_view.version().to_string().as_str()),
    _ => Err(UnknownDepInterpolationProperty { name, property }),
}
```

### Dependency Visibility Scoping: Direct Deps Only

`${deps.NAME.*}` can only reference deps DIRECTLY declared in the referring package's own `metadata.dependencies`. Transitive deps (`ResolvedPackage.dependencies` closure) are NOT queryable.

Consequences:
1. Each package's env resolution uses a dep map built from THAT package's own `Dependencies` list. The root package's map and a transitive dep's map are independent.
2. A publisher that wants a transitive dep's install path must elevate it to a direct dependency in their own metadata.
3. Both gates enforce the scope: `package create` rejects tokens for NAMEs not in the declaring package's direct deps; at resolve time, `dep_views` is built from direct deps only, so a token that somehow escaped create-time validation cannot reach a transitive dep's path by accident.

This matches the existing OCX visibility model — `sealed` deps of deps do not leak through env propagation — and mirrors Cargo's `links` scoping (only immediate dependencies get `DEP_NAME_KEY`).

### Accumulator Extension

Current signature at `crates/ocx_lib/src/package/metadata/env/accumulator.rs:14`:
```rust
pub fn new(install_path: impl AsRef<std::path::Path>, env: &'a mut env::Env) -> Self
```

New signature:
```rust
pub fn new(
    install_path: impl AsRef<std::path::Path>,
    dep_views: HashMap<String, DepView>,
    env: &'a mut env::Env,
) -> Self
```

`dep_views` maps interpolation key → `DepView` for each directly declared dependency. An empty map is valid (package has no declared deps or caller does not need dep interpolation). `Exporter::add` also constructs an `Accumulator` — it must receive and forward the map.

`resolve_var` extension:

After the existing `${installPath}` substitution, scan the value string for `${deps.NAME.PROPERTY}` tokens:

```
regex-free approach: find "${deps." prefix, extract NAME (up to "."), PROPERTY (up to "}"), then dispatch
```

For each match:
- Look up `NAME` in `dep_views` → returns `&DepView` or `Err(UnknownDepInterpolationName { name: NAME })`
- Dispatch on `PROPERTY`:
  - `"installPath"` → replace token with `dep_view.install_path()`
  - unrecognized → `Err(UnknownDepInterpolationProperty { name: NAME, property: PROPERTY })`

Multiple tokens in the same value string are replaced left-to-right. The two substitutions (`${installPath}` and `${deps.*}`) are independent — order does not matter because `${installPath}` cannot contain `deps.` and vice versa.

### Call Site Changes

**`export_env` in `tasks/common.rs`**: receives a `dep_views: HashMap<String, DepView>` parameter. The map is built from the package's *directly declared* dependencies using `PackageStore::content()` for each dep identifier, paired with the dep's `PinnedIdentifier` inside a `DepView`. Callers (`resolve_env`, shell profile commands) pass the map.

**`resolve_env` in `tasks/resolve.rs`**: builds each package's dep map from that package's own `metadata.dependencies()` — never from the transitive closure. For each iteration (root package, and each transitive dep that gets its own `export_env` call), iterate that package's direct `Dependencies`, resolve each to its content path via `objects.content()`, and wrap `(identifier, install_path)` into a `DepView`. Pass to `export_env`.

**`Exporter::new` in `metadata/env/exporter.rs`**: gains a `dep_views` parameter forwarded to `Accumulator::new`. Callers that do not need dep interpolation pass `HashMap::new()`.

### `package create` Validation

The `PackageCreate::execute` method currently copies the metadata file without inspecting it. After the schema change, add a validation step when `--metadata` is supplied:

1. Deserialize the metadata file.
2. Call `build_dep_key_map(metadata.dependencies())` — surface collision errors immediately.
3. For each env var value, scan for `${deps.NAME.*}` tokens and verify NAME exists in the key map.
4. Report errors as structured `anyhow::bail!` messages before creating the bundle.

The validation pass does not require resolving content paths — it only checks that token names match declared dep keys. Content-path resolution happens at install time.

### Error Taxonomy

| Error | Kind | Error message | When |
|-------|------|---------------|------|
| Basename collision | Schema | `dep interpolation key "{key}" is ambiguous: both "{id1}" and "{id2}" resolve to it; add explicit "alias" fields to disambiguate` | `package create`, `build_dep_key_map` |
| Unknown NAME in env token | Authoring | `env var "{key}" references unknown dependency "{name}"; declared dependencies: [{names}]` | `package create` validation pass |
| Unknown NAME at resolve time | Runtime | `dependency interpolation failed: "{name}" is not a declared dependency of {identifier}` | `Accumulator::resolve_var` |
| Unknown PROPERTY on dep | Authoring / Runtime | `dep interpolation property "{property}" is not supported on "{name}"; known properties: [installPath]` | `Accumulator::resolve_var`, also exercised at `package create` validation |
| Missing dep content path at resolve time | Runtime | `dependency interpolation failed: install path for "{name}" is unavailable; re-run install` | `export_env`, when `objects.content()` target does not exist |

All error messages: lowercase, no trailing period, no `"Error:"` prefix (Rust API Guidelines `C-GOOD-ERR`; `quality-rust-errors.md`).

---

## Component Contracts

These contracts are testable as unit tests. A tester can write failing tests for each contract before implementation.

### Contract 1: `Dependency` deserialization with `alias`

Given JSON `{"identifier": "ocx.sh/gcc:13@sha256:<64-hex>", "alias": "gcc"}`, deserialization produces a `Dependency` with `alias = Some("gcc")` and `visibility = Visibility::Sealed` (default).

### Contract 2: `Dependency` deserialization without `alias`

Given JSON `{"identifier": "ocx.sh/gcc:13@sha256:<64-hex>"}`, deserialization produces a `Dependency` with `alias = None`. Serializing this back omits the `alias` field entirely.

### Contract 3: `Dependency` serialization skips absent `alias`

A `Dependency` with `alias = None` serializes to JSON that does not contain the key `"alias"`. This preserves backward compatibility with consumers that use `serde_json::from_str` with `deny_unknown_fields`.

### Contract 4: `build_dep_key_map` with unique basenames

Given two deps `ocx.sh/gcc:13` and `ocx.sh/python:3.12` (no aliases), `build_dep_key_map` returns `Ok({"gcc" → 0, "python" → 1})`.

### Contract 5: `build_dep_key_map` with alias overriding basename

Given dep `ocx.sh/toolchains/gcc:13` with `alias = "gcc"`, `build_dep_key_map` returns `Ok({"gcc" → 0})`.

### Contract 6: `build_dep_key_map` collision without aliases

Given deps `ocx.sh/gcc:13` and `ghcr.io/myorg/gcc:12` (both basename `"gcc"`, no aliases), `build_dep_key_map` returns `Err(DepKeyCollisionError { key: "gcc", ... })`.

### Contract 7: `build_dep_key_map` collision resolved by explicit alias

Given deps `ocx.sh/gcc:13` with `alias = "gcc-sys"` and `ghcr.io/myorg/gcc:12` with `alias = "gcc-org"`, `build_dep_key_map` returns `Ok({"gcc-sys" → 0, "gcc-org" → 1})`.

### Contract 8: `Accumulator::resolve_var` with `${deps.python.installPath}` token

Given `dep_paths = {"python" → /path/to/python/content}` and var value `"${deps.python.installPath}/bin/python3"`, `resolve_var` returns `Ok(Some("/path/to/python/content/bin/python3"))`.

### Contract 9: `Accumulator::resolve_var` with unknown NAME

Given `dep_paths = {"python" → ...}` and var value `"${deps.unknown.installPath}/bin"`, `resolve_var` returns `Err(UnknownDepInterpolationName { name: "unknown" })`.

### Contract 10: `Accumulator::resolve_var` with both `${installPath}` and `${deps.*}` in the same value

Given `install_path = /pkg`, `dep_paths = {"jdk" → /jdk}`, and value `"${installPath}/bin:${deps.jdk.installPath}/bin"`, `resolve_var` returns `Ok(Some("/pkg/bin:/jdk/bin"))`.

### Contract 11: `Accumulator::resolve_var` with no `${deps.*}` tokens leaves value unchanged

Given any var value not containing `${deps.`, `resolve_var` behaves identically to the pre-feature behavior — only `${installPath}` substitution is applied.

### Contract 12: `package create` validation rejects unknown dep NAME in env

Given metadata JSON with a dep `ocx.sh/python:3.12` and env value `"${deps.unknown.installPath}"`, `PackageCreate::execute` returns an error before creating any output file.

### Contract 13: `package create` validation passes for correct dep NAME

Given metadata JSON with a dep `ocx.sh/python:3.12` (basename `python`) and env value `"${deps.python.installPath}/bin"`, `PackageCreate::execute` completes without validation errors.

### Contract 14: Transitive deps are not queryable (scoping enforcement)

Given a root package R with a direct dep D, and D has its own direct dep T, R's env values must NOT be able to reference T via `${deps.T.installPath}`.

- `package create` on R: if R's env contains `${deps.T.installPath}` (T not in R's direct deps), validation fails with "env var ... references unknown dependency 'T'; declared dependencies: [D]". Validation runs purely against R's declared deps — R's metadata does not and cannot see T.
- Resolve time in R: R's `dep_views` map is built from R's direct deps only (D → DepView). A token `${deps.T.installPath}` that slipped past create-time validation returns `UnknownDepInterpolationName { name: "T" }` — NOT the path of T's content. The scoping invariant holds at both gates.
- D's own env (resolved during D's own `export_env` call) CAN reference T via `${deps.T.installPath}` — T is D's direct dep. Each package's `dep_views` map is scoped to its own declaring metadata, never to the consumer's view of the transitive closure.

### Contract 15: Unknown interpolation property surfaces a distinct error

Given `dep_views = {"python" → DepView { install_path: /path/to/python, .. }}` and var value `"${deps.python.version}"`, `resolve_var` returns `Err(UnknownDepInterpolationProperty { name: "python", property: "version" })`. The error is distinct from `UnknownDepInterpolationName` — NAME exists, but the requested PROPERTY is not in the dispatch table. This contract guards the extensibility seam: adding `version` later is a new match arm that flips this test from error to success without changing any other contract.

---

## User Experience Scenarios

### Scenario 1: Happy Path

Publisher declares:

```json
{
  "type": "bundle",
  "version": 1,
  "dependencies": [
    {"identifier": "ocx.sh/python:3.12@sha256:...", "visibility": "sealed"}
  ],
  "env": [
    {"key": "PYTHON_PREFIX", "type": "constant", "value": "${deps.python.installPath}"}
  ]
}
```

At `package create`: validation passes — basename `python` matches the single dep.

At install time: `PYTHON_PREFIX` resolves to `/home/user/.ocx/packages/ocx.sh/sha256/.../.../content`.

### Scenario 2: Error — Unknown NAME at `package create`

Publisher writes `"${deps.unknown.installPath}"` but declares no dependency named `unknown`. `package create` output:

```
error: env var "PYTHON_PREFIX" references unknown dependency "unknown"; declared dependencies: [python]
```

No output file is created. Publisher fixes the metadata before re-running.

### Scenario 3: Error — Basename Collision at `package create`

Publisher declares:

```json
"dependencies": [
  {"identifier": "ocx.sh/gcc:13@sha256:..."},
  {"identifier": "ghcr.io/myorg/gcc:12@sha256:..."}
]
```

`package create` output:

```
error: dep interpolation key "gcc" is ambiguous: both "ocx.sh/gcc:13" and "ghcr.io/myorg/gcc:12" resolve to it; add explicit "alias" fields to disambiguate
```

Publisher adds:

```json
{"identifier": "ocx.sh/gcc:13@sha256:...", "alias": "gcc-sys"},
{"identifier": "ghcr.io/myorg/gcc:12@sha256:...", "alias": "gcc-org"}
```

Validation passes.

### Scenario 4: Error — Missing Dep at Resolve Time

Consumer installs the package but the declared dep `python` was never installed. `Accumulator::resolve_var` encounters an unknown NAME in `dep_paths` (the map is built from installed content paths, and python's is absent):

```
error: dependency interpolation failed: "python" is not a declared dependency of ocx.sh/mypackage:1.0; re-run install
```

The install pipeline must surface this with the package identifier attached (three-layer error model: `PackageError { identifier, kind: PackageErrorKind::Internal(depinterp_error) }`).

### Scenario 5: Existing Packages Unaffected

A package with no `alias` fields and no `${deps.*}` tokens in env values. `build_dep_key_map` returns an empty map (no deps). `Accumulator` is constructed with `dep_views = HashMap::new()`. `resolve_var` finds no `${deps.*}` tokens. Behavior is identical to pre-feature.

### Scenario 6: Error — Transitive Dep Referenced

Publisher authors package R with:

```json
{
  "dependencies": [
    {"identifier": "ocx.sh/cmake:3.28@sha256:...", "visibility": "sealed"}
  ],
  "env": [
    {"key": "JAVA_HOME", "type": "constant", "value": "${deps.jdk.installPath}"}
  ]
}
```

cmake has its own direct dep `jdk`, but R does not. `package create` output:

```
error: env var "JAVA_HOME" references unknown dependency "jdk"; declared dependencies: [cmake]
```

The publisher must decide: either (a) elevate jdk to a direct dep of R (explicit dependency surface), or (b) drop the token. R cannot reach into cmake's transitive closure through interpolation — scoping is explicit by design.

---

## Implementation Notes

1. **`Dependencies::interpolation_key_map()`** — add a method to `Dependencies` that calls `build_dep_key_map` and returns `Result<HashMap<String, usize>, DepKeyCollisionError>`. Both the `package create` validation pass and `DepView` map construction call this. Single source of truth.

2. **`DepView` is the extensibility seam** — all env-chain code paths that need dep information accept `HashMap<String, DepView>`, not `HashMap<String, PathBuf>`. Future properties (`version`, `binPath`, …) are added by extending `DepView` (or by computing from its existing `PinnedIdentifier`) and adding a dispatch arm in the token parser. No signature change propagates through the call chain when a new property is added — that is the point of this seam.

3. **OCI domain types, not strings** — `DepKeyCollisionError` carries `PinnedIdentifier` fields, and `DepView` carries `PinnedIdentifier`. Error formatting and downstream consumers see the structured OCI identity, not a pre-flattened `String`. The three-layer error model (`PackageError { identifier, kind }`) can wrap these without re-parsing.

4. **`export_env` signature change** — `export_env(content, metadata, dep_views, entries)` is a shared free function in `tasks/common.rs`. Both direct callers (`resolve_env`) and shell-profile callers must be updated in the same commit.

5. **`Exporter` is called from exactly one place** — inside `export_env` in `tasks/common.rs`. CLI commands reach `Exporter` via the chain `resolve_env → export_env → Exporter`. There are no direct `Exporter::new` call sites in CLI command files. Update `export_env` and `Exporter::new` signatures once; all CLI commands benefit automatically.

6. **Direct-deps-only scoping** — `resolve_env` builds a fresh `dep_views` per package by iterating that package's own `metadata.dependencies()`. It never reuses the caller's map and never walks `ResolvedPackage.dependencies`. The transitive-closure invariant is structurally impossible to violate because the construction site only has access to direct deps.

7. **Schema regeneration** — run `task schema:generate` after updating the `Dependency` struct. Update `website/src/docs/reference/metadata.md` to document the `alias` field, its purpose, and the basename fallback rule.

8. **`schemars::JsonSchema` derive** — The `Dependency` struct already derives `schemars::JsonSchema`. Adding `alias: Option<String>` with `skip_serializing_if` does not break the derive; schemars infers `Option<T>` as a non-required property.

9. **Token parsing** — Implement without a regex crate dependency. A simple iterator-based parser that recognizes `${deps.` prefix, collects NAME bytes until `.`, PROPERTY bytes until `}`, then dispatches on PROPERTY is sufficient. This avoids a new dependency and keeps the parser aligned with the `DepView` dispatch model.

10. **`DepKeyCollisionError`** — new error type in `dependency.rs` alongside `DuplicateDependencyError`. Same `thiserror::Error` pattern; fields are `PinnedIdentifier`, not `String`.

11. **Interpolation error variants** — add to the existing package error type in `crates/ocx_lib/src/package/error.rs`: `UnknownDepInterpolationName { name: String }` (NAME not in declared deps) and `UnknownDepInterpolationProperty { name: String, property: String }` (PROPERTY not in dispatch table).

---

## Links

- [Research artifact](./research_env_interpolation_patterns.md)
- [arch-principles.md](../rules/arch-principles.md) — three-layer error model
- [subsystem-package.md](../rules/subsystem-package.md) — env resolution flow
- [subsystem-package-manager.md](../rules/subsystem-package-manager.md) — task module conventions
- [subsystem-metadata-schema.md](../rules/subsystem-metadata-schema.md) — schema generation checklist
- [quality-rust-errors.md](../rules/quality-rust-errors.md) — error message conventions

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-19 | Architect worker | Initial draft |
| 2026-04-19 | Revision (user feedback) | Added Decision 3 Option B (`DepView` extensibility seam) and Decision 4 (direct-deps-only scoping); `DepKeyCollisionError` now carries `PinnedIdentifier`; added Contracts 14 & 15 and Scenario 6; added `UnknownDepInterpolationProperty` to error taxonomy |
