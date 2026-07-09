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

When `--global` is set, the following toolchain-tier commands target `$OCX_HOME/ocx.toml` instead of a discovered project file: `add`, `remove`, `lock`, `update`, `pull`, `run`, and `env`.

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

OCX aligns with BSD [sysexits.h][sysexits-manpage] (codes 64–78) for the standard failure categories, and reserves 79–84 for OCX-specific cases. The numeric values are stable across releases — `case $?` works.

:::info
The sysexits.h convention originates in BSD Unix and is documented at [man.freebsd.org][sysexits-manpage]. It assigns semantic meaning to exit codes 64–78, leaving 79–127 free for tool-specific use. OCX occupies 79–84.
:::

| Code | Name | Mnemonic | When used | Recovery |
|------|------|----------|-----------|----------|
| 0 | Success | — | Successful completion | — |
| 1 | Failure | — | Generic failure — only when no specific code applies | Inspect stderr |
| 64 | UsageError | EX_USAGE | Bad CLI invocation: unknown flag, wrong argument count, invalid syntax; `package verify` given only one of `--certificate-identity` / `--certificate-oidc-issuer`, or given neither with no matching [`[[trust.policy]]`][config-trust] scope | Check the command syntax |
| 65 | DataError | EX_DATAERR | Input data malformed: bad identifier, invalid digest, corrupted manifest, tampered Sigstore bundle; also a platform feature mismatch — the package ships for the host os/arch but no candidate's `os.features` are a subset of the host's (e.g. glibc vs musl), see [`--platform`](#package-install); also an ambiguous selection — a dual-libc host matched two equally-specific candidates (see [libc differentiation][authoring-libc]) | Validate identifiers and file contents; for a feature mismatch or ambiguous selection, override with `--platform` |
| 69 | Unavailable | EX_UNAVAILABLE | Required resource unavailable: network down, registry unreachable | Retry; check network and registry URL |
| 74 | IoError | EX_IOERR | I/O error: filesystem permission denied, disk full, read/write failure | Check filesystem permissions and free space |
| 75 | TempFail | EX_TEMPFAIL | Temporary failure that may succeed on retry: rate limit, transient network | Retry with backoff |
| 77 | PermissionDenied | EX_NOPERM | Insufficient permissions: registry 403, filesystem EPERM, offline sign refused, OIDC pre-check failed | Refresh credentials or adjust filesystem permissions |
| 78 | ConfigError | EX_CONFIG | Configuration error: bad config file, missing required field, parse failure, trust root unavailable, a matched [`[[trust.policy]]`][config-trust] entry is malformed | Inspect the config file at the printed path |
| 79 | NotFound | OCX | Resource not found: package 404, explicit config path absent, no signatures found for target | Pin a different version or correct the path |
| 80 | AuthError | OCX | Authentication failure: registry 401, missing credentials, Fulcio OIDC token rejected | Refresh or set registry credentials |
| 81 | PolicyBlocked | OCX | A deliberate local policy (`--offline` or `--frozen`) refused a network or resolution operation — not a fault. Includes an unpinned-tag resolve that the policy forbade | Loosen the flag, or populate the local index first (e.g. `ocx index update`) |
| 82 | DirtyRcBlock | OCX | A managed shell-integration block carried user edits and `ocx self setup` ran without `--force`; the block was left untouched. Distinct from ConfigError (78): the content is valid but intentionally user-modified | Re-run with `--force`, or edit the block manually and re-run |
| 83 | RekorUnavailable | OCX | Rekor transparency log unreachable during sign or verify (5xx/timeout, or SET absent with only TSA present) | Retry later; check Rekor endpoint |
| 84 | ReferrersUnsupported | OCX | Registry does not implement the OCI Referrers API — sign and verify require OCI 1.1 referrers support | Use a registry with OCI 1.1 referrers support |

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
    82) echo "managed shell rc block left dirty; rerun with --force" ;;
    83) echo "Rekor unavailable; retry signing or verification later" ;;
    84) echo "registry lacks OCI referrers support; use a compatible registry" ;;
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
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for the named platform instead of the host — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. The lock already pins every shipped platform's leaf, so this only selects which to fetch — the lock stays host-agnostic (an amd64 host can pre-warm an arm64 leaf). Defaults to the current host. A platform the publisher does not ship exits 78. |
| `--help` | `-h` | Print help information. |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand to target `$OCX_HOME/ocx.toml`: `ocx --global add ripgrep:14`.
See [`--global`][global-flag] for the full root-flag reference.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Binding added, lock updated, tool installed. |
| 64 | No `ocx.toml` found, binding already exists, invalid `--group` name, `--global` combined with `--project`, or more than one `--platform` value (single-valued flag). |
| 65 | `ocx.toml` drifted from `ocx.lock` before this add — run `ocx lock` to reconcile. |
| 69 | Registry unreachable while resolving the new tag. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock on `ocx.toml`. Retry with backoff. |
| 78 | `ocx.lock` uses an unsupported version — V1 and V2 locks are rejected; regenerate with `ocx lock`. Also: `ocx.toml` schema invalid or TOML parse error, or a requested `--platform` is not shipped by a tool. |
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
- `-p`, `--platform`: Target platform to consider when resolving packages.
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

