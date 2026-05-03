---
layout: doc
outline: deep
---
# Command Line

## General Options

The following options are available for all commands and must be specified before the command name.

### `--log-level` {#arg-log-level}

The log level for OCX, which can be set to one of the following values:

- `off`: No logs will be emitted.
- `error`: Only error messages will be emitted.
- `warn`: Warning messages and error messages will be emitted.
  This is the default log level.
- `info`: Informational messages, warning messages, and error messages will be emitted.
- `debug`: Debug messages, informational messages, warning messages, and error messages will be emitted.
- `trace`: All messages will be emitted, including trace messages.

### `--format` {#arg-format}

When set, ocx will output information in the specified format instead of plain text.
Supported formats are:

- `plain` (default): Human-readable plain text.
- `json`: Machine-readable JSON format.

The available data depends on the command being executed.

### `--offline` {#arg-offline}

Disables all network access for this invocation. Tag→digest resolution must
be satisfied by the local index or by a digest-pinned identifier; unpinned
tags missing from the local index error immediately rather than triggering
a registry query. Useful for hermetic CI runs and air-gapped environments.

::: warning
Running `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` (without a prior
online install) fails with an `OfflineManifestMissing` error that names the missing digest.
`ocx index update` writes only tag→digest pointers; it does not download manifest or layer blobs.
Run `ocx install <pkg>` online first to populate the blob cache, then offline installs will work.
:::

### `--remote` {#arg-remote}

Routes mutable lookups (tag list, catalog, tag→manifest resolution) to the
remote registry instead of the local index. Pure-query commands
([`ocx index list`](#index-list), [`ocx index catalog`](#index-catalog),
[`ocx package info`](#package-info)) do **not** persist the result to the
local index — to refresh the persistent snapshot, run
[`ocx index update`](#index-update) explicitly. Implies network access.

Digest-addressed reads (manifests and layers already identified by a content
digest) still consult the local index first and write newly fetched blobs
through to `$OCX_HOME/blobs/` — content-addressed data is immutable, so
caching is safe regardless of mode.

Combining this flag with [`--offline`](#arg-offline) is **accepted** as
"pinned-only mode" — see [Pinned-only mode](#pinned-only-mode) below.

### Pinned-only mode {#pinned-only-mode}

Setting both [`--offline`](#arg-offline) and [`--remote`](#arg-remote)
together produces a deliberately strict mode: no source contact, no local
writes, and any tag-addressed resolution that cannot be satisfied locally
errors instead of silently falling back. The CLI emits an `info` log to
confirm the mode is active.

Use it in CI to assert every project dependency is digest-pinned:

```sh
ocx --offline --remote exec -- my-build-script
```

If any tool resolution falls back to a floating tag, the command fails — a
hermetic-build sanity check without round-tripping to the registry.

### `--index` {#arg-index}

Override the path to the [local index][fs-index] directory for this invocation.
By default, ocx reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`).

```shell
ocx --index /path/to/bundled/index install cmake:3.28
```

This flag is intended for environments where the [local index][fs-index] is bundled alongside a
tool rather than living inside `OCX_HOME` — for example inside a [GitHub Action][github-actions-docs],
[Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features] that ships a frozen
index snapshot as part of its release.

The flag has no effect when [`--remote`](#arg-remote) is set.

The same override can be set persistently via the [`OCX_INDEX`][env-ocx-index] environment
variable. The `--index` flag takes precedence when both are set.

### `--quiet` {#arg-quiet}

Alias: `-q`.

Suppresses the structured stdout report that every command emits — tables in plain
mode, the JSON document in `--format json` mode. Errors, warnings, and progress
spinners continue to surface on stderr.

Quiet is opt-in and orthogonal to [`--format`](#arg-format). Use it when calling
ocx as a step in a larger pipeline that only cares about the exit code, or when
chaining commands where intermediate output would clutter logs.

```shell
# Pre-warm the project store in CI without dumping a table per package.
ocx --quiet pull
```

The same toggle is available via the [`OCX_QUIET`][env-ocx-quiet] environment
variable; the flag wins when both are set.

### `--jobs` {#arg-jobs}

Caps the number of root packages pulled in parallel. Applies to every command
that fans out through `pull_all` — `install`, `pull`, `package pull`, `exec`
(when it auto-installs missing tools), and the env-composition path of `env`.

The cap acts on the **outer dispatch only**: transitive dependencies and OCI
layer extraction stay unbounded so a child pull never deadlocks waiting for a
permit held by its own ancestor. Singleflight dedup and per-package file locks
already protect the registry against duplicate work.

| Value | Meaning |
|-------|---------|
| (unset) | Unbounded. Every root package spawns immediately — legacy behavior. |
| `0` | Use all logical cores (matches [GNU `parallel -j 0`][gnu-parallel-j0]). |
| `N > 0` | Cap at `N` concurrent root pulls. |
| Negative | Rejected at parse time. |

OCX intentionally diverges from Cargo on `--jobs 0`: GNU Parallel's "saturate
this machine" convention is more useful in CI matrices where the runner has a
variable CPU count and the user wants the cap computed for them.

The same value can be set persistently via [`OCX_JOBS`][env-ocx-jobs]. The CLI
flag wins when both are set.

```shell
# Cap parallelism on a constrained runner.
ocx --jobs 2 install cmake:3.28 ripgrep:14
```

### `--color` {#arg-color}

Controls when to use <Tooltip term="ANSI colors">Escape sequences defined by ECMA-48 / ISO 6429, supported by virtually all modern terminals.</Tooltip> in output.

- `auto` (default): Enable colors when stdout is a terminal and
  [`NO_COLOR`][env-no-color] is not set.
- `always`: Always emit color codes, even when piped.
- `never`: Disable all color output.

The `--color` flag takes the highest precedence over all
color-related environment variables ([`NO_COLOR`][env-no-color],
[`CLICOLOR`][env-clicolor], [`CLICOLOR_FORCE`][env-clicolor-force]).

### `--project` {#arg-project}

Path to the project-level `ocx.toml` (project-tier toolchain config).

When set, OCX reads this file as the project tier and skips the CWD walk entirely. Any filename is accepted (not just `ocx.toml`), which is useful for fixtures and integration tests.

The same override can be set persistently via the [`OCX_PROJECT`][env-project] environment variable. To disable project-file discovery entirely — including the `OCX_PROJECT` variable but not an explicit `--project` flag — set [`OCX_NO_PROJECT`][env-no-project]`=1`.

**Symlink policy:** Paths supplied via `--project` or `OCX_PROJECT` are trusted and followed through symlinks. Paths discovered by the CWD walk reject symlinks to prevent directory-traversal redirection.

**Error cases:** A missing explicit path exits with code 79 ([`NotFound`][exit-codes]). A path that exists but cannot be read (permission denied, not a regular file) exits with code 74 ([`IoError`][exit-codes]).

### `--config` {#arg-config}

Path to an extra [configuration file][config-ref] to load for this invocation.

```shell
ocx --config /path/to/config.toml install cmake:3.28
```

The file layers **on top of** the discovered tier chain — it does not replace it. Settings in the specified file win over system, user, and `$OCX_HOME/config.toml` values, but the discovered tiers still load first. To suppress the discovered chain entirely, combine with [`OCX_NO_CONFIG`][env-no-config]`=1`.

The specified file **must exist** — a missing path is an error ([exit code 79 / NotFound][exit-codes]). This is different from the three discovered tiers, which silently skip missing files.

The same override can be set persistently via [`OCX_CONFIG`][env-config]. When both are set, the `--config` file sits at highest file-tier precedence and wins on conflicting scalars.

See the [Configuration reference][config-ref] for the full precedence table, merge rules, and error messages.

## Exit codes {#exit-codes}

OCX exposes a stable, typed exit-code taxonomy so scripts can discriminate failures without parsing stderr.

Most package tools return 0 on success and 1 on any failure. That forces downstream scripts to either ignore the error category or grep stderr — both are fragile. A CI wrapper cannot distinguish "registry unreachable, retry in 30 seconds" from "package not found, fail the build" without parsing error text that can change.

OCX aligns with BSD [sysexits.h][sysexits-manpage] (codes 64–78) for the standard failure categories, and reserves 79–81 for OCX-specific cases. The numeric values are stable across releases — `case $?` works.

:::info
The sysexits.h convention originates in BSD Unix and is documented at [man.freebsd.org][sysexits-manpage]. It assigns semantic meaning to exit codes 64–78, leaving 79–127 free for tool-specific use. OCX occupies 79–81.
:::

| Code | Name | Mnemonic | When used | Recovery |
|------|------|----------|-----------|----------|
| 0 | Success | — | Successful completion | — |
| 1 | Failure | — | Generic failure — only when no specific code applies | Inspect stderr |
| 64 | UsageError | EX_USAGE | Bad CLI invocation: unknown flag, wrong argument count, invalid syntax | Check the command syntax |
| 65 | DataError | EX_DATAERR | Input data malformed: bad identifier, invalid digest, corrupted manifest | Validate identifiers and file contents |
| 69 | Unavailable | EX_UNAVAILABLE | Required resource unavailable: network down, registry unreachable | Retry; check network and registry URL |
| 74 | IoError | EX_IOERR | I/O error: filesystem permission denied, disk full, read/write failure | Check filesystem permissions and free space |
| 75 | TempFail | EX_TEMPFAIL | Temporary failure that may succeed on retry: rate limit, transient network | Retry with backoff |
| 77 | PermissionDenied | EX_NOPERM | Insufficient permissions: registry 403, filesystem EPERM | Refresh credentials or adjust filesystem permissions |
| 78 | ConfigError | EX_CONFIG | Configuration error: bad config file, missing required field, parse failure | Inspect the config file at the printed path |
| 79 | NotFound | OCX | Resource not found: package 404, explicit config path absent | Pin a different version or correct the path |
| 80 | AuthError | OCX | Authentication failure: registry 401, missing credentials | Refresh or set registry credentials |
| 81 | OfflineBlocked | OCX | Offline mode blocked a network operation (deliberate policy, not a fault) | Re-run online, or populate the local cache first |

Scripts can `case $?` on these stable values:

```shell
ocx install cmake:3.28
case $? in
    0)  echo "installed" ;;
    64) echo "usage error; check flags" ;;
    69) echo "registry unreachable; retry with backoff" ;;
    75) echo "temporary failure; retry later" ;;
    78) echo "bad config; inspect the config file" ;;
    79) echo "not found; pin a different version" ;;
    80) echo "auth failed; refresh credentials" ;;
    81) echo "offline mode active; re-run online" ;;
    *)  echo "unexpected failure (exit $?)"; exit 1 ;;
