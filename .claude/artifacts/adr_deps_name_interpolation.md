# ADR: `${deps.NAME.installPath}` Interpolation in Env Metadata

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** Michael Herwig
**GitHub Issue:** ocx-sh/ocx#32
**Related ADR:** [adr_package_dependencies.md](./adr_package_dependencies.md) (line 212 names this feature)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** api | data
**Supersedes:** N/A

## Context

Issue #32 adds `${deps.NAME.installPath}` as a supported token in env metadata `value` strings. The ADR for package dependencies (PR #13) already names this as a "natural companion to `export: false`" (line 212). Where visibility controls *whether* a dependency's env propagates, interpolation gives *direct path access* without env propagation.

A package publisher can now write:
```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    {"key": "PYTHON",  "type": "constant", "value": "${deps.python.installPath}/bin/python"},
    {"key": "CMAKE",   "type": "constant", "value": "${deps.cmake.installPath}"}
  ],
  "dependencies": [
    {"identifier": "ocx.sh/python:3.12@sha256:abc...", "visibility": "sealed"},
    {"identifier": "ocx.sh/cmake:3.28@sha256:def...", "visibility": "sealed"}
  ]
}
```

Three design questions need resolution (enumerated in the issue body and confirmed by research):
1. How is NAME derived from an OCI identifier?
2. Should interpolation be extensible to other fields?
3. When should `${deps.NAME.*}` references be validated?

## Decision Drivers

- Published metadata is immutable — wrong naming conventions cannot be patched after packages exist
- NAME must be human-readable and unsurprising (publisher writes it by hand)
- Collision from same-basename deps across registries must be resolvable
- Validation errors at create time are 1000× cheaper than errors at every install

## Industry Context & Research

**Research artifact:** [research_env_interpolation.md](./archive/research_env_interpolation.md)
**Trending approaches:** Structured namespace interpolation (`tools.NAME.field`, `${pkgs.NAME}`) is the direction all modern package managers have converged on; flat `ALL_CAPS_VAR` env injection is declining.
**Key insight:** Every surveyed tool (mise, Nix, Spack, Homebrew) uses the **short identifier** (basename) as NAME — never the full registry path or version. Mise's `{{tools.python.path}}` is the closest living analog.

## Considered Options

### Option A: Basename of repository path (Recommended)

NAME = last segment of the OCI repository path, lowercased. `ocx.sh/tools/cmake:3.28@sha256:...` → `cmake`. Add `alias: Option<String>` on `Dependency` for collision resolution when two deps share the same basename.

| Pros | Cons |
|------|------|
| Matches all surveyed tools (mise, Nix, Spack, Homebrew) | Requires alias when same basename from two registries |
| Short and human-readable | Alias validation adds a small parsing step |
| Already the deduplification key in the `dependencies` array | |
| edit-distance suggestions possible on typos | |

### Option B: Full repository path

NAME = full repository path without registry, e.g., `tools/cmake`. `${deps.tools/cmake.installPath}` — no alias needed.

| Pros | Cons |
|------|------|
| Unique by construction (no alias) | Slashes in template tokens require quoting or an alternate delimiter |
| Exactly matches what's in the `identifier` field | Verbose; publishers don't type `tools/cmake` today |
| | Counter to every surveyed tool convention |

### Option C: Explicit `name` field on every Dependency (no derivation)

Add a required `name: String` field. Publishers choose the NAME explicitly.

| Pros | Cons |
|------|------|
| Fully explicit; no derivation logic | Adds required field — schema breaking change |
| No collision possible | Verbose for the common case (redundant with basename) |
| | Forces publishers to repeat themselves |

## Decision Outcome

**Chosen Option: A** — basename, with optional `alias` for collision resolution.

**Rationale:** Strongest industry precedent; least typing for publishers; collision cases (same basename from different registries) are rare and explicitly resolved. An optional `alias` field is backward compatible (field is absent in the common case) and lets publishers resolve collisions without a schema breaking change.

### Consequences

**Positive:**
- `${deps.python.installPath}` — short, obvious, consistent with every other tool
- No required field additions; schema remains backward compatible for existing packages
- Alias mechanism handles the rare collision case explicitly

**Negative:**
- Derivation logic (`split('/').last()`) must be deterministic and documented
- Alias uniqueness must be validated at deserialization time (two deps with the same alias is an error)

