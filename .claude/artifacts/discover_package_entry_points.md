# Discover Notes: Package Entry Points (Issue #61)

Phase 1 output from the /swarm-plan high pipeline. Consumed by the opus
architect in Phase 4 and the reviewers in Phase 6. File:line pointers are
authoritative; prose is summary.

## End-to-End Flow Map

### 1. Metadata → Schema → Validation

- `Metadata` enum: `crates/ocx_lib/src/package/metadata.rs:18` — tagged union, currently only `Bundle` variant (`#[serde(tag = "type", rename_all = "snake_case")]`)
- `Bundle` struct: `crates/ocx_lib/src/package/metadata/bundle.rs:36` — fields `version`, `strip_components`, `env: Env`, `dependencies: Dependencies`
- Optional-field pattern: `#[serde(skip_serializing_if = "…", default)]` + `#[derive(schemars::JsonSchema)]`
- Named-item pattern precedent (for `entry_points: [{name, target}]`):
  - `Dependencies` wrapper: `crates/ocx_lib/src/package/metadata/dependency.rs:126-155` — custom Serialize/Deserialize, validates via `Dependencies::new()`
  - Uniqueness checks + error type: `dependency.rs:132-154`, error at `dependency.rs:199-211` (`DependencyError::{DuplicateIdentifier, InvalidAlias, DuplicateAlias}`)
  - **Slug regex** already in use: `^[a-z0-9][a-z0-9_-]*$` at `dependency.rs:12-13` + `28-36`; parse-time `TryFrom<String>` at `dependency.rs:53-60` — **directly reusable for entry-point `name` validation**
- Schema generation: `crates/ocx_schema/src/main.rs:30-34` → `website/src/public/schemas/metadata/v1.json` via `task schema:generate` (triggered by `task website:build`, see `website/schema.taskfile.yml:11-21`)
- Publish-time validation: `crates/ocx_cli/src/command/package_create.rs:77-79` → `ValidMetadata::try_from(metadata)?`
- `ValidMetadata::try_from`: `crates/ocx_lib/src/package/metadata.rs:47-143` — currently validates `${deps.NAME.FIELD}` tokens; **`entry_points` target validation slots in here**
- Error type: `crates/ocx_lib/src/package/error.rs:14-34` (`package::Error::EnvVarInterpolation` wraps `template::TemplateError`)

### 2. Install Pipeline (ordered stages)

From `crates/ocx_lib/src/package_manager/tasks/pull.rs` and `install.rs`:

1. Resolve via index → `ResolvedChain` — `resolve.rs:209`
2. Singleflight dedup check — `pull.rs:187-209`
3. Find plain (cross-process gate) — `pull.rs:238`
4. Acquire exclusive temp dir lock — `pull.rs:259`
5. Post-lock recheck — `pull.rs:268`
6. Pull manifest + metadata — `pull.rs:286-292`
7. Extract layers to `layers/` in parallel — `pull.rs:310-311`
8. Create `refs/layers/` forward-refs — `pull.rs:326-330`
9. **Assemble package content via hardlinks — `pull.rs:342-344`** (`assemble_from_layers(&sources, &pkg.content()).await`)
10. **Content placement complete** ← **LAUNCHER HOOK POINT**
11. `post_download_actions` writes `resolve.json`, `install.json`, `digest` — `pull.rs:355`
12. Create `refs/deps/` forward-refs — `pull.rs:361-367`
13. Create `refs/blobs/` forward-refs — `pull.rs:370-373`
14. Atomic temp → `packages/` move — `pull.rs:376-377`
15. `install.rs:35,82,87` — `rm.link()` creates candidate + optionally `current` symlinks

**Preferred launcher hook**: after `pull.rs:344` (content assembled) and before `pull.rs:355` (`post_download_actions`). Launchers land in temp dir; atomic move (step 14) carries them into `packages/`.

### 3. `ocx exec` Command Path

- Entry: `crates/ocx_cli/src/command/exec.rs:38` — `Exec::execute()`
- Clap `Exec` struct `exec.rs:12-35`:
  - `packages: Vec<options::Identifier>` positional, unbounded, terminates at `--`
  - `command: Vec<String>` positional after `--`
  - `interactive`, `clean`, `platforms` flags
- Flow:
  1. `exec.rs:40` — transform `Identifier` via `context.default_registry()`
  2. `exec.rs:42` — `context.manager()` → `PackageManager`
  3. `exec.rs:43` — `manager.find_or_install_all(identifier, platforms).await?` (auto-installs if online)
  4. `exec.rs:45` — `manager.resolve_env(&info).await?` → `Vec<EnvEntry>`
  5. `exec.rs:46-47` — build `Env` (inherit or clean), apply entries
  6. `exec.rs:52-68` — spawn child with resolved env