esac
```

### `--candidate` / `--current` {#path-resolution}

The `--candidate` and `--current` flags are available on commands that resolve a package's
location on disk, for example [`env`](#env), [`find`](#find), or [`shell env`](#shell-env).

Every mode returns a **package root** — the directory that contains the package's `content/` and
`entrypoints/` subdirectories alongside `metadata.json`, `manifest.json`, and the other per-package
files. The mode controls only the *shape* of the path that names that root.

By default these commands return the content-addressed path in the
[object store](../user-guide.md#file-structure-packages) — a hash-derived directory that changes
whenever the package is reinstalled at a different version. Use `--candidate` or `--current` to
resolve via a [stable install symlink](../user-guide.md#path-resolution) instead, whose path never
changes regardless of the underlying object. This is useful for paths embedded in editor configs,
Makefiles, or shell profiles that should survive package updates.

| Mode | Flag | Path returned |
|------|------|------|
| Object store (default) | _(none)_ | `~/.ocx/packages/…/{digest}/` |
| Candidate symlink | `--candidate` | `~/.ocx/symlinks/…/candidates/{tag}` |
| Current symlink | `--current` | `~/.ocx/symlinks/…/current` |

All three paths name the same package root: the install symlinks target the object-store package
directory directly. Consumers that need installed files traverse into `<root>/content/`, launcher
consumers traverse into `<root>/entrypoints/`, and metadata readers open `<root>/metadata.json`.

**Constraints**

- `--candidate`: the package must already be installed. Digest identifiers are rejected — use a tag identifier.
- `--current`: a version must be selected first (via [`select`](#select) or [`install --select`](#install)). Digest identifiers are rejected. The tag portion of the identifier is ignored — only registry and repository are used to locate the symlink.
- `--candidate` and `--current` are mutually exclusive.

## Commands

### `add` {#add}

Appends a tool binding to the nearest `ocx.toml`, resolves its digest into `ocx.lock`, and installs the package in one step.

The command locates the project `ocx.toml` by walking the directory tree from the current working directory upward (same discovery as [`ocx lock`](#lock) and [`ocx pull`](#pull)). It fails with exit code 64 if no `ocx.toml` is found — it does **not** scaffold one implicitly. To create a project file first, run [`ocx init`](#init).

After mutating `ocx.toml`, `ocx add` performs a partial lock update (preserving existing entries) followed by an install of the newly added tool.

**Usage**

```shell
ocx add [OPTIONS] <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Fully-qualified tool identifier to add (e.g. `ocx.sh/cmake:3.28` or `ghcr.io/acme/mytool:1.0`). Bare identifiers without a tag (e.g. `ocx.sh/cmake`) default to `:latest` — the written `ocx.toml` entry is always explicit (`cmake = "ocx.sh/cmake:latest"`), following the same convention as `docker pull`. See [Unit 3 bare-identifier default][user-guide-toml] for the design rationale.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--group <NAME>` | `-g` | Add the binding to a named group instead of the default `[tools]` table. Must be non-empty and contain only alphanumeric characters, `-`, or `_`. |
| `--help` | `-h` | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Binding added, lock updated, tool installed. |
| 64 | No `ocx.toml` found, binding already exists, or invalid `--group` name. |
| 69 | Registry unreachable while resolving the new tag. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock sentinel (`.ocx-lock`). Retry with backoff. |
| 78 | `ocx.toml` schema invalid or TOML parse error. |
| 79 | Tag not found in the registry. |
| 80 | Authentication failure against the registry. |

### `clean` {#clean}

Removes unreferenced objects from the local object store.

An object is unreferenced when nothing points to it — no candidate or current symlink, no other installed package depends on it, and no project's `ocx.lock` (registered in `$OCX_HOME/projects.json`) pins it. This happens after [`uninstall`](#uninstall) (without `--purge`) or when symlinks are removed manually. When a package with [dependencies][ug-dependencies] is removed, its dependencies may become unreferenced and are cleaned up in the same pass.

::: danger
Do not run `clean` concurrently with other OCX commands. A concurrent install may reference an object that `clean` is about to remove, causing the install to fail.
:::

**Usage**

```shell
ocx clean [OPTIONS]
```

**Options**

| Name | Short | Description | Default |
|------|-------|-------------|---------|
| `--dry-run` | — | Show what would be removed without making any changes. | false |
| `--force` | — | Bypass the project registry and collect packages held only by other projects' `ocx.lock` files. Live install symlinks are still honoured. | false |
| `--help` | `-h` | Print help information. | — |

**JSON output schema** (`--format json`)

`ocx clean --format json` emits an array of objects, one per candidate entry:

| Field | Type | Description |
|-------|------|-------------|
| `kind` | `"object"` \| `"temp"` | Storage tier of the entry. |
| `dry_run` | boolean | `true` when `--dry-run` was passed; `false` on a live run. |
| `path` | string | Absolute path to the package or temp directory. |
| `held_by` | array of strings | Absolute paths to `ocx.lock` files that pin this package. Populated only in dry-run mode, only for entries the registry retained (never collected). Empty array when nothing holds the entry. |

```json
[
  {
    "kind": "object",
    "dry_run": true,
    "path": "/home/alice/.ocx/packages/.../sha256/ab/cdef.../",
    "held_by": ["/home/alice/dev/proj-a/ocx.lock"]
  },
  {
    "kind": "object",
    "dry_run": true,
    "path": "/home/alice/.ocx/packages/.../sha256/12/3456.../",
    "held_by": []
  }
]
```

**Plain output**

Dry-run output is a table. When any entry has a non-empty `held_by`, the table gains a `Held By` column:

```
Type    Held By                     Path
object  /home/alice/dev/proj-a/ocx.lock  /home/alice/.ocx/packages/.../
object                              /home/alice/.ocx/packages/.../
temp                                /home/alice/.ocx/temp/abc.../
```

A blank `Held By` cell means the entry is unreferenced and will be collected. A populated cell lists the project(s) holding it. The `Held By` column is omitted when no entries are held. `temp` entries are never governed by the registry and never show a `Held By` value.

Non-dry-run output is always 2-column (`Type | Path`): held entries are never collected and therefore never appear.

### `deps` {#deps}

Shows the dependency tree for one or more installed packages. Operates on locally-present packages
only — no auto-install. See [Dependencies][ug-dependencies] in the user guide for
background.

**Usage**

```shell
ocx deps [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to inspect. Accepts multiple packages — when given more than one,
  the command builds the combined dependency graph (the same graph [`exec`](#exec) uses for environment
  composition).

**Options**

- `--flat`: Show the resolved evaluation order instead of the tree. This is the exact order
  [`exec`](#exec) and [`env`](#env) use for environment composition — useful for debugging
  unexpected variable values.
- `--why <DEP>`: Explain why a dependency is pulled in. Shows all paths from the given root
  packages to `<DEP>`. Mutually exclusive with `--flat`.
- `--depth <N>`: Limit tree depth. `--depth 1` shows direct dependencies only.
- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `-h`, `--help`: Print help information.

**Default output** is a logical tree showing declared dependencies. Diamond dependencies
(the same package reached via multiple paths) are marked with `(*)` and their subtree is
not expanded again:

```
myapp:1.0 (sha256:aaa1b2c3…)
├── ocx.sh/java:21 (sha256:bbb4e5f6…)
└── ocx.sh/cmake:3.28 (sha256:ccc7d8e9…)
    └── ocx.sh/gcc:13 (sha256:ddd0a1b2…)
```

**`--flat`** shows the combined evaluation order after topological sort and deduplication:

```
Package            Digest
ocx.sh/gcc:13      sha256:ddd0a1b2…
ocx.sh/cmake:3.28  sha256:ccc7d8e9…
ocx.sh/java:21     sha256:bbb4e5f6…
myapp:1.0          sha256:aaa1b2c3…
```

**`--why`** traces all paths from roots to a specific dependency:

```
myapp:1.0 → ocx.sh/cmake:3.28 → ocx.sh/gcc:13
```

### `deselect` {#deselect}

Removes the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its [candidate symlink][fs-symlinks] and object-store content remain intact. To also remove the installed files, use [`uninstall`](#uninstall).

When the deselected package declares [entry points][guide-entry-points], the launchers stop being reachable through `current/entrypoints/` as soon as the `current` symlink is removed. The symlink removal is idempotent — an already-absent link is not an error.

**Usage**

```shell
ocx deselect <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to deselect.

**Options**

- `-h`, `--help`: Print help information.

### `env` {#env}

Print the resolved environment variables for one or more packages.

Plain format outputs an aligned table with `Key`, `Type` and `Value` columns.
JSON format outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies).
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

**Usage**

```shell
ocx env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `-h`, `--help`: Print help information.

::: info Windows: synthetic `PATHEXT ⊳ .CMD`
On Windows, `env` prepends `.CMD` to `PATHEXT` in its output when the host shell's `PATHEXT` does not already include it. Generated entrypoint launchers are `.cmd` files; this prepend lets callers that adopt the printed env (e.g. by piping it into a child process) find launchers by bare name without further configuration. The host `PATHEXT` is left untouched if it already includes `.CMD`. `ocx exec` auto-injects the same prepend silently into the child environment.
:::

### `exec` {#exec}

Executes a command within the environment of one or more packages.

Each positional accepts a bare OCI identifier (e.g. `node:20`); identifiers are resolved through the index and auto-installed when missing (unless [`--offline`](#arg-offline) is set).

If a package declares [dependencies][ug-dependencies], their environment variables are applied in [topological order][ug-deps-env] before the package's own variables. Env entries layer in the order identifiers appear on the command line.

::: tip Generated launchers use `ocx launcher exec`, not `ocx exec`
Entry-point launchers generated by `ocx install` call the internal `ocx launcher exec '<pkg-root>' -- <argv0> [args...]` subcommand, not `ocx exec`. That subcommand validates the package root, forces the self view internally, and executes the resolved entrypoint. See the [Entry Points][entry-points] guide for the launcher ABI.
:::

**Usage**

```shell
ocx exec [OPTIONS] <PACKAGES>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<PACKAGES>`: Bare OCI identifiers (e.g. `node:20`).
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

- `-p`, `--platform`: Specify the platforms to use.
- `--clean`: Start with a clean environment containing only the package-defined variables, instead of inheriting the current shell environment. Resolution-affecting `OCX_*` variables (binary path, offline, remote, config file, index) are still written explicitly from the running ocx's parsed state — see [OCX Configuration Forwarding][env-composition-forwarding].
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries. Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `-h`, `--help`: Print help information.

::: info Stdin always inherits
`ocx exec` always inherits the parent's stdin so piped input flows into the child unchanged (`echo hi | ocx exec pkg -- cat` prints `hi`). There is no opt-out — the previous `--interactive` flag was removed; matching standard shell exec semantics is the default.
:::

::: info Process replacement on Unix
On Unix, `ocx exec` hands the current process image off to the target via `execvp(2)`, so the child inherits ocx's PID. Signals reach the target without an ocx forwarder, `pgrep <name>` shows the wrapped binary, and the process tree drops the ocx layer entirely — matching the same semantics shells use when chaining `exec "$@"` in entry-point scripts. On Windows, `ocx exec` spawns the target and waits for it, since `CreateProcess` has no exec equivalent; the propagated exit code is forwarded as ocx's own exit code.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Command exited successfully (`exec` propagates the wrapped command's exit code). |
| _N_ | Wrapped command exited with code _N_ — `exec` forwards the child status verbatim. |

### `find` {#find}

Resolves one or more packages and prints their package root paths.

The package root is the directory containing the package's `content/` and `entrypoints/` subdirectories alongside `metadata.json`, `manifest.json`, and the other per-package files. Consumers traverse into `<root>/content/` for installed files or `<root>/entrypoints/` for generated launchers — both stay one path join away.

By default the content-addressed object-store package root is returned. The `--candidate` and `--current` modes return the stable install symlink path; those symlinks themselves target the package root, so traversal works the same through them. See [Path Resolution](#path-resolution) for the trade-off between modes.

No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx find [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve.

**Options**

- `-p`, `--platform`: Platforms to consider when resolving. Defaults to the current platform. Ignored when `--candidate` or `--current` is set.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `-h`, `--help`: Print help information.

::: tip
Use `--format json` with `jq` to embed the path in a script:

```shell
cmake_root=$(ocx find --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"]')
```
:::

### `generate` {#generate}

Parent command for project-tier generators. Each subcommand writes a single configuration file in the current directory based on the project's `ocx.toml`.

**Usage**

```shell
ocx generate <SUBCOMMAND> [OPTIONS]
```

**Options**

- `-h`, `--help`: Print help information.

The only subcommand today is [`direnv`](#generate-direnv); additional generators (e.g. `gitignore`, `editorconfig`) may land in future releases.

#### `direnv` {#generate-direnv}

Writes a `.envrc` file in the current directory that wires [`ocx shell direnv`](#shell-direnv) into [direnv](https://direnv.net/). After running `ocx generate direnv`, run `direnv allow` in the same directory to activate the hook. The generated `.envrc` watches `ocx.toml` and `ocx.lock`, so direnv re-runs the hook whenever either file changes.

**Usage**

```shell
ocx generate direnv [OPTIONS]
```

**Options**

- `--force`: Overwrite an existing `.envrc` in the current directory. Without this flag, an existing file causes the command to exit with a `ConfigError` (78) and leave the file untouched.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `.envrc` written successfully. |
| 74 | I/O error writing `.envrc`. |
| 78 | `.envrc` already exists and `--force` was not given. |

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS] [REGISTRY...]
```

Lists all packages available in the index. Uses the local index by default; pass [`--remote`](#arg-remote) to query the registry directly without writing through to the local snapshot. Repository names are always prefixed with their registry in the output (e.g., `ocx.sh/cmake`).

**Arguments**

- `[REGISTRY...]`: Registries to query. Accepts zero or more registry hostnames. Defaults to `OCX_DEFAULT_REGISTRY` (or `ocx.sh`) when omitted.

**Options**

- `--tags`: Include available tags for each package. Slower — requires fetching additional information for each package.

#### `list` {#index-list}

```bash
ocx index list [OPTIONS] <PACKAGE>...
```

Lists available tags for one or more packages.

Identifiers carrying a digest (`@sha256:...`) are rejected with a usage
error — `index list` enumerates tags, and a digest narrows nothing. Use
[`ocx package info <pkg>@<digest>`](#package-info) for a single artifact, or
drop the `@digest` suffix. Tag-only identifiers (`<pkg>:<tag>`) still work
as a tag filter on the returned list.

**Arguments**

- `<PACKAGE>`: Package identifiers to list tags for. Must not include a digest suffix.

**Options**

- `--platforms`: Shows which platforms are available for the package. Uses the tag from the identifier, or `latest` if none specified.
- `--variants`: Lists unique variant names found in the tags.
- `-h`, `--help`: Print help information.

::: tip
`index list` is a pure-query command — under [`--remote`](#arg-remote) it
contacts the registry without writing the local tag store. To refresh the
persistent snapshot, run [`ocx index update`](#index-update) explicitly.
:::

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Explicitly refresh the local index from the remote registry. **The only
command that writes tag pointers to `$OCX_HOME/tags/` outside of
`ocx install` / `ocx package pull`** (the install/pull path commits via
`LocalIndex::commit_tag`, gated to skip pinned-id pulls). No manifest or
layer blobs are written to `$OCX_HOME/blobs/` by `index update`.

When a tagged identifier is used (e.g., `cmake:3.28`), only that single tag's digest pointer is
recorded — the remote tag listing is skipped entirely. This is ideal for lockfile workflows where
the local tag store should contain only explicitly requested tags. When a bare identifier is used
(e.g., `cmake`), digest pointers for all tags are recorded.

After running `ocx index update <pkg>`, an `ocx --offline install <pkg>` will fail with
`OfflineManifestMissing` until the blob cache is populated by a prior online `ocx install <pkg>`.

**Arguments**

- `<PACKAGE>`: Package identifiers to update in the local index for. Include a tag to update only that tag; omit the tag to update all tags.

### `info` {#info}

Prints build information: the ocx version, supported platforms, and the detected shell.

**Usage**

```shell
ocx info
```

### `init` {#init}

Creates a minimal `ocx.toml` in the current directory.

The generated file contains a commented-out `registry` declaration and an empty `[tools]` table — a non-interactive skeleton following the "backend-first, minimal output" design. Once the file exists, use [`ocx add`](#add) to append tool bindings or edit it directly.

The command is an idempotent failure: if `ocx.toml` already exists (or a symlink at that path exists), it exits with code 64 without overwriting the existing file.

**Usage**

```shell
ocx init
```

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.toml` created successfully. |
| 64 | `ocx.toml` already exists at the target path. |
| 74 | I/O error writing the new file. |

### `install` {#install}

Downloads and installs one or more packages into the local object store.

Installs packages into the [object store](../user-guide.md#file-structure-packages) and creates a [candidate symlink](../user-guide.md#path-resolution) for each package, making them available for use by other commands. If a package declares [dependencies][ug-dependencies], all transitive dependencies are downloaded to the object store automatically — only the explicitly requested packages receive install symlinks.

**Usage**

```shell
ocx install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--select`: After installing, update the [current symlink](../user-guide.md#path-resolution) for each package to point to the newly installed version. Required before using `ocx env --current` or `ocx shell env --current`.
- `-h`, `--help`: Print help information.

::: warning Windows: `PATHEXT` must include `.CMD`
On Windows, `install` prints a stderr warning when the host shell's `PATHEXT` is missing `.CMD`. Generated entrypoint launchers are `.cmd` files and require `PATHEXT` to advertise that extension before bare-name lookup (e.g. `cmake`) can find them. Add `.CMD` to `PATHEXT` (typically via your shell profile) or invoke launchers with their full filename.
:::

### `lock` {#lock}

Resolves every tool tag in the nearest `ocx.toml` to a pinned OCI manifest digest and writes the result to `ocx.lock` next to it. The command is fully transactional — either every tool resolves successfully and the file is rewritten atomically, or nothing is written and the previous `ocx.lock` survives unchanged.

The lock carries a `declaration_hash` over the canonicalized [RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785) of `ocx.toml`. Downstream commands ([`ocx pull`](#pull), [`ocx exec`](#exec)) consult this hash to detect when the lock is stale relative to the source declaration. When the resolved content of every tool is unchanged between two `ocx lock` runs, the file's `generated_at` timestamp is preserved verbatim — the byte-stable output keeps version-control diffs minimal.

After a successful write, the command checks whether the project's `.gitattributes` declares `ocx.lock merge=union` and emits a one-line stderr advisory when it does not, helping prevent merge conflicts on team projects.

**Usage**

```shell
ocx lock [OPTIONS]
```

**Options**

- `-g`, `--group <NAME>`: Restrict resolution to the named group(s). Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table. When omitted, every `[tools]` and `[group.*]` entry is resolved.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written (or preserved if content was unchanged). |
| 64 | Missing `ocx.toml`, unknown `--group` name, or empty comma segment. |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 78 | Existing `ocx.lock` is malformed (parse error) or `ocx.toml` schema-invalid. |
| 79 | Tag unresolvable during resolution (package not found in registry after retries). |
| 80 | Authentication failure against the registry. |

Concurrent invocations of `ocx lock` and `ocx update` are serialized via a hidden `.ocx-lock` sentinel in the project root. Readers (`ocx pull`, `git`, IDE tooling) never acquire this sentinel and are never blocked by a running `ocx lock`.

### `pull` {#pull}

Pre-warms the [object store][fs-objects] from the project `ocx.lock` without
creating [install symlinks][fs-symlinks]. Distinct from
[`package pull`](#package-pull): this is the **project-tier** entry point — every
tool comes from the digest-pinned lock, never from the index — making it the
recommended primitive for reproducible CI setups.

`ocx pull` is read-only on `ocx.lock`. Re-resolution lives in `ocx update`;
rewriting from the config lives in `ocx lock`.

**Usage**

```shell
ocx pull [OPTIONS]
```

**Options**

- `-g`, `--group <NAME>`: Restrict the pull to one or more named groups. Repeatable
  and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects
  the top-level `[tools]` table. When omitted, every entry from the lock is pulled.
- `--dry-run`: Print which locked tools are already cached vs. would be fetched,
  then exit without writing to the store.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (or empty group filter — nothing to pull). |
| 64 | Missing `ocx.toml`, unknown `--group` name, or empty comma segment. |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 78 | `ocx.toml` present but `ocx.lock` is missing — run `ocx lock` first. |

#### Dry-run preview {#pull-dry-run}

`ocx pull --dry-run` resolves each locked tool through the local index
(cache-first, like the real pull does) and reports whether it is already in the
store. The store is never modified. Combine with [`--offline`](#arg-offline) to
forbid the cache-miss network probe entirely.

```shell
$ ocx pull --dry-run
Package                         Status       Path
localhost:5000/cmake@sha256:... cached       /home/me/.ocx/packages/.../content
localhost:5000/ripgrep@sha256:..would-fetch  -
```

The staleness gate fires ahead of the dry-run branch, so a stale lock still
exits 65 — the preview is not a way to bypass `declaration_hash` validation.
The output respects [`--format json`](#arg-format) and [`--quiet`](#arg-quiet).

### `remove` {#remove}

Removes a tool binding from `ocx.toml`, rewrites `ocx.lock`, and uninstalls the tool.

The command accepts either a bare binding name (`cmake`), a name with a tag (`cmake:3.28`), or a fully-qualified identifier (`ocx.sh/cmake:3.28`). The binding key is always the repository basename — the tag and registry are used only to locate the correct entry and the installed package; the key match is against the TOML map key. Fails with exit code 79 if no matching binding is found.

**Usage**

```shell
ocx remove <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Binding name or fully-qualified identifier to remove (e.g. `cmake`, `cmake:3.28`, or `ocx.sh/cmake:3.28`).

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--help` | `-h` | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Binding removed, lock rewritten. |
| 64 | No `ocx.toml` found in scope. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock sentinel (`.ocx-lock`). Retry with backoff. |
| 78 | `ocx.toml` schema invalid or TOML parse error. |
| 79 | Binding not found in any group in `ocx.toml`. |

### `select` {#select}

Selects one or more packages as the current version by updating the [current symlink](../user-guide.md#path-resolution).

Each package is resolved using the [selected index](../user-guide.md#indices-selected).
No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx select [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to select.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `-h`, `--help`: Print help information.

::: warning Windows: `PATHEXT` must include `.CMD`
Same `PATHEXT` warning as `install` — see [`install`](#install). `select` flips `current` so generated launchers become reachable through `current/entrypoints/`; bare-name resolution requires `.CMD` in `PATHEXT`.
:::

::: tip
`ocx install --select` installs and selects in one step.
:::

See [path resolution modes](../user-guide.md#path-resolution) for how the `current` symlink is used downstream.

#### Entry-point name collisions {#select-entry-point-collision}

[Entry point](../in-depth/entry-points.md) name collisions are checked at two distinct points; `select` itself performs no collision check, since flipping `current` does not compose environments.

The first gate is **at install time**, scoped to a single package's interface closure. When `install` (with or without `--select`) downloads a package whose own bundle plus its interface-visible transitive deps declare the same entry-point name twice, the install aborts before the temp→object-store atomic move and exits with `EntrypointCollision` (exit code `65`, `DataError`). All N owning packages are listed so the publisher can deselect the right one.

The second gate is **at consumption time**, invoked whenever `ocx env` or `ocx exec` is given two or more roots. The compose-time check projects each root's interface surface (bundle entry points plus interface-visible TC entries) and reports the same `EntrypointCollision` error if two roots claim the same name. This catches conflicts that the per-package install gate cannot see — two roots installed independently without `--select` and combined only at exec time.

`select` is symlink-only: it flips `current` for a single package and never composes the environment, so it has no entry-point collision check. See [Exit codes](#exit-codes) for the full taxonomy.

### `shell` {#shell}

#### `hook` {#shell-hook}

Stateful prompt-hook entry point for the project toolchain. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, walks the actually-installed default-group tools, and emits shell export lines only when the resolved set has changed since the last invocation. The fingerprint that drives the fast path lives in the `_OCX_APPLIED` environment variable (a `v1:<sha256-hex>` token); when the fingerprint matches the previous invocation's value, the command exits 0 with no output.

The command is invoked from the shell's prompt-hook mechanism — see [`ocx shell init`](#shell-init) for the per-shell wiring snippet. It never contacts the network and never installs missing tools; tools that are declared in the lock but not present in the object store produce a one-line stderr note ("# ocx: cmake not installed; run `ocx pull` to fetch") and are skipped.

**Usage**

```shell
ocx shell hook [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, fast path, or fresh export emitted). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

#### `direnv` {#shell-direnv}

Stateless export generator for the project toolchain. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, looks up every default-group tool in the local object store, and prints shell-specific export lines for the resolved environment. Unlike [`shell hook`](#shell-hook), this command does not consult or update `_OCX_APPLIED` — it emits a fresh export block on every invocation, leaving the diffing/caching to the caller (typically [direnv](https://direnv.net/)).

The command never contacts the network and never installs missing tools. Tools missing from the object store produce a one-line stderr note and are skipped; a stale lock produces a stderr warning but the stale digests are still used. When no project `ocx.toml` is found in scope, the command exits 0 with no output.

**Usage**

```shell
ocx shell direnv [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, or exports emitted). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

Shell-specific export emission, completion script generation, project-toolchain hooks, and persistent shell profile management.

`shell env` and `shell profile load` print export statements meant for `eval`. `shell completion` emits a completion script for sourcing into the running shell. `shell hook` and `shell direnv` emit exports for the resolved project toolchain. `shell profile add`/`remove`/`list` manage `$OCX_HOME/profile.json` — the manifest read by `shell profile load` at shell startup.

#### `env` {#shell-env}

Generate shell export statements for the environment variables declared by one or more packages.
Intended to be evaluated by the shell:

```shell
eval "$(ocx shell env mypackage)"
```

This command does not auto-install packages — if a package is not already available locally it will fail with an error.
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

**Usage**

```shell
ocx shell env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to generate environment variable export statements for.

**Options**

- `-p`, `--platform`: Target platforms to consider. Auto-detected by default.
- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].

::: warning Windows: `PATHEXT` must include `.CMD`
On Windows, `shell env` prints a stderr warning when the host shell's `PATHEXT` is missing `.CMD`. The exported lines do not modify `PATHEXT`; the consuming shell must already advertise `.CMD` for bare-name resolution of generated launchers to work.
:::

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Usage**

```shell
ocx shell completion [OPTIONS]
```

**Options**

- `--shell <SHELL>`: Shell to generate completions for. One of `bash`, `zsh`, `fish`, `elvish`, `powershell`. Auto-detected from the parent shell when omitted; ocx fails with an error if the detected shell is unsupported (e.g. `nushell`).

**Install examples**

::: code-group

```shell [bash]
# add to ~/.bashrc
source <(ocx shell completion --shell bash)
```

```shell [zsh]
# write into the first fpath entry, then `compinit`
ocx shell completion --shell zsh > "${fpath[1]}/_ocx"
```

```shell [fish]
# load for the current session, or save under ~/.config/fish/completions/ocx.fish
ocx shell completion --shell fish | source
```

```powershell [powershell]
# add to $PROFILE
ocx shell completion --shell powershell | Out-String | Invoke-Expression
```

:::

#### `init` {#shell-init}

Prints a shell-specific init snippet that wires [`ocx shell hook`](#shell-hook) into the shell's prompt cycle. The snippet is a pure code generator — it never reads the project, never touches disk, and never contacts the network. Source the output once from your shell rc (or pipe directly via `>>`), and every subsequent prompt re-evaluates the project toolchain.

The exact mechanism differs per shell: Bash inserts into `PROMPT_COMMAND`, Zsh registers a `precmd` hook via `add-zsh-hook`, Fish defines a function `--on-event fish_prompt`, and Nushell writes a closure into `$env.config.hooks.pre_prompt` (saved to `$env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu`). All four are idempotent — sourcing the snippet repeatedly does not duplicate the hook.

**Usage**

```shell
ocx shell init <SHELL>
```

**Arguments**

- `<SHELL>`: Target shell. Required. Accepts `bash`, `zsh`, `fish`, `nushell`. Other recognized shells (`ash`, `ksh`, `dash`, `elvish`, `powershell`, `batch`) print a `# ocx shell init does not yet ship a prompt-hook snippet for this shell` placeholder.

**Options**

- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Snippet printed (or placeholder comment for unsupported shells — always 0; the command is pure code generation). |

### `uninstall` {#uninstall}

Removes the installed candidate for one or more packages.

Removes the [candidate symlink](../user-guide.md#path-resolution) and its back-reference. Object-store content is preserved unless `--purge` is given. To also remove the current symlink, pass `--deselect` or run [`deselect`](#deselect) separately. To remove all unreferenced objects at once, use [`clean`](#clean).

**Usage**

```shell
ocx uninstall [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to uninstall.

**Options**

- `-d`, `--deselect`: Also remove the [current symlink](../user-guide.md#path-resolution). Equivalent to running `ocx deselect` after uninstall — see [`deselect`](#deselect) for the full cleanup behavior.
- `--purge`: Delete the object from the store when no other references remain after uninstall.
- `-h`, `--help`: Print help information.

### `version` {#version}

Prints the ocx version number.

**Usage**

```shell
ocx version
```

### `ci` {#ci}

#### `export` {#ci-export}

Exports the environment variables declared by one or more packages directly into a CI system's
runtime files. For [GitHub Actions][github-actions-docs], this appends path entries to
`$GITHUB_PATH` and other variables to `$GITHUB_ENV`.

The CI flavor is auto-detected from the environment (e.g. `GITHUB_ACTIONS=true`) but can be
overridden with `--flavor`.

Plain format writes directly to the CI runtime files and prints nothing to stdout.
JSON format outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}` —
the same canonical envelope as [`env`](#env).

This command does not auto-install packages — if a package is not already available locally it
will fail with an error. In CI workflows, use [`package pull`](#package-pull) before `ci export`.

**Usage**

```shell
ocx ci export [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to export the environment for.

**Options**

- `--flavor <FLAVOR>`: CI system to target. Currently supported: `github-actions`. Auto-detected if omitted.
- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `-h`, `--help`: Print help information.

::: warning Windows: `PATHEXT` must include `.CMD`
On Windows runners, `ci export` prints a stderr warning when `PATHEXT` is missing `.CMD`. Generated `.cmd` launchers exported via `$GITHUB_PATH` only resolve by bare name when `PATHEXT` advertises `.CMD`; configure the runner shell or invoke launchers with their full filename.
:::

For packages that declare [entry points][guide-entry-points], `ci export` includes a synthetic PATH
entry pointing at the package's `entrypoints/` directory. This entry is placed before any
`bin/`-style paths declared by the package's `env` metadata, so installed launchers take
precedence over raw binaries when both are on the exported PATH. See the
[entry-points guide][guide-entry-points] for the rationale behind this ordering.

::: tip
Pair with [`package pull`](#package-pull) for a minimal CI setup:

```shell
ocx package pull cmake:3.28
ocx ci export cmake:3.28
```
:::

### `package` {#package}

#### `create` {#package-create}

Bundles a local directory into a compressed package archive ready for publishing. If the package
metadata includes [dependencies][ug-dependencies], the declared dependency graph is
validated for cycles at this stage — catching errors before the package reaches the registry.

**Usage**

```shell
ocx package create [OPTIONS] <PATH>
```

**Arguments**

- `<PATH>`: Path to the directory to bundle.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier, used to infer the output filename when `--output` is a directory.
- `-p`, `--platform <PLATFORM>`: Platform of the package, used to infer the output filename.
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform. The file extension controls the compression algorithm: `.tar.xz` (LZMA, default) or `.tar.gz` (Gzip).
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to a metadata file to bundle with the package. When provided, it is copied as a sidecar file next to the output archive. If omitted, no metadata sidecar is written.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`. Applies to whichever algorithm is selected.
- `-j`, `--threads <N>`: Number of compression threads. `0` (default) auto-detects from available CPU cores (capped at 16). `1` forces single-threaded compression. Only affects LZMA (`.tar.xz`) compression.
- `-h`, `--help`: Print help information.

#### `pull` {#package-pull}

Downloads packages into the local [object store][fs-objects] without creating
[install symlinks][fs-symlinks].

Unlike [`install`](#install), this command only populates the content-addressed object store — no
candidate or current symlinks are created. If a package declares [dependencies][ug-dependencies], all transitive dependencies are pulled into the object store as well. This is the recommended primitive for CI environments where reproducibility matters and symlink management is unnecessary.

**Usage**

```shell
ocx package pull [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to pull.

**Options**

- `-p`, `--platform`: Target platforms to consider. Defaults to the current platform.
- `-h`, `--help`: Print help information.

::: tip
`package pull` reports the package root for each package — the same
digest-derived directory that [`find`](#find) and [`exec`](#exec) resolve to.
The package root contains `content/` and `entrypoints/` as siblings; consumers
traverse one level in. Two pulls of the same digest are safe to run concurrently.

For project-tier setups driven by `ocx.lock`, use [`pull`](#pull) instead — it consumes the
lockfile directly and ignores the index.
:::

#### `push` {#package-push}

Publishes a package to the registry as zero or more layers. Each layer is uploaded as an OCI blob and recorded in a single image manifest, in the order given on the command line. A zero-layer push produces a config-only OCI artifact (referrer-only / description-only manifest) and requires `--metadata`.

**Usage**

```shell
ocx package push [OPTIONS] --platform <PLATFORM> <IDENTIFIER> <LAYERS>...
```

**Arguments**

- `<IDENTIFIER>`: Package identifier including the tag, e.g. `cmake:3.28.1_20260216120000`.
- `<LAYERS>...`: Zero or more layers, in order (base layer first, top layer last). Each layer is either:
  - a path to a pre-built archive file (`.tar.gz`, `.tgz`, `.tar.xz`, or `.txz`), or
  - a digest reference of the form `sha256:<hex>.<ext>` (e.g. `sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08.tar.gz`) pointing at a layer that already exists in the target registry. The `<ext>` suffix is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. Bare digests are rejected.
  - Extension aliases: `.tgz` is accepted as an alias for `.tar.gz`, and `.txz` for `.tar.xz`. The canonical forms `tar.gz` / `tar.xz` are what ocx emits internally — aliases are normalized on parse.
  - To force file interpretation of a pathological filename that happens to match the digest shape, prefix it with `./` (e.g. `./sha256:abc….tar.gz`).
  - Omitting all layers produces a config-only OCI artifact with `layers: []`, valid for referrer-only / description-only manifests. `--metadata` is required in that case.

**Options**

- `-p`, `--platform <PLATFORM>`: Target platform of the package (required).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `cmake:3.28.1_20260216120000` automatically re-points the rolling ancestors (`cmake:3.28.1`, `cmake:3.28`, `cmake:3`, and `cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided (all layers are digest references, or the layer list is empty).
- `-h`, `--help`: Print help information.

::: tip Layer reuse
Digest-referenced layers are not re-uploaded — ocx only HEADs the registry to verify they exist. This is the foundation of the [layer dedup model](../user-guide.md#file-structure-layers): a base layer pushed once can be referenced from any number of subsequent packages by digest.

```shell
# Push a fresh base + tool combination
ocx package push -p linux/amd64 mytool:1.0.0 base.tar.gz tool.tar.gz

# Reuse the same base by digest in a later release.
# The digest is the full 64-char sha256 hex written verbatim —
# the ellipsis is shown here only to keep the example short.
ocx package push -p linux/amd64 mytool:1.0.1 sha256:<hex>.tar.gz newtool.tar.gz
```
:::

::: warning Bring your own archives
`ocx package push` does not bundle a directory for you. Each file layer must be a pre-built archive. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy) and defeats layer reuse — use [`ocx package create`](#package-create) to produce a stable archive once, then push and reference it by digest from later commands.
:::

#### `describe` {#package-describe}

Pushes package description metadata (title, description, keywords, README, logo) to the registry.

**Usage**

```shell
ocx package describe [OPTIONS] <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier (repository only; tag is ignored).

**Options**

- `--readme <PATH>`: Path to a README markdown file.
- `--logo <PATH>`: Path to a logo image (PNG or SVG).
- `--title <TITLE>`: Short display title for the package catalog.
- `--description <TEXT>`: One-line summary.
- `--keywords <LIST>`: Comma-separated search keywords.
- `-h`, `--help`: Print help information.

At least one of the above metadata options must be provided.

#### `info` {#package-info}

Displays description metadata for a package from the registry.

**Usage**

```shell
ocx package info [OPTIONS] <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier (repository only).

**Options**

- `--save-readme <PATH>`: Save the README to a file or directory.
- `--save-logo <PATH>`: Save the logo to a file or directory.
- `-h`, `--help`: Print help information.

<!-- external -->
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[sysexits-manpage]: https://man.freebsd.org/cgi/man.cgi?sysexits
[gnu-parallel-j0]: https://www.gnu.org/software/parallel/parallel.html

<!-- in-depth -->
[exec-modes]: ../in-depth/environments.md#visibility-views
[env-composition-forwarding]: ../in-depth/environments.md#ocx-forwarding

<!-- environment -->
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-no-config]: ./environment.md#ocx-no-config
[env-config]: ./environment.md#ocx-config
[env-project]: ./environment.md#ocx-project
[env-no-project]: ./environment.md#ocx-no-project
[env-ocx-quiet]: ./environment.md#ocx-quiet
[env-ocx-jobs]: ./environment.md#ocx-jobs

<!-- reference -->
[config-ref]: ./configuration.md

<!-- internal -->
[entry-points]: ./metadata.md#entry-points
[guide-entry-points]: ../in-depth/entry-points.md
[exit-codes]: #exit-codes
[fs-objects]: ../user-guide.md#file-structure-packages
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
[fs-index]: ../user-guide.md#indices-local
[ug-dependencies]: ../user-guide.md#dependencies
[ug-deps-env]: ../user-guide.md#dependencies-environment
