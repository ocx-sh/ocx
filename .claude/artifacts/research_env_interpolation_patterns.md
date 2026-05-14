# Research: Dependency Path Interpolation Patterns

**Feature**: Issue #32 — `${deps.NAME.installPath}` interpolation in env metadata  
**Date**: 2026-04-19  
**Researcher**: worker-researcher (sonnet), patterns axis

---

## Summary

No package manager uses the exact `${deps.NAME.installPath}` syntax. The closest precedents are:
- **Cargo `DEP_NAME_KEY`** — explicit `links` alias field; strongest parallel
- **Nix `${pkgs.dep}`** — first-class derivation path interpolation; eager validation
- **pkg-config `${prefix}`** — single-package path self-description pattern
- **mise `{{ tools.NAME.version }}`** — two-phase deferred resolution

**Recommendation**: alias field on `Dependency` struct with basename fallback; validate at `package create`, resolve at `pull`.

---

## 1. Syntax Patterns Across Ecosystems

### pkg-config `.pc` files (30+ years)
Single-level variable interpolation. `${prefix}` is the root path; derived as `${prefix}/lib`, `${prefix}/include`. No cross-package interpolation. OCX already uses this syntax for `${installPath}`.

### Cargo `DEP_NAME_KEY` build scripts
The only mainstream mechanism for cross-package path propagation:
- Dependency declares `links = "openssl"` in Cargo.toml (explicit alias)
- Build script emits `cargo::metadata=root=/usr/local/openssl`
- Dependent receives `DEP_OPENSSL_ROOT` as env var
- **Key lesson**: The interpolation key (`openssl`) is an explicit `links` declaration, NOT derived from the crate name. Avoids name collision by design.

### Nix `${pkgs.dep}` derivations
Derivations are first-class interpolatable values: `"${pkgs.openssl}/lib"`. Eager validation — missing attribute fails at expression evaluation time (before any build). The name is the attribute key in `pkgs` scope. This is OCX's closest model for eager `package create` validation.

### mise `{{ tools.node.version }}`
Tera templates with `tools = true` deferred resolution. Short tool name (not registry path). Install path NOT exposed as template variable — mise handles PATH prepending automatically.

### conda `environment.yml`
No cross-package path interpolation in standard conda. conda-devenv adds Jinja2 but not cross-package path refs.

---

## 2. Name Resolution Strategies

| Strategy | Example | Collision Risk | Precedent |
|----------|---------|----------------|-----------|
| Repository basename | `gcc` from `ocx.sh/toolchains/gcc` | Low (OCX enforces unique `(registry, repo)`) | mise, Homebrew |
| Full path | `ocx.sh/toolchains/gcc` | None | Verbose, chars need escaping |
| Explicit alias field | `"alias": "gcc"` on dep | None (author-declared) | Cargo `links` field, pnpm aliases |
| Basename + alias fallback | Default = basename, alias required only on collision | Handled at create time | npm `devDependencies` key |

**Recommended**: Explicit `alias` field with basename fallback. Error at `package create` if two deps share a basename without explicit aliases to disambiguate.

### Basename collision scenario
Two deps from different registries with same repo basename:
```json
{"identifier": "ocx.sh/gcc:13@sha256:...", "visibility": "sealed"}
{"identifier": "ghcr.io/myorg/gcc:12@sha256:...", "visibility": "sealed"}
```
Both would map to `gcc` as interpolation key — `package create` must reject this and require `alias` fields.

---

## 3. Validation Gate

| System | When validated | What happens on failure |
|--------|---------------|------------------------|
| pkg-config | Query time (lazy) | Error on first query |
| Cargo DEP_NAME_KEY | Build time (lazy) | Env var is absent; build script gets `None` |
| Nix | Expression eval (eager) | Eval fails before build begins |
| mise | Activation time (lazy) | Silent fail or error at runtime |

**Recommendation**: Validate at `package create` (like Nix's eager model, like OCX's existing cycle detection). This is the earliest actionable point and matches existing OCX patterns.

At resolve time (`pull`/install), a missing dep symlink should return a clear error rather than silent empty string.

---

## 4. Extensibility: `installPath`-Only vs. Richer Properties

Pattern across all systems: start with root path (`prefix`, `outPath`, `installPath`), extend by FHS convention or explicit additional properties when concrete use cases arise.

- pkg-config started with `${prefix}`, added `${libdir}`, `${includedir}` by convention
- Nix uses `${dep}` root, then `${dep}/lib`, `${dep}/bin` by FHS layout

**Recommendation**: Start with `installPath` only. `${deps.python.installPath}/bin/python3` covers 95%+ of real use cases (sysroots, `CMAKE_PREFIX_PATH`, `JAVA_HOME`). Add `version`, `binPath` only when concrete requests arrive.

---

## 5. Industry Trends

- **Established**: pkg-config `${prefix}` (30+ years), Cargo `DEP_NAME_KEY` (10+ years)
- **Trending**: mise Tera templates — declarative TOML-first configs gaining adoption
- **Emerging**: pixi per-package activation scripts (push model) — shows trend toward package-declared env
- **Declining**: Bash `brew --prefix` + shell snippets — moving to declarative tooling-readable formats

---

## Design Recommendations for OCX

### Dep declaration
```json
{
  "identifier": "ocx.sh/toolchains/gcc:13@sha256:...",
  "visibility": "sealed",
  "alias": "gcc"
}
```
`alias` is optional. If omitted, basename of repo path is used as interpolation key.

### Env value interpolation
```json
{
  "key": "CMAKE_PREFIX_PATH",
  "type": "path",
  "value": "${deps.gcc.installPath}"
}
```

### Validation gates
1. **`package create`**: Validate every `${deps.NAME.*}` token references a declared dep (by alias or basename). Error on basename collision without explicit aliases.
2. **Resolve time**: Look up `deps/` symlinks for each NAME; clear error if missing.

### Implementation touchpoints
- `dependency.rs`: Add `alias: Option<String>` to `Dependency` struct
- `accumulator.rs`: Add `dep_paths: HashMap<String, PathBuf>` param; substitute `${deps.NAME.installPath}` tokens
- `export_env` in `tasks/common.rs`: Pass `objects: &PackageStore` to build dep path map
- `package create` command: Validate interpolation keys against declared dep list

---

## Sources

- [Guide to pkg-config](https://people.freedesktop.org/~dbn/pkg-config-guide.html)
- [Build Scripts — The Cargo Book](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
- [Nix Reference Manual — Derivations](https://nix.dev/manual/nix/2.22/language/derivations)
- [mise Environments docs](https://mise.jdx.dev/environments/)
- [FindPkgConfig — CMake docs](https://cmake.org/cmake/help/latest/module/FindPkgConfig.html)
- [pnpm Aliases](https://pnpm.io/aliases)
- [conda Managing Environments](https://docs.conda.io/projects/conda/en/latest/user-guide/tasks/manage-environments.html)
- OCX ADR: Package Dependencies (`.claude/artifacts/adr_package_dependencies.md`)