- **Seam for path-mode**: before `find_or_install_all` call at `exec.rs:40-43`
- `resolve_env`: `crates/ocx_lib/src/package_manager/tasks/resolve.rs:209-312` — takes `&[InstallInfo]` (needs `identifier` + `metadata` + `resolved` + `content`)
- **Blocker for path-only variant** (`accumulator.rs:18-42`): `DependencyContext` requires `oci::PinnedIdentifier`. From a path alone, reconstructing `PinnedIdentifier` requires reading `metadata.json`+`resolve.json` — feasible but new helper needed.

### 4. `ocx env` Command Path

- `crates/ocx_cli/src/command/env.rs:24-36` — clap struct
- `env.rs:41-49` — identifier transform, then either `find_symlink_all` (if `--candidate`/`--current`) or `find_or_install_all`
- `env.rs:51-59` — `manager.resolve_env(&info)`, build report; same env-compose path as `exec`

### 5. `ocx select` + PATH / Shell Profile

- Select: `crates/ocx_cli/src/command/select.rs:28-56`
  - `select.rs:35` — `manager.find_all(identifiers, platforms)` (no auto-install)
  - `select.rs:40-41` — `fs.symlinks.current(&info.identifier)` → `rm.link(&current_path, &info.content)`
- `SymlinkStore.current` layout: `crates/ocx_lib/src/file_structure/symlink_store.rs:56` → `{root}/symlinks/{registry}/{repo}/current` → points at `packages/.../content/`
- **Shell profile is separate from `select`**:
  - `shell profile add` command wires to `ProfileManager::add_all()` — `crates/ocx_lib/src/profile/manager.rs:69-93`
  - Symlink creation in profile: `profile/manager.rs:149-170`
  - Profile loading (shell init): `crates/ocx_cli/src/command/shell_profile_load.rs:25-61`
- Shell-profile PATH today: per-package install dir, driven by metadata `env` entries (e.g., `PATH=${installPath}/bin`). **No single `~/.ocx/bin` aggregator exists.**
- 9 supported shells: Ash, Ksh, Dash, Bash, Zsh, Elvish, Fish, Batch, PowerShell (`crates/ocx_lib/src/shell.rs:10-31`)
- Export generation: `Shell::export_path(k,v)` / `export_constant(k,v)` — per-shell syntax at `shell.rs:118-141`
- Shell detection: `Shell::detect` → process tree walk via `sysinfo` → `SHELL` env fallback (`shell.rs:34-103`)

## Reusable Surfaces (with file:line)