**Risks:**
- Publishers with the same-basename collision may forget to add `alias` → clear error at `package create` mitigates this
- **Older client silent mis-substitution**: the `alias` field itself is backward compatible (older OCX clients deserialize unknown fields as ignored), but a pre-release client pulling a package that uses `${deps.NAME.installPath}` tokens will leave the literal string in the env value rather than failing loudly. Mitigation: document minimum OCX version in the publishing guide; a future `min_ocx_version` metadata field is a separate consideration. Tracked in Progress Log.

## Technical Details

### Architecture

The `Accumulator` holds a `HashMap<String, DependencyContext>` rather than a flat `HashMap<String, PathBuf>`. `DependencyContext` is a per-dep struct derived from `InstallInfo` — it carries the install path now, and is designed to hold resolved env vars in a future iteration (see Extensibility below).

```
resolve_env() [tasks/resolve.rs]
  │
  │  ── BUILD dep_contexts (ALL declared direct deps, no visibility filter) ──
  │  let mut dep_contexts: HashMap<String, DependencyContext> = HashMap::new();
  │  for dep in pkg.resolved.dependencies:
  │    dep_contexts[dep.name()] = DependencyContext::new(objects.content(&dep.identifier))
  │
  │  ── ENV PROPAGATION (visible deps only, topological order, unchanged) ──
  │  for each dep in pkg.resolved.dependencies where dep.visibility.is_visible():
  │    entries = export_env(&dep_content, &dep_metadata, &dep_contexts, &mut entries)
  │    // Future: dep_contexts[dep.name()].populate_env(&entries) — env vars available for
  │    //         downstream dependents referencing ${deps.NAME.SOME_VAR}
  │  export_env(&pkg.content, &pkg.metadata, &dep_contexts, &mut entries)  ← root package
  ▼
export_env(content, metadata, dep_contexts, entries) [tasks/common.rs]
  │  dep_contexts: &HashMap<String, DependencyContext>  ← NEW parameter
  ▼
Exporter::new(install_path, dep_contexts) [env/exporter.rs]
  ▼
Accumulator::new(install_path, dep_contexts) [env/accumulator.rs]
  │  resolve_var():
  │    1. replace "${installPath}" with install_path   ← unchanged
  │    2. regex `\$\{deps\.([a-z0-9][a-z0-9_-]*)\.([a-zA-Z]+)\}` — find all tokens
  │       - unknown NAME → Error::UnknownDependencyRef
  │       - FIELD == "installPath" → dep_contexts[NAME].install_path
  │       - FIELD unknown → Error::UnknownDependencyField (lists supported fields)
  │       - content path absent → Error::DependencyNotInstalled
  │       - malformed token → passed through unchanged
  │    3. all tokens substituted in a single pass
  ▼
Entry { key, value (fully resolved), kind }
```

**`DependencyContext`** (new type in `crates/ocx_lib/src/package/metadata/env/`):
```rust
pub struct DependencyContext {
    pub identifier: oci::PinnedIdentifier,  // carries registry, repository, version, digest
    pub install_path: PathBuf,
    // Future: resolved_env: IndexMap<String, String>  — populated during dep iteration
}

impl DependencyContext {
    pub fn new(identifier: oci::PinnedIdentifier, install_path: PathBuf) -> Self
    pub fn resolve_field(&self, field: &str) -> Option<String>  // dispatches "installPath" → path
}
```

Carrying the full `PinnedIdentifier` (not a flattened `PathBuf`) has two consequences:

1. **Error chain integrity** — when substitution fails (`DependencyNotInstalled`, collision diagnostics), the error captures structured OCI identity (registry + repository + version + digest). Downstream formatters and the three-layer `PackageError` wrapper see the full identifier, not a pre-stringified form.
2. **Future-property seam** — `${deps.NAME.version}` and `${deps.NAME.digest}` are future additions (see "Out of Scope" in the plan). When added, they are derivable from `self.identifier` without threading new parameters through `Accumulator` / `Exporter` / `export_env` / `resolve_env`. The extension is a `match` arm in `resolve_field` plus a field addition to the supported set — no signature churn.

**Env-var extensibility note:** The resolution order in `resolve_env()` already processes each dep before its dependents, so after `export_env` runs for dep N, its `Entry` results could be stored back into `dep_contexts[N].resolved_env`. A future `${deps.python.PYTHON_HOME}` token would then resolve against that map without any structural change to the Accumulator — only `DependencyContext::resolve_field` needs extending. This is intentionally not implemented now (YAGNI), but the structure is designed to allow it.

**Key design invariant:** Visibility controls *env propagation* (which deps contribute key=value pairs to the consumer's env). Interpolation controls *path/value reference* (embedding a dep's data in a value string). These are **orthogonal axes**. A `sealed` dep is absent from env propagation but fully present in `dep_contexts` for interpolation purposes.

