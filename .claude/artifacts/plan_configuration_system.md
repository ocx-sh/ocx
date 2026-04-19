# Plan: Configuration System

## Overview

**Status:** Draft
**Author:** architect (swarm-plan)
**Date:** 2026-04-12
**Beads Issue:** Related: #33 (project-level toolchain config), #35 (policy-based retention)
**Related ADR:** adr_infrastructure_patches.md proposed `$OCX_HOME/config.toml`
**Research:** [research_configuration_patterns.md](./research_configuration_patterns.md)

### Classification

- **Scope**: Small (3-4 days)
- **Reversibility**: One-Way Door (Medium) — config file format becomes a public contract

## Objective

Build the **configuration infrastructure** for OCX: how config is discovered, loaded, merged, validated, and threaded through `Context`. The plan deliberately focuses on **how config works**, not **what is configured**.

The only concrete config consumer in v1 is `[registry] default` (which replaces `OCX_DEFAULT_REGISTRY` as a persistent setting). This is the proof-of-life consumer that validates the loader works end-to-end. Other config consumers (patches, registry rewrites, retention policies) get added when their backing features land.

The infrastructure must be designed so future features can plug in without rewriting the loader.

## Scope

### In Scope (config infrastructure)

- **System config** — `/etc/ocx/config.toml` (machine-wide defaults, useful for Docker images and managed environments)
- **User config** — `$XDG_CONFIG_HOME/ocx/config.toml` or `~/.config/ocx/config.toml` (personal defaults)
- **`$OCX_HOME/config.toml`** — data-directory-scoped config (co-located with data, redistributable)
- **`--config FILE`** — explicit override (existing CLI flag, currently a no-op)
- **`OCX_CONFIG_FILE` env var** — explicit config path via environment (for CI/Docker)
- **Config loading, merging, and validation** — sync file reads, layered precedence merge
- **Env var overrides** — existing `OCX_*` vars take precedence over config files
- **`OCX_NO_CONFIG`** — CI reproducibility kill switch (disables all file-based config)
- **Wiring into `Context::try_init()`** — make `--config` flag and `Config` actually work
- **Loader extension points** — designed so future tiers (project-level `ocx.toml` via CWD walk) can plug in without rewriting
- **Documentation** — new Configuration reference page, sections in user guide and getting started

### In Scope (v1 config consumers)

- **`[registry] default = "..."`** — replaces `OCX_DEFAULT_REGISTRY` as the persistent default. Primary consumer to validate the loader works end-to-end.
- **`[registries.<name>] url = "..."`** — named registry entries with a single `url` field. `[registry] default` resolves through the named map; if no matching entry, falls back to the literal name (backwards compatible with bare hostnames). Ships live in v1 so the resolution path is stable before future per-registry fields (`insecure`, `location` rewrite, `timeout`, auth) land — they slot into the same entry without breaking existing configs.

### Deferred to Later (when their features land)