A tool missing from the local object store is auto-installed as part of composition. Because it auto-installs, a tool covered by a [`[[trust.policy]]`][config-trust] is signature-verified first — the same gate as [`package install`](#package-install) (see its auto-verify contract). No `--verify`/`--no-verify` flag here; opt out via [`OCX_NO_VERIFY`][env-no-verify].

`--shell` requires the equals-form (`--shell=bash`, not `--shell bash`) to prevent shell injection through unquoted positional tokens.

**Usage**

```shell
ocx env [OPTIONS]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--group <NAME>` | `-g` | Scope env composition to the named group(s). Repeatable and comma-separated (`-g ci,lint -g release`). `default` selects `[tools]`; `all` expands to `default` + every declared `[group.*]`. An unknown group exits 64 in the project tier; the global tier is lenient (matches nothing, empty env). | `[tools]` only |
| `--shell[=NAME]` | — | Emit eval-safe shell export lines for the named shell dialect. `NAME` is one of `bash`, `zsh`, `fish`, `sh` (POSIX/Dash), `powershell`, `nushell`, `elvish`. The equals-form is required — passing `--shell NAME` as two tokens is rejected with exit 64. `--shell` bare (no `=NAME`) autodetects from `$SHELL`. Mutually exclusive with `--ci`. | *(unset — uses `--format`)* |
| `--ci[=PROVIDER]` | — | Write the composed environment into the CI system's persistence channel so the exported variables and paths are available to **later pipeline steps**. `PROVIDER` is one of `github` (alias `github-actions`) or `gitlab` (alias `gitlab-ci`). The equals-form is required (`--ci=github`, not `--ci github`). Bare `--ci` (no `=PROVIDER`) auto-detects from [`GITHUB_ACTIONS`][env-github-actions] and [`GITLAB_CI`][env-gitlab-ci]; no provider detected exits 64. Mutually exclusive with `--shell`. | *(unset)* |
| `--export-file=PATH` | — | Write GitLab CI/CD JSON-lines output to `PATH` instead of stdout. Requires `--ci=gitlab`. Rejected with exit 64 when combined with `--ci=github` (GitHub infers its sink from [`GITHUB_ENV`][env-github-env] and [`GITHUB_PATH`][env-github-path]) or when given without `--ci`. | *(unset — stdout for gitlab)* |
| `--platform <PLATFORM>` | `-p` | Compose the environment for a single target platform instead of the host (cross-build export). Single-valued: passing more than one exits 64. A tool that ships no leaf for the target exits 78 (project tier) or is skipped (global tier, lenient). Defaults to the current host. | *(current host)* |
| `--pull` | — | Materialise missing tools into the object store before composing (single batched install, like `ocx run`). A tool already present resolves locally with no network — only a genuine miss pulls. Last-wins with `--no-pull`. Ignored under `--global` — the global tier never installs. | **default** |
| `--no-pull` | — | Skip the install fallback: resolve against local state only. A lock-pinned tool that is not materialised is reported on stderr with an `ocx pull` hint and omitted from the composed env; the command never contacts the registry and the exit code stays 0. | — |
| `--show-patches` | — | Annotate each entry with its origin. When [`[patches]`][config-patches] is configured, companion overlay entries are appended after the toolchain's own entries; this flag adds a `Source` column to the plain table (a `"source"` object in JSON) naming the descriptor rule and companion that produced each overlay entry. No effect when `[patches]` is not configured. Mutually exclusive with `--shell` and `--ci`. | false |
| `-h`, `--help` | | Print help information. | — |

**Reserved group keywords**

- `default` — always valid; selects the top-level `[tools]` table.
- `all` — always valid as a `-g` argument; expands to `[default, *named_groups_alphabetical]` before composition (identical to [`run`](#run)). Not declarable: `[group.all]` in `ocx.toml` exits 78 at parse time; `ocx add --group all` exits 64 at mutate time.

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

::: tip `ocx env` installs missing tools by default
The exporter resolves each lock-pinned tool locally first — a tool already in the object store needs no network (its digest is content-addressed, nothing to look up). Only a genuine miss falls through to install it inline, like [`ocx run`](#run). Pass `--no-pull` to skip that fallback and stay strictly offline: unmaterialised tools are warned about on stderr and omitted (the deterministic-CI shape), and the command never downloads.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success. Under `--global`, any unusable global toolchain — not configured, or a corrupt/stale `$OCX_HOME/ocx.lock` — is a valid empty environment, not an error (report path and `--shell` path alike). The global tier is lenient. |
| 64 | Unknown `--group` name (project tier only — the global tier is lenient and yields an empty env); empty `--group` comma segment; `--shell NAME` passed as two tokens (use `--shell=NAME`); `--ci` and `--shell` used together; `--export-file` given without `--ci` or combined with `--ci=github`; bare `--ci` (auto-detect) used outside a recognized CI environment; more than one `--platform` (env composes a single environment); `--global` combined with `--project`; or no `ocx.toml` in scope (project tier). |
| 65 | `ocx.lock` is stale — run `ocx lock` (project tier). |
| 78 | `ocx.toml` or `ocx.lock` parse error (project tier); or `--ci=github` used outside [GitHub Actions][github-actions-workflow-commands] where [`GITHUB_ENV`][env-github-env] and [`GITHUB_PATH`][env-github-path] are unset. |

The global tier is lenient: `ocx --global env` never fails on an unconfigured or corrupt global toolchain — it exports an empty environment. This is one predictable rule that does not depend on `--shell` (which only selects the output format, never whether the command errors). A corrupt global lock surfaces instead via the commands that rewrite it — `ocx --global lock`, `ocx --global add`, `ocx --global update`. The project tier stays strict (a missing/stale/corrupt `ocx.lock` errors). A useful consequence of the lenient global rule: the installer's `env.sh`/`env.ps1`, which source `ocx --global env --shell=…` on every shell start, can never be broken by global toolchain state.

---

### `env` (package-tier — `ocx package env`) {#env}

Print the resolved environment variables for one or more OCI-tier packages.

With the root `--format plain` (default), outputs an aligned table with `Key`, `Type` and `Value` columns.
With `--format json`, outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.
Use `--shell[=NAME]` for eval-safe shell export lines — the only sourceable form.

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies). Because it auto-installs, a package covered by a [`[[trust.policy]]`][config-trust] is signature-verified before its environment is composed — the same gate as [`package install`](#package-install) (see its auto-verify contract).
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

For the full `ocx package env` entry, see [`package env`](#package-env).

**Usage**

```shell
ocx package env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platform to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes].
- `--shell[=NAME]`: Emit eval-safe shell export lines for the named dialect. Same conventions as root [`ocx env --shell`](#env-root). Mutually exclusive with `--ci`.
- `--ci[=PROVIDER]`: Write the resolved environment into the CI system's persistence channel so later pipeline steps see the exported paths and variables. `PROVIDER` ∈ `github` / `github-actions`, `gitlab` / `gitlab-ci`. Bare `--ci` auto-detects from [`GITHUB_ACTIONS`][env-github-actions] / [`GITLAB_CI`][env-gitlab-ci] (exits 64 if neither detected). Equals-form required. Mutually exclusive with `--shell`. See [CI Integration][in-depth-ci] for full walkthrough.
- `--export-file=PATH`: Write [GitLab CI/CD][gitlab-ci-export-docs] JSON-lines output to `PATH`. Requires `--ci=gitlab`; rejected with exit 64 for `--ci=github` or when given without `--ci`.
- `--show-patches`: Annotate each entry with its origin. Adds a `Source` column (plain) or a `"source"` object (JSON) naming the descriptor rule and companion behind each companion-overlay entry appended after the package's own entries. No effect when `[patches]` is not configured. Mutually exclusive with `--shell` and `--ci`.
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

- `-p`, `--platform`: Specify the platform to use.
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

- `-p`, `--platform`: Platform to consider when resolving. Defaults to the current platform. Ignored when `--candidate` or `--current` is set.
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

Stateless export generator for the project toolchain. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, resolves every default-group tool, and prints **bash** export lines for the resolved environment. It emits a fresh export block on every invocation, leaving the diffing/caching to the caller (typically [direnv](https://direnv.net/)). It is what the generated `.envrc` evaluates; you do not normally type it by hand.

Output is always bash. [direnv](https://direnv.net/) sources `.envrc` files in a bash sub-shell regardless of the user's interactive shell, then translates the resulting environment to the interactive shell internally via `direnv export <shell>`. Programs invoked via `eval` from `.envrc` therefore have to emit bash — there is no shell-dialect option on this command.

By default a tool missing from the object store is materialised on miss (like [`ocx env`](#env)): a tool already present resolves locally with no network — its digest is content-addressed, nothing to look up — so only a genuine miss falls through to install it. The pull is best-effort and is skipped whenever no registry is reachable (`--offline` / no configured remote), so a missing tool never fails or blocks the prompt. Pass `--no-pull` to keep the hook strictly offline: missing tools then produce a one-line stderr note and are skipped. Either way a stale lock produces a stderr warning but the stale digests are still used, and when no project `ocx.toml` is found in scope the command exits 0 with no output.

**Usage**

```shell
ocx direnv export [OPTIONS]
```

**Options**

- `--pull` / `--no-pull`: `--pull` (default) installs a missing tool on the object-store miss before exporting; `--no-pull` keeps the hook strictly offline and omits it. POSIX last-wins.
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

Prints environment information: the ocx version, default registry, the detected host platform, detected libc family (on Linux), detected shell, and home directory. When build provenance was baked in at compile time, two optional rows appear: `Commit` (short SHA and clean/dirty status) and `Channel` (e.g. `dev`). These rows are absent on local builds and on stable releases without a channel override.

The `Libc` row appears only when libc was detected — it is absent on non-Linux hosts and on hosts with no readable dynamic loader (a truly minimal or static-only container). Non-FHS layouts such as [Gentoo Prefix][gentoo-prefix] and Homebrew-on-Linux *are* detected: OCX reads the loader path from a present system binary rather than guessing fixed paths. [NixOS][nixos] is detected when [nix-ld][nix-ld] is active (nix-ld installs an FHS shim the probe finds); without nix-ld the probe binaries are statically linked and detection yields an empty set, so the `Libc` row is absent. The `Platform` row shows the bare `os/arch` of the detected host — see [Platforms][reference-platforms] for how that value and the `Libc` row combine into the platform OCX resolves against.

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
Platform   linux/amd64
Libc       libc.glibc
Shell      bash
Home       /home/user/.ocx
```

**JSON output**

`ocx --format json about` emits a flat object. The `commit`, `build`, and `ci` blocks are merged from the [build provenance][version-json-schema] payload and follow the same schema and suppression rules as `ocx --format json version`. The `libc` field is an array of detected libc os.feature tags (e.g. `["libc.glibc"]`, `["libc.glibc","libc.musl"]`); empty array `[]` when no libc was detected:

```json
{
  "version": "0.3.2",
  "registry": "ocx.sh",
  "platforms": ["linux/amd64"],
  "libc": ["libc.glibc"],
  "shell": "bash",
  "home": "/home/user/.ocx",
  "commit": { "sha": "...", "short": "a1b2c3d4", "describe": "...", "dirty": false },
  "build":  { "timestamp": "...", "profile": "release", "target": "...", "rustc": "..." },
  "ci":     { "provider": "github-actions", "run_url": "...", "workflow": "...", "ref": "...", "sha": "..." }
}
```

`channel` is present only when baked in (dev-deploy builds). `commit`, `build`, and `ci` blocks are absent on local builds without git or CI context. Use `ocx about` as the first diagnostic when troubleshooting [feature mismatch][faq-gcompat] errors — the `Libc` row shows exactly what the platform detector found.

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

- `-p`, `--platform`: Target platform to consider.
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
| `--verify` / `--no-verify` | — | Verify the credentials against the registry (`GET /v2/`) before storing them. On rejection, nothing is written (exit 80). `--no-verify` stores without a round-trip. | verify |
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

Resolves every tool tag in the nearest `ocx.toml` to per-platform leaf digests and writes the result to `ocx.lock` next to it. The command is a **whole-file reconcile**: when the lock is already current (its `declaration_hash` matches the config), every pin is carried forward verbatim — a byte-identical, idempotent no-op that never advances a moving tag, even if it has moved upstream. When the config drifted, every declared tag is re-resolved and a moving tag may advance to wherever it points today. To force-advance pins on a current lock, use [`ocx update`](#update).

For each tool, the lock records the bare registry/repository coordinates plus a `[tool.platforms]` table mapping every platform the publisher ships to its leaf manifest digest. The command records all shipped platforms regardless of which OS it runs on, so a lock committed on Linux is complete for macOS and Windows CI runners. The command is fully transactional — either every tool resolves successfully and the file is rewritten atomically, or nothing is written and the previous `ocx.lock` survives unchanged.

The lock carries a `declaration_hash` over the canonicalized [RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785) of `ocx.toml`. Downstream commands ([`ocx pull`](#pull), [`ocx run`](#run)) consult this hash to detect when the lock is stale relative to the source declaration. When the resolved content of every tool is unchanged between two `ocx lock` runs, the file's `generated_at` timestamp is preserved verbatim — the byte-stable output keeps version-control diffs minimal.

After a successful write, the command checks whether the project's `.gitattributes` declares `ocx.lock merge=union` and emits a one-line stderr advisory when it does not, helping prevent merge conflicts on team projects.

::: tip `ocx lock` vs `ocx update`
`ocx lock` is an idempotent reconcile — it re-resolves only when `ocx.toml` changed. Use `ocx update` to force a re-resolve of every tag regardless of drift.
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
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for the named platform instead of the host — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. Selects which already-locked leaf to fetch (the lock stays host-agnostic); a target the publisher does not ship exits 78. Defaults to the current host. | *(current host)* |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global lock`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written (or preserved if content was unchanged). |
| 64 | Missing `ocx.toml`, `--global` combined with `--project`, or more than one `--platform` value (single-valued flag). |
| 65 | `--check` reported drift. |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 78 | Existing `ocx.lock` is malformed (parse error) or uses an unsupported version (V1/V2 are rejected; regenerate with `ocx lock`), `ocx.toml` schema-invalid, `--check` reported the lock is absent, or a requested `--platform` is not shipped by a tool. |
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

Concurrent invocations of `ocx lock` and `ocx update` are serialised via an in-place exclusive flock on `ocx.toml`. Readers (`ocx pull`, `git`, IDE tooling) never acquire any lock and are never blocked by a running `ocx lock`.

### `update` {#update}

Re-resolves advisory tags in `ocx.toml` against the live registry and rewrites `ocx.lock`. Unlike [`ocx lock`](#lock) or [`ocx add`](#add), which resolve through the local index by default, `ocx update` talks to the registry every time — the same default [`ocx self update`](#self-update) uses, since the whole point is to see where a moving tag (`:latest`, `:3`) points *today*. With no arguments this is the **whole-file forced-bump verb**: every declared tag is re-resolved, even when the lock is already current. An unchanged result rewrites the lock byte-identically. The operation is fully transactional — on any resolution failure nothing is written. Resolution only ever writes `ocx.lock`; it never rewrites the local tag pointers under `~/.ocx/tags/`.

Pass binding names, `-g/--group`, or both to advance only part of the toolchain instead: every other pin in `ocx.lock` stays frozen. A scoped update advances each named binding's declared tag to today's resolution and carries every other entry forward unchanged. This only moves the resolution the declared tag already points to — it never changes the declaration itself. To pin a new explicit version, edit `ocx.toml` directly; that is a declaration change, not an update.

::: tip `ocx update` vs `ocx lock`
Whole-file `ocx update` always re-resolves every tag against the registry; scoped to `-g`/`NAME` arguments, it re-resolves only those. `ocx lock` only re-resolves when `ocx.toml` drifted (idempotent when clean), and prefers the local index like other project-tier commands. To advance versions, use `ocx update`. To reconcile a changed config, use `ocx lock`.
:::

::: tip The update family
Three verbs share the name; each refreshes exactly one record. [`ocx index update`](#index-update) refreshes the local index snapshot under `~/.ocx/tags/`. [`ocx self update`](#self-update) refreshes the managed ocx install. `ocx update` refreshes `ocx.lock`. Under [`--frozen`](#arg-frozen), `ocx update` caps discovery at that local index snapshot — a declared tag it doesn't already know exits [`81`](#exit-codes). Under [`--offline`](#arg-offline), no network call is made at all, which also exits `81` when resolution is required.
:::

**Usage**

```shell
ocx update [OPTIONS] [NAME...]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--pull` | — | After writing the lock, materialise all resolved tools into the object store and create their candidate symlinks. Default when `--no-pull` is absent. | on |
| `--no-pull` | — | Write the lock only; skip materialisation. Defer the install to a later `ocx pull` or first `ocx run`. | — |
| `--group <NAME>` | `-g` | Advance every binding in one or more named groups; freeze the rest. Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table; `all` expands to `default` plus every declared `[group.*]`. Combine with `NAME` arguments to advance only those bindings within the named groups. | *(whole file)* |
| `NAME...` | — | Binding names to advance; every other pin is frozen. Each name is advanced in every group it appears in (narrow with `-g`). | *(whole file)* |
| `--check` | — | Re-resolves the selected scope (every declared tag, or only the bindings named by `-g`/positional names), compares the candidate to the predecessor, and exits 0 (matches) or 65 (`DataError`, a pin would change). No writes, no commit. When the predecessor lock is absent, exits 78. | off |
| `--platform <PLATFORM>` | `-p` | Materialise the leaf for the named platform instead of the host — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. Selects which already-locked leaf to fetch (the lock stays host-agnostic); a target the publisher does not ship exits 78. Defaults to the current host. | *(current host)* |
| `--remote` | | Redundant — resolution already talks to the registry by default. Still accepted. | false |
| `-h`, `--help` | | Print help information. | |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global update`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written, or `--check` confirmed the candidate matches. |
| 64 | Missing `ocx.toml`, `--global` combined with `--project`, more than one `--platform` value (single-valued flag), or an unknown `-g` group or unknown binding `NAME` in a scoped update. |
| 65 | `--check` reported the candidate would change pinned content (an advisory tag moved upstream), or a scoped update whose `ocx.toml` has drifted from `ocx.lock` (hand-edited since the last `ocx lock`) — run `ocx lock` to reconcile. |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 75 | Transient failure (rate limit, temporary network error) — retry. |
| 78 | `ocx.toml` or existing `ocx.lock` malformed (parse error), an existing `ocx.lock` uses an unsupported version (V1/V2 are rejected; regenerate with `ocx lock`), `--check` invoked when the lock is absent, a requested `--platform` is not shipped by a tool, or a scoped update with no existing `ocx.lock` (there is no predecessor to carry untouched pins forward from) — run `ocx lock` first. |
| 80 | Authentication failure against the registry. |
| 81 | `--offline` or `--frozen` and a tag is not cached locally (policy blocked). |

**Examples**

```shell
# Re-resolve every declared tag against the registry:
ocx update

# Advance just ripgrep to where its declared tag points today:
ocx update ripgrep

# Advance every tool in the ci group, freezing the rest:
ocx update -g ci
```

Concurrent invocations of `ocx update` and `ocx lock` are serialised via an in-place exclusive flock on `ocx.toml`.

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

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--group <NAME>` | `-g` | Restrict the pull to one or more named groups. Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table; the reserved name `all` expands to `default` + every declared `[group.*]`. When omitted, every entry from the lock is pulled. | *(all groups)* |
| `--dry-run` | — | Print which locked tools are already cached vs. would be fetched, then exit without writing to the store. | off |
| `--platform <PLATFORM>` | `-p` | Pre-warm the leaf for the named platform instead of the host — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. Selects which already-locked leaf to fetch (the lock stays host-agnostic — an amd64 host can pre-warm an arm64 leaf); a target the publisher does not ship exits 78. Defaults to the current host. | *(current host)* |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global pull`. See [`--global`][global-flag].
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (or empty group filter — nothing to pull). |
| 64 | Missing `ocx.toml`, unknown `--group` name, empty comma segment, `--global` combined with `--project`, or more than one `--platform` value (single-valued flag). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 78 | `ocx.toml` present but `ocx.lock` is missing — run `ocx lock` first. Also: an existing `ocx.lock` uses an unsupported version (V1/V2 are rejected; regenerate with `ocx lock`). |
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

A binding missing from the local object store is auto-installed as part of composition. Because it auto-installs, a binding covered by a [`[[trust.policy]]`][config-trust] is signature-verified first — the same gate as [`package install`](#package-install) (see its auto-verify contract). No `--verify`/`--no-verify` flag here; opt out via [`OCX_NO_VERIFY`][env-no-verify].

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
| 65 | `ocx.lock` is stale — run `ocx lock`; or a policy-covered binding's Sigstore bundle is tampered (auto-verify). |
| 69 | Registry unreachable during auto-install of a missing package. |
| 77 | A policy-covered binding's certificate identity or OIDC issuer does not match (auto-verify). |
| 78 | `ocx.lock` absent — run `ocx lock`; or `ocx.toml` parse error (e.g. `[group.all]` declared); or no leaf digest for the host platform at the locked version (no `"any"` fallback key in `[tool.platforms]`) — run `ocx update <tool>` to re-resolve; or a policy-covered binding's trust root/policy is misconfigured (auto-verify). The host-leaf check fires only for tools actually composed: the named subset when `NAME` is given, or every tool in scope when it is omitted. |
| 79 | Package not found in registry during auto-install; or no signature found for a policy-covered binding (auto-verify). |
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
| 78 | A survivor's legacy lock entry can no longer be migrated exactly — run `ocx update` to re-resolve. Also: `ocx.toml` schema invalid or TOML parse error. |
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

- `-p`, `--platform`: Target platform to consider when resolving packages.
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

Setup runs phases in a hard order: **bootstrap first** (install the specified or latest published `ocx.sh/ocx/cli` so the shims have a `current` to point at — a no-op when the same version is already installed), then **[managed-config][config-managed] adoption** (resolve the ref from `--managed-config`, else [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], else the existing seed; whichever one resolves is synchronously fetched and persisted, then the `[managed]` seed fence is written only on success — a fetch failure leaves no partial state; no source at any of the three levels reports `not_configured` and the phase is a no-op), then the five `env.*` shims, then the profile activation blocks. A failed bootstrap stops the run before any shim, profile, or managed-config write is touched.

Re-running is safe. The shims and the managed block are diff-gated: an unchanged setup is a no-op. A stale ocx-authored block is rewritten in place (format upgrade); a legacy `# BEGIN ocx` block is migrated to the versioned fence. A block the user edited by hand is reported dirty and left untouched (exit 82) unless `--force` is passed.

**Usage**

```shell
ocx self setup [VERSION] [--no-modify-path] [--profile PATH]... [--dry-run] [--force] [--managed-config REF]
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
| `--managed-config REF` | — | Adopt (or clear) the corporate [managed-config][config-managed] tier. `REF` is resolved as an OCI reference, synchronously fetched and persisted, then the `[managed]` seed fence in `config.toml` is written only on success — a fetch failure leaves no partial state. Pass `--managed-config ""` to clear an existing seed and delete the snapshot. Omitting the flag does not skip resolution: it falls back to [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], then the existing seed, and re-syncs (self-heals) whichever one resolves — so a bare re-run repairs a wiped or mismatched snapshot without repeating the full onboarding command. | *(resolved: env, then existing seed)* |
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
  "reload_hint": true,
  "managed_config": {"status": "not_configured"}
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
| `managed_config` | object | Result of adopting or clearing the `--managed-config` tier (see below). Always present, even when the flag was not passed. |

Root-level `status` values:

| Value | Meaning |
|-------|---------|
| `completed` | At least one shim or profile was written or upgraded. |
| `no_op` | Everything was already current; nothing changed. |
| `skipped` | At least one profile, or the `[managed]` config fence, carried user edits and was left untouched (no `--force`). Exit 82. |
| `migrated` | A legacy activation block was migrated to the versioned fence; no dirty profiles or fence. |

**`managed_config` object** — result of the [managed-config][config-managed] adoption phase, discriminated by `managed_config.status`:

| Value | Carries `digest`? | Meaning |
|-------|---|---------|
| `not_configured` | No | No source resolved from `--managed-config`, [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], or an existing seed. |
| `already_adopted` | Yes | The resolved ref matches the existing seed AND a matching snapshot is on disk; no re-fetch. The digest is the existing snapshot's verified digest. A wiped or mismatched snapshot self-heals instead: the run re-fetches and reports `adopted`. |
| `adopted` | Yes | A new or changed ref was fetched, persisted, and the seed fence written. |
| `cleared` | No | `--managed-config ""` removed the seed fence and deleted the snapshot. |
| `dirty` | No | The `[managed]` fence carries user edits; left untouched without `--force` — drives root `status: skipped` (exit 82). |
| `would_adopt` | No | `--dry-run`: an adopt or re-adopt would run, but nothing was fetched or written. |

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
| 0 | Setup completed, no-op, or migrated; or a dry-run (including over a dirty profile or fence). |
| 64 | Malformed `VERSION` syntax (empty, short or uppercase hex, unknown algorithm, double `@`, trailing `@`). |
| 65 | `tag@digest` immutability assertion failed — the tag resolved to a different digest than the one specified. Also returned when a `--managed-config` sync fetch succeeds but the package is malformed (no `any/any` entry, no `config.toml`, digest mismatch, over the 64 KiB cap, or not valid TOML). |
| 69 | Registry unreachable while bootstrapping, or while syncing a `--managed-config` snapshot. |
| 74 | I/O error writing a shim, shell profile, or `--managed-config` snapshot. |
| 78 | The `--managed-config` value is not a valid OCI identifier. |
| 79 | The pinned tag or digest was not found in the registry. |
| 80 | Authentication failed while syncing a `--managed-config` snapshot. |
| 81 | A policy (`--offline` or `--frozen`) blocked resolution and the version was not cached locally. |
| 82 | A managed activation block — a shell profile fence or the `[managed]` config fence — carried user edits and `--force` was not passed. Scripts can `case $? in 82)` to detect this and re-run with `--force`. |

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

# Adopt the corporate managed-config tier (sync fetch, then seed the fence):
ocx self setup --managed-config internal.company.com/ocx-config:user

# Clear a previously adopted managed-config tier:
ocx self setup --managed-config ""
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
| `--verbose` | `-v` | Emit multi-line build provenance: host platform (OS/arch + detected libc), commit SHA, dirty flag, build timestamp, profile, target triple, rustc version, and CI run URL. Absent fields are suppressed — a local build without git shows no commit row; a build outside CI shows no CI row; the `host:` row is suppressed when OCX's supported platform set does not include the host OS/arch. | off |
| `-h`, `--help` | | Print help information. | — |

**Plain output — default (no flag)**

```
0.3.2
```

The bare semver string is the stable contract for script consumers. No trailing newline formatting varies by shell — pipe safely to `grep`, `cut`, or similar.

**Plain output — `--verbose`**

```
ocx 0.3.2-dev+20260528143045 (cargo: 0.3.1, channel: dev)
host:     linux/amd64 (libc.glibc)
commit:   a1b2c3d4 (clean) — 2026-05-28T12:00:00Z
built:    2026-05-28T14:30:45Z (release)
target:   x86_64-unknown-linux-gnu
rustc:    1.79.0
ci:       https://github.com/ocx-sh/ocx/actions/runs/1234567890
```

The `host:` row shows the detected OS/arch and, when detected, the libc family in parentheses (e.g. `(libc.glibc)` or `(libc.musl)`). It is suppressed when the host OS/arch is not in OCX's supported set. Rows for `commit`, `built`/`target`/`rustc`, and `ci` appear only when the corresponding data was baked in at build time. Local `cargo build` without git shows no `commit` row; builds outside GitHub Actions show no `ci` row.

The `host:` row is plain-output only — it does not add a field to the `version` JSON wire shape, so the self-update parser contract is unaffected. To inspect libc detection programmatically, use [`ocx --format json about`](#about) which includes a `libc` field.

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

When `--metadata` is given, `create` is also the compiler for [dependency pins][reference-manifest-pins]:
it validates the sidecar and always rewrites it, in canonical form, next to the output bundle — never a
byte copy of the input file.

- `--platform` omitted: every dependency in the sidecar must already carry a manifest digest (else
  usage error, exit 64, hinting `--platform`); no network access.
- `--platform <PLATFORM>` (a concrete platform): each dependency without a digest is resolved
  against the selected index to the one manifest [compatible][reference-platforms-compatibility]
  with `<PLATFORM>`. Zero compatible candidates fails with exit 65 (lists what is available); more
  than one is ambiguous (exit 65). The sidecar is rewritten with the resolved digest pinned
  directly on each dependency's identifier.
- `--platform any`: every unpinned dependency must itself offer an `any` manifest — an `any`
  requirement is satisfied only by an `any` offer, so a dependency with no `any` build fails
  `create` (exit 65), naming it. The resolved digest is recorded in the dependency's `platforms`
  pin map under a single `"any"` key, never a bare digest pin (a leaf manifest carries no platform
  descriptor, so an unmapped digest could never be verified as `any`-offered). `create` also
  rejects a direct digest pin anywhere in an `any`-targeted bundle's dependency list, including one
  already present before `create` ran (exit 65). See [Multi-Platform Packages][authoring-multi-platform].

Resolution honors [`--remote`][arg-remote], [`--offline`][arg-offline], and [`--frozen`][arg-frozen]
exactly like every other tag resolution: the default checks the local index first and fetches on a
miss; `--offline`/`--frozen` refuse to resolve a dependency tag not already cached (exit 81); a
dependency tag absent from the selected index fails with exit 79. See
[Resolving Dependency Pins][authoring-building-pushing-dependency-pins] for the full workflow and the
[Dependencies reference][reference-dependencies] for the sidecar field shapes.

**Usage**

```shell
ocx package create [OPTIONS] <PATH>
```

**Arguments**

- `<PATH>`: Path to the directory to bundle.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier, used to infer the output filename when `--output` is a directory.
- `-p`, `--platform <PLATFORM>`: Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content). Used to infer the output filename, and required whenever `--metadata` declares a dependency without a digest (see above).
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform. The file extension controls the compression algorithm: `.tar.xz` (LZMA, default), `.tar.gz` (Gzip), or `.tar.zst` (Zstandard).
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to a `metadata.json` sidecar to validate, resolve, and write alongside the output bundle. Dependencies without a digest are pinned to platform manifest digests (requires `--platform`, see above); the resolved sidecar is written next to the output bundle in canonical form. If omitted, no metadata sidecar is written.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`. Applies to whichever algorithm is selected.
- `-j`, `--threads <N>`: Number of compression threads. `0` (default) auto-detects from available CPU cores (capped at 16). `1` forces single-threaded compression. Affects LZMA (`.tar.xz`) and Zstandard (`.tar.zst`) compression; Gzip is always single-threaded.
- `-h`, `--help`: Print help information.

#### `pull` {#package-pull}

Downloads packages into the local [object store][fs-objects] without creating
[install symlinks][fs-symlinks].

Unlike [`install`](#install), this command only populates the content-addressed object store — no
candidate or current symlinks are created. If a package declares [dependencies][ug-dependencies], all transitive dependencies are pulled into the object store as well. This is the recommended primitive for CI environments where reproducibility matters and symlink management is unnecessary.

Like [`package install`][cmd-package-install], `pull` verifies a policy-covered package's [Sigstore][sigstore] signature automatically before downloading, aborting fail-closed on a mismatch or a tampered artifact. See the auto-verify contract under [`install`](#package-install) below for the seam, the operator-config-only policy scope, the `--no-verify` / [`OCX_NO_VERIFY`][env-no-verify] opt-out, and offline behavior.

**Usage**

```shell
ocx package pull [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to pull.

**Options**

- `-p`, `--platform`: Target platform to consider. Defaults to the current platform.
- `--verify`: Verify the package's signature when a [`[[trust.policy]]`][config-trust] covers it (default); re-enables verification for this invocation even if [`OCX_NO_VERIFY`][env-no-verify] is set.
- `--no-verify`: Skip that verification for this invocation. Equivalent env var: [`OCX_NO_VERIFY`][env-no-verify] (the flag wins over the env).
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

Publishes a package to the registry as zero or more layers, all recorded in one image manifest for the single target platform this invocation publishes. Each layer is uploaded as an OCI blob, in the order given on the command line. A zero-layer push produces a config-only OCI artifact (referrer-only / description-only manifest) and requires `--metadata`. Publishing a package for more than one platform means running `push` once per platform under the same tag — see [Multi-Platform Packages][authoring-multi-platform] for the full pattern; OCX merges each push into the existing image index rather than replacing it.

`push` makes no dependency-resolution decisions — it is a gate. If the metadata sidecar declares [dependencies][ug-dependencies], every one of them must already carry a manifest digest pin covering every platform this invocation publishes ([`ocx package create`][cmd-package-create] is what resolves them; see [Resolving Dependency Pins][authoring-building-pushing-dependency-pins]). `push` fails before uploading anything if:

| Condition | Exit code |
|---|---|
| The metadata sidecar has no recorded platform (predates `create`, or hand-authored) | 65 |
| An explicit `--platform` disagrees with the sidecar's recorded platform | 65 |
| A dependency has no digest and no pin map | 65 |
| A dependency's pin map has no entry covering a platform being published | 65 |
| A dependency's pin resolves to an OCI Image Index instead of a manifest | 65 |
| A dependency's pinned manifest does not exist in its registry | 79 |
| Authentication to a dependency's registry fails | 80 |

**Usage**

```shell
ocx package push [OPTIONS] --identifier <IDENTIFIER> <LAYERS>...
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
- `-p`, `--platform <PLATFORM>`: Target platform to publish — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. Defaults to the platform [`ocx package create`][cmd-package-create] recorded in the metadata sidecar; an explicit value must equal that recorded platform or the push is rejected (exit 65, naming both) — this flag is a checked assertion, not an override. Every dependency is projected for this platform (see the gate table above).
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

#### Layer layout {#package-push-layout}

Any layer argument to [`push`](#package-push) or [`test`](#package-test) accepts an optional `:strip=N,prefix=P` suffix that overrides [`strip_components`][metadata-strip-components] for that one layer:

```
<ref>:strip=<N>,prefix=<P>
```

Both fields are optional and input order is free — `prefix=share,strip=1` parses the same as `strip=1,prefix=share`. `<N>` is a `u8`. `<P>` is a relative, non-escaping path — no leading `/`, no `..`, no Windows drive or UNC prefix, bounded to 32 components / 4096 bytes total — under which the layer's post-strip tree is placed in the assembled `content/` directory instead of at the package root.

```shell
# base.tar.gz ships wrapped in a `1.2.3/` directory; strip it and relocate the
# remainder under `share/`. tool.tar.gz keeps the default (root, no strip).
ocx package push -p linux/amd64 -i mytool:1.0.0 \
  base.tar.gz:strip=1,prefix=share \
  tool.tar.gz
```

The resolved values are carried in the manifest layer descriptor's `annotations` as `sh.ocx.layer.strip-components` and `sh.ocx.layer.prefix` — never in `metadata.json`. Each field falls back independently at read time: an explicit annotation wins; a missing `strip` annotation falls back to the package-wide `strip_components` from [metadata][metadata-strip-components], then to `0`; a missing `prefix` annotation falls back to the package root. A push with no `:strip=`/`:prefix=` on any layer writes no `sh.ocx.layer.*` annotations at all, so the manifest stays byte-identical to a pre-layout publish.

**Constraints**

- Layers are a flat merge, not an overlay stack. OCI whiteout entries (`.wh.*`, `.wh..wh..opq`) — e.g. inside a foreign layer reused by digest from a Docker/BuildKit build — pass through as ordinary files; OCX never interprets them as deletions.
- A deep `prefix` combined with a deep layer tree can approach the legacy Windows `MAX_PATH` limit. Keep `prefix` shallow for packages that install on Windows.
- `<P>` cannot contain a comma — the layout suffix is a comma-separated `key=value` list with no escaping. This constrains the (typically short) prefix value itself, not the file paths inside the layer.
- A layer filename that literally contains `:strip=` or `:prefix=` cannot be pushed as a plain path — there is no escape hatch, unlike the `./` disambiguation used above for digest-shaped filenames. Avoid such filenames.

Malformed layout syntax at publish (`strip` not a `u8`, an unknown key, a duplicate key, an empty value, or an escaping/oversized `prefix`) is a CLI usage error (exit 64). The same `prefix` bound is re-validated when a layer is read back — manifests are third-party-writable — and a malformed annotation there is a data error (exit 65).

#### `test` {#package-test}

Materializes a package locally without a registry round-trip and runs a command or script in its composed env. Mirrors the argument shape of [`package push`][cmd-package-push]: identifier as `-i/--identifier`, then layers, then `--platform`. Either a trailing `-- CMD [ARGS...]` or a `--script PATH` is required; the two forms are mutually exclusive.

**Usage**

```shell
# Trailing-command form
ocx package test [OPTIONS] --identifier <IDENTIFIER> [LAYERS]... -- <CMD> [ARGS]...

# Script form
ocx package test [OPTIONS] --identifier <IDENTIFIER> [LAYERS]... --script <PATH|->
```

**Arguments**

- `<LAYERS>...`: Zero or more layers, in order (base first, top last). Same syntax as `package push`, including the optional [`:strip=N,prefix=P` layout suffix][cmd-package-push-layout]: a path to a `.tar.gz`/`.tar.xz`/`.tar.zst` archive, or a `sha256:<hex>.<ext>` digest reference to a layer already in the registry. Digest refs are fetched on demand; missing digest blobs when a local policy (`--offline` or `--frozen`) is active produce exit code 81 (`PolicyBlocked`).
- `-- <CMD> [ARGS]...`: Command to run inside the composed env. Required unless `--script` is given.

**Options**

| Name | Short | Description | Default |
|------|-------|-------------|---------|
| `--identifier <IDENTIFIER>` | `-i` | Package identifier in tag form (`repo:tag`) — required. An explicit `@digest` suffix is rejected (the digest is computed locally from the supplied layers). | — |
| `--platform <PLATFORM>` | `-p` | Target platform. Defaults to the platform [`ocx package create`][cmd-package-create] recorded in the metadata sidecar; an explicit value must equal that recorded platform or the command is rejected (exit 65, naming both). | recorded platform |
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
- `65` (`DataError`) — the resolved metadata is malformed or fails validation; with `--resolve -p <platform>`, also a platform feature mismatch or an ambiguous dual-libc selection (see [exit codes](#exit-codes)).

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

#### `sign` {#package-sign}

Publishes a [Sigstore][sigstore] keyless signature for a package manifest as an [OCI Referrers][oci-referrers-spec] artifact. The signing flow uses an ephemeral ECDSA P-256 keypair: [Fulcio][fulcio] issues a short-lived certificate binding the key to your OIDC identity, the manifest digest is signed, and the entry is logged to [Rekor][rekor]. The resulting [Sigstore bundle v0.3][sigstore-bundle] is pushed to the registry as a referrer of the target manifest, discoverable and verifiable by `ocx package verify`. Verifying it with [`cosign verify`][cosign] is not yet supported — see [Deferred to Future Work][signing-deferred] in the signing guide.

Signing requires network access — `--offline` is rejected with exit 77.

**Usage**

```shell
ocx package sign [OPTIONS] --platform <PLATFORM> <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier to sign (`registry/repo:tag[@digest]`).

**Options**

| Name | Short | Default | Purpose |
|------|-------|---------|---------|
| `--platform` | `-p` | *(required)* | Target platform — selects the single-platform manifest under the image index to sign |
| `--fulcio-url` | — | `https://fulcio.sigstore.dev` | [Fulcio][fulcio] CA endpoint (override for private deployments) |
| `--rekor-url` | — | `https://rekor.sigstore.dev` | [Rekor][rekor] transparency-log endpoint (override for private deployments) |
| `--identity-token-file` | — | — | Read the OIDC identity token from this file (highest precedence). File must be owner-readable only (`chmod 600`); world- or group-readable files are rejected with exit 77 (`IdentityTokenFilePermissive`). File must be **owned by the effective user** (uid match required); a foreign-owned file with mode `0600` is still rejected with exit 77 (CWE-732). Symlinks are not followed; a symlink at the supplied path is rejected with exit 77 (CWE-367 mitigation). **Windows:** permission validation is not implemented; use `--identity-token-stdin` or [`OCX_IDENTITY_TOKEN`][env-identity-token] instead (the command exits 77 if `--identity-token-file` is used on Windows). |
| `--identity-token-stdin` | — | — | Read the OIDC identity token from stdin (second precedence). Mutually exclusive with `--identity-token-file` |
| `--no-tty` | — | `false` | Suppress the interactive browser OAuth fallback; ambient token detection must succeed or an override flag must supply a token |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |

**Token precedence**

`ocx package sign` resolves an OIDC identity token from the following sources, in order:

1. `--identity-token-file <PATH>` — read from file (highest precedence)
2. `--identity-token-stdin` — read from stdin
3. [`OCX_IDENTITY_TOKEN`][env-identity-token] environment variable
4. Ambient CI detection (`ACTIONS_ID_TOKEN_REQUEST_TOKEN`, `GOOGLE_OAUTH_TOKEN`, etc.)
5. Interactive browser OAuth (suppressed when `--no-tty` is set)

Never pass a raw token on the command line — it would appear in shell history and process listings.

:::warning Verified against the fake Sigstore stack only
`ocx package sign` runs the full keyless pipeline end-to-end — keypair generation, Fulcio certificate, Rekor entry, bundle assembly, and the referrer push. Its positive path is currently exercised only against the in-repo fake Sigstore stack: a fake [Fulcio][fulcio], [Rekor][rekor], and OIDC issuer. Signing against the public-good Fulcio/Rekor has not yet been wired or tested — the hand-rolled Fulcio/Rekor clients target the fake stack's wire shapes. The exit codes and flag contracts below are stable.
:::

**Exit codes**

| Code | Condition |
|------|-----------|
| 0 | Signature published successfully |
| 64 | `InvalidEndpointUrl` — malformed `--fulcio-url` or `--rekor-url` (must be `https://`, or `http://` on loopback only; no credentials, no unsupported schemes) |
| 77 | `OidcPreCheckFailed` — OIDC pre-check rejected the token (missing scopes, audience mismatch, expired) |
| 77 | `OfflineSignRefused` — `--offline` is incompatible with `package sign`; Fulcio + Rekor are hard dependencies |
| 77 | `IdentityTokenFilePermissive` — `--identity-token-file` is readable by group/other (must be `0600` or tighter) |
| 78 | Fulcio rejected the certificate signing request as malformed |
| 80 | Fulcio rejected the OIDC token (issuer mismatch, expired, wrong audience) |
| 83 | Rekor transparency log unavailable at time of signing |
| 84 | Registry does not support the OCI Referrers API |

**JSON output** (`--format json`)

On success, `ocx package sign` emits a C-S1-1 success envelope. The top-level shape is:

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 0,
  "data": {
    "identifier": "registry.example/pkg:1.0",
    "subject_digest": "sha256:<64-hex>",
    "bundle_digest": "sha256:<64-hex>",
    "referrer_digest": "sha256:<64-hex>",
    "platform": "linux/amd64",
    "signer": "keyless-fulcio",
    "certificate_identity": "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main",
    "certificate_oidc_issuer": "https://token.actions.githubusercontent.com"
  }
}
```

`data` fields:

| Field | Type | Description |
|-------|------|-------------|
| `identifier` | string | Identifier argument passed to the command |
| `subject_digest` | string (`sha256:...`) | Digest of the manifest that was signed |
| `bundle_digest` | string (`sha256:...`) | SHA-256 of the Sigstore bundle v0.3 blob (the referrer layer content) |
| `referrer_digest` | string (`sha256:...`) | SHA-256 of the OCI referrer manifest wrapping the bundle |
| `platform` | string | Platform that was signed (e.g. `"linux/amd64"`) |
| `signer` | string | Signing mechanism; always `"keyless-fulcio"` in Slice 1 |
| `certificate_identity` | string | SAN from the Fulcio-issued certificate |
| `certificate_oidc_issuer` | string | OIDC issuer URL from the Fulcio-issued certificate |

Note: `bundle_digest` and `referrer_digest` are distinct. `bundle_digest` covers the protobuf blob that [Rekor][rekor] includes in its transparency log; `referrer_digest` identifies the OCI manifest returned by the Referrers API.

On error, `ocx package sign` emits a C-S1-1 error envelope. The `error.detail` field (when present) is a snake_case discriminant for programmatic matching:

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 80,
  "error": {
    "kind": "auth_error",
    "detail": "oidc_token_rejected",
    "message": "Fulcio rejected OIDC token: issuer not in trust root",
    "remediation": "Verify --certificate-oidc-issuer matches a Fulcio-trusted issuer",
    "context": {
      "identifier": "registry.example/pkg:1.0"
    }
  }
}
```

`detail` is omitted when no fine-grained discriminant is available. `context` is always present (may be `{}`). The `kind` values are the snake_case `ErrorCategory` variants: `usage_error`, `auth_error`, `permission_denied`, `config_error`, `data_error`, `not_found`, `rekor_unavailable`, `referrers_unsupported`, `io_error`, `internal`.

**`detail` discriminants for `package sign`** (frozen contract C-S1-1):

| `detail` value | Exit | Meaning |
|----------------|------|---------|
| `fulcio_bad_request` | 78 | Fulcio rejected the CSR as malformed |
| `oidc_token_rejected` | 80 | Fulcio rejected the OIDC token (issuer mismatch, expired, wrong audience) |
| `rekor_unavailable` | 83 | Rekor transparency log unavailable at time of signing |
| `rekor_set_malformed` | 65 | Rekor returned the entry but the SET could not be extracted or parsed |
| `referrers_unsupported` | 84 | Registry does not implement the OCI Referrers API |
| `oidc_pre_check_failed` | 77 | OIDC pre-check failed client-side before the token was sent to Fulcio |
| `offline_sign_refused` | 77 | `--offline` is incompatible with `package sign` |
| `identity_token_file_permissive` | 77 | Token file has permissive permissions, wrong owner, or is a symlink |
| `invalid_endpoint_url` | 64 | Malformed `--fulcio-url` or `--rekor-url` |
| `internal` | 1 | Unexpected internal error |

**Example — CI keyless signing with GitHub Actions ambient OIDC**

```yaml
- name: Sign package
  run: |
    ocx package sign \
      -p linux/amd64 \
      registry.example/pkg:1.0
```

In GitHub Actions, the `ACTIONS_ID_TOKEN_REQUEST_TOKEN` variable is present automatically (requires `id-token: write` permission). No `--identity-token-*` flag is needed.

#### `verify` {#package-verify}

Verifies a [Sigstore][sigstore] keyless signature attached to a package manifest via [OCI Referrers][oci-referrers-spec]. The command fetches the [Sigstore bundle v0.3][sigstore-bundle] referrer for the target, verifies the [Fulcio][fulcio] certificate chain against a supplied trust root (see `--tuf-root` / `--trust-root` below), verifies the [Rekor][rekor] Signed Entry Timestamp (SET), verifies the signature over the subject manifest digest, and checks the certificate identity and OIDC issuer against the identity you either supply as flags or have pinned in a [`[[trust.policy]]`][config-trust] entry. All five checks must pass for the command to exit 0.

`--offline` (or [`OCX_OFFLINE`][env-offline]) scopes to the Sigstore trust services — the Rekor-key fetch and TUF — not the registry: verify still fetches the target and its signature referrer from the registry in every mode. Offline verify requires a pinned Rekor key from `--tuf-root` or a fresh trust-root cache entry; see [Offline and Air-Gapped Verification][signing-offline] for the full model.

`--certificate-identity` and `--certificate-oidc-issuer` are optional — but only when a [`[[trust.policy]]`][config-trust] scope covers the target (see [Identity resolution](#package-verify-identity) below). Keyless verification is meaningless without an identity from one source or the other.

**Usage**

```shell
ocx package verify [OPTIONS] --platform <PLATFORM> \
  [--certificate-identity <IDENTITY> --certificate-oidc-issuer <URL>] \
  <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier to verify (`registry/repo:tag[@digest]`).

**Options**

| Name | Short | Default | Purpose |
|------|-------|---------|---------|
| `--platform` | `-p` | *(required)* | Target platform — selects the single-platform manifest under the image index |
| `--certificate-identity` | — | *(policy-resolved)* | Expected certificate SAN (Subject Alternative Name), exact match. Optional when a [`[[trust.policy]]`][config-trust] scope covers the target; when given, overrides any policy and requires `--certificate-oidc-issuer` too. Examples: `you@example.com`, `https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main` |
| `--certificate-oidc-issuer` | — | *(policy-resolved)* | Expected OIDC issuer URL, exact match. Used together with `--certificate-identity` — passing one without the other is a usage error. Examples: `https://github.com/login/oauth`, `https://token.actions.githubusercontent.com` |
| `--rekor-url` | — | `https://rekor.sigstore.dev` | [Rekor][rekor] transparency-log endpoint (override for private deployments) |
| `--tuf-root` | — | *(none)* | Path to a Sigstore [trusted-root][sigstore-tuf] JSON (or a directory holding `trusted_root.json`) — supplies both the [Fulcio][fulcio] CA and the pinned [Rekor][rekor] public key, so no Rekor-key fetch is needed. Takes precedence over `--trust-root`. Equivalent env var: [`OCX_SIGSTORE_TUF_ROOT`][env-sigstore-tuf-root]; the flag wins. The air-gapped seam — required for [`--offline`](#arg-offline) verify unless a fresh trust-root cache entry already exists |
| `--trust-root` | — | *(embedded root)* | Path to a PEM file of [Fulcio][fulcio] CA certificate(s) to validate the leaf chain against. The PEM carries no Rekor key, so it is fetched from `--rekor-url` on first use (trust-on-first-use) and then cached; supply `--tuf-root` instead to pin the Rekor key up front. Equivalent env var: [`OCX_SIGSTORE_TRUST_ROOT`][env-sigstore-trust-root]; the flag wins. One of `--tuf-root`, `--trust-root`, or a fresh trust-root cache entry is required — the embedded production root is stubbed |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |

#### Identity resolution {#package-verify-identity}

Two ways to tell `ocx package verify` whose signature to accept:

- **Flags** — pass both `--certificate-identity` and `--certificate-oidc-issuer`. This is an exact-match pair that overrides any configured policy, matching the original flag-only behavior byte-for-byte.
- **[`[[trust.policy]]`][config-trust]** — omit both flags. Verify first checks the pooled `config.toml`-tier ("operator") policies against the target's canonical `registry/repository`; if any match, the project `ocx.toml` is not consulted at all. Only when no operator policy matches does verify fall back to the project `ocx.toml`'s policies. See the [configuration reference][config-trust] for scope matching, most-specific-wins resolution, regex identities, and the operator-authoritative precedence rule. Reading `[[trust.policy]]` from `ocx.toml` here is the one documented exception to "OCI-tier commands never consult `ocx.toml`" — trust policy is a security posture, not toolchain-binding resolution.

Supplying exactly one of the two flags is a usage error (exit 64) — a `--certificate-identity` without a matching `--certificate-oidc-issuer`, or vice versa, cannot express a valid match. Supplying neither flag with no `[[trust.policy]]` scope covering the target is also exit 64: there is no identity to check the signature against.

:::warning Verified against the fake Sigstore stack only
`ocx package verify` runs the full five-check pipeline end-to-end — referrer discovery, [Fulcio][fulcio] chain, [Rekor][rekor] SET, subject-digest signature, identity and issuer match. Its positive path is currently exercised only against the in-repo fake Sigstore stack. Verifying signatures from public-good Fulcio/Rekor is not yet wired or tested: the embedded production [TUF][sigstore-tuf] trust root is stubbed (supply a trust root via `--tuf-root` / [`OCX_SIGSTORE_TUF_ROOT`][env-sigstore-tuf-root], or `--trust-root` / [`OCX_SIGSTORE_TRUST_ROOT`][env-sigstore-trust-root]), and the Rekor SET is checked against the fake stack's payload format rather than the public Rekor wire format. See [Current limitations][signing-limitations] before relying on this against production Sigstore. Exit codes and flag contracts below are stable.
:::

**Exit codes**

| Code | Condition |
|------|-----------|
| 0 | Signature verified — identity and issuer match, bundle cryptographically valid |
| 64 | `UsageError` — malformed `--rekor-url` (must be `https://`, non-loopback, no credentials, no userinfo) |
| 64 | `NoIdentityProvided` — neither `--certificate-identity` nor `--certificate-oidc-issuer` was given and no [`[[trust.policy]]`][config-trust] scope covers the target, or only one of the two flags was given |
| 65 | Data integrity failure: signature invalid, certificate chain invalid, Rekor SET invalid (bundle tampered), bundle parse failed |
| 77 | Certificate identity or OIDC issuer mismatch |
| 78 | Trust root unavailable or failed to load — includes [`--offline`](#arg-offline) verify with no pinned Rekor key available (no `--tuf-root` and no fresh trust-root cache entry); the message names the remedy |
| 78 | `TrustPolicyInvalid` — the [`[[trust.policy]]`][config-trust] entry matched for this target sets both `identity` and `identity_regexp`, sets neither, or its `identity_regexp` fails to compile |
| 79 | No signatures found for target, or no usable Sigstore bundle among referrers |
| 80 | Registry authentication failed while fetching referrers |
| 83 | Rekor unavailable, or SET absent with only TSA timestamp present (Rekor v2 transition) |
| 84 | Registry does not support the OCI Referrers API |

::: tip Automatic verification on install and pull
When a [`[[trust.policy]]`][config-trust] entry covers a package, [`ocx package install`][cmd-package-install] and [`ocx package pull`][cmd-package-pull] verify it automatically before any layer downloads — see the auto-verify contract under [`install`](#package-install) below and [Verify by default][guide-auto-verify] in the user guide. Run `ocx package verify` directly to check a signature by hand, verify a package outside every policy's scope, or verify without installing.
:::

**JSON output** (`--format json`)

On success, `ocx package verify` emits a success envelope wrapping the flat verification report:

```json
{
  "schema_version": 1,
  "command": "package verify",
  "exit_code": 0,
  "data": {
    "subject_digest": "sha256:<64-hex>",
    "referrer_digest": "sha256:<64-hex>",
    "certificate_identity": "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main",
    "certificate_oidc_issuer": "https://token.actions.githubusercontent.com",
    "signed_at": "2026-04-19T12:00:00Z"
  }
}
```

`data` fields:

| Field | Type | Description |
|-------|------|-------------|
| `subject_digest` | string (`sha256:...`) | Digest of the subject manifest whose signature was verified |
| `referrer_digest` | string (`sha256:...`) | Digest of the OCI referrer manifest carrying the verified bundle |
| `certificate_identity` | string | Subject Alternative Name (identity) read back from the Fulcio cert |
| `certificate_oidc_issuer` | string | OIDC issuer URL read back from the Fulcio cert |
| `signed_at` | string (ISO-8601) | [Rekor][rekor] integrated time of the signature entry |

On error, `ocx package verify` emits a C-S1-1 error envelope. The `error.detail` field is a snake_case discriminant for programmatic matching:

```json
{
  "schema_version": 1,
  "command": "package verify",
  "exit_code": 79,
  "error": {
    "kind": "not_found",
    "message": "no signatures found for registry.example/pkg:1.0",
    "context": {
      "identifier": "registry.example/pkg:1.0"
    }
  }
}
```

The envelope shape matches the `package sign` error envelope (see [`package sign`](#package-sign)), but the `detail` discriminants are different — `package verify` operates on a distinct error taxonomy. `detail` is omitted when no fine-grained discriminant applies.

**`detail` discriminants for `package verify`** (frozen contract C-S1-1):

| `detail` value | Exit | Meaning |
|----------------|------|---------|
| `no_signatures_found` | 79 | No referrers found for the target manifest; publisher has not signed this platform |
| `no_usable_bundle` | 79 | Referrers found but none has a recognized Sigstore bundle artifact type |
| `identity_mismatch` | 77 | Certificate SAN does not satisfy the expected identity, whether supplied via `--certificate-identity` or resolved from a [`[[trust.policy]]`][config-trust] entry |
| `issuer_mismatch` | 77 | Certificate OIDC issuer does not match the expected issuer, whether supplied via `--certificate-oidc-issuer` or resolved from a [`[[trust.policy]]`][config-trust] entry |
| `cert_chain_invalid` | 65 | Certificate chain does not verify against the supplied trust root |
| `signature_invalid` | 65 | Signature does not verify over the subject manifest digest |
| `rekor_set_invalid` | 65 | Rekor SET does not verify (bundle tampered) |
| `rekor_set_absent_tsa_present` | 83 | Rekor SET absent but RFC 3161 TSA timestamp present (Rekor v2 transition) |
| `referrers_unsupported` | 84 | Registry does not implement the OCI Referrers API |
| `rekor_unavailable` | 83 | Rekor transparency log unavailable during verify |
| `bundle_parse_failed` | 65 | Bundle is not valid Sigstore bundle v0.3 or is corrupted JSON |
| `trust_root_unavailable` | 78 | Embedded TUF trust root asset not present in this build (Slice 1) |
| `trust_root_load` | 78 | Trust root failed to load — malformed PEM, no certificate blocks, TUF fetch failed, or [`--offline`](#arg-offline) verify with no pinned Rekor key available (supply `--tuf-root`, or run an online verify first to populate the cache) |
| `no_identity_provided` | 64 | No identity to verify against: certificate flags omitted and no [`[[trust.policy]]`][config-trust] scope matched the target, or only one of the two flags was given |
| `trust_policy_invalid` | 78 | A matched [`[[trust.policy]]`][config-trust] entry is malformed — identity XOR violation, or an `identity_regexp` that does not compile |
| `invalid_endpoint_url` | 64 | Malformed `--rekor-url` |

**Example — verify a package signed in CI, with flags**

```shell
ocx package verify \
  -p linux/amd64 \
  --certificate-identity https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  registry.example/pkg:1.0
```

**Example — verify with a `[[trust.policy]]` covering the target, no flags**

```toml
# ocx.toml or config.toml
[[trust.policy]]
scope       = "registry.example/pkg"
identity    = "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

```shell
ocx package verify -p linux/amd64 registry.example/pkg:1.0
```

See the [configuration reference][config-trust] for the full schema, scope matching, and rotation semantics.

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

When a [`[[trust.policy]]`][config-trust] entry in the operator `config.toml` tier covers the package's `registry/repository`, install verifies its [Sigstore][sigstore] signature automatically — at the metadata-first seam, after the manifest digest resolves and before any layer downloads. A failed check aborts before any package-store or symlink state is written, so a rejected artifact costs a manifest fetch, not a wasted download. Auto-verify consults the operator tier only; unlike [`package verify`][cmd-package-verify], a project `ocx.toml` policy is never considered here.

The same gate applies to **every** command that fetches a package, not just `install`: `package pull`, and every command that auto-installs on demand — [`package exec`](#package-exec), [`package env`](#package-env), root [`env`](#env-root), [`run`](#run), and patch discovery ([`patch why`](#patch-why) / [`patch test`](#patch-test)). Only `install` and `pull` carry the `--verify` / `--no-verify` flag; the others opt out via [`OCX_NO_VERIFY`][env-no-verify].

A package outside every policy's scope is not verified — trust is opt-in, and OCX logs an `INFO` line noting the skip. This opt-in is per scope: a covered package's transitive dependencies are verified only if a policy also covers *their* scope. When a policy does cover the package, a failed check exits with the same taxonomy [`package verify`][cmd-package-verify] uses: `65` for a tampered bundle, `77` for a certificate identity or issuer mismatch, `78` for a trust-root or policy configuration problem, `79` for no signature found.

Pass `--no-verify` (below), or set [`OCX_NO_VERIFY`][env-no-verify] for a CI-wide opt-out, to skip a policy-covered package's verification; the flag wins when both are set, and the bypass logs a single `WARN` per invocation. Under [`--offline`][arg-offline] (or [`OCX_OFFLINE`][env-offline]), verification reuses whatever trust material is already local — [`OCX_SIGSTORE_TUF_ROOT`][env-sigstore-tuf-root] or a warm `$OCX_HOME/state/trust_root/` cache entry — and fails closed with exit `78` when neither is available, rather than installing an artifact it could not check. See [Verify by default][guide-auto-verify] in the user guide for the full model.

**Usage**

```shell
ocx package install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platform — see [Platforms][reference-platforms] for the grammar (e.g. `linux/amd64`, `linux/amd64+libc.glibc`, `linux/amd64+libc.musl`, `darwin/arm64`). Defaults to the auto-detected current platform. When a feature-tagged value is supplied, OCX selects the manifest whose `os.features` are a subset of the supplied features — use this to force a specific libc variant when you know it will run on the host. If the package ships for the host os/arch but no candidate's `os.features` are a subset of the resolved features (e.g. a glibc-only host against a musl-only entry), install exits [`65`](#exit-codes) (`DataError`) and the error lists the available platforms to override with. |
| `-s`, `--select` | | After installing, update the [current symlink][fs-symlinks] for each package to point to the newly installed version. |
| `--verify` | | Verify the package's signature when a [`[[trust.policy]]`][config-trust] covers it (default); re-enables verification for this invocation even if [`OCX_NO_VERIFY`][env-no-verify] is set. No effect on a package outside every policy's scope. |
| `--no-verify` | | Skip that verification for this invocation. Equivalent env var: [`OCX_NO_VERIFY`][env-no-verify] (the flag wins over the env). |
| `-h`, `--help` | | Print help information. |

::: warning Host-only symlinks for foreign-platform installs
The [candidate and current symlinks][fs-symlinks] are written only when the resolved platform matches the host (or the package is platform-agnostic). Installing a foreign platform — e.g. `-p windows/amd64` on Linux — still populates the object store, but leaves the host's `candidates/{tag}` and `current` slots untouched so a platformless `which` or [`env`][cmd-package-env] never resolves to a package the host cannot run. The install reports a null `path` in that case; reference the foreign platform by its digest instead.
:::

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
| `-p`, `--platform` | | Target platform to consider. |
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

This is the OCI-tier equivalent of the root [`exec`](#exec) command. Identifiers are OCI references (e.g. `cmake:3.28`), resolved through the index and auto-installed when missing. Because it auto-installs, a package covered by a [`[[trust.policy]]`][config-trust] is signature-verified before it runs — the same gate as [`package install`](#package-install) (see its auto-verify contract). For project-tier execution driven by `ocx.toml`, use [`ocx run`](#run).

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
| `-p`, `--platform` | | Target platform to consider. |
| `--clean` | | Start with a clean environment; only package-declared variables and `OCX_*` config vars reach the child. |
| `--self` | | Use the self view (expose `private` + `public` entries). Default: consumer view (`public` + `interface` only). |
| `-h`, `--help` | | Print help information. |

#### `env` {#package-env}

Print the resolved environment variables for one or more OCI-tier packages.

Output format is controlled by the root [`--format`](#arg-format) flag (default: `plain`). Plain format outputs an aligned table with `Key`, `Type` and `Value` columns. JSON format (`ocx --format json package env`) outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`. Use `--shell[=NAME]` for eval-safe shell export lines — the only sourceable form.

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies). Because it auto-installs, a package covered by a [`[[trust.policy]]`][config-trust] is signature-verified before its environment is composed — the same gate as [`package install`](#package-install) (see its auto-verify contract).
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
| `-p`, `--platform` | | Target platform to consider. |
| `--candidate`, `--current` | | Path resolution mode — see [Path Resolution](#path-resolution). |
| `--self` | | Self view: emits `private` + `public` entries. Default: consumer view (`public` + `interface`). |
| `--shell[=NAME]` | | Emit eval-safe shell export lines for the named dialect. Same conventions as root [`ocx env --shell`](#env-root). Mutually exclusive with `--ci`. |
| `--ci[=PROVIDER]` | | Write the resolved environment into the CI system's persistence channel for later pipeline steps. `PROVIDER` ∈ `github` / `github-actions`, `gitlab` / `gitlab-ci`. Bare `--ci` auto-detects. Equals-form required. Mutually exclusive with `--shell`. |
| `--export-file=PATH` | | Write [GitLab CI/CD][gitlab-ci-export-docs] JSON-lines to `PATH`. Requires `--ci=gitlab`; exit 64 for `--ci=github` or without `--ci`. |
| `--show-patches` | | Annotate each entry with its origin. When [`[patches]`][config-patches] is configured, companion overlay entries are appended after the package's own entries; this flag adds a `Source` column to the plain table (a `"source"` object in JSON) naming the descriptor rule and companion that produced each overlay entry. No effect when `[patches]` is not configured. Mutually exclusive with `--shell` and `--ci`. |
| `-h`, `--help` | | Print help information. |

::: warning `--ci=gitlab` requires GitLab Functions / step runner
`--ci=gitlab` writes JSON-lines (`{"name":"…","value":"…"}`), which is the format consumed by the [GitLab step runner][gitlab-step-runner-docs] via `${{ export_file }}` (experimental, `run:` keyword jobs only). It does **not** work with traditional `script:` jobs. See [CI Integration][in-depth-ci] for a full step-runner example.
:::

::: info Windows: synthetic `PATHEXT ⊳ .CMD`
On Windows, `package env` prepends `.CMD` to `PATHEXT` in its output when the host shell's `PATHEXT` does not already include it. Generated entrypoint launchers are `.cmd` files; this lets callers that adopt the printed env find launchers by bare name without further configuration.
:::

### `patch` {#patch}

Manage site-infrastructure patch overlays. Patch descriptors map glob patterns over
package identifiers to **companion packages** that carry operator-controlled environment
overlays (CA bundles, proxy variables, license-server endpoints). The `[patches]`
configuration tier must be set before any patch sub-command that contacts the registry.

For a full walkthrough, see the [Patching packages guide][patches-user-guide].

**Usage**

```shell
ocx patch <SUBCOMMAND>
```

**Sub-commands**

| Sub-command | Purpose |
|-------------|---------|
| `freeze` | Write a `patches.snapshot.json` file that pins companion digests for reproducible builds. |
| `sync` | Refresh descriptors and install newly-referenced companion packages from the registry. |
| `publish` | Push a patch descriptor to the configured patch registry. |
| `test` | Compose a descriptor onto a base package locally without publishing (maintainer preview). |
| `why` | Show which companion, and which descriptor rule, contributes each patched env var to a base. |

#### `patch freeze` {#patch-freeze}

Resolves every companion and descriptor digest in the active patch overlay and writes
`patches.snapshot.json` beside `ocx.lock` (or in `$OCX_HOME` under `--global`). Once
written, set [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot] to the file path so all
subsequent composition prefers the pinned digests over live tag lookups.

Works offline: only the local object store is consulted.

**Usage**

```shell
ocx patch freeze
```

**Options**

- `-h`, `--help`: Print help information.

::: tip Target the global toolchain
Pass `--global` **before** the subcommand to write `patches.snapshot.json` beside
`$OCX_HOME/ocx.lock`: `ocx --global patch freeze`. See [`--global`][global-flag] for the
full root-flag reference.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Snapshot written successfully. |
| 74 | I/O error writing the snapshot file. |
| 78 | No `ocx.lock` found for the project tier (run `ocx lock` first). |

#### `patch sync` {#patch-sync}

Re-fetches every patch descriptor for all installed packages and the global descriptor. Installs
any newly-referenced companion packages. Requires network access.

This command also picks up patches for packages installed before the `[patches]` tier was
configured. All states are re-checked regardless of what was previously recorded. Running
`patch sync` is equivalent to `ocx index update` for the patch tier.

Without `--platform`, `patch sync` resolves **every concrete ship platform**
(`linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`, `windows/amd64`) — not just the
host platform. This is `patch sync`'s one sanctioned multi-platform fan-out: an explicit
enumeration over the concrete matrix, not a selection among candidates. A synced
descriptor/companion set is shared across a team the same way [`ocx lock`](#lock) is: it must
cover every platform a teammate might run, or an offline or required-patch launch on their machine
silently breaks. Pass a single `--platform` to narrow to just that one platform instead.

**Usage**

```shell
ocx patch sync [OPTIONS]
```

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--platform PLATFORM` | `-p` | Target platform for companion resolution. Single-valued: passing more than one exits 64. Bare (omitted) fans out to the full five-platform concrete ship matrix; an explicit value narrows to that one platform. |
| `-h`, `--help` | | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Sync complete, including a no-op when no `[patches]` tier is configured. |
| 81 | `--offline` blocked the sync — `patch sync` is an explicit online action and always requires network access, unlike lazy discovery at install time. |
| *other* | A `required` companion failed to install for one of the known bases; the exit code reflects the underlying cause — see [Exit codes][exit-codes] (e.g. 79 not found, 69 registry unreachable, 80 authentication failure). |

#### `patch publish` {#patch-publish}

Reads a descriptor JSON file, validates it, and pushes it to the configured
[`[patches]`][config-patches] registry. Use `--global` for a descriptor that
applies to every package; supply a base identifier to publish a per-package descriptor.
Publish companion packages separately with `ocx package push` before publishing the
descriptor that references them.

Requires network access; fails in offline mode.

**Usage**

```shell
ocx patch publish --descriptor <FILE> [--global | <BASE-ID>]
```

**Arguments**

- `<BASE-ID>`: The base package whose per-package patch path receives the descriptor.
  Required unless `--global` is set.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--descriptor <FILE>` | | Path to the patch descriptor JSON file. Required. |
| `--global` | | Publish the descriptor to the reserved `global` repository under the patch registry so it applies to every base. Mutually exclusive with `<BASE-ID>`. |
| `--registry <HOST/PATH>` | | Patch registry to publish to, e.g. `registry.corp.example/ocx-patches`. Overrides the configured [`[patches]`][config-patches] tier, so you can bootstrap a brand-new patch registry without first adding a config block. Defaults to the configured registry. |
| `-h`, `--help` | | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Descriptor published. |
| 64 | No patch registry available — pass `--registry <HOST/PATH>`, configure a `[patches]` tier, or set `OCX_PATCHES` before publishing. |
| 65 | Descriptor JSON is malformed or the version is unsupported. |
| 69 | Registry unreachable. |
| 81 | `--offline` blocked the publish — `patch publish` requires network access. |

#### `patch test` {#patch-test}

Composes a patch descriptor onto a base package in a scratch environment without
publishing or modifying `$OCX_HOME`. Use this to verify a descriptor before publishing.

Without a trailing command, prints the composed environment so you can inspect the
entries contributed by the matched companions. With `-- <COMMAND>`, runs the command in
the composed environment. With `--script`, runs a [Starlark test script][authoring-testing-scripted]
against the composed environment.

Required companion packages must be resolvable (installed locally or pullable from the
registry). An unresolvable required companion fails the command. An optional companion
that cannot be resolved is warned-and-skipped, matching the production fail-open path.

**Usage**

```shell
ocx patch test --descriptor <FILE> [OPTIONS] <BASE-ID> [-- COMMAND [ARGS...]]
```

**Arguments**

- `<BASE-ID>`: The base package identifier to compose the descriptor onto. Required.
- `[-- COMMAND [ARGS...]]`: Command to run in the composed environment. Mutually
  exclusive with `--script`. When neither is given, the composed environment is printed.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--descriptor <FILE>` | | Path to the patch descriptor JSON file. Required. |
| `--companion-archive <PATH>` | | Local archive for a companion package; avoids a registry round-trip. Repeatable for multiple companions. |
| `--platform <PLATFORM>` | `-p` | Target platform for composing the environment. Defaults to host platform. |
| `--registry <HOST/PATH>` | | Patch registry to compose against, e.g. `registry.corp.example/ocx-patches`. Overrides the configured [`[patches]`][config-patches] tier, so you can preview a descriptor against a new patch registry without a config block. Defaults to the configured registry. |
| `--script <FILE>` | | Starlark test script to run in the composed environment. Mutually exclusive with `-- COMMAND`. |
| `-h`, `--help` | | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Environment printed, or the trailing command/script exited 0. |
| *(child's exit code)* | With a trailing command, the child's exit code is forwarded unchanged — a command that exits 7 makes `patch test` exit 7. |
| 64 | No patch registry available — pass `--registry <HOST/PATH>`, configure a `[patches]` tier, or set `OCX_PATCHES` before testing. |
| 65 | Descriptor JSON is malformed or the version is unsupported. |
| 81 | `--offline` blocked resolving the base or a required companion. |
| *other* | A required companion could not be resolved; the exit code reflects the underlying cause — see [Exit codes][exit-codes] (e.g. 79 not found, 69 registry unreachable, 80 authentication failure). |

With `--script`, the exit code follows the [scripted-tests contract][authoring-testing-scripted-exit-codes] instead — assertion failures exit 1, script-level errors exit 64/65/74.

#### `patch why` {#patch-why}

Shows which companion, and which descriptor rule, contributes each patched env var to a base
package. Resolves `<BASE-ID>` directly against the configured [`[patches]`][config-patches]
registry — an OCI-tier diagnostic that never consults `ocx.toml`. Use this to trace a companion
overlay back to the rule that admitted it, without reading through the full composed environment.

A base with no applicable patch (no `[patches]` tier configured, or no descriptor rule matches
the base) prints a clean "no patches apply" result and exits `0` — not an error.

**Usage**

```shell
ocx patch why [OPTIONS] <BASE-ID>
```

**Arguments**

- `<BASE-ID>`: The base package identifier to trace patch provenance for. Required.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `--platform <PLATFORM>` | `-p` | Target platform for resolving the base. Single-valued: passing more than one exits 64. Defaults to the host platform. |
| `-h`, `--help` | | Print help information. |

Output follows the root [`--format`][arg-format] flag like every other command — there is no
subcommand-level `--format` override. With `--format plain` (default), the result is a
`Variable | Rule | Companion` table, one row per patched env var:

```shell
ocx patch why java:21
```

```
Variable     Rule          Companion
JAVA_TRUST   ocx.sh/java:* corp/jdk-trust:1.0
```

With `--format json`, the result is a bare array of `{ "variable", "rule", "companion" }` objects
(`[]` when no patches apply):

```shell
ocx --format json patch why java:21
```

```json
[
  { "variable": "JAVA_TRUST", "rule": "ocx.sh/java:*", "companion": "corp/jdk-trust:1.0" }
]
```

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Result printed — including a base with no applicable patch. |
| 69 | Registry unreachable while resolving the base. |
| 79 | Base identifier not found in the registry. |

### `config` {#config}

Manage the corporate [managed-configuration][config-managed] tier (`[managed]`) — an
operator-published `config.toml` payload synced from an OCI registry and merged above the
user config on every invocation. The payload travels as an ordinary OCX package (its
content is one `config.toml`), so publishing, versioning, cascade tags, and rollbacks all
behave exactly like packages. Decoupled from [`ocx self update`][cmd-self-update]: this
tier tracks an operator's config package, not the ocx binary itself.

**Usage**

```shell
ocx config <SUBCOMMAND>
```

**Sub-commands**

| Sub-command | Purpose |
|-------------|---------|
| `setup` | Adopt (or clear) the `[managed]` tier — the configuration-only counterpart to [`ocx self setup --managed-config`][cmd-self-setup] (consumer side). |
| `push` | Validate and publish a config file as a managed-config package (operator side). |
| `update` | Fetch and persist the managed-config snapshot — optionally pinned to a VERSION — or pause/resume the background tick, or report status with `--check`. |

#### `config setup` {#config-setup}

Adopts (or clears) the corporate managed-config tier without touching anything else: no
binary bootstrap, no env shims, no shell profiles. This is the configuration entry point
for automation and CI environments, where OCX arrives as a plain binary and the only
setup that matters is which managed configuration to apply.

The command resolves its source with the same precedence as
[`ocx self setup --managed-config`][cmd-self-setup] — the explicit `--managed-config`
flag, then [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], then the existing
`[managed]` seed — and runs the identical adoption sequence: synchronously fetch and
persist the snapshot **first**, then write the `[managed]` seed fence in
`$OCX_HOME/config.toml` only on success. A fetch failure leaves no partial state. A bare
re-run re-adopts the configured seed, so a wiped or mismatched snapshot self-heals.

Unlike `ocx self setup` — where an unresolved source is a no-op (setup has other phases
to run) — a bare `ocx config setup` with nothing configured at any of the three levels is
a usage error (exit 64): the command exists only to set up this tier.

**Usage**

```shell
ocx config setup [--managed-config REF] [--dry-run] [--force]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--managed-config REF` | — | OCI reference of the managed-config artifact to adopt (published with [`config push`](#config-push)). Pass an empty string (`--managed-config ""`) to clear an existing seed and delete the snapshot. Omit to fall back to [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], then the existing seed. | *(env var, then seed)* |
| `--dry-run` | — | Report the intended action without fetching or writing anything. | off |
| `--force` | — | Overwrite a `[managed]` fence that carries user edits (the dirty state). | off |
| `-h`, `--help` | | Print help information. | — |

**Output** — the same `managed_config` entry [`ocx self setup`][cmd-self-setup] reports
(`{"managed_config":{"status":"adopted","digest":"sha256:…"}}`), so fleet tooling parses
both commands with one schema. Statuses: `adopted`, `already_adopted`, `cleared`,
`dirty`, `would_adopt`.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Adopted, already adopted, cleared, or `--dry-run` reported. |
| 64 | Nothing to set up — no `--managed-config`, no `OCX_MANAGED_CONFIG`, no existing seed. |
| 65 | The fetched managed-config package is malformed — digest mismatch, no `any/any` entry, missing `config.toml`, over 64 KiB, or invalid TOML. |
| 69 | Registry unreachable while fetching the snapshot. |
| 74 | Writing the snapshot or the `[managed]` fence failed. |
| 78 | The reference is not a valid OCI identifier, or a system-locked tier would be redirected or cleared. |
| 79 | The managed-config package does not exist in the registry. |
| 80 | Authentication failed while fetching the snapshot. |
| 82 | The `[managed]` fence carries user edits and `--force` was not passed. Nothing was touched. |

::: tip CI recipe
`ocx config setup --managed-config <ref>` persists the seed, so every later `ocx`
invocation on that machine resolves the managed tier with no further env plumbing. For
ephemeral runners where persisting is pointless, the env-var pairing
(`OCX_MANAGED_CONFIG=… ocx config update`) works without writing a seed — see
[`OCX_MANAGED_CONFIG`][env-ocx-managed-config].
:::

#### `config push` {#config-push}

Publishes a config file as a managed-config package. Validates the payload first — it
must parse as an ocx config, must not contain a `[managed]` section (a published payload
can never redirect the tier that fetches it), and must stay within 64 KiB — then stages
it under the canonical entry name `config.toml` (whatever the input file is called),
bundles it as a tar+gzip layer, and pushes it with the same machinery as
[`ocx package push`][cmd-package-push].

With `--cascade`, pushing `user-1.4.2` also advances the rolling tags `user-1.4`,
`user-1`, and `user` — the same [cascade algebra][in-depth-versioning-cascades] packages
use, so fleets track a floating tag while individual hosts can pin any published version.

**Usage**

```shell
ocx config push -i <IDENTIFIER> [--cascade] [--new] [--platform PLATFORM] <CONFIG>
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--identifier ID` | `-i` | Identifier to publish under (e.g. `corp/ocx-config:user-1.4.2`). Required. | — |
| `--cascade` | `-c` | Update rolling variant tags derived from the version tag. | off |
| `--new` | `-n` | The repository does not exist yet; tolerate a failing tag listing during `--cascade`. | off |
| `--platform` | `-p` | Platform entry written into the package index. `ocx config update` only consumes the platform-independent `any` entry — keep the default. | `any` |
| `-h`, `--help` | | Print help information. | — |

**Output** — the same push report as [`ocx package push`][cmd-package-push]
(`identifier`, `status`, `manifest_digest`, `cascade_tags_written`). The reported digest
is the operator's trust-on-first-use signal: it is the value a digest-pinned seed and
every consumer's [`config update --check`](#config-update) compare against.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Pushed. |
| 69 | Registry unreachable. |
| 74 | I/O error reading the payload file (other than not found or permission denied), or staging it for the push. |
| 77 | The payload file could not be read — permission denied. |
| 78 | Payload rejected — not valid config TOML, contains a `[managed]` section, or exceeds 64 KiB. Nothing was pushed. |
| 79 | The payload file does not exist. |
| 80 | Authentication failed. |

#### `config update` {#config-update}

Fetches the configured managed-config package, persists a new snapshot, and reports what
changed. Always bypasses the background-refresh throttle — explicit user intent, mirroring
[`ocx self update`][cmd-self-update].

An optional `VERSION` positional pins the sync to a specific tag, digest, or
`tag@digest` combination — rollback is `ocx config update <older-version>`. The snapshot
identity is the repository, not the tag, so a pinned snapshot still satisfies the
[`required` gate][config-managed-required] of a seed tracking a floating tag.

`--pause <duration>` holds the background tick for up to 7 days (a temporary hold — set
`refresh = "manual"` in the seed for a permanent opt-out). Without a VERSION it freezes
the on-disk state as-is (no fetch); with a VERSION it syncs the pin first and records the
pause only after the persist succeeded. `--resume` clears the pause and syncs. Any
explicit update without `--pause` clears an active pause. A pause affects only the
background tick — never the `required` gate, never an explicit update.

With `--check`, only reports the tier's current status — effective source, snapshot
digest and tag, last-fetch timestamp, refresh policy, pause state, active kill switches,
and live drift against the registry when reachable — without fetching or swapping
anything. Offline (or any fetch failure) degrades to a local-state-only report. `--check`
never modifies the pause file.

The tier is adopted by [`ocx self setup --managed-config <ref>`][cmd-self-setup] or by
setting [`OCX_MANAGED_CONFIG`][env-ocx-managed-config]; `ocx config update` is the only
command that fetches the package after that first adoption. See
[`[managed]`][config-managed] for the full tier schema and the
[managed-configuration walkthrough][user-guide-managed-config] for onboarding, rollout,
and CI recipes.

**Usage**

```shell
ocx config update [VERSION] [--pause DURATION] [--resume] [--check]
```

**Options**

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `VERSION` | — | Version to sync: tag, `sha256:<hex>`, or `tag@sha256:<hex>` (the latter verifies the tag resolves to the given digest before persisting). Conflicts with `--check` and `--resume`. | *(seed source)* |
| `--pause DURATION` | — | Pause the background tick for `\d+[smhd]` (max `7d`). Conflicts with `--check` and `--resume`. | — |
| `--resume` | — | Clear an active pause and sync. | off |
| `--check` | — | Report the tier's status without fetching or swapping. | off |
| `-h`, `--help` | | Print help information. | — |

**Invocation matrix**

| Invocation | Fetches? | Pause file |
|---|---|---|
| `config update` | yes, seed source | cleared |
| `config update 1.4.2` | yes, pinned | cleared |
| `config update --pause 3d` | no (state frozen as-is) | written |
| `config update --pause 3d 1.4.2` | yes, pin first | written after the persist succeeds |
| `config update --resume` | yes, seed source | cleared |
| `config update --check` | probe only | untouched |

**Behavior without `--check`**

Reports one of:

- **`not_configured`** — no `[managed]` source is resolved (no seed, no `OCX_MANAGED_CONFIG`).
- **`already_current`** — the local snapshot's digest already matches the registry.
- **`updated`** — a new snapshot was fetched and persisted.
- **`check_unavailable`** — `--pause` without a VERSION: nothing was fetched or verified; the report is the local state plus the fresh pause window.

**Behavior with `--check`**

Probes the registry for the current top-level manifest digest (never the full payload,
never a swap) and reports one of three outcomes: **`checked`** when the probe succeeds and
the digest differs from the local snapshot, **`already_current`** when the probe succeeds
and the digest matches, or **`check_unavailable`** when the probe could not run at all
(offline, no managed-config client, source absent in the registry, authentication failure,
or a registry error) — the report then degrades to a local-state-only summary
(source/digest/tag/fetched-at/pause) instead of falsely claiming the tier is current.

**JSON output** (`--format json`)

```json
{"status": "updated", "source": "internal.company.com/ocx-config:user-1.4.1", "digest": "sha256:ab12cd...", "policy": "notify", "tag": "user-1.4.1"}
{"status": "checked", "source": "internal.company.com/ocx-config:user", "digest": "sha256:ab12cd...", "fetched_at": "2026-07-04T00:00:00Z", "policy": "notify", "kill_switches": ["OCX_NO_CONFIG_REFRESH"], "drift": true, "tag": "user-1.4.1", "paused_until": "2026-07-08T12:00:00+00:00", "pinned": "user-1.4.1"}
```

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | `not_configured`, `already_current` (probe ran and matched), `updated` (full-update path), `checked` (probe ran and detected drift), or `check_unavailable` (probe could not run — offline, source absent, auth, or registry error — or a fetch-free `--pause`). |
| `source` | string | The effective managed-config source (flag > env > seed), with any VERSION pin applied. Omitted when `not_configured`. |
| `digest` | string | The local snapshot's top-level manifest digest, `sha256:<hex>`. Omitted when no snapshot exists yet. |
| `fetched_at` | string | ISO-8601 UTC timestamp of the snapshot's last fetch. Status reports only. |
| `policy` | string | The tier's `refresh` posture — `apply`, `notify`, or `manual`. |
| `kill_switches` | array of strings | Active kill-switch env-var names (`OCX_NO_CONFIG_REFRESH`, `OCX_NO_CONFIG`) affecting this tier. Empty array when none are set. |
| `drift` | boolean | `--check` only: whether the registry's current digest differs from the local snapshot. Present only when the registry was reachable. |
| `tag` | string | The tag the snapshot was fetched under (the floating or pinned version this host tracks). Omitted for pre-v2 snapshots until their next sync. |
| `paused_until` | string | ISO-8601 UTC end of an in-force pause. Omitted when no pause is active. |
| `pinned` | string | The VERSION pinned alongside the pause (`--pause <d> <VERSION>`). Omitted when the pause carries no pin. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Report printed — `not_configured`, `already_current`, `updated`, `checked`, or `check_unavailable` (an offline or otherwise unreachable `--check` degrades to a local-state report). |
| 64 | Conflicting flags (`--check` with `--pause`/`--resume`/VERSION, `--resume` with `--pause`/VERSION), a malformed VERSION, or a `--pause` duration that is malformed or exceeds `7d`. |
| 65 | `tag@digest` immutability assertion failed (the tag resolved to a different digest — snapshot untouched), or the fetched payload is malformed (no `any/any` entry, no `config.toml`, digest mismatch, over the 64 KiB cap, or not valid TOML). |
| 69 | Registry unreachable (full-update path — `--check` degrades to a local-state report instead of failing). |
| 74 | I/O error writing the snapshot file. |
| 78 | The effective managed-config source or interval is invalid (bad seed or `OCX_MANAGED_CONFIG` value). |
| 79 | The resolved managed-config source has no package in the registry (full-update path). |
| 80 | Authentication failed against the registry (full-update path only). |

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
[nixos]: https://nixos.org/
[nix-ld]: https://github.com/nix-community/nix-ld
[gentoo-prefix]: https://wiki.gentoo.org/wiki/Project:Prefix
[sigstore]: https://www.sigstore.dev/
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[cosign]: https://github.com/sigstore/cosign
[sigstore-bundle]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto
[sigstore-tuf]: https://docs.sigstore.dev/certificate_authority/overview/
[oci-referrers-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers

<!-- in-depth -->
[exec-modes]: ../in-depth/environments.md#visibility-views
[env-composition-forwarding]: ../in-depth/environments.md#ocx-forwarding
[in-depth-project-running]: ../in-depth/project.md#running
[env-composition-strict-isolation]: ./env-composition.md#strict-isolation
[in-depth-ci]: ../in-depth/ci.md
[signing-limitations]: ../in-depth/signing.md#current-limitations
[signing-offline]: ../in-depth/signing.md#offline-verification
[signing-deferred]: ../in-depth/signing.md#deferred-future-work

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-ocx-frozen]: ./environment.md#ocx-frozen
[env-ocx-patch-snapshot]: ./environment.md#ocx-patch-snapshot
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
[env-identity-token]: ./environment.md#ocx-identity-token
[env-sigstore-trust-root]: ./environment.md#ocx-sigstore-trust-root
[env-sigstore-tuf-root]: ./environment.md#ocx-sigstore-tuf-root
[env-offline]: ./environment.md#ocx-offline
[env-no-verify]: ./environment.md#ocx-no-verify

<!-- external: completions -->
[clap-complete]: https://docs.rs/clap_complete/latest/clap_complete/

<!-- reference -->
[config-ref]: ./configuration.md
[config-patches]: ./configuration.md#keys-patches
[config-managed]: ./configuration.md#keys-managed
[config-managed-required]: ./configuration.md#keys-managed-required
[in-depth-versioning-cascades]: ../in-depth/versioning.md#cascades
[env-ocx-managed-config]: ./environment.md#ocx-managed-config
[user-guide-managed-config]: ../user-guide.md#managed-config
[config-trust]: ./configuration.md#keys-trust

<!-- external: login/logout interop -->
[docker-login]: https://docs.docker.com/reference/cli/docker/login/
[docker-logout]: https://docs.docker.com/reference/cli/docker/logout/
[oras-login]: https://oras.land/docs/commands/oras_login/
[oras-logout]: https://oras.land/docs/commands/oras_logout/
[helm-logout]: https://helm.sh/docs/helm/helm_registry_logout/

<!-- internal -->
[entry-points]: ./metadata.md#entry-points
[metadata-strip-components]: ./metadata.md#extraction-strip-components
[guide-entry-points]: ../in-depth/entry-points.md
[exit-codes]: #exit-codes
[fs-objects]: ../in-depth/storage.md#packages
[fs-symlinks]: ../in-depth/storage.md#symlinks
[fs-index]: ../in-depth/indices.md#local
[ug-dependencies]: ../user-guide.md#dependencies
[ug-deps-env]: ../user-guide.md#dependencies-environment
[patches-user-guide]: ../user-guide/patches.md
[guide-auto-verify]: ../user-guide.md#supply-chain-auto-verify

<!-- commands (package-test options) -->
[cmd-package-push]: #package-push
[cmd-package-push-layout]: #package-push-layout
[cmd-package-test]: #package-test
[cmd-exec-self]: #exec
[cmd-exec-clean]: #exec

<!-- global flags (package-inspect) -->
[arg-offline]: #arg-offline
[arg-remote]: #arg-remote
[arg-format]: #arg-format

<!-- commands (package group) -->
[cmd-package-install]: #package-install
[cmd-package-pull]: #package-pull
[cmd-package-verify]: #package-verify
[cmd-package-uninstall]: #package-uninstall
[cmd-package-select]: #package-select
[cmd-package-deselect]: #package-deselect
[cmd-package-exec]: #package-exec
[cmd-package-env]: #package-env
[cmd-package-create]: #package-create

<!-- global flags (package-create/package-push dependency pins) -->
[arg-frozen]: #arg-frozen

<!-- reference (package-create/package-push dependency pins) -->
[reference-manifest-pins]: ./metadata.md#dependencies-manifest-pins
[reference-dependencies]: ./metadata.md#dependencies
[reference-platforms]: ./platforms.md

<!-- global flag -->
[global-flag]: #global-flag

<!-- commands (root env) -->
[cmd-env-root]: #env-root
[version-json-schema]: #version

<!-- commands (self group) -->
[cmd-self-setup]: #self-setup
[cmd-self-update]: #self-update

<!-- authoring -->
[authoring-testing]: ../authoring/testing.md
[authoring-testing-scripted]: ../authoring/testing.md#scripted-tests
[authoring-testing-scripted-exit-codes]: ../authoring/testing.md#scripted-tests-exit-codes
[authoring-libc]: ../authoring/multi-platform.md#libc
[authoring-multi-platform]: ../authoring/multi-platform.md
[authoring-building-pushing-dependency-pins]: ../authoring/building-pushing.md#dependency-pins

<!-- faq -->
[faq-gcompat]: ../faq.md#linux-gcompat
