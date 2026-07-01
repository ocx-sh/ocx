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

An unpinned tag that is absent from the local index exits [`81`](#exit-codes)
(`PolicyBlocked`) — the same code `--frozen` produces for the same class of
miss. To recover, either run `ocx index update` online first, or switch to a
digest-pinned identifier.

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

### `--frozen` {#arg-frozen}

Freezes tag→digest resolution to the local index. A tag already in the local
index resolves from cache; a digest-pinned reference (`repo@sha256:…`, or a tag
pinned by `ocx.lock`) still fetches its content over the network. But an
unpinned tag that is missing from the local index **errors** with exit
[`81`](#exit-codes) instead of being fetched and recorded — `--frozen`
guarantees that no unknown (un-pinned) version slips in. To resolve a new tag
under `--frozen`, populate the local index first with
[`ocx index update`](#index-update).

Unlike [`--offline`](#arg-offline), `--frozen` is **not** a network ban: it
still reaches the registry for known and digest-pinned content. It only refuses
to *discover* a new tag→digest mapping. Use it in CI to assert a project never
installs a version that was not already locked or indexed.

```sh
ocx --frozen pull                 # succeeds when every tool is already locked
ocx --frozen add some/tool:tag    # exit 81 if that tag is not in the index
```

`--frozen` conflicts with [`--remote`](#arg-remote) (exit
[`64`](#exit-codes)); the two are contradictory. Combining `--frozen` with
[`--offline`](#arg-offline) is accepted — offline is the stricter constraint and
takes effect. The same policy can be set persistently via the
[`OCX_FROZEN`][env-ocx-frozen] environment variable.

::: tip Cargo divergence
[Cargo][cargo]'s `--frozen` implies `--offline`; OCX's `--frozen` does not disable the network —
known and digest-pinned content still downloads. For `cargo build --frozen` semantics use
[`--offline`](#arg-offline) alone: offline is the stronger constraint and already refuses
unpinned tags (adding `--frozen` is accepted but has no further effect).
:::

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
that fans out through `pull_all` — `package install`, `pull`, `package pull`, `package exec`
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

### `--global` {#global-flag}

Selects `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`) as the project file. This is a **root flag** — it must appear before the subcommand name (like `--project` or `--offline`), not after it.

```sh
ocx --global add ripgrep:14      # correct
ocx add --global ripgrep:14      # error: unknown flag
```

When `--global` is set, the following toolchain-tier commands target `$OCX_HOME/ocx.toml` instead of a discovered project file: `add`, `remove`, `lock`, `upgrade`, `pull`, `run`, and `env`.

`--global` is mutually exclusive with `--project`. Passing both — whether as flags or via the `OCX_GLOBAL` / `OCX_PROJECT` environment variables — exits with code 64 (`UsageError`). The global toolchain never composes into project resolution; see [strict isolation][env-composition-strict-isolation] for the full hermetic contract.

::: warning No implicit home discovery
There is no implicit fallback to `$OCX_HOME/ocx.toml` when no project is found in the CWD walk. You must pass `--global` explicitly to target the global file. The prior automatic home-tier discovery has been removed.
:::

**Strict isolation**

The global toolchain is a shell-convenience tier only. `ocx run` and `ocx exec` are always hermetic:

- `ocx run` without `--global` reads only the in-effect project file. The global file is never consulted.
- `ocx exec` reads no project file at all.

Neither command performs gap-fill from the global toolchain.

**Environment variable**

[`OCX_GLOBAL`][env-ocx-global] is the environment-variable equivalent. It is forwarded to child `ocx` processes the same way as other resolution-affecting flags.

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
| 81 | PolicyBlocked | OCX | A deliberate local policy (`--offline` or `--frozen`) refused a network or resolution operation — not a fault. Includes an unpinned-tag resolve that the policy forbade | Loosen the flag, or populate the local index first (e.g. `ocx index update`) |

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
    81) echo "policy blocked (offline/frozen); loosen the flag or update the index" ;;
    *)  echo "unexpected failure (exit $?)"; exit 1 ;;
esac
```

### `--candidate` / `--current` {#path-resolution}

The `--candidate` and `--current` flags are available on commands that resolve a package's
location on disk, for example [`package env`](#package-env), [`package which`](#which), or [`exec`](#exec).

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

After mutating `ocx.toml`, `ocx add` resolves only the new bindings and carries every existing lock entry forward unchanged, then installs the newly added tools.

Multiple identifiers may be given in one invocation. They are staged together and committed atomically — if any identifier is invalid or its binding name already exists, nothing is written. `--group` applies to every identifier in the batch.

The same binding name may coexist in the default `[tools]` table and in any named `[group.*]` table — binding identity is `(group, name)`. This lets a project carry different versions of the same tool in different contexts:

```shell
ocx add shfmt:3.13              # adds to default [tools]
ocx add --group ci shfmt:3.13   # also legal — coexists in [group.ci]
```

**Usage**

```shell
ocx add [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more fully-qualified tool identifiers to add (e.g. `ocx.sh/cmake:3.28` or `ghcr.io/acme/mytool:1.0`). Bare identifiers without a tag (e.g. `ocx.sh/cmake`) default to `:latest` — the written `ocx.toml` entry is always explicit (`cmake = "ocx.sh/cmake:latest"`), following the same convention as `docker pull`. See [Unit 3 bare-identifier default][user-guide-toml] for the design rationale.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--group <NAME>` | `-g` | Add the binding to a named group instead of the default `[tools]` table. Must be non-empty and contain only alphanumeric characters, `-`, or `_`. |
| `--pull` | — | After writing the lock, materialise the newly added tool into the object store and create its candidate symlink. Default when `--no-pull` is absent. |
| `--no-pull` | — | Write the lock only; skip materialisation. Defer the install to a later `ocx pull` or first `ocx run`. |
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for each named platform instead of the host. Repeatable and comma-separated (e.g. `-p linux/amd64,linux/arm64`). The lock already pins every shipped platform's leaf, so this only selects which to fetch — the lock stays host-agnostic (an amd64 host can pre-warm an arm64 leaf). Defaults to the current host. A platform the publisher does not ship exits 78. |
| `--help` | `-h` | Print help information. |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand to target `$OCX_HOME/ocx.toml`: `ocx --global add ripgrep:14`.
See [`--global`][global-flag] for the full root-flag reference.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Binding added, lock updated, tool installed. |
| 64 | No `ocx.toml` found, binding already exists, invalid `--group` name, `--global` combined with `--project`, or multiple `--platform` values against a legacy (V1) lock (run `ocx lock` to migrate). |
| 65 | `ocx.toml` drifted from `ocx.lock` before this add — run `ocx lock` to reconcile. |
| 69 | Registry unreachable while resolving the new tag. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock on `ocx.toml`. Retry with backoff. |
| 78 | A carried legacy lock entry can no longer be migrated exactly — run `ocx upgrade` to re-resolve. Also: `ocx.toml` schema invalid or TOML parse error, or a requested `--platform` is not shipped by a tool. |
| 79 | Tag not found in the registry. |
| 80 | Authentication failure against the registry. |

### `clean` {#clean}

Removes unreferenced objects from the local object store.

An object is unreferenced when nothing points to it — no candidate or current symlink, no other installed package depends on it, and no registered project's `ocx.lock` pins it. Projects are registered in the `$OCX_HOME/projects/` ledger (a flat directory of symlinks, one per project; created automatically when `ocx lock` or `ocx add` writes a lockfile). This happens after [`uninstall`](#uninstall) (without `--purge`) or when symlinks are removed manually. When a package with [dependencies][ug-dependencies] is removed, its dependencies may become unreferenced and are cleaned up in the same pass.

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
| `--force` | — | Bypass the `$OCX_HOME/projects/` ledger and collect packages held only by other projects' `ocx.lock` files. Live install symlinks are still honoured. | false |
| `--help` | `-h` | Print help information. | — |

**JSON output schema** (`--format json`)

`ocx clean --format json` emits an array of objects, one per candidate entry:

| Field | Type | Description |
|-------|------|-------------|
| `kind` | `"object"` \| `"temp"` | Storage tier of the entry. |
| `dry_run` | boolean | `true` when `--dry-run` was passed; `false` on a live run. |
| `path` | string | Absolute path to the package or temp directory. |
| `held_by` | array of strings | Absolute paths to project directories whose `ocx.lock` pins this package. Populated only in dry-run mode, only for entries the ledger retained (never collected). Empty array when nothing holds the entry. |

```json
[
  {
    "kind": "object",
    "dry_run": true,
    "path": "/home/alice/.ocx/packages/.../sha256/ab/cdef.../",
    "held_by": ["/home/alice/dev/proj-a"]
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
object  /home/alice/dev/proj-a      /home/alice/.ocx/packages/.../
object                              /home/alice/.ocx/packages/.../
temp                                /home/alice/.ocx/temp/abc.../
```

A blank `Held By` cell means the entry is unreferenced and will be collected. A populated cell lists the project directory (or directories) holding the package. The `Held By` column is omitted when no entries are held. `temp` entries are never governed by the ledger and never show a `Held By` value.

Non-dry-run output is always 2-column (`Type | Path`): held entries are never collected and therefore never appear.

### `deps` (package-tier — `ocx package deps`) {#deps}

Shows the dependency tree for one or more installed packages. Operates on locally-present packages
only — no auto-install. This is an OCI-tier command under the [`ocx package`](#package) group — it operates on OCI identifiers and never consults `ocx.toml`. See [Dependencies][ug-dependencies] in the user guide for
background.

**Usage**

```shell
ocx package deps [OPTIONS] <PACKAGE>...
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

> **Moved to `ocx package deselect`** — exits 64 if invoked as bare `ocx deselect`. See [`package deselect`](#package-deselect) for the current form.

Removes the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its [candidate symlink][fs-symlinks] and object-store content remain intact. To also remove the installed files, use [`package uninstall`](#package-uninstall).

When the deselected package declares [entry points][guide-entry-points], the launchers stop being reachable through `current/entrypoints/` as soon as the `current` symlink is removed. The symlink removal is idempotent — an already-absent link is not an error.

**Usage**

```shell
ocx package deselect <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to deselect.

**Options**

- `-h`, `--help`: Print help information.

### `env` (root — toolchain-tier) {#env-root}

Export the composed toolchain environment for the active project or global toolchain.

This is the **toolchain-tier** env exporter. It reads `ocx.toml` + `ocx.lock` and emits the combined environment for the resolved tool set. Output format is controlled by the root [`--format`](#arg-format) flag (default: `plain` table). Use `--shell` to get eval-safe shell export lines — that is the only form safe to pass to `eval`.

`--shell` requires the equals-form (`--shell=bash`, not `--shell bash`) to prevent shell injection through unquoted positional tokens.

**Usage**

```shell
ocx env [OPTIONS]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--shell[=NAME]` | — | Emit eval-safe shell export lines for the named shell dialect. `NAME` is one of `bash`, `zsh`, `fish`, `sh` (POSIX/Dash), `powershell`, `nushell`, `elvish`. The equals-form is required — passing `--shell NAME` as two tokens is rejected with exit 64. `--shell` bare (no `=NAME`) autodetects from `$SHELL`. Mutually exclusive with `--ci`. | *(unset — uses `--format`)* |
| `--ci[=PROVIDER]` | — | Write the composed environment into the CI system's persistence channel so the exported variables and paths are available to **later pipeline steps**. `PROVIDER` is one of `github` (alias `github-actions`) or `gitlab` (alias `gitlab-ci`). The equals-form is required (`--ci=github`, not `--ci github`). Bare `--ci` (no `=PROVIDER`) auto-detects from [`GITHUB_ACTIONS`][env-github-actions] and [`GITLAB_CI`][env-gitlab-ci]; no provider detected exits 64. Mutually exclusive with `--shell`. | *(unset)* |
| `--export-file=PATH` | — | Write GitLab CI/CD JSON-lines output to `PATH` instead of stdout. Requires `--ci=gitlab`. Rejected with exit 64 when combined with `--ci=github` (GitHub infers its sink from [`GITHUB_ENV`][env-github-env] and [`GITHUB_PATH`][env-github-path]) or when given without `--ci`. | *(unset — stdout for gitlab)* |
| `--platform <PLATFORM>` | `-p` | Compose the environment for a single target platform instead of the host (cross-build export). Single-valued: passing more than one exits 64. A tool that ships no leaf for the target exits 78 (project tier) or is skipped (global tier, lenient). Defaults to the current host. | *(current host)* |
| `-h`, `--help` | | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand to target `$OCX_HOME/ocx.toml`: `ocx --global env --shell=bash`.
See [`--global`][global-flag] for the full root-flag reference.
:::

::: warning `--ci=gitlab` requires GitLab Functions / step runner
`--ci=gitlab` writes JSON-lines (`{"name":"…","value":"…"}`), which is the format consumed by the [GitLab step runner][gitlab-step-runner-docs] via `${{ export_file }}`. This is an **experimental** feature for `run:` keyword jobs. It does **not** work with traditional `script:` jobs, which use [`artifacts: reports: dotenv`][gitlab-ci-dotenv] (`KEY=VALUE` format) for cross-job variable passing. See [CI Integration][in-depth-ci] for a full step-runner example.
:::

**Examples**

```shell
# Plain table output (default):
ocx env

# Machine-readable JSON via the root --format flag:
ocx --format json env

# Eval-safe export for the current project toolchain (bash):
eval "$(ocx env --shell=bash)"

# Eval-safe export for the global toolchain (POSIX sh):
eval "$(ocx --global env --shell=sh)"

# Sourced from $OCX_HOME/env.sh (written by the installer):
eval "$(ocx --global env --shell=sh)"

# Persist toolchain env to GitHub Actions (reads $GITHUB_ENV / $GITHUB_PATH):
ocx env --ci=github

# Persist toolchain env to GitLab step runner (experimental; run: keyword jobs only):
ocx env --ci=gitlab --export-file="${{ export_file }}"

# Or redirect stdout when --export-file is omitted:
ocx env --ci=gitlab >> "${{ export_file }}"
```

::: warning Plain and JSON output are not sourceable
`ocx env` and `ocx --format json env` print an aligned table or JSON document — neither form is eval-safe. The only eval-safe channel is `--shell[=NAME]`.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success. Under `--global`, any unusable global toolchain — not configured, or a corrupt/stale `$OCX_HOME/ocx.lock` — is a valid empty environment, not an error (report path and `--shell` path alike). The global tier is lenient. |
| 64 | `--shell NAME` passed as two tokens (use `--shell=NAME`); `--ci` and `--shell` used together; `--export-file` given without `--ci` or combined with `--ci=github`; bare `--ci` (auto-detect) used outside a recognized CI environment; more than one `--platform` (env composes a single environment); `--global` combined with `--project`; or no `ocx.toml` in scope (project tier). |
| 65 | `ocx.lock` is stale — run `ocx lock` (project tier). |
| 78 | `ocx.toml` or `ocx.lock` parse error (project tier); or `--ci=github` used outside [GitHub Actions][github-actions-workflow-commands] where [`GITHUB_ENV`][env-github-env] and [`GITHUB_PATH`][env-github-path] are unset. |

The global tier is lenient: `ocx --global env` never fails on an unconfigured or corrupt global toolchain — it exports an empty environment. This is one predictable rule that does not depend on `--shell` (which only selects the output format, never whether the command errors). A corrupt global lock surfaces instead via the commands that rewrite it — `ocx --global lock`, `ocx --global add`, `ocx --global upgrade`. The project tier stays strict (a missing/stale/corrupt `ocx.lock` errors). A useful consequence of the lenient global rule: the installer's `env.sh`/`env.ps1`, which source `ocx --global env --shell=…` on every shell start, can never be broken by global toolchain state.

---

### `env` (package-tier — `ocx package env`) {#env}

Print the resolved environment variables for one or more OCI-tier packages.

With the root `--format plain` (default), outputs an aligned table with `Key`, `Type` and `Value` columns.
With `--format json`, outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.
Use `--shell[=NAME]` for eval-safe shell export lines — the only sourceable form.

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies).
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

For the full `ocx package env` entry, see [`package env`](#package-env).

**Usage**

```shell
ocx package env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `--shell[=NAME]`: Emit eval-safe shell export lines for the named dialect. Same conventions as root [`ocx env --shell`](#env-root). Mutually exclusive with `--ci`.
- `--ci[=PROVIDER]`: Write the resolved environment into the CI system's persistence channel so later pipeline steps see the exported paths and variables. `PROVIDER` ∈ `github` / `github-actions`, `gitlab` / `gitlab-ci`. Bare `--ci` auto-detects from [`GITHUB_ACTIONS`][env-github-actions] / [`GITLAB_CI`][env-gitlab-ci] (exits 64 if neither detected). Equals-form required. Mutually exclusive with `--shell`. See [CI Integration][in-depth-ci] for full walkthrough.
- `--export-file=PATH`: Write [GitLab CI/CD][gitlab-ci-export-docs] JSON-lines output to `PATH`. Requires `--ci=gitlab`; rejected with exit 64 for `--ci=github` or when given without `--ci`.
- `-h`, `--help`: Print help information.

### `exec` {#exec}

> **Moved to `ocx package exec`** — exits 64 if invoked as bare `ocx exec`. See [`package exec`](#package-exec) for the current form.

Executes a command within the environment of one or more packages.

Each positional accepts a bare OCI identifier (e.g. `node:20`); identifiers are resolved through the index and auto-installed when missing (unless [`--offline`](#arg-offline) is set).

If a package declares [dependencies][ug-dependencies], their environment variables are applied in [topological order][ug-deps-env] before the package's own variables. Env entries layer in the order identifiers appear on the command line.

<span id="launcher-exec"></span>

::: tip Generated launchers use `ocx launcher exec`, not `ocx exec`
Entry-point launchers generated by `ocx install` call the internal `ocx launcher exec '<pkg-root>' -- <argv0> [args...]` subcommand, not `ocx exec`. That subcommand validates the package root, forces the self view internally, resolves `${installPath}` in any baked entry-point `args` and prepends them before user-supplied arguments, then executes the resolved entrypoint. The wire ABI (`<pkg-root> -- <argv0> [args...]`) is frozen so launchers generated by older OCX releases keep working after an upgrade. See the [Entry Points][entry-points] guide for the launcher ABI. On Windows, the native `.exe` shim makes this call without routing through `cmd.exe`, closing the `%*` argument-injection surface for default resolution.
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

### `which` (package-tier — `ocx package which`) {#which}

Resolves one or more packages and prints their package root paths. This is an OCI-tier command under the [`ocx package`](#package) group — it operates on OCI identifiers and never consults `ocx.toml`.

The package root is the directory containing the package's `content/` and `entrypoints/` subdirectories alongside `metadata.json`, `manifest.json`, and the other per-package files. Consumers traverse into `<root>/content/` for installed files or `<root>/entrypoints/` for generated launchers — both stay one path join away.

By default the content-addressed object-store package root is returned. The `--candidate` and `--current` modes return the stable install symlink path; those symlinks themselves target the package root, so traversal works the same through them. See [Path Resolution](#path-resolution) for the trade-off between modes.

No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx package which [OPTIONS] <PACKAGE>...
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
cmake_root=$(ocx package which --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"]')
```
:::

### `direnv` {#direnv}

[direnv](https://direnv.net/) integration for the project toolchain. Bare `ocx direnv` is shorthand for [`ocx direnv init`](#direnv-init) — the once-per-project setup that writes a `.envrc`. The generated `.envrc` evaluates [`ocx direnv export`](#direnv-export) on every directory entry.

**Usage**

```shell
ocx direnv [SUBCOMMAND] [OPTIONS]
```

**Options**

- `-h`, `--help`: Print help information.

#### `init` {#direnv-init}

Writes a `.envrc` file in the current directory that wires [`ocx direnv export`](#direnv-export) into [direnv](https://direnv.net/). After running `ocx direnv init` (or bare `ocx direnv`), run `direnv allow` in the same directory to activate the hook. The generated `.envrc` watches `ocx.toml` and `ocx.lock`, so direnv re-runs the hook whenever either file changes.

**Usage**

```shell
ocx direnv init [OPTIONS]
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

#### `export` {#direnv-export}

Stateless export generator for the project toolchain. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, looks up every default-group tool in the local object store, and prints **bash** export lines for the resolved environment. It emits a fresh export block on every invocation, leaving the diffing/caching to the caller (typically [direnv](https://direnv.net/)). It is what the generated `.envrc` evaluates; you do not normally type it by hand.

Output is always bash. [direnv](https://direnv.net/) sources `.envrc` files in a bash sub-shell regardless of the user's interactive shell, then translates the resulting environment to the interactive shell internally via `direnv export <shell>`. Programs invoked via `eval` from `.envrc` therefore have to emit bash — there is no shell-dialect option on this command.

The command never contacts the network and never installs missing tools. Tools missing from the object store produce a one-line stderr note and are skipped; a stale lock produces a stderr warning but the stale digests are still used. When no project `ocx.toml` is found in scope, the command exits 0 with no output.

**Usage**

```shell
ocx direnv export [OPTIONS]
```

**Options**

- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, or exports emitted). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

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

- `--platforms`: Show the platforms (`os/arch`) the package publishes, read from its image index manifest. Uses the tag from the identifier, or `latest` if none specified. Resolved live from the registry under [`--remote`](#arg-remote); otherwise read from the local index.
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

A tag-refresh failure for any requested package fails the whole invocation; the
[exit code][exit-codes] corresponds to the first failure in request order
(deterministic across repeated runs). Packages refreshed earlier in the batch
keep their updated tags.

**Arguments**

- `<PACKAGE>`: Package identifiers to update in the local index for. Include a tag to update only that tag; omit the tag to update all tags.

### `about` {#about}

Prints environment information: the ocx version, default registry, supported platforms, detected shell, and home directory. When build provenance was baked in at compile time, two optional rows appear: `Commit` (short SHA and clean/dirty status) and `Channel` (e.g. `dev`). These rows are absent on local builds and on stable releases without a channel override.

In a terminal, `ocx about` renders an isometric logo alongside the info table. In non-interactive contexts (piped output without `--color always`), the plain key-value fallback is used instead.

**Usage**

```shell
ocx about
```

**Plain output — terminal**

```
              ++++++               ++++++
          ...                              (logo)

Version    0.3.2-dev+20260528143045
Commit     a1b2c3d4 (clean)
Channel    dev
Registry   ocx.sh
Platform   linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
Shell      bash
Home       /home/user/.ocx
```

**JSON output**

`ocx --format json about` emits a flat object. The `commit`, `build`, and `ci` blocks are merged from the [build provenance][version-json-schema] payload and follow the same schema and suppression rules as `ocx --format json version`:

```json
{
  "version": "0.3.2",
  "registry": "ocx.sh",
  "platforms": ["linux/amd64", "linux/arm64", "darwin/amd64", "darwin/arm64", "windows/amd64"],
  "shell": "bash",
  "home": "/home/user/.ocx",
  "commit": { "sha": "...", "short": "a1b2c3d4", "describe": "...", "dirty": false },
  "build":  { "timestamp": "...", "profile": "release", "target": "...", "rustc": "..." },
  "ci":     { "provider": "github-actions", "run_url": "...", "workflow": "...", "ref": "...", "sha": "..." }
}
```

`channel` is present only when baked in (dev-deploy builds). `commit`, `build`, and `ci` blocks are absent on local builds without git or CI context.

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

> **Moved to `ocx package install`** — exits 64 if invoked as bare `ocx install`. See [`package install`](#package-install) for the current form.

Downloads and installs one or more OCI-tier packages into the local object store.

Installs packages into the [object store](../user-guide.md#file-structure-packages) and creates a [candidate symlink](../user-guide.md#path-resolution) for each package, making them available for use by other commands. If a package declares [dependencies][ug-dependencies], all transitive dependencies are downloaded to the object store automatically — only the explicitly requested packages receive install symlinks.

**Usage**

```shell
ocx package install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--select`: After installing, update the [current symlink](../user-guide.md#path-resolution) for each package to point to the newly installed version. Required before using `ocx env --current`.
- `-h`, `--help`: Print help information.


### `login` {#login}

Authenticate to a registry and persist the credentials for use by subsequent
`ocx install`, `ocx pull`, and other registry-accessing commands.

Credentials are stored in the same `~/.docker/config.json` that [`docker login`][docker-login]
and [`oras login`][oras-login] write. The three tools interoperate: a credential written by any
one of them is readable by the others.

**Usage**

```shell
ocx login [OPTIONS] [REGISTRY]
```

**Arguments**

- `[REGISTRY]`: Registry hostname (e.g. `ghcr.io`, `registry.example.com`). Optional — falls back
  to `OCX_DEFAULT_REGISTRY` (default `ocx.sh`) when omitted.

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--username <USER>` | `-u` | Username for the registry. Prompted interactively when omitted on a TTY. | *(prompt)* |
| `--password-stdin` | — | Read the password or token from stdin. Required in non-interactive contexts. No `-p/--password VALUE` flag — argv-visible secrets leak via `ps` and shell history (CWE-214). | off |
| `--allow-insecure-store` | — | Permit storing credentials as base64 in `auths[registry]` when no native credential helper is configured. Default: refuse, exit 78. | off |
| `-h`, `--help` | | Print help information. | — |

:::details Reserved flags

`--auth-type <TYPE>` is reserved for a future v2 OIDC / browser-OAuth flow. Passing it today emits a usage error (exit 64). Plain HTTP registries are enabled via `OCX_INSECURE_REGISTRIES` environment variable rather than a login flag.

:::

**Credential storage tiers** (highest priority first):

1. `credHelpers[REGISTRY]` in `~/.docker/config.json` — per-registry helper
2. `credsStore` — global default helper
3. Plaintext `auths[REGISTRY].auth` — base64 fallback (requires `--allow-insecure-store`)

**Examples**

Interactive login on a developer workstation with a configured credential helper:

```shell
ocx login ghcr.io
# Username: myuser
# Password: ****
# Login succeeded
```

Non-interactive CI login piping a token on stdin:

```shell
echo "$GHCR_TOKEN" | ocx login -u "$GHCR_USER" --password-stdin ghcr.io
```

Headless environment without a native keychain daemon:

```shell
echo "$TOKEN" | ocx login -u ci --password-stdin --allow-insecure-store internal.registry.example.com
```

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Credentials persisted successfully. |
| 64 | Usage error — missing required flag, empty password, or `--password VALUE` attempted. |
| 74 | I/O error writing `~/.docker/config.json`. |
| 75 | Credential helper timed out (transient — retry). |
| 78 | No credential store available (no helper configured and `--allow-insecure-store` not passed), or helper not on PATH. |
| 80 | Credential helper failed (non-sentinel exit), helper output too large, or credentials rejected. |

**Docker interop**

`ocx login` writes to the same `~/.docker/config.json` as `docker login` and `oras login`.
Override the location with [`DOCKER_CONFIG`][env-docker-config].

**JSON output**

```shell
ocx --format json login ghcr.io
# {"registry":"ghcr.io","username":"ocx-bot"}
```

---

### `logout` {#logout}

Remove stored credentials for a registry.

Always exits 0 — including when the registry was never logged in. This matches the convention of
[`docker logout`][docker-logout], [`oras logout`][oras-logout], and [`helm registry logout`][helm-logout].
CI cleanup scripts must not fail when a previous step already removed the credentials.

**Usage**

```shell
ocx logout [REGISTRY]
```

**Arguments**

- `[REGISTRY]`: Registry hostname. Optional — falls back to `OCX_DEFAULT_REGISTRY` when omitted.

**Examples**

```shell
ocx logout ghcr.io
# Logged out of ghcr.io

# CI cleanup — safe even if no login occurred
ocx logout internal.registry.example.com || true  # redundant: already exits 0
```

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Credentials removed, or registry was not logged in (noop). |
| 74 | I/O error writing `~/.docker/config.json`. |

**JSON output**

```shell
ocx --format json logout ghcr.io
# {"registry":"ghcr.io"}
```

---

### `lock` {#lock}

Resolves every tool tag in the nearest `ocx.toml` to per-platform leaf digests and writes the result to `ocx.lock` next to it. The command is a **whole-file reconcile**: when the lock is already current (its `declaration_hash` matches the config), every pin is carried forward verbatim — a byte-identical, idempotent no-op that never advances a moving tag, even if it has moved upstream. When the config drifted, every declared tag is re-resolved and a moving tag may advance to wherever it points today. To force-advance pins on a current lock, use [`ocx upgrade`](#upgrade).

For each tool, the lock records the bare registry/repository coordinates plus a `[tool.platforms]` table mapping every platform the publisher ships to its leaf manifest digest. The command records all shipped platforms regardless of which OS it runs on, so a lock committed on Linux is complete for macOS and Windows CI runners. The command is fully transactional — either every tool resolves successfully and the file is rewritten atomically, or nothing is written and the previous `ocx.lock` survives unchanged.

The lock carries a `declaration_hash` over the canonicalized [RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785) of `ocx.toml`. Downstream commands ([`ocx pull`](#pull), [`ocx run`](#run)) consult this hash to detect when the lock is stale relative to the source declaration. When the resolved content of every tool is unchanged between two `ocx lock` runs, the file's `generated_at` timestamp is preserved verbatim — the byte-stable output keeps version-control diffs minimal.

After a successful write, the command checks whether the project's `.gitattributes` declares `ocx.lock merge=union` and emits a one-line stderr advisory when it does not, helping prevent merge conflicts on team projects.

::: tip `ocx lock` vs `ocx upgrade`
`ocx lock` is an idempotent reconcile — it re-resolves only when `ocx.toml` changed. Use `ocx upgrade` to force a re-resolve of every tag regardless of drift.
:::

**Usage**

```shell
ocx lock [OPTIONS]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--pull` | — | After writing the lock, materialise all resolved tools into the object store and create their candidate symlinks. Default when `--no-pull` is absent. | on |
| `--no-pull` | — | Write the lock only; skip materialisation. Defer the install to a later `ocx pull` or first `ocx run`. | — |
| `--check` | — | Verify `ocx.lock` is current relative to `ocx.toml` and exit. No re-resolution, no writes, no network calls. Exit 0 if the lock matches; 65 if stale; 78 if the lock file is absent. CI primitive for "is the lock committed and current?" verification. | off |
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for each named platform instead of the host. Repeatable and comma-separated. Selects which already-locked leaf to fetch (the lock stays host-agnostic); a target the publisher does not ship exits 78. Use `-p linux/amd64 -p linux/arm64` to validate that a lock satisfies multiple targets. Defaults to the current host. | *(current host)* |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global lock`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written (or preserved if content was unchanged). |
| 64 | Missing `ocx.toml`, `--global` combined with `--project`, or multiple `--platform` values against a legacy (V1) lock (run `ocx lock` to migrate). |
| 65 | `--check` reported drift. |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 78 | Existing `ocx.lock` is malformed (parse error), `ocx.toml` schema-invalid, `--check` reported the lock is absent, or a requested `--platform` is not shipped by a tool. |
| 79 | Tag unresolvable during resolution (package not found in registry after retries). |
| 80 | Authentication failure against the registry. |
| 81 | `--offline` or `--frozen` and a tag is not cached locally (policy blocked). |

**JSON output** (`--format json`)

`ocx lock --format json` emits an array of objects, one per resolved tool:

| Field | Type | Description |
|-------|------|-------------|
| `binding` | string | The binding name from `ocx.toml` (the left-hand key). |
| `group` | string | Group the binding belongs to (`"default"` for the top-level `[tools]` table). |
| `digest` | string | Host-platform leaf digest in `sha256:<hex>` form. |
| `platforms` | object | Full available-only map: platform key string to leaf digest. Keys follow the lossless platform encoding (e.g. `"linux/amd64"`, `"darwin/arm64"`, `"any"`). |

Concurrent invocations of `ocx lock` and `ocx upgrade` are serialised via an in-place exclusive flock on `ocx.toml`. Readers (`ocx pull`, `git`, IDE tooling) never acquire any lock and are never blocked by a running `ocx lock`.

### `upgrade` {#upgrade}

Re-resolves every advisory tag in `ocx.toml` and rewrites `ocx.lock`. This is the **whole-file forced-bump verb**: every declared tag is re-resolved against the registry, even when the lock is already current. A moving tag (`:latest`, `:3`) advances to wherever it points today; an unchanged result rewrites the lock byte-identically. The operation is fully transactional — on any resolution failure nothing is written.

::: tip `ocx upgrade` vs `ocx lock`
`ocx upgrade` always re-resolves every tag. `ocx lock` only re-resolves when `ocx.toml` drifted (idempotent when clean). To advance versions, use `ocx upgrade`. To reconcile a changed config, use `ocx lock`.
:::

**Usage**

```shell
ocx upgrade [OPTIONS]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--pull` | — | After writing the lock, materialise all resolved tools into the object store and create their candidate symlinks. Default when `--no-pull` is absent. | on |
| `--no-pull` | — | Write the lock only; skip materialisation. Defer the install to a later `ocx pull` or first `ocx run`. | — |
| `--check` | — | Re-resolves every declared tag, compares the candidate to the predecessor, and exits 0 (matches) or 65 (`DataError`, a pin would change). No writes, no commit. When the predecessor lock is absent, exits 78. | off |
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for each named platform instead of the host. Repeatable and comma-separated. Selects which already-locked leaf to fetch (the lock stays host-agnostic); a target the publisher does not ship exits 78. Defaults to the current host. | *(current host)* |
| `--remote` | | Route tag resolution to the remote registry instead of the local index. | false |
| `-h`, `--help` | | Print help information. | |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global upgrade`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written, or `--check` confirmed the candidate matches. |
| 64 | Missing `ocx.toml`, `--global` combined with `--project`, or multiple `--platform` values against a legacy (V1) lock (run `ocx lock` to migrate). |
| 65 | `--check` reported the candidate would change pinned content (an advisory tag moved upstream). |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 75 | Transient failure (rate limit, temporary network error) — retry. |
| 78 | `ocx.toml` or existing `ocx.lock` malformed (parse error), `--check` invoked when the lock is absent, or a requested `--platform` is not shipped by a tool. |
| 80 | Authentication failure against the registry. |
| 81 | `--offline` or `--frozen` and a tag is not cached locally (policy blocked). |

**Examples**

```shell
# Re-resolve every declared tag:
ocx upgrade

# Re-resolve with a live registry lookup (bypass local index cache):
ocx upgrade --remote
```

Concurrent invocations of `ocx upgrade` and `ocx lock` are serialised via an in-place exclusive flock on `ocx.toml`.

### `pull` {#pull}

Pre-warms the [object store][fs-objects] from the project `ocx.lock` without
creating [install symlinks][fs-symlinks]. Distinct from
[`package pull`](#package-pull): this is the **project-tier** entry point — every
tool comes from the digest-pinned lock, never from the index — making it the
recommended primitive for reproducible CI setups.

`ocx pull` is read-only on `ocx.lock`. Re-resolution lives in `ocx upgrade`;
rewriting from the config lives in `ocx lock`.

**Usage**

```shell
ocx pull [OPTIONS]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--group <NAME>` | `-g` | Restrict the pull to one or more named groups. Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table. When omitted, every entry from the lock is pulled. | *(all groups)* |
| `--dry-run` | — | Print which locked tools are already cached vs. would be fetched, then exit without writing to the store. | off |
| `--platform <PLATFORM>` | `-p` | Pre-warm the leaf for each named platform instead of the host. Repeatable and comma-separated (e.g. `-p linux/arm64`). Selects which already-locked leaf to fetch (the lock stays host-agnostic — an amd64 host can pre-warm an arm64 leaf); a target the publisher does not ship exits 78. Defaults to the current host. | *(current host)* |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global pull`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (or empty group filter — nothing to pull). |
| 64 | Missing `ocx.toml`, unknown `--group` name, empty comma segment, `--global` combined with `--project`, or multiple `--platform` values against a legacy (V1) lock (run `ocx lock` to migrate). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 78 | `ocx.toml` present but `ocx.lock` is missing — run `ocx lock` first. |
| 78 | No leaf digest for the host (or requested `--platform`) at the locked version (and no `"any"` fallback key in `[tool.platforms]`) — the publisher does not ship that platform. |

**Lock mtime touch**

After a successful pull, `ocx pull` re-saves `ocx.lock` with byte-identical content so the file's mtime advances. This re-fires [`ocx direnv`](#direnv) `watch_file ocx.lock`, ensuring direnv refreshes the shell environment once the object store is warmed. The save is skipped under `--dry-run`.

#### Dry-run preview {#pull-dry-run}

`ocx pull --dry-run` resolves each locked tool through the local index
(cache-first, like the real pull does) and reports whether it is already in the
store. The store is never modified. Combine with [`--offline`](#arg-offline) to
forbid the cache-miss network probe entirely.

```shell
$ ocx pull --dry-run
Package                         Status       Path
localhost:5000/cmake@sha256:... cached       /home/me/.ocx/packages/...
localhost:5000/ripgrep@sha256:..would-fetch  -
```

The `Path` column matches the contract of [`ocx package which`](#package-which): it
is the **package root** (parent of `content/` and `entrypoints/`), not the
`content/` subdirectory. Consumers traverse into `<path>/content/` for files or
prefer [`ocx env`](#env) to compose `PATH` and friends.

The staleness gate fires ahead of the dry-run branch, so a stale lock still
exits 65 — the preview is not a way to bypass `declaration_hash` validation.
The output respects [`--format json`](#arg-format) and [`--quiet`](#arg-quiet).

### `run` {#run}

Spawns a child process whose environment is composed from the project's `ocx.lock`. This is the **project-tier** env-composition command — symbols are binding names from `ocx.toml`, not OCI identifiers. For OCI-identifier-based invocations, use [`exec`](#exec).

`--` is mandatory and at least one token after it is required. A missing `--` or empty argv produces exit 64.

**Usage**

```shell
ocx run [OPTIONS] [NAME...] -- ARGV...
```

**Arguments**

- `[NAME...]`: Zero or more binding names to include in the composed environment. Each name must exist and be unambiguous in the selected scope. When omitted, every binding in the selected scope is composed. The `-g` scope only *selects the namespace* for name resolution — when you name a subset, only those tools must resolve and install; an unrelated tool in scope that ships no leaf for the current host (exit 78) does not block the run. When you omit `NAME`, the whole scope is the set and every tool must resolve.
- `ARGV...`: Command to execute with arguments. The first token is the binary name; the rest are passed unchanged to the child. `--` is mandatory before `ARGV`.

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--group <NAME>` | `-g` | Scope env composition to the named group(s). Repeatable and comma-separated (`-g ci,lint -g release`). `default` selects `[tools]`; `all` expands to `default` + every declared `[group.*]`. | `[tools]` only |
| `--clean` | — | Start with a clean environment containing only the composed package variables, instead of inheriting the current shell environment. | off |
| `--self` | — | Expose each package's private-visibility env entries (same semantics as `ocx exec --self`). | off |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global run -- cmake --version`. The global file must exist (no auto-init for read commands). See [`--global`][global-flag].
:::

**Composition order**

> First by group-selection order (the order of `-g` flags after `all` expansion, deduplicated); then alphabetical by binding name within each group.

The composer prepends env entries in iteration order, so the **last group listed** has its `bin/` directories searched **first** in the child's `PATH`. Example: `-g default,ci` puts `[group.ci]`'s entries ahead of `[tools]`' on `PATH`; flip to `-g ci,default` to invert. Same rule applies within a group — alphabetically-later bindings land ahead of alphabetically-earlier ones.

**Reserved group keywords**

- `default` — always valid; selects the top-level `[tools]` table.
- `all` — always valid as a `-g` argument; expands to `[default, *named_groups_alphabetical]` before composition. Not declarable: `[group.all]` in `ocx.toml` exits 78 at parse time; `ocx add --group all` exits 64 at mutate time.

**Exit codes**

| Code | Meaning |
|------|---------|
| *(child)* | Child ran; its exit code is forwarded byte-for-byte. |
| 1 | Child spawn failed (binary not found, exec errno). |
| 64 | `--` missing; empty argv; empty `-g` segment; no `ocx.toml` found; unknown `-g` group; unknown binding NAME; ambiguous NAME across groups with conflicting identifiers; or `--global` combined with `--project`. (OCX remaps clap's default exit 2 to 64.) |
| 65 | `ocx.lock` is stale — run `ocx lock`. |
| 69 | Registry unreachable during auto-install of a missing package. |
| 78 | `ocx.lock` absent — run `ocx lock`; or `ocx.toml` parse error (e.g. `[group.all]` declared); or no leaf digest for the host platform at the locked version (no `"any"` fallback key in `[tool.platforms]`) — run `ocx update <tool>` to re-resolve. The host-leaf check fires only for tools actually composed: the named subset when `NAME` is given, or every tool in scope when it is omitted. |
| 79 | Package not found in registry during auto-install. |
| 80 | Authentication failure during auto-install. |

**Examples**

```shell
# Run task in the default [tools] environment
ocx run -- task build

# Run shellcheck from [group.ci] only
ocx run -g ci -- shellcheck ./script.sh

# Compose all groups and print the resulting environment
ocx run -g all -- env

# Use only the cmake binding from the default scope
ocx run cmake -- cmake --version

# Pass flags to the child (-- separates ocx args from child argv)
ocx run -g ci -- shellcheck --format=gcc ./script.sh

# Clean environment — only package-declared vars, no shell inheritance
ocx run --clean -- env
```

::: tip Project-tier vs OCI-tier
`ocx run` requires `ocx.toml` and `ocx.lock`. If you do not have a project file, use [`ocx exec`](#exec) with an OCI identifier instead.
:::

See [Project Toolchain In Depth → Running tools][in-depth-project-running] for composition order, PATH precedence, the `all` keyword, and worked examples.

### `remove` {#remove}

Removes one or more tool bindings from `ocx.toml`, rewrites `ocx.lock`, and uninstalls the tools.

Each argument accepts either a bare binding name (`cmake`), a name with a tag (`cmake:3.28`), or a fully-qualified identifier (`ocx.sh/cmake:3.28`). The binding key is always the repository basename — the tag and registry are used only to locate the correct entry and the installed package; the key match is against the TOML map key. Fails with exit code 79 if any argument matches no binding; the removals are staged together, so a fail-fast leaves `ocx.toml` unchanged.

When the same binding name appears in more than one group (e.g. in both `[tools]` and `[group.ci]`), `ocx remove` cannot determine which entry to drop and exits with code 64. Pass `--group` to make the target group explicit:

```shell
ocx remove cmake                  # ok — unambiguous
ocx remove --group ci shellcheck  # removes from [group.ci] only
ocx remove shellcheck             # error 64 — ambiguous; use --group
```

**Usage**

```shell
ocx remove [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more binding names or fully-qualified identifiers to remove (e.g. `cmake`, `cmake:3.28`, or `ocx.sh/cmake:3.28`).

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--group <NAME>` | `-g` | Remove the binding from this named group only. Use when the same name exists in multiple groups. |
| `--help` | `-h` | Print help information. |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global remove ripgrep`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Binding removed, lock rewritten. |
| 64 | No `ocx.toml` found in scope, binding name is ambiguous across groups (use `--group`), or `--global` combined with `--project`. |
| 65 | `ocx.toml` drifted from `ocx.lock` before this remove — run `ocx lock` to reconcile. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock on `ocx.toml`. Retry with backoff. |
| 78 | A survivor's legacy lock entry can no longer be migrated exactly — run `ocx upgrade` to re-resolve. Also: `ocx.toml` schema invalid or TOML parse error. |
| 79 | Binding not found in the specified group (or any group when `--group` is omitted). |

### `select` {#select}

> **Moved to `ocx package select`** — exits 64 if invoked as bare `ocx select`. See [`package select`](#package-select) for the current form.

Selects one or more packages as the current version by updating the [current symlink](../user-guide.md#path-resolution).

Each package is resolved using the [selected index](../user-guide.md#indices-selected).
No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx package select [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to select.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `-h`, `--help`: Print help information.


::: tip
`ocx package install --select` installs and selects in one step.
:::

See [path resolution modes](../user-guide.md#path-resolution) for how the `current` symlink is used downstream.

#### Entry-point name collisions {#select-entry-point-collision}

[Entry point](../in-depth/entry-points.md) name collisions are checked at two distinct points; `select` itself performs no collision check, since flipping `current` does not compose environments.

The first gate is **at install time**, scoped to a single package's interface closure. When `install` (with or without `--select`) downloads a package whose own bundle plus its interface-visible transitive deps declare the same entry-point name twice, the install aborts before the temp→object-store atomic move and exits with `EntrypointCollision` (exit code `65`, `DataError`). All N owning packages are listed so the publisher can deselect the right one.

The second gate is **at consumption time**, invoked whenever `ocx env` or `ocx exec` is given two or more roots. The compose-time check projects each root's interface surface (bundle entry points plus interface-visible TC entries) and reports the same `EntrypointCollision` error if two roots claim the same name. This catches conflicts that the per-package install gate cannot see — two roots installed independently without `--select` and combined only at exec time.

`select` is symlink-only: it flips `current` for a single package and never composes the environment, so it has no entry-point collision check. See [Exit codes](#exit-codes) for the full taxonomy.

### `shell` {#shell}

#### `hook` {#shell-hook}

> **REMOVED** — exits 64. The per-prompt hook model has been replaced by the `$OCX_HOME/env.sh` activation model. See [handshake_toolchain_cli.md §4] for the current activation contract. The `_OCX_APPLIED` fingerprint variable and the per-prompt hook are both gone.
>
> Use [`ocx direnv`](#direnv) for project-toolchain activation, or `eval "$(ocx --global env --shell=sh)"` for global toolchain activation.

#### `env` {#shell-env}

> **REMOVED** — exits 64. The `ocx shell env` command has been removed.
>
> For eval-safe shell export of package env, use the root [`ocx env --shell=<SHELL>`](#env-root) command (toolchain-tier) or [`ocx package env`](#package-env) for OCI-tier packages.

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Usage**

```shell
ocx shell completion [OPTIONS]
```

**Options**

- `--shell <SHELL>`: Shell to generate completions for. One of `bash`, `zsh`, `fish`, `elvish`, `powershell`. Auto-detected from the parent shell when omitted; ocx fails with an error if the detected shell is unsupported. `nushell` is not supported for completions (clap has no Nushell completion backend); this does not affect `ocx env --shell=nushell` activation, which works independently.

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

> **REMOVED** — exits 64. The `ocx shell init` command has been removed along with the per-prompt hook model.
>
> Global toolchain activation is now handled by `$OCX_HOME/env.sh`, written by the in-repo installer with a block-marker idempotent `.`-source line in the login profile. The file runs `eval "$(ocx --global env --shell=sh)"`. For project toolchain activation, use [`ocx direnv`](#direnv).

### `self` {#self}

The `ocx self` group manages the OCX installation itself: PATH activation, shell-completion injection, and binary self-update.

#### `self setup` {#self-setup}

Complete a bare-binary install: bootstrap OCX into the content store, write the per-shell env shims (`$OCX_HOME/env.*`), and add a managed activation block to the detected shell profiles.

This is the answer to "I won't pipe `curl` into a shell": download the standalone `ocx` binary from [GitHub Releases][releases], run `ocx self setup`, and reach the same state the install script produces — no shell script involved. The loose binary bootstraps the managed copy, writes the shims, and wires shell profiles in one command.

Setup runs three things in a hard order: **bootstrap first** (install the specified or latest published `ocx.sh/ocx/cli` so the shims have a `current` to point at — a no-op when the same version is already installed), then the five `env.*` shims, then the profile activation blocks. A failed bootstrap stops the run before any shim or profile is touched.

Re-running is safe. The shims and the managed block are diff-gated: an unchanged setup is a no-op. A stale ocx-authored block is rewritten in place (format upgrade); a legacy `# BEGIN ocx` block is migrated to the versioned fence. A block the user edited by hand is reported dirty and left untouched (exit 82) unless `--force` is passed.

**Usage**

```shell
ocx self setup [VERSION] [--no-modify-path] [--profile PATH]... [--dry-run] [--force]
```

**Arguments**

| Argument | Description |
|----------|-------------|
| `VERSION` | Optional version to install. Three forms are accepted: |
| | `1.2.3` — install the release with that tag. |
| | `sha256:<64hex>` — install the exact content identified by that digest (no tag resolution; written bare, without `@`). |
| | `1.2.3@sha256:<64hex>` — install that tag and verify it resolves to the given digest (immutability assertion). If the tag resolves to a different digest, the command fails with exit 65 and names both digests. Under `--frozen`, comparison uses the local index; a mismatch message hints `ocx index update`. |
| | Omit `VERSION` to install the latest published release. The literal value `latest` is treated as an ordinary tag lookup and resolves only if the registry publishes such a tag — omitting `VERSION` is the recommended way to request the latest release. Malformed input exits 64. |

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--no-modify-path` | — | Write the env shims only; skip every shell profile. Equivalent env var: [`OCX_NO_MODIFY_PATH`][env-ocx-no-modify-path] (truthy). The opt-out is not remembered between runs. | off |
| `--profile PATH` | — | Override the auto-detected profiles; repeatable. Explicit targets use POSIX-fence semantics regardless of file name. | *(autodetect)* |
| `--dry-run` | — | Report what would change and write nothing. Resolves the version and reports `WouldPull` with the resolved digest, but writes nothing. Never returns exit 82. | off |
| `--force` | — | Overwrite a managed block that carries user edits (the dirty state). | off |
| `-h`, `--help` | | Print help information. | — |

**Version grammar**

The `VERSION` positional applies to the `ocx.sh/ocx/cli` identifier as a suffix. It never accepts a registry or repository — only the tag, digest, or tag-plus-digest portion.

| Form | Example | Behavior |
|------|---------|----------|
| Tag only | `0.9.2` | Resolves the tag, installs, points `current` at it. |
| Digest only | `sha256:ab12…` | Fetches by content digest; `version` field omitted from JSON output. |
| Tag + digest | `0.9.2@sha256:ab12…` | Resolves tag, cross-checks digest (immutability assertion), fails closed on mismatch (exit 65). |

sha256 (64 hex chars) is the standard OCI digest algorithm; sha384 (96 hex chars) and sha512 (128 hex chars) are also accepted. Hex digits must be lowercase for all three algorithms — uppercase letters are rejected with exit 64.

Tag characters are restricted: the first character must match `[a-zA-Z0-9_]`; subsequent characters must match `[a-zA-Z0-9._-]`; maximum 128 characters. The `+` character is accepted in tag strings and normalized to `_` internally (the `adr_version_build_separator.md` convention).

A `sha256:` digest pin selects a **platform-specific** package digest — the same tag yields a different digest per OS and architecture. For CI matrices, pin by tag (each runner resolves its own platform digest) or supply a per-platform digest map; never share one digest across platforms.

When a pinned version is already installed and already pointed to by `current`, the command exits 0 with status `already_present` — no re-download.

When a pinned tag is semver-older than the currently installed version, a warning is emitted to stderr and the downgrade proceeds. This is an informational signal for CI logs, not a block.

The `[--frozen]` global flag affects pin resolution: a tag-only pin not present in the local index exits 81. A digest-only pin works under `--frozen` when the blobs are already cached locally.

**JSON output** (`--format json`)

A typical pinned run that pulled a new version:

```json
{
  "status": "completed",
  "bootstrap": {
    "status": "pulled",
    "version": "0.9.2",
    "digest": "sha256:ab12cd34..."
  },
  "shims": [
    "/home/alice/.ocx/env.sh",
    "/home/alice/.ocx/env.fish"
  ],
  "profiles": [
    {"path": "/home/alice/.bashrc", "outcome": "completed"},
    {"path": "/home/alice/.zshrc", "outcome": "no_op"}
  ],
  "reload_hint": true
}
```

The root object is discriminated by `status`:

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | Top-level run outcome — one of `completed`, `no_op`, `skipped`, `migrated` (see table below). |
| `bootstrap` | object | Nested sub-object describing the ocx binary install step (see below). |
| `shims` | array of strings | Absolute paths to the env shim files written during this run. Empty when no shims changed. |
| `profiles` | array of objects | Per-profile outcome: `{"path": "…", "outcome": "completed"|"no_op"|"migrated"|"skipped_dirty"}`. |
| `dirty_profiles` | array of strings | Paths of profiles that carried user edits and were skipped. Present only when `status` is `skipped`. |
| `exec_policy_warning` | string | Windows-only advisory when the execution policy is `Restricted`. Omitted when absent. |
| `conflicting_ocx` | string | Absolute path to a shadowing `ocx` binary found ahead of the shim directory on `PATH`. Omitted when absent. |
| `reload_hint` | boolean | `true` when at least one shim or profile was written and the shell must be re-sourced to activate the changes. Omitted when `false`. |

Root-level `status` values:

| Value | Meaning |
|-------|---------|
| `completed` | At least one shim or profile was written or upgraded. |
| `no_op` | Everything was already current; nothing changed. |
| `skipped` | At least one profile carried user edits and was left untouched (no `--force`). Exit 82. |
| `migrated` | A legacy activation block was migrated to the versioned fence; no dirty profiles. |

::: warning `jq .status` returns the root discriminant, not the bootstrap status
`jq .status` on a `self setup --format json` result returns `completed`, `no_op`, `skipped`, or `migrated` — the overall run outcome. The bootstrap-specific values (`pulled`, `already_present`, `would_pull`) are nested one level deeper under `bootstrap.status`. Use `jq .bootstrap.status` to inspect the binary install step.
:::

The `bootstrap` sub-object:

| Field | Type | Description |
|-------|------|-------------|
| `bootstrap.status` | string | Binary install outcome: `already_present`, `pulled`, or `would_pull` (dry-run). |
| `bootstrap.version` | string | Version string of the installed or would-install release. Omitted for digest-only pins. |
| `bootstrap.digest` | string | Platform-selected content digest in `sha256:<hex>` form. Present only on pinned runs; omitted on unpinned latest-release runs so JSON consumers stay byte-identical to prior behaviour. |

`bootstrap.status` values:

| Value | Meaning |
|-------|---------|
| `already_present` | The requested version was already installed: on a pinned run, `current` already pointed at the pinned digest; on an unpinned run, the latest published release was already current. |
| `pulled` | The version was downloaded and `current` updated. |
| `would_pull` | Dry-run: this version would be downloaded. |

The `version` field is omitted for digest-only pins. The `digest` field round-trips as a pin: use `jq -r .bootstrap.digest` to extract it and pass it back as `ocx self setup "0.9.2@$digest"`.

To script against the bootstrap outcome:

```shell
result=$(ocx --format json self setup 0.9.2)
root_status=$(echo "$result" | jq -r .status)          # completed / no_op / skipped / migrated
bootstrap_status=$(echo "$result" | jq -r .bootstrap.status)  # pulled / already_present / would_pull
digest=$(echo "$result" | jq -r '.bootstrap.digest // empty')  # sha256:<hex>, or empty when unpinned
```

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Setup completed, no-op, or migrated; or a dry-run (including over a dirty profile). |
| 64 | Malformed `VERSION` syntax (empty, short or uppercase hex, unknown algorithm, double `@`, trailing `@`). |
| 65 | `tag@digest` immutability assertion failed — the tag resolved to a different digest than the one specified. |
| 69 | Registry unreachable while bootstrapping. |
| 74 | I/O error writing a shim or shell profile. |
| 79 | The pinned tag or digest was not found in the registry. |
| 81 | A policy (`--offline` or `--frozen`) blocked resolution and the version was not cached locally. |
| 82 | A managed activation block carried user edits and `--force` was not passed. Scripts can `case $? in 82)` to detect this and re-run with `--force`. |

**Examples**

```shell
# Install the latest published release (default behavior):
ocx self setup

# Install a specific release by tag:
ocx self setup 0.9.2

# Install a specific release and assert the exact content:
ocx self setup 0.9.2@sha256:ab12cd34ef56...

# Install by digest alone (useful when a prior JSON run produced the digest):
ocx self setup sha256:ab12cd34ef56...

# Repeat a prior run's pin using the JSON output's digest field:
digest=$(ocx --format json self setup 0.9.2 | jq -r .bootstrap.digest)
ocx self setup "0.9.2@$digest"   # round-trip: asserts the same content
```

#### `self activate` {#self-activate}

Emit eval-safe shell activation lines for the current OCX installation.

Running `ocx self activate` prints three blocks of shell code to stdout:

1. A `PATH` prepend with the resolved absolute path to `<OCX_HOME>/symlinks/ocx.sh/ocx/cli/current/content/bin`. The path is resolved at runtime from the binary's own `OCX_HOME` — no shell variable reference is emitted.
2. A completion script for the detected shell — emitted inline into the activation stream, only when completions are enabled (skipped silently when `OCX_NO_COMPLETIONS=1` is set, when `--no-completion` is passed, when the session is non-interactive, or when the shell has no [`clap_complete`][clap-complete] backend). The completion block is emitted **first** so that, for PowerShell, its `using namespace` directives lead the stream — `Invoke-Expression` accepts them only as the first statement. The installer's `env.sh`/`env.ps1` shim decides interactivity itself and passes the explicit `--completion`/`--no-completion` flag; a direct `ocx self activate` with neither flag falls back to whether stderr is a terminal.
3. A global env eval line: `if command -v ocx >/dev/null 2>&1; then eval "$(ocx --global env --shell=NAME)"; fi` (POSIX form shown). Per-shell variants use the target shell's native idiom — `fish` uses `command -v ocx >/dev/null 2>&1; and ocx --global env --shell=fish | source`; `powershell`/`pwsh` use `if (Get-Command ocx -ErrorAction SilentlyContinue) { (ocx --global env --shell=pwsh) | Out-String | Invoke-Expression }`; `elvish` and `nushell` use their respective eval-from-string idioms.

The `OCX_HOME` assignment-with-fallback lives in `env.sh` itself — written once by the installer, not emitted by `ocx self activate`. See the [environment reference][env-ocx-home] for details.

The output is designed to be sourced from `$OCX_HOME/env.sh` at login:

```sh
: "${OCX_HOME:=$HOME/.ocx}"
export OCX_HOME
if command -v ocx >/dev/null 2>&1; then
    eval "$(ocx self activate --shell=sh)"
fi
```

*Simplified illustration; the installer writes a byte-identical `env.sh` shim — `OCX_HOME` is resolved at runtime via `${OCX_HOME:=$HOME/.ocx}`, not substituted at install time. Re-running the shim is safe because the emitted `PATH` updates are idempotent (move-to-front).*

**Usage**

```shell
ocx self activate [--shell[=NAME]] [--completion | --no-completion]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--shell[=NAME]` | — | Target shell dialect. Must use the `=` form (`--shell=bash`). Bare `--shell` (no value) and absent `--shell` both trigger autodetect from `$SHELL` or the parent process. Exit 64 if undetectable. `--shell=sh` ≡ `--shell=dash` (POSIX strict alias). | *(autodetect)* |
| `--completion` | — | Force completion injection on, regardless of session interactivity. | *(auto)* |
| `--no-completion` | — | Force completion injection off. Last of `--completion`/`--no-completion` wins. | *(auto)* |
| `-h`, `--help` | | Print help information. | — |

**Supported shells**

| Name | Dialect |
|------|---------|
| `sh` | POSIX strict (alias for `dash`) |
| `dash` | Dash |
| `bash` | Bash |
| `zsh` | Zsh |
| `ash` | Almquist shell |
| `ksh` | Korn shell |
| `fish` | Fish |
| `powershell` / `pwsh` | PowerShell |
| `elvish` | Elvish |
| `nushell` / `nu` | Nushell |
| `batch` / `cmd` | Windows CMD (Command Prompt) |

::: tip Shell completion coverage
Completion injection wraps [`clap_complete`][clap-complete]. Not every shell supported by `ocx self activate` has a `clap_complete` backend. Unsupported shells silently skip the completion block — PATH prepend and global env eval still run. Set `OCX_NO_COMPLETIONS=1` to suppress completion injection entirely.

Completions load only for **interactive** sessions. The installer's `env.sh`/`env.ps1` shim decides interactivity itself (`$-`, `status is-interactive`, `[Environment]::UserInteractive`) and passes the explicit `--completion`/`--no-completion` flag, so the gate never depends on the binary probing a stderr the shim has redirected. Non-interactive sources — scripts, `ssh host cmd` — get the PATH prepend and global env eval but skip the completion block entirely. A direct `ocx self activate` with neither flag falls back to whether stderr is a terminal.
:::

**Environment variables**

| Variable | Effect |
|----------|--------|
| [`OCX_NO_COMPLETIONS`][env-ocx-no-completions] | Set to a truthy value to skip the completion injection block. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Activation lines emitted successfully. |
| 64 | Shell undetectable (bare or absent `--shell` and `$SHELL` unset or unrecognised). |

#### `self update` {#self-update}

Check for a newer version of OCX and, if found, install it.

Both forms bypass the [auto-check throttle][env-ocx-update-check-interval] — explicit user intent always runs the lookup regardless of when the last automatic check ran.

Version discovery queries the registry directly for the newest published release — self update exists to reach the freshest upstream ocx, so it does not read the (possibly stale) [local index][fs-index]. This matches [`ocx self setup`](#self-setup) and the background update notice ocx prints on other commands. Under [`--offline`][arg-offline] the check is skipped and the running binary is left unchanged; [`--remote`][arg-remote] is redundant (already the default) but still accepted.

**Usage**

```shell
ocx self update [--check]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--check` | — | Report whether an update is available, without installing anything. | off |
| `-h`, `--help` | | Print help information. | — |

**Behavior without `--check`**

Queries the registry for the latest `major.minor.patch` release tag (rolling tags like `1`, `1.2`, build-tagged versions like `1.2.3+build`, and pre-releases like `1.2.3-rc1` are filtered out). If the resolved version is greater than the running binary, installs via the same path as `ocx package install --select`. Reports one of three outcomes:

- **Already up to date** — the running version is the latest.
- **Installed** — a newer version was downloaded and selected.
- **Skipped** — a soft failure (lookup unreachable, version unparseable) prevented the check; the running binary is unchanged.

After a successful install, `ocx self update` also refreshes the shell integration that `ocx self setup` owns: it regenerates the `$OCX_HOME/env.*` shims and re-applies the managed activation block in your shell profiles when its body has drifted from the current form. This refresh only *heals* an existing block — it never adds one where you have none (so a `--no-modify-path` install stays untouched) and never overwrites a block you have edited (it advises `ocx self setup --force` instead). When a block or shim is updated, it prints a one-line hint to re-source your profile.

**Behavior with `--check`**

Same lookup, no installation. Exits 0 when the lookup completes (including "already up to date" and "update available") — the result is printed to stdout. Exits 75 when the check is skipped (source unreachable, version unparseable, throttled). Use `ocx --format json self update --check` for machine-readable output.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Check or install succeeded (including "already up to date" and "update available"). |
| 69 | Registry unreachable. |
| 74 | I/O error writing the installed binary. |
| 75 | Skipped — soft failure (registry probe failed, version unparseable, throttled, bootstrap, etc.); the running binary is unchanged. |
| 79 | No release version found in the registry. |
| 80 | Authentication failure against the registry. |

**JSON output shape**

`ocx --format json self update [--check]` emits a single document with a `status` field:

```json
{"status": "up_to_date"}
{"status": "update_available", "identifier": "ocx.sh/ocx/cli:1.2.3"}
{"status": "installed", "from": "0.0.1", "to": "0.0.2"}
{"status": "skipped", "skipped_reason": {"reason": "offline"}}
{"status": "skipped", "skipped_reason": {"reason": "registry_probe_failed", "detail": "503 Service Unavailable"}}
```

`from` is omitted on `installed` when the previous version could not be determined (subprocess version query failed — bootstrap mode). `skipped_reason.reason` is one of:

| `reason` | Meaning | Carries `detail`? |
|------|---------|--------|
| `bootstrap` | Subprocess version query failed — binary absent, non-zero exit, or malformed JSON. | No |
| `offline` | OCX is in offline mode; no probe attempted. | No |
| `throttled` | Auto-check window has not elapsed (only emitted on the auto-check path; `self update [--check]` always bypasses). | No |
| `registry_probe_failed` | Remote tag listing returned an error. | Yes — error text |
| `not_found` | The canonical `ocx.sh/ocx/cli` repository was not found in the registry. | No |
| `unparseable_current` | The installed binary returned a version string that does not parse as a release version. | Yes — the offending string |
| `unparseable_latest` | The newest tag in the registry does not parse as a release version. | No |
| `no_release_tag` | No clean `major.minor.patch` release tag exists in the registry tag list. | No |

::: tip Dogfood install
`ocx self update` installs the new version into the package store and updates the `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current` symlink to point at it. No candidate symlink is created — only `current` is swapped. The same `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin` PATH entry that `ocx self activate` sets up resolves to the new binary automatically.
:::

### `uninstall` {#uninstall}

> **Moved to `ocx package uninstall`** — exits 64 if invoked as bare `ocx uninstall`. See [`package uninstall`](#package-uninstall) for the current form.

Removes the installed candidate for one or more packages.

Removes the [candidate symlink](../user-guide.md#path-resolution) and its back-reference. Object-store content is preserved unless `--purge` is given. To also remove the current symlink, pass `--deselect` or run [`package deselect`](#package-deselect) separately. To remove all unreferenced objects at once, use [`clean`](#clean).

**Usage**

```shell
ocx package uninstall [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to uninstall.

**Options**

- `-d`, `--deselect`: Also remove the [current symlink](../user-guide.md#path-resolution). Equivalent to running `ocx package deselect` after uninstall — see [`package deselect`](#package-deselect) for the full cleanup behavior.
- `--purge`: Delete the object from the store when no other references remain after uninstall.
- `-h`, `--help`: Print help information.

### `version` {#version}

Prints the ocx version. Without flags, prints a bare `major.minor.patch` string suitable for script consumption. With `--verbose`, prints a multi-line build provenance summary. JSON output always includes the populated subset of provenance fields regardless of `--verbose`.

**Usage**

```shell
ocx version [--verbose]
ocx --format json version
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--verbose` | `-v` | Emit multi-line build provenance: commit SHA, dirty flag, build timestamp, profile, target triple, rustc version, and CI run URL. Absent fields are suppressed — a local build without git shows no commit row; a build outside CI shows no CI row. | off |
| `-h`, `--help` | | Print help information. | — |

**Plain output — default (no flag)**

```
0.3.2
```

The bare semver string is the stable contract for script consumers. No trailing newline formatting varies by shell — pipe safely to `grep`, `cut`, or similar.

**Plain output — `--verbose`**

```
ocx 0.3.2-dev+20260528143045 (cargo: 0.3.1, channel: dev)
commit:   a1b2c3d4 (clean) — 2026-05-28T12:00:00Z
built:    2026-05-28T14:30:45Z (release)
target:   x86_64-unknown-linux-gnu
rustc:    1.79.0
ci:       https://github.com/ocx-sh/ocx/actions/runs/1234567890
```

Rows for `commit`, `built`/`target`/`rustc`, and `ci` appear only when the corresponding data was baked in at build time. Local `cargo build` without git shows no `commit` row; builds outside GitHub Actions show no `ci` row.

**JSON output**

`ocx --format json version` emits a single object. Only `version` is required; all other fields are optional and absent when their source data was unavailable at build time:

```json
{
  "version": "0.3.2-dev+20260528143045",
  "cargo_pkg_version": "0.3.1",
  "channel": "dev",
  "commit": {
    "sha": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
    "short": "a1b2c3d4",
    "describe": "v0.3.1-5-ga1b2c3d4",
    "dirty": false,
    "timestamp": "2026-05-28T12:00:00Z"
  },
  "build": {
    "timestamp": "2026-05-28T14:30:45Z",
    "profile": "release",
    "target": "x86_64-unknown-linux-gnu",
    "rustc": "1.79.0"
  },
  "ci": {
    "provider": "github-actions",
    "run_url": "https://github.com/ocx-sh/ocx/actions/runs/1234567890",
    "workflow": "release",
    "ref": "refs/tags/v0.3.2-dev",
    "sha": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
  }
}
```

`cargo_pkg_version` is present only when it differs from `version` — this occurs on dev-deploy builds where the effective version is overridden via `__OCX_BUILD_VERSION`. A stable release always omits this field.

The `version` key is the only field the [self update](#self-update) parser reads when comparing versions. Additional provenance fields are additive and ignored by the self-update parser — the JSON schema is open for extension without breaking wire compatibility.

### `ci` {#ci}

> **REMOVED** — exits 64. The `ocx ci` command group has been removed.
>
> CI environment export is available as the `--ci[=PROVIDER]` flag on [`ocx env`](#env-root) (toolchain-tier) and [`ocx package env`](#package-env) (OCI-tier). See [CI Integration][in-depth-ci] for full examples.

#### `export` {#ci-export}

> **REMOVED** — exits 64. See the [`ci`](#ci) section above.

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
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform. The file extension controls the compression algorithm: `.tar.xz` (LZMA, default), `.tar.gz` (Gzip), or `.tar.zst` (Zstandard).
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to a metadata file to bundle with the package. When provided, it is copied as a sidecar file next to the output archive. If omitted, no metadata sidecar is written.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`. Applies to whichever algorithm is selected.
- `-j`, `--threads <N>`: Number of compression threads. `0` (default) auto-detects from available CPU cores (capped at 16). `1` forces single-threaded compression. Affects LZMA (`.tar.xz`) and Zstandard (`.tar.zst`) compression; Gzip is always single-threaded.
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
digest-derived directory that [`package which`](#which) and [`exec`](#exec) resolve to.
The package root contains `content/` and `entrypoints/` as siblings; consumers
traverse one level in. Two pulls of the same digest are safe to run concurrently.

For project-tier setups driven by `ocx.lock`, use [`pull`](#pull) instead — it consumes the
lockfile directly and ignores the index.
:::

#### `push` {#package-push}

Publishes a package to the registry as zero or more layers. Each layer is uploaded as an OCI blob and recorded in a single image manifest, in the order given on the command line. A zero-layer push produces a config-only OCI artifact (referrer-only / description-only manifest) and requires `--metadata`.

**Usage**

```shell
ocx package push [OPTIONS] --platform <PLATFORM> --identifier <IDENTIFIER> <LAYERS>...
```

**Arguments**

- `<LAYERS>...`: Zero or more layers, in order (base layer first, top layer last). Each layer is either:
  - a path to a pre-built archive file (`.tar.gz`, `.tgz`, `.tar.xz`, `.txz`, `.tar.zst`, `.tzst`, or `.tar.zstd`), or
  - a digest reference of the form `sha256:<hex>.<ext>` (e.g. `sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08.tar.gz`) pointing at a layer that already exists in the target registry. The `<ext>` suffix is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. Bare digests are rejected.
  - Extension aliases: `.tgz` is accepted as an alias for `.tar.gz`, `.txz` for `.tar.xz`, and `.tzst` / `.tar.zstd` for `.tar.zst`. The canonical forms `tar.gz` / `tar.xz` / `tar.zst` are what ocx emits internally — aliases are normalized on parse.
  - To force file interpretation of a pathological filename that happens to match the digest shape, prefix it with `./` (e.g. `./sha256:abc….tar.gz`).
  - Omitting all layers produces a config-only OCI artifact with `layers: []`, valid for referrer-only / description-only manifests. `--metadata` is required in that case.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier including the tag, e.g. `cmake:3.28.1_20260216120000` (required).
- `-p`, `--platform <PLATFORM>`: Target platform of the package (required).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `cmake:3.28.1_20260216120000` automatically re-points the rolling ancestors (`cmake:3.28.1`, `cmake:3.28`, `cmake:3`, and `cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided (all layers are digest references, or the layer list is empty).
- `--build-timestamp [<FORMAT>]`: Append a UTC build-metadata segment to the published tag. `datetime` (default when flag passed bare) appends `_YYYYMMDDhhmmss`, `date` appends `_YYYYMMDD`, `none` is a no-op. The identifier's tag must already be `X.Y.Z` (optionally with a variant prefix or pre-release suffix) and must not already carry build metadata. Use this in continuous-deploy pipelines that publish rolling pre-release versions like `dev.ocx.sh/ocx/cli:0.3.0-dev_20260514120000`. The wire-format tag uses `_` (OCI tags forbid `+`); semver `+` is accepted on input and normalized. When the flag is omitted entirely, no build-metadata segment is appended. Passing `--build-timestamp=none` is the explicit equivalent.
- `-h`, `--help`: Print help information.

::: tip Layer reuse
Digest-referenced layers are not re-uploaded — ocx only HEADs the registry to verify they exist. This is the foundation of the [layer dedup model](../user-guide.md#file-structure-layers): a base layer pushed once can be referenced from any number of subsequent packages by digest.

```shell
# Push a fresh base + tool combination
ocx package push -p linux/amd64 -i mytool:1.0.0 base.tar.gz tool.tar.gz

# Reuse the same base by digest in a later release.
# The digest is the full 64-char sha256 hex written verbatim —
# the ellipsis is shown here only to keep the example short.
ocx package push -p linux/amd64 -i mytool:1.0.1 sha256:<hex>.tar.gz newtool.tar.gz
```
:::

::: warning Bring your own archives
`ocx package push` does not bundle a directory for you. Each file layer must be a pre-built archive. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy) and defeats layer reuse — use [`ocx package create`](#package-create) to produce a stable archive once, then push and reference it by digest from later commands.
:::

#### `test` {#package-test}

Materializes a package locally without a registry round-trip and runs a command or script in its composed env. Mirrors the argument shape of [`package push`][cmd-package-push]: identifier as `-i/--identifier`, then layers, then `--platform`. Either a trailing `-- CMD [ARGS...]` or a `--script PATH` is required; the two forms are mutually exclusive.

**Usage**

```shell
# Trailing-command form
ocx package test [OPTIONS] --platform <PLATFORM> --identifier <IDENTIFIER> [LAYERS]... -- <CMD> [ARGS]...

# Script form
ocx package test [OPTIONS] --platform <PLATFORM> --identifier <IDENTIFIER> [LAYERS]... --script <PATH|->
```

**Arguments**

- `<LAYERS>...`: Zero or more layers, in order (base first, top last). Same syntax as `package push`: a path to a `.tar.gz`/`.tar.xz`/`.tar.zst` archive, or a `sha256:<hex>.<ext>` digest reference to a layer already in the registry. Digest refs are fetched on demand; missing digest blobs when a local policy (`--offline` or `--frozen`) is active produce exit code 81 (`PolicyBlocked`).
- `-- <CMD> [ARGS]...`: Command to run inside the composed env. Required unless `--script` is given.

**Options**

| Name | Short | Description | Default |
|------|-------|-------------|---------|
| `--identifier <IDENTIFIER>` | `-i` | Package identifier in tag form (`repo:tag`) — required. An explicit `@digest` suffix is rejected (the digest is computed locally from the supplied layers). | — |
| `--platform <PLATFORM>` | `-p` | Target platform — required. | — |
| `--script <PATH\|->` | — | Path to a [Starlark][starlark-lang] test script, or `-` to read the script source from stdin. Mutually exclusive with the trailing `-- CMD` form. | — |
| `--metadata <PATH>` | `-m` | Path to the metadata JSON file. Defaults to a sibling of the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided. | auto-detected |
| `--keep` | — | Preserve the temp build directory after the command exits. Path is printed to stderr. Default temp root is `$OCX_HOME/temp/test/`. Mutually exclusive with `--output`. | false |
| `--output <DIR>` | `-o` | Materialize into `DIR` instead of an auto-managed temp dir. `DIR` must not exist or must be empty. Implies keep. Must reside on the same filesystem as `$OCX_HOME/layers/`. On Windows, must point under `$OCX_HOME/`. Mutually exclusive with `--keep`. | — |
| `--self` | — | Compose the package's private env surface (default: interface surface). Same semantics as [`ocx exec --self`][cmd-exec-self]. | false |
| `--clean` | — | Strip ambient parent env before composing — only `OCX_*` config and composed package vars reach the child. Mirrors [`ocx exec --clean`][cmd-exec-clean]. | false |
| `--help` | `-h` | Print help information. | — |

**Examples**

```shell
# Run the binary in its composed env (trailing-command form).
ocx package test -p linux/amd64 -i mytool:1.0.0 mytool.tar.xz -- mytool --version

# Run a Starlark test script against the package.
ocx package test -p linux/amd64 -i mytool:1.0.0 mytool.tar.xz --script smoke.star

# Read a Starlark script from stdin.
printf 'r = ocx.run("mytool", "--version")\nexpect.ok(r)\n' \
  | ocx package test -p linux/amd64 -i mytool:1.0.0 mytool.tar.xz --script -

# Keep the temp dir for inspection on failure.
ocx package test -p linux/amd64 --keep -i mytool:1.0.0 mytool.tar.xz -- mytool --version

# Materialize to a named directory.
ocx package test -p linux/amd64 --output ./build -i mytool:1.0.0 mytool.tar.xz -- mytool --version

# Explicit metadata path + digest base layer.
ocx package test -p linux/amd64 -m metadata.json -i mytool:1.0.1 \
  sha256:<hex>.tar.xz ./newtool.tar.xz -- mytool --version
```

::: tip Tempdir lifecycle
Without `--keep` or `--output`, the temp directory is deleted on any exit — success or failure. Use `--keep` to opt in to preservation on failure. Re-run with `--keep` to inspect.
:::

**Exit codes — `--script` branch**

| Code | Meaning |
|------|---------|
| 0 | All expectations passed |
| 1 | An expectation failed; `expect.fail` or `fail()` was called; or a host API returned a failure |
| 64 | Usage error — both `--script` and `-- CMD` supplied; neither supplied; script file not found or unreadable |
| 65 | Script syntax, type, or arity error |
| 74 | I/O error — stdin read failure (`--script -`) or scratch directory creation failure |

Exit code is the primary machine signal. When `--format json` is passed, a structured `ScriptRunReport` envelope is written to stdout alongside the exit code:

```json
{
  "status": "passed|failed|usage|script_error|io|timeout",
  "assertion": { "kind": "ok|eq|ne|…|unknown", "message": "…" },
  "run":       { "exit_code": 0, "stdout": "…", "stderr": "…", "duration_ms": 12, "truncated": false }
}
```

`assertion` and `run` are `null` when not applicable. `assertion.kind` reflects the failing `expect.*` function and is the stable machine field; `assertion.message` prose is not stable. The three top-level keys and their sub-field shapes are stable v1 contract.

The `--script` command returns `Ok(ExitCode)` directly — it bypasses `classify_error` so exit codes always match this table regardless of upstream error state.

See the [testing locally guide][authoring-testing] for a full pre-push workflow example including the scripted form.

#### `inspect` {#package-inspect}

Inspects what sits at a package reference — nothing is installed and no symlinks are created. It is not strictly free of local writes: default mode resolves the tag through the index, so a tag cache miss may populate the local index / blob cache (a `Resolve`-class read). The output adapts to the reference shape:

- **Default, image-index reference** (the usual multi-platform tag): lists the platform **candidates** — for each child manifest the platform, child digest, media type, and size. No metadata is loaded and no platform is selected.
- **Default, single-manifest reference** (a flat tag or an `@digest` pointing directly at an image manifest): emits the declared **metadata** (bundle version, `strip_components`, env vars, dependencies, entrypoints) plus the manifest's **layers** (digest, media type, size). No resolution chain.
- **`--resolve`**: platform-selects through the index, then emits metadata and layers plus the OCI **resolution** chain (the walk-order `index` → `manifest` → `config` blobs).

Unlike [`package test`][cmd-package-test], the identifier accepts an explicit `@digest` (a tag or digest both resolve).

**Usage**

```shell
ocx package inspect [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more package identifiers to inspect. Each is a tag (`repo:tag`) or `@digest`.

With more than one identifier, JSON output is an object keyed by the requested identifier (`{"<id>": {...}}`, keyed even for a single package); plain output renders each package's tree in request order.

**Options**

- `-p`, `--platform <PLATFORM>`: Platform to select. Applies **only** with `--resolve`; ignored in default mode (the candidate list always shows every platform).
- `--resolve`: Platform-select through the index and emit the resolution chain — the pinned identifier and the walk-order chain blob descriptors (index → platform manifest → config blob, each with its `role`, media type, and size) — alongside the metadata and layers (the layers are shown for the selected manifest in both default and `--resolve` mode).
- `-h`, `--help`: Print help information.

Honors the global [`--offline`][arg-offline], [`--remote`][arg-remote], and [`--format`][arg-format] flags. JSON is the primary consumer surface.

**JSON shape**

The top-level JSON is an object keyed by the requested identifier — `{"<id>": <per-package>}`, keyed even for a single package. Each value has one of the shapes below.

Default, image index — candidate listing:

```json
{
  "identifier": "registry/repo:tag",
  "pinned_digest": "sha256:…",
  "candidates": [
    { "digest": "sha256:…", "platform": "linux/amd64", "media_type": "…", "size": 123 }
  ]
}
```

Default, single manifest (`@digest` or flat tag) — metadata plus layers:

```json
{
  "identifier": "registry/repo@sha256:…",
  "pinned_digest": "sha256:…",
  "metadata": { "type": "bundle", "version": 1, "env": [], "dependencies": [], "entrypoints": {} },
  "layers": [{ "digest": "sha256:…", "media_type": "…", "size": 123 }]
}
```

`--resolve` — platform-selected metadata and layers + chain:

```json
{
  "identifier": "registry/repo:tag",
  "pinned_digest": "sha256:…",
  "platforms": ["linux/amd64"],
  "metadata": { "type": "bundle", "version": 1, "env": [], "dependencies": [], "entrypoints": {} },
  "layers": [{ "digest": "sha256:…", "media_type": "…", "size": 123 }],
  "resolution": {
    "pinned": "registry/repo:tag@sha256:…",
    "chain": [
      { "digest": "sha256:…", "role": "index", "media_type": "…", "size": 429 },
      { "digest": "sha256:…", "role": "manifest", "media_type": "…", "size": 448 },
      { "digest": "sha256:…", "role": "config", "media_type": "…", "size": 244 }
    ]
  }
}
```

**Examples**

```shell
# List the platforms a multi-platform tag offers (output keyed by identifier).
ocx --format json package inspect mytool:1.0.0 | jq '.["mytool:1.0.0"].candidates'

# Inspect several packages at once — one keyed entry each.
ocx --format json package inspect mytool:1.0.0 othertool:2.0.0

# Inspect one platform child by digest (same repo, online or cached).
ocx package inspect mytool@sha256:abc…

# Platform-select and include the OCI resolution chain.
ocx --format json package inspect --resolve -p linux/arm64 mytool:1.0.0 | jq '.["mytool:1.0.0"].resolution'
```

**Plain output**

With `--format plain` (the default) the report renders as a tree rooted at the pinned identifier. The candidate listing shows one node per platform child; the single-manifest view shows the `metadata` branch (`env`, `dependencies`, and `entrypoints`) followed by a `layers` branch listing each layer by index, annotated with media type and a human-readable size. Under `entrypoints`, an entry whose dispatch command diverges from its invocable name carries a `→ <command>` annotation; entries whose command matches the name (the common case) are shown without annotation:

```text
registry/repo@sha256:…
├─ metadata
│  └─ entrypoints
│     ├─ fmt → cargo-fmt
│     └─ build
└─ layers
   └─ [0] · sha256:… · application/vnd.oci.image.layer.v1.tar+xz · 192 B
```

Here `fmt` dispatches to the `cargo-fmt` binary while `build` dispatches to a binary named `build`.

With `--resolve`, a `resolution` branch is added alongside `metadata` and `layers`, listing each chain blob by its `role` (`index`, `manifest`, `config`), annotated with media type and a human-readable size. The layers stay under the manifest — they are content the manifest references, not steps in the walk:

```text
registry/repo:tag@sha256:…
├─ metadata
│  └─ …
├─ layers
│  └─ [0] · sha256:… · application/vnd.oci.image.layer.v1.tar+xz · 192 B
└─ resolution
   ├─ pinned · registry/repo:tag@sha256:…
   └─ chain
      ├─ index · sha256:… · application/vnd.oci.image.index.v1+json · 429 B
      ├─ manifest · sha256:… · application/vnd.oci.image.manifest.v1+json · 448 B
      └─ config · sha256:… · application/vnd.sh.ocx.package.v1+json · 244 B
```

**Exit codes**

- `79` (`NotFound`) — the tag or digest does not resolve.
- `81` (`PolicyBlocked`) — a local policy (`--offline` or `--frozen`) refused the resolution: the manifest or config blob is absent from the local cache, or an unpinned tag was not in the local index.
- `65` (`DataError`) — the resolved metadata is malformed or fails validation.

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

Displays description metadata for one or more packages from the registry.

JSON output is an object keyed by the requested identifier (`{"<id>": {...}|null}`, keyed even for a single package); plain output always prints a `== <id> ==` header line per package, even for a single package, followed by its description fields.

**Usage**

```shell
ocx package info [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more package identifiers (repository only).

**Options**

- `--save-readme <PATH>`: Save the README to a file or directory. Requires exactly one identifier.
- `--save-logo <PATH>`: Save the logo to a file or directory. Requires exactly one identifier.
- `-h`, `--help`: Print help information.

#### `install` {#package-install}

Downloads and installs one or more packages into the local object store.

Installs packages into the [object store][fs-objects] and creates a [candidate symlink][fs-symlinks] for each package. If a package declares [dependencies][ug-dependencies], all transitive dependencies are downloaded to the object store automatically — only the explicitly requested packages receive install symlinks.

This is the OCI-tier install command. For project-tier installs driven by `ocx.toml`, use [`ocx add`](#add).

**Usage**

```shell
ocx package install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platforms to consider. Defaults to the current platform. |
| `-s`, `--select` | | After installing, update the [current symlink][fs-symlinks] for each package to point to the newly installed version. |
| `-h`, `--help` | | Print help information. |

::: warning Windows: `PATHEXT` must include `.CMD`
On Windows, `package install` prints a stderr warning when the host shell's `PATHEXT` is missing `.CMD`. Generated entrypoint launchers are `.cmd` files and require `PATHEXT` to advertise that extension before bare-name lookup (e.g. `cmake`) can find them.
:::

#### `uninstall` {#package-uninstall}

Removes the installed candidate for one or more packages.

Removes the [candidate symlink][fs-symlinks] and its back-reference. Object-store content is preserved unless `--purge` is given. To also remove the current symlink, pass `--deselect` or run [`package deselect`](#package-deselect) separately. To remove all unreferenced objects at once, use [`clean`](#clean).

**Usage**

```shell
ocx package uninstall [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to uninstall.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-d`, `--deselect` | | Also remove the [current symlink][fs-symlinks]. Equivalent to running `ocx package deselect` after uninstall. |
| `--purge` | | Delete the object from the store when no other references remain after uninstall. |
| `-h`, `--help` | | Print help information. |

#### `select` {#package-select}

Selects one or more packages as the current version by updating the [current symlink][fs-symlinks].

No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx package select [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to select.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platforms to consider. |
| `-h`, `--help` | | Print help information. |

::: tip
`ocx package install --select` installs and selects in one step.
:::

#### `deselect` {#package-deselect}

Removes the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its [candidate symlink][fs-symlinks] and object-store content remain intact. The symlink removal is idempotent — an already-absent link is not an error.

**Usage**

```shell
ocx package deselect <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to deselect.

**Options**

- `-h`, `--help`: Print help information.

#### `exec` {#package-exec}

Executes a command within the environment of one or more OCI-tier packages.

This is the OCI-tier equivalent of the root [`exec`](#exec) command. Identifiers are OCI references (e.g. `cmake:3.28`), resolved through the index and auto-installed when missing. For project-tier execution driven by `ocx.toml`, use [`ocx run`](#run).

**Usage**

```shell
ocx package exec [OPTIONS] <PACKAGES>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<PACKAGES>`: OCI identifiers to resolve (e.g. `cmake:3.28`).
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platforms to consider. |
| `--clean` | | Start with a clean environment; only package-declared variables and `OCX_*` config vars reach the child. |
| `--self` | | Use the self view (expose `private` + `public` entries). Default: consumer view (`public` + `interface` only). |
| `-h`, `--help` | | Print help information. |

#### `env` {#package-env}

Print the resolved environment variables for one or more OCI-tier packages.

Output format is controlled by the root [`--format`](#arg-format) flag (default: `plain`). Plain format outputs an aligned table with `Key`, `Type` and `Value` columns. JSON format (`ocx --format json package env`) outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`. Use `--shell[=NAME]` for eval-safe shell export lines — the only sourceable form.

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies).
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

**Usage**

```shell
ocx package env [OPTIONS] <PACKAGE>...
ocx --format json package env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platforms to consider. |
| `--candidate`, `--current` | | Path resolution mode — see [Path Resolution](#path-resolution). |
| `--self` | | Self view: emits `private` + `public` entries. Default: consumer view (`public` + `interface`). |
| `--shell[=NAME]` | | Emit eval-safe shell export lines for the named dialect. Same conventions as root [`ocx env --shell`](#env-root). Mutually exclusive with `--ci`. |
| `--ci[=PROVIDER]` | | Write the resolved environment into the CI system's persistence channel for later pipeline steps. `PROVIDER` ∈ `github` / `github-actions`, `gitlab` / `gitlab-ci`. Bare `--ci` auto-detects. Equals-form required. Mutually exclusive with `--shell`. |
| `--export-file=PATH` | | Write [GitLab CI/CD][gitlab-ci-export-docs] JSON-lines to `PATH`. Requires `--ci=gitlab`; exit 64 for `--ci=github` or without `--ci`. |
| `-h`, `--help` | | Print help information. |

::: warning `--ci=gitlab` requires GitLab Functions / step runner
`--ci=gitlab` writes JSON-lines (`{"name":"…","value":"…"}`), which is the format consumed by the [GitLab step runner][gitlab-step-runner-docs] via `${{ export_file }}` (experimental, `run:` keyword jobs only). It does **not** work with traditional `script:` jobs. See [CI Integration][in-depth-ci] for a full step-runner example.
:::

::: info Windows: synthetic `PATHEXT ⊳ .CMD`
On Windows, `package env` prepends `.CMD` to `PATHEXT` in its output when the host shell's `PATHEXT` does not already include it. Generated entrypoint launchers are `.cmd` files; this lets callers that adopt the printed env find launchers by bare name without further configuration.
:::

<!-- external -->
[releases]: https://github.com/ocx-sh/ocx/releases/latest
[cargo]: https://doc.rust-lang.org/cargo/
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-actions-workflow-commands]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions
[gitlab-ci-export-docs]: https://docs.gitlab.com/ee/ci/variables/#pass-an-environment-variable-to-another-job
[gitlab-step-runner-docs]: https://docs.gitlab.com/ci/functions/create/
[gitlab-ci-dotenv]: https://docs.gitlab.com/ee/ci/yaml/artifacts_reports.html#artifactsreportsdotenv
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[sysexits-manpage]: https://man.freebsd.org/cgi/man.cgi?sysexits
[gnu-parallel-j0]: https://www.gnu.org/software/parallel/parallel.html
[starlark-lang]: https://github.com/bazelbuild/starlark

<!-- in-depth -->
[exec-modes]: ../in-depth/environments.md#visibility-views
[env-composition-forwarding]: ../in-depth/environments.md#ocx-forwarding
[in-depth-project-running]: ../in-depth/project.md#running
[env-composition-strict-isolation]: ./env-composition.md#strict-isolation
[in-depth-ci]: ../in-depth/ci.md

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-ocx-frozen]: ./environment.md#ocx-frozen
[env-no-config]: ./environment.md#ocx-no-config
[env-config]: ./environment.md#ocx-config
[env-project]: ./environment.md#ocx-project
[env-no-project]: ./environment.md#ocx-no-project
[env-ocx-quiet]: ./environment.md#ocx-quiet
[env-ocx-jobs]: ./environment.md#ocx-jobs
[env-docker-config]: ./environment.md#external-docker-config
[env-ocx-home]: ./environment.md#ocx-home
[env-ocx-no-modify-path]: ./environment.md#ocx-no-modify-path
[env-ocx-no-completions]: ./environment.md#ocx-no-completions
[env-ocx-update-check-interval]: ./environment.md#ocx-update-check-interval
[env-github-actions]: ./environment.md#external-github-actions
[env-github-env]: ./environment.md#external-github-env
[env-github-path]: ./environment.md#external-github-path
[env-gitlab-ci]: ./environment.md#external-gitlab-ci

<!-- external: completions -->
[clap-complete]: https://docs.rs/clap_complete/latest/clap_complete/

<!-- reference -->
[config-ref]: ./configuration.md

<!-- external: login/logout interop -->
[docker-login]: https://docs.docker.com/reference/cli/docker/login/
[docker-logout]: https://docs.docker.com/reference/cli/docker/logout/
[oras-login]: https://oras.land/docs/commands/oras_login/
[oras-logout]: https://oras.land/docs/commands/oras_logout/
[helm-logout]: https://helm.sh/docs/helm/helm_registry_logout/

<!-- internal -->
[entry-points]: ./metadata.md#entry-points
[guide-entry-points]: ../in-depth/entry-points.md
[exit-codes]: #exit-codes
[fs-objects]: ../user-guide.md#file-structure-packages
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
[fs-index]: ../user-guide.md#indices-local
[ug-dependencies]: ../user-guide.md#dependencies
[ug-deps-env]: ../user-guide.md#dependencies-environment

<!-- commands (package-test options) -->
[cmd-package-push]: #package-push
[cmd-package-test]: #package-test
[cmd-exec-self]: #exec
[cmd-exec-clean]: #exec

<!-- global flags (package-inspect) -->
[arg-offline]: #arg-offline
[arg-remote]: #arg-remote
[arg-format]: #arg-format

<!-- commands (package group) -->
[cmd-package-install]: #package-install
[cmd-package-uninstall]: #package-uninstall
[cmd-package-select]: #package-select
[cmd-package-deselect]: #package-deselect
[cmd-package-exec]: #package-exec
[cmd-package-env]: #package-env

<!-- global flag -->
[global-flag]: #global-flag

<!-- commands (root env) -->
[cmd-env-root]: #env-root
[version-json-schema]: #version

<!-- authoring -->
[authoring-testing]: ../authoring/testing.md