**Scoping invariant (direct deps only):** Each package's `dep_contexts` map is built exclusively from *that package's own* `metadata.dependencies()` — never from the transitive closure `ResolvedPackage.dependencies`. This is enforced structurally at the construction site in `resolve_env`:

- Root package R: map contains R's direct deps only.
- Transitive dep D (invoked during R's install): during D's own `export_env` call, D's map is built from D's `metadata.dependencies()` — independent of R's map. D can reference its own direct deps (T); R cannot reach T via `${deps.T.installPath}`.
- Each package's interpolation surface is exactly its own declared dep set. Action-at-a-distance through transitive closures is impossible by construction.

Mirrors Cargo `DEP_NAME_KEY` scoping (only immediate dependencies). A publisher that needs a transitive dep's path must elevate it to a direct dep in their own metadata.

### API Contract

**OCI identifier getter (shared logic — the raw `rsplit` lives ONLY here):**
```rust
// Identifier already has name() → Option<String> at identifier.rs:145.
// PinnedIdentifier is a newtype over Identifier and inherits it via Deref.
// Strategy: add an infallible &str-returning variant alongside the existing optional one,
// keeping backward compat. Name it `repository_name()` to avoid collision with Dependency::name():
impl Identifier {
    pub fn repository_name(&self) -> &str   // infallible; parse guarantees non-empty basename
    // existing name() → Option<String> kept for callers that rely on it
}
// PinnedIdentifier: expose via delegation (self.0.repository_name() or Deref auto-impl).
// The raw rsplit lives ONLY in Identifier::repository_name(); name() delegates to it.
```

**`Dependency` struct extension:**
```rust
pub struct Dependency {
    pub identifier: oci::PinnedIdentifier,
    pub visibility: Visibility,
    pub alias: Option<String>,   // NEW: explicit NAME override; validated on deserialization
}
impl Dependency {
    pub fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or_else(|| self.identifier.repository_name())
    }
}
```

Alias validation rules — **two distinct locations, distinct responsibilities:**

`Dependencies::new()` (runs on every metadata read, must be backward-compatible):
- Alias format: `[a-z0-9][a-z0-9_-]*`
- Alias uniqueness: no two entries share the same explicit alias string

`validate_env_dep_refs()` (runs only at write boundaries: `package create`, `package push`):
- Basename collision detection: when a `${deps.NAME.*}` token references a name that could match multiple deps' basenames and no alias disambiguates → hard error with list of colliding identifiers
- Reason for split: existing packages with same-basename deps but no `${deps.*}` tokens must continue to deserialize without error

**`Accumulator` extension:**
```rust
pub struct Accumulator {
    install_path: PathBuf,
    dep_contexts: HashMap<String, DependencyContext>,  // NEW — keyed by dep.name()
}
```

**`export_env` signature change:**
```rust
pub fn export_env(
    content: &Path,
    metadata: &metadata::Metadata,
    dep_contexts: &HashMap<String, DependencyContext>,  // NEW
    entries: &mut Vec<metadata::env::exporter::Entry>,
) -> crate::Result<()>
```

**`Exporter` extension:**
```rust
pub struct Exporter {
    accumulator: Accumulator,  // Accumulator now carries dep_contexts
}
```

### Error Taxonomy

```rust
// In crates/ocx_lib/src/package/error.rs (or metadata/env/accumulator.rs):
UnknownDependencyRef {
    var_key: String,
    ref_name: String,
    declared: Vec<String>,
},
UnknownDependencyField {
    var_key: String,
    ref_name: String,
    field: String,
    supported_fields: Vec<String>,
},
AmbiguousDependencyRef {
    var_key: String,
    ref_name: String,
    first: oci::PinnedIdentifier,
    second: oci::PinnedIdentifier,
},
DependencyNotInstalled {
    var_key: String,
    ref_name: String,
    dep_identifier: oci::PinnedIdentifier,
},
```

`AmbiguousDependencyRef` and `DependencyNotInstalled` carry `PinnedIdentifier` rather than flattened strings — the three-layer `PackageError` wrapper and any log/diagnostic consumers see full OCI identity (registry + repository + version + digest). Stringification happens at the display boundary via `thiserror::Error`'s `Display`.

Error messages follow `C-GOOD-ERR`:
```
env variable 'PYTHON' references unknown dependency 'pythn' in value '${deps.pythn.installPath}/bin/python'
declared dependencies are: python, cmake
did you mean 'python'?
```

### `package create` and `package push` Validation

Validator lives in `ocx_lib` so both command paths can call it:
```rust
// crates/ocx_lib/src/package/metadata.rs or metadata/validation.rs
pub fn validate_env_dep_refs(metadata: &Metadata) -> crate::Result<()>
```
Called from both `PackageCreate::execute()` and `PackagePush::execute()` (pre-flight, before upload). The `package push` path currently has no validation — this closes the gap.

Scans all `Var.value` strings for `${deps.NAME.FIELD}` using regex `\$\{deps\.([a-z0-9][a-z0-9_-]*)\.([a-zA-Z]+)\}`. For each match:
1. NAME must be a dep's `name()` (basename or alias) in `metadata.dependencies()`
2. FIELD must be `"installPath"` — unknown fields are hard errors, list supported fields in the error
3. Returns `Err` on first failure with an actionable message

**Collision detection** lives in `validate_env_dep_refs()`, NOT in `Dependencies::new()`. Reason: two same-basename deps without any `${deps.*}` tokens is not an error — adding the collision check to deserialization would silently break existing packages. Only fire the check when a token actually references the ambiguous name.

**Token grammar (one-way door):**
- NAME: `[a-z0-9][a-z0-9_-]*` (lowercase; enforced by OCI parse validation upstream)
- FIELD: `[a-zA-Z]+` — only `installPath` supported now; unknown fields error with supported-fields list
- Tokens not matching the grammar pass through unchanged (no error — backward compat)
- Unknown FIELD is a forward-compat limitation: publishers targeting future fields must use a future schema version

### User Experience Scenarios

| Scenario | Expected Outcome |
|----------|-----------------|
| Valid: `${deps.python.installPath}/bin/python`, python declared | Resolves to actual content path at resolve time |
| Valid: `sealed` dep + interpolation | Works — sealed controls env propagation, not path access |
| Valid: `export: false` (any visibility) + interpolation | Works — visibility and interpolation are orthogonal |
| Error: `${deps.pythn.installPath}`, python declared | `package create` fails: unknown dep 'pythn'; did you mean 'python'? |
| Error: `${deps.python.version}`, version not supported yet | `package create` fails: unsupported field 'version'; supported fields: installPath |
| Error: dep declared but not installed at resolve time | `ocx env`/`ocx exec` fails with clear error naming the missing dep |
| Collision: two deps with basename `python` | `package create` requires `alias` field; error explains why |
| `ocx env --format json` | Resolved `${deps.*}` values appear in output |
| `ocx exec ... -- cmd` | Resolved `${deps.*}` values are set in the subprocess env |

### Edge Cases

- `${deps.NAME.installPath}` inside a `path`-type var: applies the same expansion, then the relative-path join and `required` check proceed on the expanded value.
- Multiple `${deps.*}` tokens in the same value string: all expanded in a single pass (regex `replace_all`).
- Self-referential `${installPath}` and `${deps.NAME.installPath}` in the same value: both expand correctly in the same pass.
- Transitive dep not in `dependencies` array: NOT addressable via `${deps.*}` — direct deps only.
- Case: `${deps.Python.installPath}` (uppercase): error at `package create` (basenames are always lowercase; provide the lowercase suggestion in the error).

## Component Contracts

Each contract is a testable invariant. A tester writes failing tests for these before Phase 4; `worker-reviewer` checks that the final diff satisfies each one.

1. **`Dependency` deser with `alias`** — JSON `{"identifier": "ocx.sh/gcc:13@sha256:<hex>", "alias": "gcc"}` → `Dependency { alias: Some("gcc"), .. }`.
2. **`Dependency` deser without `alias`** — missing field → `alias: None`.
3. **`Dependency` ser omits `None` alias** — round-trip JSON does not contain the `alias` key. Preserves `deny_unknown_fields` consumers.
4. **Alias format validation** — invalid alias (empty, contains `/`, uppercase, duplicate across deps) → `Dependencies::new()` errors.
5. **`Dependency::name()` dispatch** — returns `alias` when set, `identifier.repository_name()` otherwise.
6. **`Accumulator::resolve_var` — token expansion** — `${deps.python.installPath}/bin/python3` → `/path/to/python/content/bin/python3`.
7. **`Accumulator::resolve_var` — mixed tokens** — `${installPath}/bin:${deps.jdk.installPath}/bin` → both expanded in one pass.
8. **`Accumulator::resolve_var` — unchanged when no `${deps.*}`** — values without deps tokens behave identically to pre-feature.
9. **Unknown NAME at resolve time** — `${deps.unknown.installPath}` with `unknown` absent from `dep_contexts` → `UnknownDependencyRef` with declared names listed.
10. **Unsupported FIELD at resolve time** — `${deps.python.version}` with FIELD not in the dispatch table → `UnknownDependencyField` with supported fields listed.
11. **Case sensitivity** — `${deps.Python.installPath}` (uppercase) → error (basenames are lowercase).
12. **Dep declared but not installed** — `dep_contexts` entry present, `objects.content(&dep.identifier)` path absent → `DependencyNotInstalled` carrying the `PinnedIdentifier`.
13. **`validate_env_dep_refs` — happy path** — env refs match declared dep basenames → `Ok(())`.
14. **`validate_env_dep_refs` — unknown ref at create/push time** — `package create` and `package push` both reject with the same error.
15. **`validate_env_dep_refs` — collision without alias, token present** — two deps with same basename *and* a `${deps.NAME.*}` token for that basename → `AmbiguousDependencyRef` with both `PinnedIdentifier`s preserved. Without the token, same deps deserialize cleanly (backward compat preserved).
16. **Transitive scoping — create-time gate** — package R references `${deps.T.installPath}` where T is not in R's direct deps → `validate_env_dep_refs` fails with "unknown dependency 'T'; declared: [D]". T being a transitive dep of D is irrelevant — R's metadata does not see T.
17. **Transitive scoping — resolve-time gate** — R's `dep_contexts` map (built in `resolve_env` from R's direct deps only) does not contain T; a token `${deps.T.installPath}` that escaped create-time validation returns `UnknownDependencyRef`. Scoping holds at both gates.

