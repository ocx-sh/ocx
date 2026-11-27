# Research: which ocx code builds selections, package directories, environments and launchers, and what each takes as a parameter

## Metadata

**Date:** 2026-11-25
**Lane:** codebase read (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ, in the main checkout at `f41c6684` (`origin/main`). The worktree of this discussion is at `22dbac1a`, 42 commits behind, and has no export or import command, so it was not the basis. A subagent read the code. The orchestrator re-read on main: `PathLane` and `ComposePaths::install_path_for`, `EnvResolver`, `ContentState`, `RenderRequest`, `toolchain_surface`, `TrampolineTarget`, `exec_env.rs` in full, `lock_pins`, the `dest_override` parameter and its test, `assemble_from_layers`, `EnvEntry`, `ModifierKind`, the signature of `exposed_names`, and which commands call the shared selection. A line marked [older tree] was read by the subagent at `22dbac1a` and not re-read on main. Section 9 was read by the orchestrator alone, later the same day.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Nothing was run. Not read: `package_export.rs`, the bodies of `exposed_names`, `reconcile_bin`, `reconcile_links`, `deferred_root`, `prepare_lazy` and `materialize_lazy`, `app::project_context`, `exec_env/assemble.rs`, `ocx_config::exec_state`, the helpers of `move_to_front`, the shell emitters, the patches tier, a field-by-field path audit of the `inspect` report, and every Windows-only path. Section 8 does not list every site that depends on the home.

## 1. Selection to resolved packages

There is no single function. The project tier and the package tier differ, and export has a third route.

- Project tier: `ocx_project::select_tool_set(config, lock, groups, positionals) -> Vec<SelectedTool>` (`crates/ocx_project/src/compose.rs:150`), `check_duplicate_selection` (`:250`) and `resolve_selected_tools(&[SelectedTool], &Platform) -> Vec<ResolvedTool>` (`:283`). `compose_tool_set` (`:306`) chains the three.
  - Callers, counted by a search on main: `crates/ocx_cli/src/command/toolchain_env.rs`, `toolchain_exec.rs` and `inspect.rs`.
  - Not callers (zero matches): `export.rs`, `import.rs` and `pull.rs`.
- Export route: `ocx export` selects with its own `lock_pins(tools, groups, platforms)` (`crates/ocx_cli/src/command/export.rs:67`). `ocx import` uses the same function.
- Package tier: identifiers go to `PackageManager::compose_roots(requests, platform, materialization, concurrency) -> ComposeRoots` (`crates/ocx_package_manager/src/composer.rs:1059`).
  - `Materialization::Install` pulls on a miss, `Symlink` reads through an install link, and `LocalOnly` leaves out what is absent (`composer.rs:1080-1100`).
  - A lazy request needs metadata only and writes shims (`composer.rs:1132` onward).
- Closure from metadata alone: `PackageManager::toolchain_surface(roots, platform) -> Vec<ClosureNode>` (`crates/ocx_package_manager/src/tasks/render_toolchain.rs:158`). Its comment says "its metadata-only dependency closure, deps before dependents". The inspect commands use `inspect_all(identifiers, platform, InspectOptions{resolve, closure})`.
- Platform: the `-p` value through `conventions::platform_or_default` (`crates/ocx_cli/src/conventions.rs:119`), else the host's. A caller can name a platform that is not the host's.

## 2. Package directories

- Pipeline: `PackageManager::pull` (`crates/ocx_package_manager/src/tasks/pull.rs:80`), `setup_owned(mgr, pinned, resolved, platform, groups, dest_override: Option<&Path>, provided_metadata)` (`pull.rs:424`) and `pull_local(…, dest_override: Option<&Path>)` (`crates/ocx_package_manager/src/tasks/pull_local.rs:80`).
- The destination is a parameter. `dest_override` reaches the move into place (`pull.rs:822-824`). With it set, the already-installed check is skipped (`pull.rs:500`, `:528`) and the entry-point launcher bakes that root (`pull.rs:642`). A test asserts that the package then sits at the override and not under `$OCX_HOME/packages/` (`pull_local.rs:547-570`).
- Assembly: `assemble_from_layers(sources: &[&Path], dest_content: &Path)` and `assemble_from_layers_with_layouts` (`crates/ocx_store/src/file_structure/assemble.rs:142`, `:153`).
  - A regular file is hardlinked from `layers/…/content` at one call site (`assemble.rs:578`, `hardlink::create_in_existing_parent`). Links are recreated verbatim.
  - There is no copy mode. A hardlink across devices fails with `CrossesDevices` (`assemble.rs:130`).
- What a package directory holds beside `content/` (subagent's table; the writers in `pull.rs`):

| Sibling | Absolute path inside |
|---|---|
| `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest` | no |
| `entrypoints/` (from `launcher::generate`) | yes: the package root, in the script body and in the Windows sidecar |
| `refs/layers/`, `refs/deps/`, `refs/blobs/` | yes: links to absolute store paths |
| `refs/symlinks/` | yes: a back-reference to the install link |
| `refs/origins/`, `refs/verified/` | no |

## 3. Environments

- Composition for a selection: `PackageManager::resolve_env_with_attribution(packages, self_view, scope, platform) -> (Vec<Entry>, usize, Vec<PatchProvenance>, AdmittedClaims)` (`crates/ocx_package_manager/src/tasks/resolve.rs:650` [older tree]). A project's `[env]` comes from `project_env_entries(config, config_path, groups)` (`crates/ocx_project/src/compose.rs:22`).
- Composition for one package: `resolve_env` (`resolve.rs:612` [older tree]), `composer::compose`, and `emit_package_vars(metadata, content: &Path, …)` (`crates/ocx_package_manager/src/composer.rs:510`).
- Install paths have one producer: `ComposePaths::install_path_for(&PackageDir, PathLane) -> PackageDir` (`composer.rs:893`). Its type comment says "The composition's single install-path producer". `PathLane` has two values (`composer.rs:769-775`): `Following`, the rendered toolchain link when it is trusted and else the digest root, and `Digest`, the digest root always.
- Template resolution: `EnvResolver::new(install_path: &Path, dep_contexts)` (`crates/ocx_package/src/metadata/env/resolver.rs:41`).
  - `${installPath}` resolves through `TemplateResolver::new(self.install_path, …)`.
  - A relative `path` value is joined to `install_path` and passed through `dunce::simplified`.
  - The resolver does not check that `install_path` is absolute or canonical.
- Content state: `ContentState::{Materialized, Deferred}` (`resolver.rs:26`), set with `with_content_state`. The probe for a `required` path runs only under `Materialized`. `Deferred` is documented as "Nothing under `content/` exists until the first invocation".
- Entry: `Entry { key, value, kind, separator }`. `ModifierKind` (`crates/ocx_package/src/metadata/env/modifier.rs:71-77`): `Path` is prepended, `Constant` replaces, `List` is appended and joined by its separator.
- Applying entries to a process: `exec_env::compose(inherited, spawn) -> Projected` (`crates/ocx_package_manager/src/exec_env.rs:177`) and `apply_child_env(inherited, &ChildEnvInputs) -> ChildEnvironment` (`:464`), with the modes of `ExecMode` (`:24`). Callers: `exec.rs`, `toolchain_exec.rs` and `launcher/exec.rs`. List separators are reconciled in `reconcile_list_separators` (`crates/ocx_package/src/metadata/env/apply.rs:86`). Section 9 describes the step in full.
- The same entries are rendered three ways: as JSON through `api::data::env::EnvVars` (`crates/ocx_cli/src/api/data/env.rs`), as shell lines through `conventions::emit_lines(shell, entries)` (`crates/ocx_cli/src/conventions.rs:151`) and `Shell::export_path`, `export_constant` and `export_list` (`crates/ocx_shell/src/shell.rs`), and as CI files through `export_ci` (`conventions.rs:371`).

## 4. Launchers

- Entry-point launchers: `launcher::generate(pkg_root: &Path, entries: &Entrypoints, dest: &Path, shim_bin: &ShimBinStore)` (`crates/ocx_package_manager/src/launcher/generate.rs:26`). The baked root, the names and the target directory are parameters.
- Toolchain launchers: `PackageManager::render_toolchain(request: RenderRequest) -> RenderReport` (`crates/ocx_package_manager/src/tasks/render_toolchain.rs:202`).
  - `RenderRequest { home: &ToolchainHome, scope, lock, surface: &[ClosureNode], groups, pinned, platform, dry_run, unrendered }` (`render_toolchain.rs:49-81`). Its comment says "the rendered tree is a pure function of it plus the store root".
  - `dry_run` is documented as "Report the delta and write nothing".
  - The launcher directory is `home.shell_bin(DEFAULT_SHELL)` (`render_toolchain.rs:525`).
- Launcher names: `exposed_names(nodes: &[ClosureNode], policy: NotEnumerablePolicy) -> BTreeMap<BinaryName, NameOwner>` (`crates/ocx_package_manager/src/tasks/toolchain_names.rs:55`), from the closure's metadata. A node that claims neither `binaries` nor entry points is `ShimNamesNotEnumerable` under the refusing policy. On a collision the last node walked wins and the others are recorded as shadowed (from `.claude/rules/subsystem-package-manager.md`; the body was not read).
- What a trampoline bakes: `TrampolineTarget::{Project(PathBuf), Global}` (`crates/ocx_package_manager/src/launcher/body.rs:42`), "the only thing it bakes", and the path of `ocx` (`generate.rs:175`).

## 5. Export and import

- Export is at blob level: `DIR/blobs`, `DIR/index` and `DIR/ocx-export.json`. It writes no unpacked package.
- `ocx export` (`crates/ocx_cli/src/command/export.rs:35`) calls `lock_pins` and `PackageManager::export` (`crates/ocx_package_manager/src/tasks/export.rs:103`). It uses no composer, environment or launcher code.
- `ocx import` (`crates/ocx_cli/src/command/import.rs:30`) calls `import_project` (`crates/ocx_package_manager/src/tasks/import.rs:116`), which pulls each leaf through the pull path of section 2 with a blob lookup and an offline index. It then renders the toolchain through `render_toolchain`.
- Both take a lock under `$OCX_HOME/locks`. Import registers the project under `$OCX_HOME/projects/` (`import.rs:150`).

## 6. Reports

- `ocx env --format json`: `EnvVars` with `items`, `binaries`, `entrypoints`, `integrations` and `advisories`. An item is `EnvEntry { key, value, kind, separator, source }` (`crates/ocx_cli/src/api/data/env.rs`). `separator` is present on a `list` item only, `source` under `--show-patches` only. `value` holds an absolute path for a `path` item and for a constant that embeds an install path.
- `ocx inspect` and `ocx package inspect`: `InspectReport { platform, packages, env }` (`crates/ocx_cli/src/api/data/package_inspect.rs:996`). Environment entries of a closure carry no value, because `${installPath}` is "concrete only after install" (`package_inspect.rs:1002`).

## 7. Layouts

Read from `.claude/rules/subsystem-file-structure.md` on main.

- Package store: `{root}/packages/{registry_slug}/{algorithm}/{2hex}/{30hex}/`. The repository is not in the path, so two repositories that resolve to one digest share a directory.
- Rendered toolchain home: `{root}/toolchain/` with `links/<group>/<entry>/` (a directory link to a package root, one per selected group), `shells/default/bin/` (one launcher per exposed name, for the default group only) and `active` (a link to `shells/default/`).
  - `ToolchainHome` is a value type. The rule says "A project's home is the same tree at a different root".
  - `shells/` holds exactly one entry today. A selection per shell is [ocx-sh/ocx#363](https://github.com/ocx-sh/ocx/issues/363), open and not designed.

## 8. What depends on the home's absolute path

- The entry-point launcher and its Windows sidecar bake the package root (`generate.rs:37`, `pull.rs:642-645`).
- A trampoline bakes the path of `ocx` and the project root (`render_toolchain.rs:538-542`, `body.rs:74`, `:111`).
- `refs/deps`, `refs/layers` and `refs/blobs` are links to absolute store paths (`pull.rs:1297`, `:1320`, `:683`).
- `content/` is hardlinked from the home's `layers/` (section 2).
- On Windows a launcher is a hardlink into `$OCX_HOME/.bin/ocx-shim/` (`generate.rs:232`).
- The install path is the digest root or a toolchain link, both under the home (`composer.rs:769-775`, `:893`).
- `ocx launcher exec` refuses a package root outside the home's packages root or a scratch root (`crates/ocx_cli/src/command/launcher/exec.rs:47-56`).
- The value of `OCX_BINARY_PIN` is chosen relative to `$OCX_HOME` (`generate.rs:88-127`).

## 9. Applying an environment to a process

Read on main: `crates/ocx_package_manager/src/exec_env.rs` in full (514 lines), `crates/ocx_package/src/metadata/env/apply.rs:1-162`, `crates/ocx_config/src/env.rs:1-128`, and the two string functions named below.

The step has three layers.

- Primitives, on `ocx_config::env::Env` (`crates/ocx_config/src/env.rs`):
  - `set` (`:74`) replaces the value.
  - `add_path` (`:79`) prepends. An occurrence that is already present moves to the front, and empty segments are dropped. The string work is `ocx_util::path::move_to_front` (`crates/ocx_util/src/path.rs:133`).
  - `add_list` (`:92`) appends. An occurrence that is already present moves to the back, and an empty value changes nothing. The string work is `ocx_util::list::append_unique` (`crates/ocx_util/src/list.rs:23`).
  - A key is folded to upper case on Windows (`EnvKey`, `:13`).
- The mapping from an entry's kind to a primitive. It exists twice: `apply_value` over `ComposedValue` (`exec_env.rs:290`) and `EnvEntriesExt::apply_entries` over `Entry` (`apply.rs:27`). Both also note a composed `PATH` directory with `note_package_path`.
- Process policy, in `exec_env::compose` (`exec_env.rs:177`):
  - The mode (`ExecMode`, `:24`): on top of the caller's environment, the same after reverting what ocx applied before, or an empty environment.
  - `remove_unforwarded` (`:247`) removes credentials, hook bookkeeping and the received state from the child's environment.
  - `set_binary_pin` (`:255`) sets or removes `OCX_BINARY_PIN`.
  - `record_of` and `fold_into` (`:326`, `:401`) build the record of applied values. `project` (`:426`) encodes it into `__OCX_ENV_STATE`.

Dependencies:

- Both string functions take and return standard-library string types. `path.rs` imports only `std::ffi` at module level, and `list.rs` imports nothing.
- `ocx_config` links `ocx_oci`, `ocx_trust`, `ocx_util`, `ocx_exit` and `ocx_env`, and `base64`, `chrono`, `flate2`, `futures`, `http` and `url` among others (`crates/ocx_config/Cargo.toml`).
- The launcher crate may link no workspace crate: `ocx_shim = []` (`scripts/crate_map.toml:112`). Its manifest lists `windows-sys` and `dunce`, for Windows only.

Credentials:

- Five variables are declared with `propagation = Credential` (`crates/ocx_env/src/vars.rs:443-489`): `OCX_IDENTITY_TOKEN`, `OCX_KEY_PASSWORD`, `OCX_SIGNING_KEY`, `OCX_TELEMETRY_HEADERS` and `OCX_ANNOUNCE_GIT_TOKEN`.
- The registry calls them "The names a spawned child must not inherit" (`crates/ocx_env/src/lib.rs:529`).

## What this shows

- Parameters exist for the destination of a package directory, the root of a rendered toolchain home and the install path given to the template resolver.
- Single producers exist for the install paths of a composition and for the launcher names of a closure.
- A closure and its launcher names can be computed from metadata alone, and the template resolver has a state for content that is not on disk.
- Three things do not exist: a link mode in assembly, an install-path lane for a path under an output directory, and a launcher kind that starts the tool without `ocx`.
- Applying an environment to a process has three layers (section 9). The lowest one is two string functions that use the standard library only. They sit in crates with many dependencies, and the launcher crate may link no workspace crate today.
- `ocx exec` removes five credential variables from a child's environment, replaces the execution state and sets the binary pin. A program that starts a tool without `ocx` does none of that unless it is built to.
- `ocx export` and `ocx import` select with their own function and not with the one that `env`, `exec` and `inspect` share.

## What this does not show

- That a relative install path passes every later step of the composer. Only the resolver was read; nothing was run.
- That the launcher stays inside its size budget of 512 KiB with one more crate linked.
- Whether the shell emitters treat a value that is already present as `Env` does. The emitters were not read.
- How the patches tier reaches the composer.
- Anything about Windows.