| Surface | Location | Purpose |
|---|---|---|
| Env template resolver (from #60) | `crates/ocx_lib/src/package/metadata/env/accumulator.rs:45` | Resolves `${installPath}` + `${deps.NAME.installPath}` against `DependencyContext` |
| Structured exporter variant | `crates/ocx_lib/src/package/metadata/env/exporter.rs:28` | Returns `Vec<Entry>` (key,value,kind) — the right shape for resolving launcher `target` |
| Template resolver (raw) | `crates/ocx_lib/src/package/metadata/template.rs:23-104` | Standalone `TemplateResolver` with `${installPath}` + `${deps.NAME.field}` substitution |
| `FileStructure` composite | `crates/ocx_lib/src/file_structure.rs:35` | `packages.path(id)`, `packages.content(id)` — pattern for adding sibling: `packages.path(id).join("entrypoints")` |
| `PackageDir` accessors | `crates/ocx_lib/src/file_structure/package_store.rs:21-76` | Add `entrypoints()` method mirroring `content()` |
| `PackageStore` store-level methods | `package_store.rs:106-150` | Add `entrypoints(&id)` + `entrypoints_for_content(content_path)` |
| `PackageStore::metadata_for_content` | `package_store.rs` | Canonicalizes content path → reads sibling `metadata.json` (key helper for path-mode CLI) |
| `PackageStore::resolve_for_content` | `package_store.rs` | Same for `resolve.json` |
| `ReferenceManager::link` | `crates/ocx_lib/src/reference_manager.rs:64` | Idempotent symlink create/update with GC back-refs |
| `ReferenceManager` install-lifecycle calls | `install.rs:35,82,87` | Existing pattern for `current`/candidate symlink management |
| `symlink::update` | `crates/ocx_lib/src/symlink.rs:75-99` | Atomic symlink replace — low-level |
| Slug regex + validator | `dependency.rs:12-13,28-36` | Reuse for entry-point `name` validation |
| `Dependencies::new()` uniqueness check | `dependency.rs:132-154` | Pattern for deserialization-time collision detection |

## Hook Points (for design)

- **Launcher generation**: `crates/ocx_lib/src/package_manager/tasks/pull.rs:355` (call site of `post_download_actions`). Insert new step after content hardlink (line 344) and before `post_download_actions` (355). Files written into temp `pkg.entrypoints()` and carried by atomic move at `pull.rs:376-377`.
- **CLI path-mode branch for `ocx exec`**: `crates/ocx_cli/src/command/exec.rs:40-43` — before `find_or_install_all`. Constructs `InstallInfo` by reading `metadata.json` + `resolve.json` via `PackageStore::{metadata_for_content, resolve_for_content}` — no Index/Client init.
- **PATH surface via `ocx select`**: `crates/ocx_cli/src/command/select.rs:40-41` — after `current` symlink update. Options: (a) add second symlink `entrypoints-current` → `packages/.../entrypoints/` wired into shell profile; (b) extend shell profile to add `${installPath}/../entrypoints` as PATH entry.
- **Validation of `entry_points` targets**: `crates/ocx_lib/src/package/metadata.rs:47-143` (`ValidMetadata::try_from`). Validate name slug + template tokens reference declared deps + resolved target path stays under `${installPath}` or a dep's `installPath` (no absolute escape).

## Gaps / Unknowns for Architect to Resolve

1. **`InstallInfo` reconstruction from path** — today `InstallInfo` is built from index resolution. A path-only reconstruction is feasible (metadata + resolve.json on disk) but the `resolved: ResolvedChain` field was shaped for index data. Architect must decide:
   - introduce `PathOnlyInstallInfo` variant / struct, or
   - make `resolved` optional / stub it when loaded from path, or
   - bypass `InstallInfo` entirely and build a `Vec<EnvEntry>` directly from `resolve.json`.

2. **PATH selection model** (Tension 2 from issue):
   - Model A: one stable symlink `symlinks/{registry}/{repo}/entrypoints-current` → `packages/.../entrypoints/`; shell profile adds that one dir to PATH.
   - Model B: per-install PATH entry via metadata `env` augmentation — fragile (relative traversal `${installPath}/../entrypoints`).
   - Model C: one global `~/.ocx/entrypoints/` dir populated on `select`, holding symlinks to per-install launchers. Simpler PATH wiring, but collision resolution must be explicit (which package wins).
   - Architect must pick and document.

3. **Atomic swap semantics** on `ocx select`: if entry points exist, changing selection should swap the visible launcher set atomically. `ReferenceManager::link` is single-symlink atomic; a directory-of-symlinks swap needs either (i) atomic renaming of a `entrypoints-current` symlink (Model A, easy) or (ii) iterative per-launcher symlink updates under a lock (Model C, more work).

4. **Windows specifics** (Tension 3): `.cmd` files on PATH are discoverable via `PATHEXT`. `.ps1` needs `powershell -File` or explicit association. MSYS/Git Bash sees `.cmd` but argument translation is tricky (`;` vs `:` PATH sep, path prefixing). Architect must fix the launcher generation matrix: Unix `.sh`, Windows `.cmd` primary, `.ps1` TBD.

5. **`ocx exec` CLI contract** (Tension 1): positional overload vs `--install-dir` flag. **No existing precedent** in OCX for positional path-or-identifier disambiguation. All existing commands parse positional as identifier first.

6. **CAS invariant re: non-content-addressed files in `packages/`**: Launchers contain absolute `installPath` — they are NOT content-addressed. Confirmed: no explicit CAS enforcement gate in `package_store` would reject them. GC reachability is package-rooted; a sibling `entrypoints/` dir is safe as long as launchers don't create outgoing symlinks into `layers/` or `blobs/` (launchers are plain script files, so this is safe).

7. **Correlation with #25 (portable OCX home)** (Tension 5): Option A (overloaded positional for `ocx exec`) is the only one that also works when `OCX_HOME` is portable — the launcher passes an absolute install-dir path it discovers via `$0`/`%~dp0`, and that path may be outside the canonical `OCX_HOME`. Option B (`--install-dir`) composes equally well but is more explicit.

## Confirmed Non-Issues

- **GC**: sibling `entrypoints/` under `packages/.../{digest}/` is protected by package root reachability. No GC changes needed.
- **Atomic move**: launchers in temp dir move to final location atomically with the rest of the package.
- **Schema/JsonSchema**: straightforward extension; follow Dependencies pattern; add `#[derive(schemars::JsonSchema)]`.
- **Template resolution**: no new resolver needed; reuse extracted `Exporter`/`TemplateResolver` from #60.