Contract 10 guards the extensibility seam: adding `${deps.NAME.version}` in a future release is a new match arm plus dispatch-table entry that flips this contract's expected outcome from error to success, without changing any other contract.

Contracts 16 and 17 together prove the direct-deps-only scoping invariant: create-time validation catches the author mistake, and the resolve-time map is structurally incapable of reaching a transitive dep's path even if a token slipped through. Both gates must pass in test coverage.

## Implementation Plan

1. [ ] Extend `Dependency` struct with `alias: Option<String>`, update `Dependencies::new()` validation
2. [ ] Extend `Accumulator` with `dep_paths: HashMap<String, PathBuf>` and expand the `resolve_var` method
3. [ ] Update `Exporter::new()` to accept and pass `dep_paths` to `Accumulator`
4. [ ] Update `export_env()` signature and call site in `resolve_env()` to build + pass the map
5. [ ] Add `validate_env_dep_refs()` in `package create` command
6. [ ] Update `Dependencies::new()` to detect basename/alias collisions and produce errors
7. [ ] Regenerate JSON schema (`task schema:generate`)
8. [ ] Update metadata reference documentation (`website/src/docs/reference/metadata.md`)
9. [ ] Unit tests: `Accumulator` expansion cases, `Dependency` alias validation, collision detection
10. [ ] Acceptance tests: end-to-end `ocx env` and `ocx exec` with `${deps.NAME.installPath}`, `sealed` dep scenario