- **`[registries.<name>] location` rewrite** — no rewriter exists in OCX today; defer until that feature lands
- **`[registries.<name>] insecure` per-registry flag** — `OCX_INSECURE_REGISTRIES` already works as a list; per-registry config can wait
- **`[patches]` section** — no patch resolver in OCX today; defer to the patches feature
- **`[clean]` section** — defer to #35 (retention policies)
- **Project-level `ocx.toml` + CWD walk** — separate feature (#33). Filename is `ocx.toml` at the project root (matching `pyproject.toml` / `Cargo.toml` convention), **not** `.ocx/config.toml`. The `$OCX_HOME/config.toml` and `ocx.toml` are deliberately different names so that the data-directory tier and project tier are never confused.
- **Mirror spec management in config** — mirrors remain standalone YAML
- **Credential helper integration** — env vars remain the auth mechanism
- **`$VAR` interpolation in config values** — design considered now, implementation deferred
- **`include` key for config composition** — design considered now, implementation deferred
- **JSON schema generation** — deferred until format stabilizes

## Technical Approach

### Architecture

```
Precedence (lowest → highest):

  Compiled defaults (in Config::default())
    ↓
  System config (/etc/ocx/config.toml)
    ↓
  User config (~/.config/ocx/config.toml, XDG)
    ↓
  $OCX_HOME/config.toml
    ↓
  OCX_CONFIG_FILE env var  ─┐
  --config FILE CLI flag   ─┤ (explicit paths, LAYER on top of discovered tiers)
    ↓                       │
  Environment variables (OCX_*)
    ↓
  CLI flags (--offline, --remote, etc.)

  OCX_NO_CONFIG=1 → prune the DISCOVERED chain only (system/user/$OCX_HOME).
                    Explicit paths (--config / OCX_CONFIG_FILE) still load.
  OCX_CONFIG_FILE="" → treated as unset (escape hatch for shell-exported vars).
```

**Three file tiers by design**:
- **System** (`/etc/ocx/config.toml`) — machine-wide defaults set by sysadmins, Dockerfiles, or provisioning tools. Useful for Docker images (`COPY config.toml /etc/ocx/config.toml`) and managed environments where users should not need to configure OCX.
- **User** (`$XDG_CONFIG_HOME/ocx/config.toml` or `~/.config/ocx/config.toml`) — personal defaults. Separate from data (`~/.ocx/`) following XDG convention.
- **OCX_HOME** (`$OCX_HOME/config.toml`) — co-located with the data directory. Supports the "redistributable OCX_HOME" use case (#25): zip up `~/.ocx/` and it works offline, including its config.

**`OCX_CONFIG_FILE` env var**: Equivalent to `--config` but injectable via environment. Critical for CI/Docker where you control env vars but not CLI flags (e.g., GitHub Actions setting env vars for all steps, Docker `ENV` directives, setup scripts wrapping `ocx`). When set, replaces all file tier discovery — only the specified file is loaded.

### Config File Format (v1)

```toml
# $OCX_HOME/config.toml — minimal v1 format

[registry]
# Default registry for bare identifiers like `cmake:3.28`.
# Overridden by OCX_DEFAULT_REGISTRY env var.
default = "ocx.sh"
```

That's it for v1. The `[registry]` section holds **global registry settings**; right now it has only one field (`default`), but it's the natural home for future global registry settings (timeout, retry policy, default insecure mode, etc.).

**Why `[registry] default` and not top-level `default_registry`**: matches Cargo's convention. The `[registry]` section is the namespace for global registry-subsystem settings. Future fields (`[registry] timeout = "30s"`, etc.) slot in cleanly.

**Why `offline` and `remote` are NOT in the config**: both are per-invocation modes, not persistent settings.
- `offline` blocks network for one run. Setting it persistently would silently break `ocx install` on a fresh setup.
- `remote` skips the local index for one run. It's a debugging/CI mode, not a default.

These remain CLI flags and env vars only.

### Future Format Considerations (NOT in v1)

The infrastructure is designed so these can be added later without breaking changes. Decisions locked in now to prevent painful migrations:

**Named registry tables — use the plural form `[registries.<name>]`**

When per-registry settings are needed (rewrites, per-registry insecure flag, custom auth), use `[registries.<name>]` (plural), **not** `[registry.<name>]` (singular). This avoids a TOML collision with the singular `[registry]` section that holds global registry settings — TOML cannot have both `[registry]` (plain table) and `[registry.foo]` (sub-table) of the same key.

This matches Cargo exactly (`[registry]` for `default`, `[registries.<name>]` for named entries). Locking in the plural now means no rename later.

```toml
# Future shape (not in v1):
[registry]
default = "ghcr"
timeout = "30s"

[registries.ghcr]
url = "ghcr.io"
insecure = false

[registries.private]
url = "registry.company.com"
location = "mirror.company.com"
```

**`$VAR` string interpolation — DO NOT ADD. Use named indirection instead.**

OCX's existing dead-code config schema already had the right pattern:

```toml
auth = { type = "env", token = "CUSTOM" }
```

The string `"CUSTOM"` is an **env var name** resolved at runtime in `auth.rs`, not a literal token. This is the **Named Indirection Pattern** (also used by Cargo `credential.helper`, AWS CLI `credential_process`). It is strictly safer than `${VAR}` interpolation:

| Concern | `$VAR` interpolation | Named indirection |
|---------|---------------------|-------------------|
| Logged config leaks secrets | Risk | Safe (only var names in file) |
| Escape sequences (`$$`) | Required | None |
| Schema clarity | Implicit/global | Explicit per-field |
| Parser complexity | High | Zero |
| Secret-scanner false positives | Yes | No |

Cargo rejected `$VAR` interpolation in `config.toml` for the same reason ([issue #10789](https://github.com/rust-lang/cargo/issues/10789)): TOML is a data format, not a template language. **OCX adopts the same stance.** When a future field needs an env var reference, give it a typed wrapper (e.g., `token_env = "MY_TOKEN"`) — never sigils.

**`include` key — DEFERRED INDEFINITELY.**

Multi-tier file model (system + user + `$OCX_HOME` + `OCX_CONFIG_FILE` + future project) already provides layering for free. Cargo took years to stabilize `include`; OCX's automation-first target users don't need it. If enterprise users later request shared config bundles, the path is Cargo's exact model: array of `{ path, optional }`, paths relative to including file, including file wins, no globs, cycle detection. Documented here so we don't re-litigate.

**CWD walk for project-level `ocx.toml` — DEFERRED to #33, but loader must accommodate it now.**

Design recommendation when #33 lands (locked in here so the implementation doesn't drift):

- **Walk start**: CWD at invocation
- **Walk stop**: filesystem root, with optional `OCX_CEILING_PATH` env var (mise pattern) to cap the walk for CI/Docker
- **Load strategy**: NEAREST `ocx.toml` only (uv model), not all files in the chain. Composition across tiers happens via system/user/OCX_HOME, not via stacked project files. Sidesteps the "project silently overrides infra" problem.
- **No `.git` boundary**: no surveyed tool stops there; it breaks nested workspaces
- **Position in precedence**: between `$OCX_HOME/config.toml` and `OCX_CONFIG_FILE` (project beats data-dir, explicit beats project)

The v1 loader must be designed so adding the project tier later requires only new code in `discover_paths()`, not a rewrite. See "Loader Extension Points" below.

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Named registry tables (`[registry.<name>]`) over arrays (`[[registry]]`) | Enables surgical override at closer tiers; matches Cargo convention; the current `[[registry]]` is unused anyway |
| Three file tiers (system + user + OCX_HOME) | System for Docker/managed envs, user for personal defaults (XDG), OCX_HOME for redistributable stores. Low marginal cost once loader exists |
| `OCX_CONFIG_FILE` env var | CI/Docker need config injection via env, not CLI flags. Equivalent to `--config` but environment-injectable |
| Env vars always win over config files | Matches Cargo/uv; critical for CI where env vars are the primary config mechanism |
| `OCX_NO_CONFIG=1` kill switch | CI reproducibility — uv pattern; ensures locked workflows ignore ambient config |
| Sync I/O for config loading | Config files are < 1 KB; async adds complexity with zero benefit. Use `std::fs::read_to_string` |
| `config` submodule stays private; `Config`, `ConfigInputs`, `ConfigLoader` re-exported at crate root | Minimum public API surface — `ocx_cli` imports only the three types it needs via `ocx_lib::{Config, ConfigInputs, ConfigLoader}`; all submodule internals stay internal. Avoids the `pub(crate)` path-qualifier smell. |
| TOML format (not YAML/JSON) | Industry consensus for dev tooling config; already used by existing dead-code `Config` |
| Sync I/O for config loading | Config files are < 1 KB; async adds complexity with zero benefit. Use `std::fs::read_to_string` |
| Explicit paths (`--config`, `OCX_CONFIG_FILE`) layer on top of the discovered chain | Gives four orthogonal CI modes from two primitives (default / layer / hermetic-plus-file / hermetic-empty). Avoids pip/uv footguns where a kill switch silently suppresses explicit paths. `OCX_NO_CONFIG=1` prunes the discovered chain only; explicit paths still load. Empty `OCX_CONFIG_FILE=""` is an escape hatch treated as unset. |
| No `deny_unknown_fields` on `Config` root | Forward compatibility — new sections (e.g., `[registries.<name>]`, `[patches]`, `[clean]`, `[toolchain]`) should not break existing configs. Unknown fields are silently ignored at the root level. Sub-structs like `RegistryGlobals` use `deny_unknown_fields` where the schema is tight |

### Env Var Boundary

Each existing `OCX_*` env var and whether it gets a config equivalent:

| Env Var | Config Equivalent | Rationale |
|---------|-------------------|-----------|
| `OCX_HOME` | N/A | Determines *where* config lives — cannot be in config |
| `OCX_CONFIG_FILE` | N/A (meta) | Points to explicit config file; equivalent to `--config` but env-injectable for CI/Docker |
| `OCX_NO_CONFIG` | N/A (meta) | Disables all file-based config; CI reproducibility |
| `OCX_DEFAULT_REGISTRY` | `[registry] default` | Primary use case for config file — persistent default |
| `OCX_OFFLINE` | **No** | Per-invocation mode, not a persistent setting. Config equivalent would silently break fresh installs |
| `OCX_REMOTE` | **No** | Per-invocation mode (debug/CI), not a persistent setting. Defeats offline-first local index if persisted |
| `OCX_INSECURE_REGISTRIES` | Deferred | Per-registry config will live under future `[registries.<name>] insecure = true`. v1 keeps the env var only |
| `OCX_NO_UPDATE_CHECK` | No | CI-only concern, env var sufficient |
| `OCX_NO_MODIFY_PATH` | No | Security/install concern, env var sufficient |
| `OCX_INDEX` | No (use `--index` flag) | Path override, not a persistent setting |

### Merge Strategy

Shallow precedence merge following the Cargo model. Tiers are loaded in order (system → user → OCX_HOME) and merged sequentially:
- **Scalars** (strings, booleans, numbers): nearest (highest-precedence) value wins
- **Tables** (`[registry.<name>]`): merged key-by-key across tiers; inner keys use nearest-wins
- **`[patches]`**: highest-precedence `[patches]` section wins entirely (no merge — a site's patch config is atomic). Note: the patches ADR describes `$OCX_HOME/config.toml` as the primary home for `[patches]`. In the multi-tier model, a user-tier `[patches]` can override the `$OCX_HOME` tier's `[patches]`. This is intentional — a developer may need to point at a different patch registry for local testing. However, this means care is needed when project-level config is added later (#33): a project's `[patches]` would replace infra-operator patches entirely. **Decision deferred to #33 scope**: whether project-level `[patches]` should merge with or replace the infra tier.

When `OCX_CONFIG_FILE` or `--config` is set, the specified file **layers on top of** the discovered chain (system + user + OCX_HOME). To suppress the discovered chain, use `OCX_NO_CONFIG=1` — explicit paths still load. This gives CI full control via two orthogonal primitives: the kill switch for ambient config, and the explicit path for a known override.

### Component Contracts

#### `Config` struct (redesigned)

```rust
// crates/ocx_lib/src/config.rs

/// Root config struct. No `deny_unknown_fields` — unknown top-level sections
/// are silently ignored for forward compatibility (future sections like
/// `[patches]`, `[clean]`, `[toolchain]` should not break existing configs).
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Config {
    /// `[registry]` section — global registry-subsystem settings.
    pub registry: Option<RegistryGlobals>,

    /// `[registries.<name>]` named entries — per-registry settings.
    /// In v1 each entry only defines `url`, giving `[registry] default` a
    /// lookup target. Future per-registry fields (`insecure`, `location`,
    /// `timeout`, auth) slot into the same `RegistryConfig` struct.
    pub registries: Option<HashMap<String, RegistryConfig>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGlobals {
    /// Default registry for bare identifiers (e.g., `cmake:3.28` → `<default>/cmake:3.28`).
    /// Overridden by `OCX_DEFAULT_REGISTRY` env var. May be either a literal
    /// hostname or the name of a `[registries.<name>]` entry.
    pub default: Option<String>,
}

// crates/ocx_lib/src/config/registry.rs — re-exported as `RegistryConfig`

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// Registry hostname this entry resolves to.
    pub url: Option<String>,
}
```

**v1 defines two config fields**: `Config.registry.default` and `Config.registries[name].url`. Resolution via `Config::resolved_default_registry()` walks from the default name through the named map to a hostname, falling back to the literal name when no entry matches (backwards-compat with bare hostnames). Future per-registry fields extend `RegistryConfig` without breaking existing configs.

#### `ConfigLoader` (new) — designed for future CWD walk

The loader is split into **discovery** and **loading** to make adding the project tier (#33) a pure additive change. These three constraints (from CWD-walk research) cost nothing now and eliminate rewrite risk later:

1. **Tier ordering as `Vec<PathBuf>`** internally — not hardcoded N-tier logic. Adding a tier = inserting into the vec.
2. **Discovery separated from loading** — `discover_paths()` returns the ordered list; `load_and_merge()` does I/O.
3. **CWD passed in as parameter** — never call `std::env::current_dir()` inside the loader. Caller controls walk root; loader is testable without filesystem side effects.

```rust
// crates/ocx_lib/src/config/loader.rs

pub struct ConfigLoader;

/// Inputs to config discovery — captures all caller-provided context.
pub struct ConfigInputs<'a> {
    /// `--config FILE` CLI flag (highest priority among explicit paths)
    pub explicit_path: Option<&'a Path>,
    /// CWD for future project tier walk (#33). Pass None in v1 — unused.
    pub cwd: Option<&'a Path>,
}

impl ConfigLoader {
    /// Top-level entry: build the ordered path list, load, and merge.
    ///
    /// Layering (lowest → highest precedence):
    /// 1. Discovered tiers (system, user, $OCX_HOME) — skipped when `OCX_NO_CONFIG=1`
    /// 2. `OCX_CONFIG_FILE` — if set and non-empty; layers on top of discovered
    /// 3. `--config FILE` — layers on top of everything else
    ///
    /// Explicit paths always load (even under `OCX_NO_CONFIG=1`); the kill
    /// switch only prunes the discovered chain. Missing explicit files are
    /// an error. Empty `OCX_CONFIG_FILE=""` is treated as unset (escape hatch).
    ///
    /// Uses sync I/O — config files are < 1 KB.
    pub fn load(inputs: ConfigInputs<'_>) -> Result<Config>;

    /// Discover the ordered list of config files to load (lowest precedence first).
    ///
    /// v1 returns: [system_path, user_path, home_path] (filtering None / nonexistent).
    /// Future #33 will append project_path (CWD walk result) before any explicit override.
    ///
    /// Pure function: no I/O beyond `Path::exists()`. Easily mockable in tests.
    pub fn discover_paths(inputs: &ConfigInputs<'_>) -> Vec<PathBuf>;

    /// Load and merge an ordered list of config files (lowest precedence first).
    /// Missing files at this stage are an error — discovery should have filtered them.
    /// Pure I/O + parse + merge; no discovery logic.
    pub fn load_and_merge(paths: &[PathBuf]) -> Result<Config>;

    /// System config: `/etc/ocx/config.toml`
    pub fn system_path() -> PathBuf;

    /// User config: `$XDG_CONFIG_HOME/ocx/config.toml` or `~/.config/ocx/config.toml`
    pub fn user_path() -> Option<PathBuf>;

    /// OCX_HOME config: `$OCX_HOME/config.toml`
    pub fn home_path() -> Option<PathBuf>;
}
```

**Future extension hook (#33)**: adding the project tier is purely additive — implement `fn project_path(cwd: &Path) -> Option<PathBuf>` (CWD walk for `ocx.toml`), insert its result into `discover_paths()` after `home_path()`. No existing function changes signature.

#### `Config::merge()` (for multi-tier layering)

```rust
impl Config {
    /// Merge `other` into `self` (other has higher precedence).
    /// Scalars: other wins if present (Some). Tables: merged key-by-key.
    /// `[patches]`: other's patches replace self's entirely (atomic section).
    pub fn merge(&mut self, other: Config);
}
```

#### Integration into `Context::try_init()`

```rust
// In Context::try_init():
// 1. Load config (respects OCX_NO_CONFIG, --config flag)
// 2. Apply config values as defaults
// 3. Env vars override config values
// 4. CLI flags override env vars
let config = ConfigLoader::load(ConfigInputs {
    explicit_path: options.config.as_deref(),
    cwd: None,  // v1 doesn't use CWD; #33 will pass Some(&std::env::current_dir()?)
})?;
let default_registry = env::string(
    "OCX_DEFAULT_REGISTRY",
    config.registry
        .as_ref()
        .and_then(|r| r.default.clone())
        .unwrap_or_else(|| oci::DEFAULT_REGISTRY.into()),
);
```

### User Experience Scenarios

| Action | Expected Outcome | Error Cases |
|--------|-----------------|-------------|
| `ocx install cmake:3.28` with `[registry] default = "ghcr.io"` in `$OCX_HOME/config.toml` | Resolves `ghcr.io/cmake:3.28` instead of `ocx.sh/cmake:3.28` | Config parse error → clear error with file path and line number |
| `OCX_DEFAULT_REGISTRY=ocx.sh ocx install cmake:3.28` with same config | Env var wins → resolves `ocx.sh/cmake:3.28` | — |
| `OCX_NO_CONFIG=1 ocx install cmake:3.28` | Discovered chain ignored, pure env var + CLI flag behavior | — |
| `OCX_NO_CONFIG=1 ocx --config /path/to/custom.toml install cmake:3.28` | Discovered chain suppressed, explicit file still loads (hermetic + explicit override) | — |
| `ocx --config /path/to/custom.toml install cmake:3.28` | Discovered chain loads, then `--config` file layers on top | File not found → `error: config file not found: /path/to/custom.toml (check --config or OCX_CONFIG_FILE)` |
| `OCX_CONFIG_FILE=/path/to/ci.toml ocx install cmake:3.28` | Discovered chain loads, then env-var file layers on top | File not found → same error |
| `OCX_CONFIG_FILE="" ocx install cmake:3.28` | Empty env var treated as unset (escape hatch); discovered chain loads normally | — |
| Both `OCX_CONFIG_FILE` and `--config` set | Both load; `--config` sits at the top and wins on conflicts | — |
| System config sets `[registry] default`, user config overrides it | User tier wins (higher precedence) | — |
| Config file with unknown top-level key `[foo]` | Silently ignored (forward compat) | — |
| Config with unknown field inside `[registry]` e.g. `[registry] foo = "bar"` | `deny_unknown_fields` on `RegistryGlobals` → parse error | `error: unknown field 'foo' in [registry] at $OCX_HOME/config.toml` |
| Config file with invalid TOML syntax | Parse error with line/column | `error: invalid TOML at $OCX_HOME/config.toml:5:3: expected '='` |
| Config with `[registries.foo]` table (future feature, not implemented in v1) | Silently ignored (unknown root field, forward compat) | — |
| No config file exists | `Config::default()` used, all values from env vars / compiled defaults | — |

### Error Taxonomy

| Error | Trigger | Remediation |
|-------|---------|-------------|
| `Config::Parse { path, source }` | Invalid TOML syntax, missing required field, or `deny_unknown_fields` violation in sub-struct | Show file path + line/column from `toml::de::Error` |
| `Config::FileNotFound { path }` | `--config` or `OCX_CONFIG_FILE` points to nonexistent file | `"config file not found: {path} (check --config or OCX_CONFIG_FILE)"` |
| `Config::Io { path, source }` | Permission denied, unreadable file, non-regular file (e.g. directory) | Show path + OS error chain (via `{err:#}` in `main.rs`) |
| `Config::FileTooLarge { path, size, limit }` | Config file exceeds the 64 KiB safety cap | `"config file {path} exceeds maximum allowed size ({size} bytes > {limit} bytes); OCX config files are typically under 1 KiB — did you point at the wrong file?"` |

Missing files at system/user/OCX_HOME tiers are **not errors** — silently skipped. Only explicit paths (`--config` or `OCX_CONFIG_FILE`) pointing to a missing file are errors (explicit path = user intent).

### Edge Cases

1. **`OCX_HOME` is a symlink** — `home_path()` follows the symlink (use canonical path for display, raw path for loading)
2. **Config file is empty** — valid TOML, produces `Config::default()` with all `None` fields
3. **Config file is a directory** — IoError, not ParseError
4. **`OCX_NO_CONFIG=1` with `--config` flag** — discovered chain suppressed, explicit file still loads (hermetic + explicit override pattern)
5. **`OCX_NO_CONFIG=1` with `OCX_CONFIG_FILE`** — same: discovered chain suppressed, env-var file still loads
6. **Both `OCX_CONFIG_FILE` and `--config` set** — both load; `--config` sits at the top of the layered chain and wins on conflicting scalars
7. **Windows paths** — `dirs::config_dir()` returns `%APPDATA%\ocx\config.toml`; `$OCX_HOME` already cross-platform
8. **`XDG_CONFIG_HOME` set to relative path** — resolve relative to CWD (per XDG spec)
9. **System config exists but is world-writable** — not our problem (OS security), but document that system config should be root-owned

## Implementation Steps

### Phase 1: Stubs

- [ ] **Step 1.1:** Redesign `Config` struct (minimal v1)
  - Files: `crates/ocx_lib/src/config.rs`
  - Public API: `Config` (root), `RegistryGlobals` (`[registry]` section with `default` field). Both with serde derives.
  - Replace existing `Config`/`RegistryConfig`/`AuthenticationConfig` (all dead code)
  - Note: `Config` root has NO `deny_unknown_fields`; `RegistryGlobals` HAS it

- [ ] **Step 1.2:** Create `ConfigLoader` with discovery/loading split
  - Files: `crates/ocx_lib/src/config/loader.rs`
  - Public API: `ConfigInputs` struct, `ConfigLoader::load(inputs)`, `discover_paths(inputs)`, `load_and_merge(paths)`, `system_path()`, `user_path()`, `home_path()`
  - **Critical**: discovery and loading MUST be separate functions; CWD MUST be passed in via `ConfigInputs`, not read inside the loader. This is what enables future CWD walk (#33) without rewrite.

- [ ] **Step 1.3:** Add config error variants
  - Files: `crates/ocx_lib/src/config/error.rs`
  - Public API: `Error::FileNotFound`, `Error::Io`, `Error::FileTooLarge` variants (keep existing `Parse`)

- [ ] **Step 1.4:** Wire `Config` into `Context`
  - Files: `crates/ocx_cli/src/app/context.rs`
  - Public API: `Context` gains `config: Config` field, `try_init()` calls `ConfigLoader::load()`

Gate: `cargo check` passes with all new types defined, bodies `unimplemented!()`.

### Phase 2: Architecture Review

Review stubs match this design record. Verify:
- Config struct fields match the TOML format spec above
- `ConfigLoader::load()` signature supports the precedence chain
- Error types cover all documented failure modes
- `Context` integration point is clean

### Phase 3: Specification Tests

- [ ] **Step 3.1:** Unit tests for Config parsing
  - Files: `crates/ocx_lib/src/config.rs` (inline `#[cfg(test)]`)
  - Cases:
    - Parse minimal config (just `[registry] default = "x"`)
    - Parse empty file → `Config::default()` with `registry = None`
    - Unknown top-level key `[foo]` → silently ignored (no `deny_unknown_fields` on root)
    - Unknown future section `[registries.foo]` → silently ignored (forward compat)
    - Unknown future section `[patches]` → silently ignored (forward compat)
    - Unknown field in `[registry]` (e.g., `[registry] foo = "bar"`) → rejected (`deny_unknown_fields` on `RegistryGlobals`)

- [ ] **Step 3.2:** Unit tests for Config merging and accessors
  - Files: `crates/ocx_lib/src/config.rs` (inline `#[cfg(test)]`)
  - Cases:
    - `Config::default()` has `registry = None`
    - Merge two configs: higher-precedence `[registry] default` overrides
    - Merge with `None` fields: lower-precedence values preserved
    - Merge two configs where both have `[registry]` but only one has `default` set → preserved correctly

- [ ] **Step 3.3:** Unit tests for ConfigLoader discovery (pure, no I/O)
  - Files: `crates/ocx_lib/src/config/loader.rs` (inline `#[cfg(test)]`)
  - Cases for `discover_paths()`:
    - All tiers present → returns `[system, user, home]` in order
    - Some tiers missing → only existing files in returned vec
    - No tiers exist → empty vec
    - Explicit path set → returns just that path
  - Cases for `load()`:
    - `OCX_NO_CONFIG=1` with no explicit path → returns `Config::default()`
    - `OCX_NO_CONFIG=1` with explicit `--config` → loads only the explicit file (discovered chain suppressed)
    - `OCX_NO_CONFIG=1` with `OCX_CONFIG_FILE` → loads only the env-var file (discovered chain suppressed)
    - `OCX_CONFIG_FILE=""` empty string → treated as unset (escape hatch)
    - No config files exist at any tier → `Config::default()`
    - Only system config exists → loads system config
    - System + user configs exist → merged, user wins on conflicts
    - System + user + OCX_HOME configs → merged, OCX_HOME wins
    - Explicit `--config` to missing file → error (`FileNotFound`)
    - Explicit `--config` layers on top of the discovered chain
    - `OCX_CONFIG_FILE` layers on top of the discovered chain
    - Both `OCX_CONFIG_FILE` and `--config` → both load; `--config` wins on conflicts

- [ ] **Step 3.4:** Acceptance tests for config behavior
  - Files: `test/tests/test_config.py`
  - Scenarios:
    - `[registry] default = "x"` changes default registry resolution
    - `OCX_DEFAULT_REGISTRY` overrides config file
    - `OCX_NO_CONFIG=1` ignores all config files
    - Invalid config file produces clear error message with file path
    - `--config` flag loads specified file
    - `OCX_CONFIG_FILE` env var loads specified file

Gate: Tests compile and fail with `unimplemented`.

### Phase 4: Implementation

- [ ] **Step 4.1:** Implement `Config` struct redesign and serde
  - Files: `crates/ocx_lib/src/config.rs`
  - Details: Replace existing dead-code structs with minimal v1 (`Config` + `RegistryGlobals`). Implement `merge()` with field-by-field logic for `RegistryGlobals.default`.

- [ ] **Step 4.2:** Implement `ConfigLoader` with discovery/loading split
  - Files: `crates/ocx_lib/src/config/loader.rs`
  - Details: `discover_paths()` returns ordered `Vec<PathBuf>`. `load_and_merge()` reads + parses + merges. `load()` orchestrates: handles `OCX_NO_CONFIG`, `--config`, `OCX_CONFIG_FILE`, then delegates. Sync I/O. Error reporting with file paths.

- [ ] **Step 4.3:** Wire into `Context::try_init()`
  - Files: `crates/ocx_cli/src/app/context.rs`
  - Details: Load config (sync), use `config.registry.default` from config as fallback for `default_registry`. Env vars still override config. CLI flags still override env vars.

- [ ] **Step 4.4:** Update test data config.toml
  - Files: `crates/ocx_lib/test/data/config.toml`
  - Details: Update to new format (`[registry] default = "..."`)

Gate: All tests pass. `task verify` succeeds.

### Phase 5: Review & Documentation

- [ ] **Step 5.1:** Spec compliance review
- [ ] **Step 5.2:** Code quality review
- [ ] **Step 5.3:** Documentation
  - **New page**: `website/src/docs/reference/configuration.md` — full configuration reference
    - All v1 config keys (`[registry] default`) with field descriptions
    - Future format considerations (`[registries.<name>]`, `[patches]`) clearly marked as not-yet-implemented
    - File locations and discovery algorithm (system → user → OCX_HOME)
    - Merge precedence rules with examples
    - Env var overrides table (which env var maps to which config field)
    - `OCX_NO_CONFIG`, `OCX_CONFIG_FILE` controls
    - Example configs for common scenarios (private registry, Docker, CI)
  - **Update**: `website/src/docs/user-guide.md` — new "Configuration" section
    - Overview of config layers (env vars, config files, CLI flags)
    - How precedence works (env vars > CLI flags > config files, with tier ordering)
    - Link to the configuration reference page for details
  - **Update**: `website/src/docs/getting-started.md` — mention config file locations
    - Where config files live (`$OCX_HOME/config.toml`, `/etc/ocx/config.toml`, `~/.config/ocx/config.toml`)
    - Link to configuration reference for setup details

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/config.rs` | Rewrite | Replace dead-code Config with new design |
| `crates/ocx_lib/src/config/error.rs` | Modify | Add FileNotFound, Io, ConflictingOptions error variants |
| `crates/ocx_lib/src/config/loader.rs` | Create | Config discovery (3 tiers + explicit) and merge; designed for future CWD walk extension |
| `crates/ocx_cli/src/app/context.rs` | Modify | Load config in `try_init()`, wire into subsystems |
| `crates/ocx_lib/test/data/config.toml` | Rewrite | Update to new named-table format |
| `test/tests/test_config.py` | Create | Acceptance tests for config behavior |
| `website/src/docs/reference/configuration.md` | Create | Configuration reference page |
| `website/src/docs/user-guide.md` | Modify | Add Configuration section with overview |
| `website/src/docs/getting-started.md` | Modify | Mention config file locations |

## Dependencies

### Code Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| `toml` | (already in workspace) | TOML parsing |
| `serde` | (already in workspace) | Deserialization |
| `dirs` | latest | XDG path resolution for user config (`dirs::config_dir()`) |

`dirs` is a small, well-maintained crate with minimal transitive deps. Also fixes the deprecated `std::env::home_dir()` usage in `file_structure.rs` and the dead-code `Config::user_path()`.

### Service Dependencies

None — config is purely local filesystem.

## Testing Strategy

### Unit Tests (from component contracts)

| Component | Behavior | Expected | Edge Cases |
|-----------|----------|----------|------------|
| `Config` parsing | Deserialize valid TOML | Populated struct fields | Empty file, unknown top-level keys silently ignored, unknown sub-fields rejected |
| `Config::merge()` | Layer two configs | Higher-precedence wins | Scalar override, `None` preservation |
| `Config` defaults | `Config::default()` fields | `registry = None` | — |
| `ConfigLoader::discover_paths()` | Pure discovery, no I/O | Ordered `Vec<PathBuf>` of existing files | All tiers, partial tiers, no tiers, explicit override |
| `ConfigLoader::load()` | End-to-end orchestration | Merged config | Missing tiers skipped, `OCX_NO_CONFIG`, `OCX_CONFIG_FILE`, `--config`, conflicts |
| `ConfigLoader::user_path()` | XDG resolution | `~/.config/ocx/config.toml` | `XDG_CONFIG_HOME` set, Windows path |
| `Context::try_init()` | Config feeds defaults | `default_registry` from `config.registry.default` | Env var overrides config, CLI flag overrides both |

### Acceptance Tests (from user experience)

| User Action | Expected Outcome | Error Cases |
|-------------|------------------|-------------|
| Config with `[registry] default = "ghcr.io"` | Short identifiers resolve against ghcr.io | Parse error → file path + line |
| `OCX_DEFAULT_REGISTRY=ocx.sh` overrides config | Env var wins | — |
| `OCX_NO_CONFIG=1` | Config files ignored | — |
| `OCX_NO_CONFIG=1 --config /path` | Discovered chain suppressed, explicit file still loads | — |
| `--config /missing` | Clear error message | `error: config file not found: /missing (check --config or OCX_CONFIG_FILE)` |
| `OCX_CONFIG_FILE=/path/to/ci.toml` | CI config layers on top of discovered chain | Missing file → error |
| Multi-tier: system + user configs | Both loaded, user overrides system on conflict | — |
| Future-section `[registries.foo]` in config | Silently ignored (forward compat) | — |

## Risks

| Risk | Mitigation |
|------|------------|
| Config format becomes public contract — hard to change later | No `deny_unknown_fields` on root (unknown sections silently ignored); all fields `Option` for forward compat |
| Dead code `Config` struct has no users — clean break | Correct; the `[[registry]]` format was never loaded by any production code path |
| Config loading adds startup latency | Sync reads of < 1 KB files; bail immediately if `OCX_NO_CONFIG=1` |
| `dirs` crate adds a dependency | Small, well-maintained; also fixes deprecated `std::env::home_dir()` usage |
| Three tiers may confuse users about where to put config | Documentation explains each tier's purpose and precedence clearly |

## Checklist

### Before Starting

- [x] Research completed ([research_configuration_patterns.md](./research_configuration_patterns.md))
- [x] Related ADRs reviewed (patches, dependencies)
- [x] Related issues reviewed (#33, #35)
- [ ] Plan approved by human

### Before PR

- [ ] All tests passing
- [ ] `task verify` succeeds
- [ ] Config file format documented
- [ ] Self-review complete

---

## Deferred Findings (require human judgment)

1. **Env vars without config equivalents**: `OCX_NO_UPDATE_CHECK`, `OCX_NO_MODIFY_PATH`, `OCX_INDEX` remain env-var-only. If users request config equivalents, they can be added as `Option` fields without breaking changes.

2. **When to add `[registries.<name>]` named tables**: Locked in to use the plural form when added, but the trigger is unclear. Options: (a) when the patches feature lands (per-registry insecure flag), (b) when `OCX_INSECURE_REGISTRIES` users request migration, (c) only when registry-rewriting is implemented. Currently waiting for the first concrete consumer.

3. **`OCX_CEILING_PATH` documentation**: The future CWD-walk design references it. Should we document this env var name as reserved now (so users don't squat on it), or wait until #33?

## Progress Log

| Date | Update |
|------|--------|
| 2026-04-12 | Plan created via swarm-plan |
| 2026-04-12 | Review Round 1: spec-compliance (4 actionable, 2 deferred) + architecture (5 actionable, 2 deferred). Initially simplified to 2 file tiers per arch reviewer. |
| 2026-04-12 | Human review: restored system + user tiers (Docker/CI need fixed paths, env var injection). Added `OCX_CONFIG_FILE` env var. Added documentation deliverables. |
| 2026-04-12 | Human review: refocused plan on **config infrastructure**, not config content. Stripped `[patches]`, `[registries.<name>]`, registry rewrites — these defer until backing features land. The only v1 config field is `[registry] default`. Restructured `[registry]` as Cargo-style singular section (matches future plural `[registries.<name>]` for named tables). |
| 2026-04-12 | Research integrated (4 parallel sub-agents): named tables (Cargo split locked in), `$VAR` interpolation (rejected — use named indirection), `include` key (deferred indefinitely), CWD walk (deferred to #33 but loader designed with 3 critical hooks: tier-as-Vec, discovery/loading split, CWD as parameter). |
| 2026-04-12 | **Mid-implementation redesign (human review during `/swarm-execute`):** (1) flipped `OCX_NO_CONFIG` semantics from "kill everything" to "prune discovered chain only"; explicit paths (`--config`, `OCX_CONFIG_FILE`) still load even under the kill switch. Gives four orthogonal CI modes from two primitives and avoids pip/uv footguns. (2) Empty `OCX_CONFIG_FILE=""` added as escape hatch (treated as unset) for shell-exported vars. (3) Scoped `[registries.<name>] url` into v1 as a live feature — resolution path via `Config::resolved_default_registry()` ships now so future per-registry fields (`insecure`, `location`, `timeout`, auth) slot into a stable entry shape. Removed `ConflictingOptions` error variant; added `FileTooLarge` variant. |
| 2026-04-13 | **Post-review fix pass (`/swarm-review sion`):** (1) bounded `Read::take(MAX+1)` in `load_and_merge` to harden against `/proc/self/mem` et al. where `metadata.len()` is 0 but reads are unbounded. (2) Replaced hand-rolled `EnvGuard` with project's blessed `crate::test::env::EnvLock` (no `unsafe`, process-wide mutex). (3) Removed dead `config: Config` field from `Context`; extracted `default_registry: String` is the only thing kept. (4) Reverted `pub mod config` to private `mod config` + crate-root re-exports of `Config`, `ConfigInputs`, `ConfigLoader` — matches OCX visibility convention. (5) `main.rs` error print changed `{err}` → `{err:#}` so `anyhow` walks the source chain (fixes all chained errors, not just config). (6) Error-message polish: lowercase `invalid boolean string`, `FileNotFound` hint mentioning `--config`/`OCX_CONFIG_FILE`, `FileTooLarge` hint about typical <1 KiB, debug trace for empty `OCX_CONFIG_FILE`. (7) `Config::merge` registries loop switched to `or_default().merge()` (zero clone). (8) Six new unit tests (FileTooLarge, non-regular-file, three-tier precedence, registries cross-tier merge, OCX_HOME fallback) + one acceptance test (named registry with no url falls back to literal name). |