## Validation

- [ ] `task verify` passes all existing tests (no regressions)
- [ ] New unit tests cover all edge cases above
- [ ] New acceptance tests cover all user experience scenarios above
- [ ] `task schema:generate` produces correct JSON schema for `alias` field and updated value description
- [ ] Documentation updated with `sealed` + interpolation canonical example

## Links

- [Package Dependencies ADR](./adr_package_dependencies.md) — predecessor; names this feature at line 212
- [Research: Env Interpolation](./archive/research_env_interpolation.md)
- [GitHub Issue #32](https://github.com/ocx-sh/ocx/issues/32)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-19 | Michael Herwig | Initial draft (swarm-plan output) |
| 2026-04-19 | Merge | Cross-worktree merge: `DependencyContext` now carries `PinnedIdentifier` (enables future `version`/`digest` properties + structured error chain); added explicit direct-deps-only scoping invariant; added `AmbiguousDependencyRef` error variant with `PinnedIdentifier` pair; added 17-contract list including transitive-scoping double gate and extensibility-seam guard; added older-client silent-mis-substitution risk |

## Post-implementation update

The original ADR chose Option A (basename derivation with optional `alias` override) and named
the override field `alias`. During implementation on `feat/package-entry-points`, the field was
renamed from `alias` to `name` — the `Dependency` struct now carries `pub name: Option<String>`
instead of `pub alias: Option<String>`, and `Dependency::name()` returns it directly. The
template token form `${deps.NAME.installPath}` is unchanged. See the [`Dependency.name` field
in `metadata.md`][metadata-dep-name] for the current field reference.

[metadata-dep-name]: ../../website/src/docs/reference/metadata.md#dependencies
