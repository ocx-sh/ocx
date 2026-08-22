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

Under `--format json`, a command that fails writes a single error envelope to
stdout — `schema_version`, `command`, `exit_code`, and an `error` object — so a
failure is parseable the same way a success is. This applies to every command,
not only the ones documenting a `detail` discriminant table below. A command
that already wrote its result document before failing keeps that document and
emits no envelope, so stdout is never two concatenated JSON values. Under
`--quiet`, success prints nothing and a failure still prints the envelope.

### `--json` {#arg-json}

Shorthand for `--format json`. Combining it with [`--format`](#arg-format) is not an error — the last one on the command line wins, in both directions. So `ocx --json --format plain` prints plain text, which keeps plain reachable when `--json` comes from a shell alias or wrapper script rather than from you.

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
`ocx index update <pkg>` writes the tag's dispatch object and root document into the local index,
so a subsequent `ocx --offline package install <pkg>` can resolve the tool's per-platform digest
with no network. The install itself still needs the actual manifest and layer archives, which the
index does not carry — those are fetched into the [package store][fs-objects] only by an online
`ocx package install` or `ocx package pull`. Run one of those online first if you need the binary
itself available offline.
:::

### `--remote`, `-r` {#arg-remote}

Routes mutable lookups (tag list, catalog, tag→manifest resolution) to the
remote registry instead of the local index. Pure-query commands
([`ocx index list`](#index-list), [`ocx index catalog`](#index-catalog),
[`ocx package info`](#package-info)) do **not** persist the result to the
local index — to refresh it, run
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
[`ocx index update`](#index-update) — run without the flag, because recording a
new mapping is itself the discovery a freeze exists to refuse, so a frozen index
update is rejected with exit [`81`](#exit-codes).

The flag scopes to the **package tier** — the local index is the pin it freezes.
Patches float by design, so a patch companion resolves live under `--frozen`
exactly as it does without it, and pins in the patch tier's own state
(`$OCX_HOME/state/patch-companions/`) rather than the index; freeze the patch
tier deliberately with [`ocx patch freeze`](#patch-freeze) plus
[`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot]. The managed-configuration tier is
likewise unaffected: [`ocx config setup`](#config-setup) and
[`ocx config update`](#config-update) behave identically with and without the
flag.

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

Override the path to the [local index][fs-index] collection directory for this invocation.
By default, ocx reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`) — a
directory holding one subtree per source (`ocx.sh/`, `ghcr.io/`, …).

```shell
ocx --index /path/to/bundled/index install cmake:3.28
```

This flag swaps the *whole* collection for a shipped one — never a partial overlay of the two. It
is intended for environments where an index copy is bundled alongside a tool rather than living
inside `OCX_HOME` — for example inside a [GitHub Action][github-actions-docs],
[Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features] that ships a frozen
index copy as part of its release.

Under [`--remote`](#arg-remote), tag- and catalog-addressed lookups bypass the local index entirely
and query the registry directly, so `--index` has no effect on those. Digest-addressed lookups —
including the digest a resolved tag carries once pinned — still consult the redirected collection
first in every mode, `--remote` included, so `--index` keeps mattering for anything already pinned
by digest.

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

### `--global`, `-g` {#global-flag}

Selects `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`) as the project file. This is a **root flag** — it must appear before the subcommand name (like `--project` or `--offline`), not after it.

```sh
ocx --global add ripgrep:14      # correct
ocx -g add ripgrep:14            # same thing
ocx add --global ripgrep/ripgrep:14      # error: unknown flag
```

The short form is position-sensitive, and deliberately so: `-g` before the subcommand is `--global`, while `-g` after it is the `--group` selector of the toolchain-tier commands. `ocx -g update -g ci` updates the `ci` group of the global toolchain.

When `--global` is set, the following toolchain-tier commands target `$OCX_HOME/ocx.toml` instead of a discovered project file: `add`, `remove`, `lock`, `update`, `pull`, `run`, and `env`.

`--global` is mutually exclusive with `--project`. Passing both — whether as flags or via the `OCX_GLOBAL` / `OCX_PROJECT` environment variables — exits with code 64 (`UsageError`). The global toolchain never composes into project resolution; see [strict isolation][env-composition-strict-isolation] for the full hermetic contract.

::: warning No implicit home discovery
There is no implicit fallback to `$OCX_HOME/ocx.toml` when no project is found in the CWD walk. You must pass `--global` explicitly to target the global file. The prior automatic home-tier discovery has been removed.
:::

**Strict isolation**

The global toolchain is a shell-convenience tier only. `ocx run` and `ocx package exec` are always hermetic:

- `ocx run` without `--global` reads only the in-effect project file. The global file is never consulted.
- `ocx package exec` reads no project file at all.

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
| 65 | DataError | EX_DATAERR | Input data malformed: bad identifier, invalid digest, corrupted manifest, tampered Sigstore bundle; also a manifest fetch that got back something other than a manifest — an HTML page from a misconfigured [mirror][config-mirrors], for instance — refused by its content type before digest verification ever runs; also registry-served content whose digest does not match the descriptor; also a platform feature mismatch — the package ships for the host os/arch but no candidate's `os.features` are a subset of the host's (e.g. glibc vs musl), see [`--platform`](#package-install); also an ambiguous selection — a dual-libc host matched two equally-specific candidates (see [libc differentiation][authoring-libc]) | Validate identifiers and file contents; for a mirror serving a non-manifest response, check the mirror's own health and its `[mirrors]` routing; for a feature mismatch or ambiguous selection, override with `--platform` |
| 69 | Unavailable | EX_UNAVAILABLE | The registry answered, but not usefully — and a rerun will not change that. Also a local resource that cannot be reached | Inspect stderr; fix the registry or the URL before retrying |
| 74 | IoError | EX_IOERR | I/O error: filesystem permission denied, disk full, read/write failure | Check filesystem permissions and free space |
| 75 | TempFail | EX_TEMPFAIL | Temporary failure that may succeed on retry: registry connect failure or timeout, 429, 502, 503, 504, rate limit, transient network, or a layer blob that arrived short of its manifest-declared size | Retry with backoff |
| 77 | PermissionDenied | EX_NOPERM | Insufficient permissions: filesystem EPERM, offline sign refused, OIDC pre-check failed | Adjust filesystem permissions, or drop `--offline` to sign |
| 78 | ConfigError | EX_CONFIG | Configuration error: bad config file, missing required field, parse failure, trust root unavailable, a matched [`[[trust.policy]]`][config-trust] entry is malformed | Inspect the config file at the printed path |
| 79 | NotFound | OCX | Resource not found: package 404, explicit config path absent, no signatures found for target | Pin a different version or correct the path |
| 80 | AuthError | OCX | Authentication failure: registry 401 or 403, missing credentials, Fulcio OIDC token rejected | Refresh or set registry credentials |
| 81 | PolicyBlocked | OCX | A deliberate local policy (`--offline` or `--frozen`) refused a network or resolution operation — not a fault. Includes an unpinned-tag resolve that the policy forbade | Loosen the flag, or populate the local index first with `ocx index update` — itself run without the flag |
| 82 | DirtyRcBlock | OCX | A managed shell-integration block carried user edits and `ocx self setup` ran without `--force`; the block was left untouched. Distinct from ConfigError (78): the content is valid but intentionally user-modified | Re-run with `--force`, or edit the block manually and re-run |
| 83 | TransparencyLogUnavailable | OCX | Rekor transparency log unreachable during sign or verify (5xx/timeout, or SET absent with only TSA present) | Retry later; check Rekor endpoint |
| 84 | ReferrersUnsupported | OCX | Registry does not implement the OCI Referrers API — sign and verify require OCI 1.1 referrers support | Use a registry with OCI 1.1 referrers support |

**75 means the same command may succeed if run again; 69 does not.** That distinction is what makes automated retry safe: a wrapper loops on 75 and stops on 69, without parsing a single line of stderr. The per-command tables below still name 69 as "registry unreachable" — those rows exit 75 instead whenever the failure is transient (the connect never completed, the request timed out, or the registry answered 429/502/503/504).

Scripts can `case $?` on these stable values:

```shell
ocx package install kitware/cmake:3.28
case $? in
    0)  echo "installed" ;;
    64) echo "usage error; check flags" ;;
    69) echo "registry answered badly; a rerun will not help" ;;
    75) echo "transient failure; retry with backoff" ;;
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

### `--lazy-mode` {#arg-lazy-mode}

Available on the seven commands that compose or pre-warm an environment: [`env`](#env-root), [`run`](#run), [`pull`](#pull), [`direnv export`](#direnv-export), [`package env`](#env), [`package exec`](#exec), and [`package which`](#which). Not available on [`package install`](#package-install) or [`package select`](#package-select) — those are the only two commands that write the [candidate/current symlink namespace](#path-resolution), and a symlink must never point at a shim directory.

Controls when a declared tool's content downloads: now, or on first use.

| Value | Behavior |
|-------|----------|
| `never` (default) | Compose eagerly — content is materialized before the tool reaches `PATH`. |
| `always` | Compose a shim — the tool's declared names are on `PATH` immediately; content downloads the first time one of those names runs. |

`--lazy-mode` is the top tier of a five-level resolution ladder, most specific first:

| Tier | Source |
|------|--------|
| 1 | `--lazy-mode` on the invoked command |
| 2 | `[package."<id>"]` in [`ocx.toml`][config-project-package] |
| 3 | `[group.<name>]` in [`ocx.toml`][config-project-groups] |
| 4 | The toolchain-level `lazy-mode` key in `ocx.toml` |
| 5 | [`OCX_LAZY_MODE`][env-ocx-lazy-mode] |
| — | Floor: `never` |

An omitted flag leaves the CLI tier absent, letting the more general tiers speak — it never means `never`. See [Deferred Tools][in-depth-lazy-loading] for the full lifecycle, and [`ocx package which`](#which) below for how a deferred tool reports its on-disk `kind`.

::: tip Windows composes eagerly regardless of this flag
`lazy-mode` has no effect on Windows in this release — a tool resolved to `always` composes eagerly instead, with a debug-level log noting why. See [Deferred Tools][in-depth-lazy-loading] for the current state of Windows shim support.
:::

### `--lazy-report` {#arg-lazy-report}

Controls whether a deferred tool's first-invocation download renders progress. Declared on exactly one subcommand in the whole CLI — the hidden `ocx launcher shim` verb that a generated shim launcher execs into, never one a user types directly.

| Value | Behavior |
|-------|----------|
| `silent` (default) | No progress channel is opened. |
| `progress` | Render progress on the controlling terminal; falls back to `silent` where none is reachable (a Docker build, a CI runner, anything under `setsid`). |

It cannot be a flag on any of the seven composing commands above: the process that renders it is a separate one, spawned by the generated launcher long after the composing command exec'd away, so a value given at compose time has no route to the process that would use it. It resolves instead through its own four-tier ladder — one tier shorter than `--lazy-mode`'s, since there is no group to consult once composition is over:

| Tier | Source |
|------|--------|
| 1 | `--lazy-report` (on `ocx launcher shim` only) |
| 2 | `[package."<id>"]` in `ocx.toml` |
| 3 | The toolchain-level `lazy-report` key in `ocx.toml` |
| 4 | [`OCX_LAZY_REPORT`][env-ocx-lazy-report] |
| — | Floor: `silent` |

See [Deferred Tools][in-depth-lazy-loading] for why `lazy-report` has no `[group.<name>]` tier.

## Commands

### `add` {#add}

Appends a tool binding to the nearest `ocx.toml`, resolves its digest into `ocx.lock`, and installs the package in one step.

The command locates the project `ocx.toml` by walking the directory tree from the current working directory upward (same discovery as [`ocx lock`](#lock) and [`ocx pull`](#pull)). It fails with exit code 64 if no `ocx.toml` is found — it does **not** scaffold one implicitly. To create a project file first, run [`ocx init`](#init).

After mutating `ocx.toml`, `ocx add` resolves only the new bindings and carries every existing lock entry forward unchanged, then installs the newly added tools.

Multiple identifiers may be given in one invocation. They are staged together and committed atomically — if any identifier is invalid or its binding name already exists, nothing is written. `--group` applies to every identifier in the batch.

The same binding name may coexist in the default `[tools]` table and in any named `[group.*]` table — binding identity is `(group, name)`. This lets a project carry different versions of the same tool in different contexts:

```shell
ocx add shfmt/shfmt:3.13              # adds to default [tools]
ocx add --group ci shfmt/shfmt:3.13   # also legal — coexists in [group.ci]
```

**Usage**

```shell
ocx add [OPTIONS] <[NAME=]IDENTIFIER>...
```

**Arguments**

- `<[NAME=]IDENTIFIER>...`: One or more fully-qualified tool identifiers to add (e.g. `ocx.sh/kitware/cmake:3.28` or `ghcr.io/acme/mytool:1.0`). Bare identifiers without a tag (e.g. `ocx.sh/kitware/cmake`) default to `:latest` — the written `ocx.toml` entry is always explicit (`cmake = "ocx.sh/kitware/cmake:latest"`), following the same convention as `docker pull`. See [Unit 3 bare-identifier default][user-guide-toml] for the design rationale. Prefix an identifier with `NAME=` to bind it under an explicit key instead of the derived repository basename — see [Binding names](#add-binding-names) below.

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
| 1 | The in-place `ocx.toml` edit could not be expressed safely (rare); the command aborts rather than falling back to a lossy rewrite. |
| 64 | No `ocx.toml` found, binding already exists, invalid `--group` name, invalid binding `NAME`, `--global` combined with `--project`, or more than one `--platform` value (single-valued flag). |
| 65 | `ocx.toml` drifted from `ocx.lock` before this add — run `ocx lock` to reconcile. |
| 69 | Registry unreachable while resolving the new tag. |
| 74 | I/O error reading or writing `ocx.toml` or `ocx.lock`. |
| 75 | Another `ocx` process holds the project lock on `ocx.toml`, or a transient registry failure (connect failure, timeout, 429/502/503/504) survived the resolve retries. Retry with backoff. |
| 78 | `ocx.lock` uses an unsupported version — V1 and V2 locks are rejected; regenerate with `ocx lock`. Also: `ocx.toml` schema invalid or TOML parse error, or a requested `--platform` is not shipped by a tool. |
| 79 | Tag not found in the registry. |
| 80 | Authentication failure against the registry. |

#### Binding names {#add-binding-names}

Without `NAME=`, the binding key is the repository basename — `ocx add ocx.sh/kitware/cmake:3.28` binds under `cmake`. Two tools that share a basename in different namespaces collide under that default: `ocx.sh/gitlab/cli` and `ocx.sh/github/cli` both derive to `cli`, so adding the second fails with "binding already exists".

Prefix either identifier with an explicit `NAME=` to bind it under a distinct key instead:

```shell
ocx add gh=ocx.sh/github/cli:2.40
ocx add glab=ocx.sh/gitlab/cli:1.30
```

Both tools now coexist under their own keys — `ocx run gh`, `ocx run glab`, `ocx remove glab`, and the `ocx.lock` entry all key on the name you gave, not the repository path. `NAME` must be non-empty and contain only `[A-Za-z0-9._-]`; an invalid name exits 64.

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

`deps` never resolves any package's `env` values — it walks structural metadata only, so a
package whose env references an undeclared dependency, or that carries any other unresolvable
[interpolation token][reference-env-interpolation], still appears in the tree unchanged. Only two
things drop a package from the output, each logged at `warn` naming the package: `metadata.json`
that fails to parse or fails schema validation, and a declared env-var modifier type this ocx does
not recognize (a newer publisher, not a broken install).

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
└── ocx.sh/kitware/cmake:3.28 (sha256:ccc7d8e9…)
    └── ocx.sh/gcc:13 (sha256:ddd0a1b2…)
```

**`--flat`** shows the combined evaluation order after topological sort and deduplication:

```
Package            Digest
ocx.sh/gcc:13      sha256:ddd0a1b2…
ocx.sh/kitware/cmake:3.28  sha256:ccc7d8e9…
ocx.sh/java:21     sha256:bbb4e5f6…
myapp:1.0          sha256:aaa1b2c3…
```

**`--why`** traces all paths from roots to a specific dependency:

```
myapp:1.0 → ocx.sh/kitware/cmake:3.28 → ocx.sh/gcc:13
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

With `--format json`, the document carries `binaries`/`entrypoints`/`integrations` sibling arrays alongside `entries`, plus an `advisories` array for any [deferred tool][in-depth-lazy-loading] in the composition — see [`package env`'s JSON shape][cmd-package-env] for the full field reference; both commands report through the same envelope.

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
| [`--lazy-mode <MODE>`](#arg-lazy-mode) | — | Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. `always` composes a shim for every tool the ladder resolves to `always`, instead of downloading its content up front. | *(inherit from `ocx.toml` / `OCX_LAZY_MODE`)* |
| `--pull` | — | Materialise missing tools into the object store before composing (single batched install, like `ocx run`). A tool already present resolves locally with no network — only a genuine miss pulls. Last-wins with `--no-pull`. Ignored under `--global` — the global tier never installs. | **default** |
| `--no-pull` | — | Skip the install fallback: resolve against local state only. A lock-pinned tool that is not materialised is reported on stderr with an `ocx pull` hint and omitted from the composed env; the command never contacts the registry and the exit code stays 0. | — |
| `--show-patches` | — | Annotate each entry with its origin. When [`[patches]`][config-patches] is configured, companion overlay entries are appended after the toolchain's own entries; this flag adds a `Source` column to the plain table (a `"source"` object in JSON) naming the descriptor rule and companion that produced each overlay entry. No effect when `[patches]` is not configured. Mutually exclusive with `--shell` and `--ci`. | false |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` → `a=b`. Only the segment before that first `=` is checked for a `:TYPE[:SEP]` qualifier — an environment variable name can never contain `:`, so a Windows-style value with its own colon (`--env PATH:path=C:\tools\bin`) is read correctly, and `--env FOO:constant=a=b` sets `FOO` to `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends) — the same three kinds [`[env]`][config-project-env] uses. `SEP` qualifies `list` only: the string a `list` contribution is joined to the existing value with (`--env GODEBUG:list:,=gctrace=1`); omitted, the key inherits whatever separator another contributor already declared, or a single space if none did — see [Env Composition][env-composition-list]. A relative `path` value resolves against the **current directory** the flag was invoked from, not the project root [`[env]`][config-project-env] resolves against: a checked-in file must mean the same thing from any subdirectory, while a flag is composed by whatever script invokes `ocx`, and the current directory is the one base that script can compute. Highest-precedence stage: wins over ambient, package, patch, and project/group [`[env]`][config-project-env] (see [Project Environment][env-composition-project-env]). A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). See the `PATH` override warning under [`ocx run`](#run). | — |
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
| 65 | `ocx.lock` is stale — run `ocx lock` (project tier); or two contributors to one env key declared conflicting list separators (see [Separator agreement][env-composition-list-separator]). |
| 78 | `ocx.toml` or `ocx.lock` parse error (project tier); or `--ci=github` used outside [GitHub Actions][github-actions-workflow-commands] where [`GITHUB_ENV`][env-github-env] and [`GITHUB_PATH`][env-github-path] are unset. |

The global tier is lenient: `ocx --global env` never fails on an unconfigured or corrupt global toolchain — it exports an empty environment. This is one predictable rule that does not depend on `--shell` (which only selects the output format, never whether the command errors). A corrupt global lock surfaces instead via the commands that rewrite it — `ocx --global lock`, `ocx --global add`, `ocx --global update`. The project tier stays strict (a missing/stale/corrupt `ocx.lock` errors). A useful consequence of the lenient global rule: the installer's `env.sh`/`env.ps1`, which source `ocx --global env --shell=…` on every shell start, can never be broken by global toolchain state.

---

### `env` (package-tier — `ocx package env`) {#env}

Print the resolved environment variables for one or more OCI-tier packages.

With the root `--format plain` (default), outputs an aligned table with `Key`, `Type` and `Value` columns.
With `--format json`, outputs `{"entries": [...], "binaries": [...], "entrypoints": [...], "integrations": [...]}` — see [`package env`](#package-env) for the full shape.
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
- `--self`: Use the self view (mask `Visibility::PRIVATE`) — emits `private` and `public` entries (everything publisher marked for own runtime). Default off = consumer view (mask `Visibility::INTERFACE`) emits `public` and `interface`. See [Visibility Views][exec-modes]. `integrations` is always `[]` under `--self` — integrations reach only the interface surface, regardless of view.
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

::: tip Generated launchers use `ocx launcher exec`, not `ocx package exec`
Entry-point launchers generated by `ocx package install` call the internal `ocx launcher exec '<pkg-root>' -- <argv0> [args...]` subcommand, not `ocx package exec`. That subcommand validates the package root, forces the self view internally, resolves `${installPath}` (or its exact alias `${self.installPath}`, each optionally `:native`/`:posix`) in any baked entry-point `args`, then prepends the resolved arguments before user-supplied ones and executes the resolved entrypoint. A `${deps.*}` or `${self.env.*}` token is not legal in entry-point `args` — neither is any other unrecognised token — and refuses the launcher at exit 65, at run time, after install. The wire ABI (`<pkg-root> -- <argv0> [args...]`) is frozen so launchers generated by older OCX releases keep working after an upgrade. See the [Entry Points][entry-points] guide for the launcher ABI. On Windows, the native `.exe` shim makes this call without routing through `cmd.exe`, closing the `%*` argument-injection surface for default resolution.
:::

**Usage**

```shell
ocx package exec [OPTIONS] <PACKAGES>... -- <COMMAND> [ARGS...]
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
`ocx package exec` always inherits the parent's stdin so piped input flows into the child unchanged (`echo hi | ocx package exec pkg -- cat` prints `hi`). There is no opt-out — the previous `--interactive` flag was removed; matching standard shell exec semantics is the default.
:::

::: info Process replacement on Unix
On Unix, `ocx package exec` hands the current process image off to the target via `execvp(2)`, so the child inherits ocx's PID. Signals reach the target without an ocx forwarder, `pgrep <name>` shows the wrapped binary, and the process tree drops the ocx layer entirely — matching the same semantics shells use when chaining `exec "$@"` in entry-point scripts. On Windows, `ocx package exec` spawns the target and waits for it, since `CreateProcess` has no exec equivalent; the propagated exit code is forwarded as ocx's own exit code.
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

Never downloads anything, whether or not [`--lazy-mode`](#arg-lazy-mode) is passed — this command only reports what already exists on disk. Every entry also names which **kind** of directory it found: `package` for a materialized package root, or `shim` for a tool composed with `--lazy-mode always` whose content has not downloaded yet. Once such a tool has been used once, its content is on disk and the entry reports `package` again. `--candidate` and `--current` always report `package`, because the install symlinks they resolve are only ever written for materialized content. See [Deferred Tools][in-depth-lazy-loading] for the full lifecycle.

**Usage**

```shell
ocx package which [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve.

**Options**

- `-p`, `--platform`: Platform to consider when resolving. Defaults to the current platform. Ignored when `--candidate` or `--current` is set.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- [`--lazy-mode`](#arg-lazy-mode): Report a deferred tool's shim directory instead of refusing it — see below. Has no effect together with `--candidate`/`--current`, which always report a materialized package.
- `-h`, `--help`: Print help information.

**JSON shape (breaking, pre-1.0):** the value under each requested identifier is now an object, `{"path": "...", "kind": "package"|"shim"}`, rather than a bare path string. Plain output gains a matching `Kind` column.

::: tip
Use `--format json` with `jq` to embed the path in a script:

```shell
cmake_root=$(ocx package which --candidate --format json kitware/cmake:3.28 | jq -r '.["kitware/cmake:3.28"].path')
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

- `--group <NAME>` / `-g`: Scope composition to the named group(s), same grammar as [`ocx run -g`](#run). Omitted, the scope is the top-level `[tools]` table and its `[env]` — a group's `[env]` is otherwise unreachable from an `.envrc`.
- `--env <KEY[:TYPE[:SEP]]=VALUE>`: Set an environment variable for this invocation only, same grammar as [`ocx run --env`](#run). A relative `path` value resolves against the directory ocx runs in, which under direnv is the directory holding `.envrc`.
- [`--lazy-mode <MODE>`](#arg-lazy-mode): Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. Without it, a project declaring `lazy-mode = "always"` in `ocx.toml` would still compose eagerly here even though [`ocx env`](#env-root) defers it — `ocx direnv export` composes through the same ladder as every other env-composing command, so the environment does not depend on which command opened the shell.
- `--pull` / `--no-pull`: `--pull` (default) installs a missing tool on the object-store miss before exporting; `--no-pull` keeps the hook strictly offline and omits it. POSIX last-wins.
- `-h`, `--help`: Print help information.

::: tip Widening the scope
[`ocx direnv init`](#direnv-init) writes an `.envrc` that calls this command with no arguments. The line is yours to edit afterwards — `eval "$(ocx direnv export -g ci --env FORCE_COLOR=1)"` — and direnv picks it up on the next reload.
:::

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, or exports emitted). |
| 64 | Malformed `--env` argument, empty `-g` comma segment, or a `-g` naming no declared group. Unlike a missing lock or an unmaterialised tool, these are argv faults in a file you edited — they fail loudly rather than exporting nothing. |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`); or two contributors to one env key declared conflicting list separators (see [Separator agreement][env-composition-list-separator]). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS] [REGISTRY...]
```

Lists all packages available in the index. Uses the local index by default; pass [`--remote`](#arg-remote) to query the registry directly without writing through to the local index. Repository names are always prefixed with their registry in the output (e.g., `ocx.sh/kitware/cmake`).

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
as a tag filter on the returned list. With `--platforms`, a digest-bearing
identifier (`<pkg>@<digest>`) is accepted and resolves directly to that
artifact's platform set, offline when already cached locally.

**Arguments**

- `<PACKAGE>`: Package identifiers to list tags for. Must not include a digest suffix.

**Options**

- `--platforms`: Show the platforms (`os/arch`) the package publishes, read from its image index manifest. Uses the tag from the identifier, or `latest` if none specified. Resolved live from the registry under [`--remote`](#arg-remote); otherwise read from the local index.
- `--variants`: Lists unique variant names found in the tags.
- `-h`, `--help`: Print help information.

::: tip
`index list` is a pure-query command — under [`--remote`](#arg-remote) it
contacts the registry without writing the local index. To refresh the
persistent snapshot, run [`ocx index update`](#index-update) explicitly.
:::

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Explicitly refresh the local index for one or more packages from the remote source. Per tag, it
writes the tag's dispatch object plus its root document into the [local index collection][in-depth-indices-layout]
— never a leaf platform manifest — in a fixed, crash-safe order, then upserts the package's
catalog entry. This is what keeps a committed `.ocx/index/` self-contained for **version
choice**: resolving a locked tool's platform-manifest digest from the index afterward needs no
other store or network access. The manifest bytes and layers themselves are content, still fetched
on demand from the registry when actually installed.

`<PACKAGE>...` names what gets refreshed, and at least one is required. A tagged identifier (e.g.,
`kitware/cmake:3.28`) records only that tag — the remote tag listing is skipped entirely, which is
ideal for lockfile workflows where the local index should contain only explicitly requested tags. A
bare identifier (e.g., `kitware/cmake`) records every tag the source currently lists.

**Only the packages you name are touched, and nothing else is fetched.** A package left out keeps
every tag pin and its `repository` pointer exactly as committed, even when the source has moved on.

Nothing here syncs a whole index as a side effect, because a remote index floats (packages appear,
platforms get added, tags move) while the local copy is the set of snapshots you deliberately asked
for. See [Indices][in-depth-indices-update] for why that is the shape. The whole-registry form is a
verb of its own — [`ocx index sync <REGISTRY>`](#index-sync) — and is equally explicit. To see what
a source has without refreshing anything: [`ocx index sync --dry-run`](#index-sync), or ask the
source directly with [`ocx index catalog --remote`](#index-catalog) /
[`ocx index list --remote`](#index-list).

A bare `ocx index update` with no `<PACKAGE>` is a usage error ([exit 64][exit-codes]) — with no
"everything" to fall back on, there is nothing for it to mean.

`ocx index update` never writes to `$OCX_HOME/blobs/` or `$OCX_HOME/layers/` — those are populated
only by an online `ocx package install` or `ocx package pull` that actually materializes a package.
After running `ocx index update <pkg>`, an `ocx --offline package install <pkg>` resolves the
tool's per-platform digest from the index but still fails to install, since the manifest and layer
archives themselves are not part of the index.

On the first successful update for a given published source, `ocx index update` also writes that
source's `config.json` if one is not already present. See
[Serving a local index snapshot][in-depth-indices-servable] for what that unlocks.

A tag-refresh failure for any requested package fails the whole invocation; the
[exit code][exit-codes] corresponds to the first failure in request order
(deterministic across repeated runs). Packages that refresh successfully keep
their updated tags.

**Arguments**

- `<PACKAGE>...`: Package identifiers to update in the local index for. Include a tag to update only that tag; omit the tag to update all tags. At least one is required.

**Options**

- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Every named package refreshed. |
| 64 | No `<PACKAGE>` given. |
| 81 | [`--offline`](#arg-offline) or [`--frozen`](#arg-frozen) refused the command. Recording a new tag→digest mapping *is* the discovery a freeze forbids, and this is the package tier `--frozen` scopes to — so `index update` refuses in its own right. Re-run it without the flag to move the pin. |
| *other* | The first failure in request order decides — see [Exit codes][exit-codes] (e.g. 79 not found, 80 authentication failure). |

#### `sync` {#index-sync}

```bash
ocx index sync <REGISTRY>... [--dry-run]
```

The whole-registry form of [`update`](#index-update): name registries instead of packages, and each
registry's own catalog names the packages. This is how a whole mirror is snapshotted in one command
— see [Serving a local index snapshot][in-depth-indices-servable].

The package set is read **live from the source** — a published source's own `c/index.json`, a
plain-OCI registry's repository listing — never from the local copy, which is the set you already
have. Each package is then refreshed exactly as if named bare to `update`, so it adopts every tag
the source lists and keeps any tag only the local copy holds.

The derived branch's repository listing depends on the registry implementing OCI's `_catalog`
endpoint: Docker Hub disables it outright, and GHCR supports it only with an authenticated,
correctly-scoped token — a registry that refuses `_catalog` fails that registry's whole snapshot.

It is one explicit, operator-invoked read of each source's catalog **at that instant**, not a
standing subscription and not a replica. A merge never deletes, so repeated runs accumulate a union
of snapshots: a package that disappeared upstream keeps the tags this machine already recorded.
[`regenerate`](#index-regenerate) is the only command that removes anything, and only a catalog
entry whose root document is already gone — it cannot retract a package, for which the answer is a
fresh index home.

**Several registries are one run, not several.** Every `<REGISTRY>` is enumerated before any of them
is refused, so one unreachable source does not cost the others their snapshot; the command still
fails afterwards and reports each failure separately on stderr. The per-package refresh is bounded
at the same in-flight ceiling `update` uses, across all registries together rather than per
registry, so naming ten registries does not multiply the load on any of them.

On the first successful sync for a given published source, `ocx index sync` also writes that
source's `config.json` if one is not already present, exactly as `update` does.

A registry whose catalog cannot be read at all — a missing endpoint, an auth refusal (exit 80), or a
reachable, correctly-configured published source that simply serves no `c/index.json` (exit 69) —
fails the command. It is never read as an empty catalog. A served catalog listing **zero packages**
is a different fact: the source answered, so that enumerates cleanly and exits 0. That case still
prints a warning on stderr naming the registry, since a pull token without catalog scope commonly
answers with an empty listing rather than an authentication refusal.

A published source whose catalog carries a yanked tag is refused fail-closed: the mirror operator
must set [`OCX_ALLOW_YANKED`](./environment.md#ocx-allow-yanked) before running `ocx index sync`, or
the run exits 65 and snapshots nothing for that registry. This is the operator's own opt-in, separate
from and prior to the client-side refusal a resolve against the snapshot hits later.

**Arguments**

- `<REGISTRY>...`: Registries whose catalogs name the packages to refresh. At least one is required; repeat for several. A registry named twice is enumerated once, in first-mention order.

**Options**

- `--dry-run`: Print the packages this would refresh, one per line and sorted within each registry, and refresh none of them. Enumeration still runs, which contacts the source, so [`--offline`](#arg-offline) and [`--frozen`](#arg-frozen) still refuse it. Returns before both the per-package refresh and the patch-descriptor sync that otherwise follows a successful run: nothing under the index home is written — not even `config.json` — and no `[patches]` sync runs either. Exits 0 even when the enumerated set is empty; a registry that fails to enumerate still fails the dry run, with no partial listing printed.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Every enumerated package refreshed — or, under `--dry-run`, printed. |
| 64 | No `<REGISTRY>` given. |
| 65 | A registry's catalog served a key that is not a well-formed repository path. That registry is refused whole, before any of its packages is touched, and `--dry-run` fails the same way. |
| 81 | [`--offline`](#arg-offline) or [`--frozen`](#arg-frozen) refused the command — including a `--dry-run`, since printing the set still means contacting the source. |
| *other* | A registry that could not be enumerated decides, ahead of any per-package failure — including **69** for a reachable source that serves no `c/index.json`. Otherwise the first per-package failure in the order `--dry-run` prints them decides; see [Exit codes][exit-codes] (e.g. 79 not found, 80 authentication failure). |

#### `regenerate` {#index-regenerate}

```bash
ocx index regenerate <REGISTRY>...
```

Re-derives a published index source's `c/index.json` catalog from the root documents actually
present under its `p/` tree, and writes it back. `c/index.json` is derived data — every entry
restates a digest the root document beside it already carries — and every other writer only ever
*adds* to it, which leaves one drift nothing else repairs: an entry naming a package whose root
document is gone. `regenerate` is the one operation that clears such an entry, by replacing the
whole map with what the walk actually finds.

It makes no assumption the tree was written by this `ocx`: root documents possibly written by
another implementation, and a tree with no prior `c/index.json` at all, are both valid input. It
consults no source and moves no pin — it never contacts a registry or an index endpoint — so
[`--frozen`](#arg-frozen) and [`--offline`](#arg-offline) both permit it: [`update`](#index-update)
and [`sync`](#index-sync) are the commands both flags refuse (theirs is the write both flags exist to
gate); `regenerate` is the only one whose *purpose* is to write, rewriting the catalog deliberately; [`catalog`](#index-catalog) and
[`list`](#index-list) are permitted because, without `--remote`, they read the local copy — though
`list` can still trigger a read-path self-heal on an already-drifted tree, a write neither flag gates.

It writes exactly one file per registry, that registry's `c/index.json` — clearing a stale
`c/index.json.etag` beside it too, if the tree carries one left by an older `ocx` — and never a root
document, never a dispatch object, and never `config.json`: `name_segments` is an operator
declaration no tree can be read for, so fabricating one while repairing a foreign tree would be
wrong. A tree whose catalog already matches its `p/` tree is left byte- and mtime-identical, once any
stale `c/index.json.etag` has been cleared — that clearing happens even on an otherwise-clean first
run against a tree written by an older `ocx`.

::: warning Symlinked layouts lose packages silently
`regenerate` does not follow symlinks under `p/`. A symlinked root document is skipped, and a
**symlinked directory is never queued at all — taking every root beneath it with it in one step.**
Because the catalog is replaced wholesale, that is not a missing entry but silent bulk removal from
`c/index.json`: the packages still resolve by tag (resolution reads roots directly and never through
the catalog), but they vanish from `ocx index catalog`, from [`index sync`](#index-sync) enumeration, and from
anything else that reads the catalog. `regenerate` is specified for trees whose roots and
intermediate directories are real files and directories — every tree `ocx` itself produces. A
symlink-deduplicated layout (a package staged once and linked into several locations) needs the real
files underneath, not links to them — hardlinks are unaffected, since a hard-linked file *is* a
regular file to the walk.

The removal is not always permanent: on a tree this machine also resolves, not merely serves, a
resolve's own catalog self-heal re-adds a dropped entry the next time the package is resolved, since
the root read behind it follows symlinks directly. It sticks for a tree that is served and never
locally resolved.
:::

Each `<REGISTRY>` must be a **published** index source — one configured with
[`[registries."<ns>"] index`][config-registries-index]. A derived, plain-OCI namespace has no
catalog document by grammar (its catalog *is* the `p/` enumeration itself) and is refused with
[exit 78][exit-codes], naming the registry.

**Arguments**

- `<REGISTRY>...`: One or more published index sources to regenerate. At least one is required.

**Report**

Per registry: the number of root documents the walk found, and every package added (a root on disk
with no catalog entry), corrected (a catalog entry whose digest disagreed with the root on disk), or
removed (a catalog entry naming a root no longer on disk). A run that changed nothing for every named
registry prints a single line instead of an empty table. Honours the root
[`--format json`][arg-format].

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Every named registry's catalog matches its `p/` tree, and the report was printed. |
| 64 | No `<REGISTRY>` given. |
| 78 | A named registry — or a configured namespace resolving to the same on-disk subtree under a different name — is not a published index source. |
| *other* | 65 — a root document under `p/` failed to parse. 74 — the named registry's subtree does not exist (a mistyped `<REGISTRY>`, or a checkout not yet materialized): `regenerate` never creates one, it refuses instead; 74 also on a `p/` walk that found no root document while the catalog still names packages (refusing to replace a non-empty catalog with an empty one), and on a source lock that timed out. Per-registry failures aggregate in argument order; the lowest-index failure is the process error. |

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

### `status` {#status}

Reports what `ocx.toml` and `ocx.lock` say, without resolving anything. Offline, read-only, and never writes either file: no registry is contacted, no platform is selected, no package metadata is read, and no relative `path` value is anchored to the project root.

Status answers on projects that other commands refuse. A missing `ocx.lock` exits 78 for [`pull`](#pull) and [`run`](#run); a drifted one exits 65; an unparseable one fails outright. All three are states `status` reports as payload and still exits `0` for — it is the command you reach for when the project is broken.

For what those declarations *resolve to* on this host — the pinned digest per binding, the composed environment, what would land on `PATH` — use [`inspect`](#inspect).

**Usage**

```shell
ocx status
```

**Options**

- `-h`, `--help`: Print help information.

There is no `-g`/`--group` and no NAME argument. The report is a keyed object a caller narrows itself, and a filter here would only hide rows rather than change any answer — unlike in `inspect`, where the selection decides what gets composed.

Honors the global [`--format`][arg-format] and [`--project`][arg-project] / [`--global`][global-flag] flags. [`--offline`][arg-offline] is accepted and inert: this command never reaches the network.

**JSON shape**

```json
{
  "project": "/home/you/code/app/ocx.toml",
  "lock": {
    "present": true,
    "current": false,
    "lock_version": 3,
    "declaration_hash": "sha256:67d0ab…",
    "declaration_hash_expected": "sha256:91ffcc…",
    "generated_by": "ocx 0.4.3",
    "generated_at": "2026-06-14T23:29:57Z"
  },
  "groups": {
    "default": {
      "tools": {
        "go-task": {
          "declared": "ocx.sh/go-task/task:3",
          "platforms": {
            "linux/amd64": "sha256:fcfad8…",
            "darwin/arm64": "sha256:7ab019…"
          }
        },
        "newtool": { "declared": "ocx.sh/newtool:1" },
        "oldtool": { "platforms": { "linux/amd64": "sha256:11c0d6…" } }
      },
      "env": { "CI": { "type": "constant", "value": "1" } }
    },
    "ci": {
      "tools": {},
      "env": { "PATH": { "type": "path", "value": "node_modules/.bin" } }
    }
  },
  "package_settings": { "ocx.sh/foo:1": { "no-patches": true } }
}
```

`default` is a group like any other. The top-level `[tools]` and `[env]` tables in `ocx.toml` **are** its tools and env — which is why `default` is a reserved group name — so the report has no separate top-level env.

Each binding under `groups.<name>.tools` reports its state by which keys are present, the same convention [`binaries`][reference-binaries-none-vs-empty] uses:

| `declared` | `platforms` | Meaning |
|---|---|---|
| present | present | Declared in `ocx.toml` and locked. |
| present | absent | Declared but not yet locked — added since the last `ocx lock`. |
| absent | present | Locked but no longer declared — orphaned in a stale lock. |

`platforms` carries **every** leaf the lock records, not the host's. Picking the host leaf is resolution, which is `inspect`'s job.

`env` values are verbatim: a relative `type = "path"` value stays relative here, because anchoring it to the project root is composition. `inspect` and [`env`](#env-root) show the anchored form.

`lock` describes the lock file itself:

- `present` — whether `ocx.lock` exists.
- `error` — present only when the file exists but could not be parsed (an unsupported `lock_version`, a corrupt file). The header fields and every `platforms` map are then absent; the declaration half of the report is unaffected.
- `current` — whether the lock's stored `declaration_hash` still matches the config's. Absent when nothing was parsed.
- `declaration_hash` (stored) and `declaration_hash_expected` (recomputed) are both reported so a consumer sees *why* `current` is false without recomputing the project's canonicalization itself. Both cover `[tools]` and `[group.*]` only — `[env]` and `[package.*]` are excluded by design, so editing either leaves `current` true.

`package_settings` reports `[package."<id>"]` precisely because it is excluded from the declaration hash: nothing lock-derived can surface it.

**Exit codes**

- `0` — success, including "no lock", "stale lock", and "unreadable lock".
- `64` (`UsageError`) — no `ocx.toml` in scope, or a selector was passed.

### `inspect` {#inspect}

Inspects what the project toolchain resolves to, without installing. The toolchain-tier counterpart to [`ocx package inspect`](#package-inspect): the same report and the same flags, keyed by `ocx.toml` binding instead of raw identifier, and carrying the project's composed environment alongside.

Selects the bindings in the requested groups, narrows them to any `NAME`s given, and reports each one. Read-only — nothing is installed, no symlink is created, neither project file is written.

`--resolve` is what selects a platform, here exactly as on the OCI-tier command. By default each binding lists the platform **candidates** `ocx.lock` pins for it, so the default report is a pure projection of the two project files: no registry is contacted, no host leaf is chosen, and `-p` stays inert.

Needs a current `ocx.lock`: exit 78 when absent, 65 when it no longer matches `ocx.toml`. Without a pin there is no stable answer — re-resolving declared tags live would make the report depend on where a moving tag points at that moment. Use [`status`](#status) for the declaration itself, including those two states.

**Usage**

```shell
ocx inspect [OPTIONS] [NAME]...
```

**Arguments**

- `[NAME]...`: Binding names to inspect. Each is an `ocx.toml` binding key. Defaults to every binding in the selected groups. Only the named bindings are reported, so under `--resolve` an unrelated sibling that ships no leaf for this host does not block the report.

**Options**

- `-g`, `--group <GROUP>`: Restrict the selection to the named group(s). Repeatable and comma-separated. `default` selects the top-level `[tools]` table; `all` expands to `default` plus every declared `[group.*]`. Omitted means the default group, not everything — matching [`run`](#run) and [`env`](#env-root).
- `-p`, `--platform <PLATFORM>`: Platform to resolve each binding's leaf against. Defaults to the host. Applies with `--resolve` and `--closure`; ignored in default mode, where the candidate list always shows every locked platform.
- `--resolve`: Select this host's leaf and emit its metadata plus the OCI resolution chain. The lock already pins a platform manifest, so the chain starts there and carries no `index` entry.
- `--closure`: Compute each binding's dependency closure from metadata alone, plus the `interface` / `private` surface projections. Because the walk sees the whole selection at once, it also reports collisions between two *different* tools before either is installed.
- `--env <KEY[:TYPE[:SEP]]=VALUE>`: Set an environment variable for this invocation. Repeatable; appended last in the report's `env` array, matching where it lands in composition.
- `-h`, `--help`: Print help information.

Honors the global [`--offline`][arg-offline], [`--remote`][arg-remote], [`--format`][arg-format], and [`--project`][arg-project] / [`--global`][global-flag] flags. Default mode reads no registry at all. A cold `--closure` costs one config-blob fetch per selected binding; a warm one is served from the local cache and works under `--offline`.

**JSON shape**

The same envelope [`ocx package inspect`](#package-inspect) emits. Default — the locked candidates for each binding:

```json
{
  "packages": [
    {
      "name": "shellcheck",
      "identifier": "ocx.sh/shellcheck/shellcheck:0.11",
      "candidates": [
        {
          "digest": "sha256:5238fe…",
          "pinned": "ocx.sh/shellcheck/shellcheck:0.11@sha256:5238fe…",
          "platform": "linux/amd64"
        },
        {
          "digest": "sha256:7ab019…",
          "pinned": "ocx.sh/shellcheck/shellcheck:0.11@sha256:7ab019…",
          "platform": "darwin/arm64"
        }
      ]
    }
  ],
  "env": [
    { "key": "CI", "type": "constant", "value": "1" },
    { "key": "PATH", "type": "path", "value": "/home/you/code/app/node_modules/.bin" },
    { "key": "CI", "type": "constant", "value": "0" }
  ]
}
```

`--resolve` replaces the candidate list with the selected leaf, and adds `pinned_identifier` / `pinned_digest` to the entry plus `platform` to the envelope:

```json
{
  "platform": "linux/amd64",
  "packages": [
    {
      "name": "shellcheck",
      "identifier": "ocx.sh/shellcheck/shellcheck:0.11",
      "pinned_identifier": "ocx.sh/shellcheck/shellcheck:0.11@sha256:5238fe…",
      "pinned_digest": "sha256:5238fe…",
      "metadata": { "…": "…" },
      "layers": [ { "digest": "sha256:…", "media_type": "…", "size": 4051232 } ],
      "resolution": { "…": "…" }
    }
  ],
  "env": []
}
```

`name` is the binding, so an entry is addressable by the same key `ocx.toml` uses. `identifier` is the declaration verbatim, tag included. `packages` is in selection order: group order, then lock order within each group.

A candidate carries no `media_type` or `size`, and the entry no `pinned_identifier` or `pinned_digest`: the lock records one leaf digest per platform, never the descriptors that pointed at them nor the index that carried them. Absence is the signal — each candidate's own `pinned` is the pullable reference. Use [`status`](#status) if you want the same map keyed by platform.

`env` is an **ordered array**, in application order: `[env]` first, then each selected group's `[group.<name>.env]` in `-g` order, then `--env` last. Every contributing declaration is kept rather than merged, so one key can legitimately appear more than once — which is why this is an array and `status`'s per-scope view is an object. The array shows *what was declared and in what order*; [`ocx env`](#env-root) is what answers *what the final value is*, and it materializes packages to do so because their values are `${installPath}`-templated.

Package-declared environment is not in this array. It lives inside each entry's `closure.surface.env` under `--closure`, attributed per package and without values, exactly as in `ocx package inspect`.

**Examples**

```shell
# What does the default group pin, per platform? (offline)
ocx --format json inspect | jq -r '.packages[] | .name as $n | .candidates[] | "\($n) \(.platform) \(.pinned)"'

# What does it pin on this host?
ocx --format json inspect --resolve | jq -r '.packages[] | "\(.name) \(.pinned_identifier)"'

# One binding, from a named group.
ocx --format json inspect -g ci shellcheck

# What would the whole toolchain put on PATH, without installing it?
ocx --format json inspect -g all --closure | jq '.packages[].closure.surface.interface.binaries'

# Does any pair of tools collide before I install them? (exits 65 if so)
ocx --format json inspect -g all --closure | jq '.packages[] | select(.closure.conflicts.entrypoints != [])'
```

**Exit codes**

- `0` — success.
- `64` (`UsageError`) — unknown group, unknown binding name, or a binding two selected groups resolve differently *and* that binding is in the narrowed set.
- `65` (`DataError`) — `ocx.lock` is stale, or `--closure` found a conflict that makes the surface unrealizable (the conflict is still reported in full).
- `78` (`ConfigError`) — `ocx.lock` is absent; run `ocx lock`.
- `81` (`PolicyBlocked`) — `--offline` and a needed manifest or config blob is not cached. Default mode reads no registry, so this needs `--resolve` or `--closure`.

### `init` {#init}

Creates a minimal `ocx.toml` in the current directory.

The generated file contains a [`#:schema` directive][config-schemas] and an empty `[tools]` table — a non-interactive skeleton following the "backend-first, minimal output" design. Once the file exists, use [`ocx add`](#add) to append tool bindings or edit it directly; comments and declaration order in the file survive every mutation.

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
`ocx package install`, `ocx pull`, and other registry-accessing commands.

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
| 75 | Transient registry failure (connect failure, timeout, 429/502/503/504) survived the resolve retries — rerunning may succeed. |
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

Re-resolves advisory tags in `ocx.toml` against the live registry and rewrites `ocx.lock`. Unlike [`ocx lock`](#lock) or [`ocx add`](#add), which resolve through the local index by default, `ocx update` talks to the registry every time — the same default [`ocx self update`](#self-update) uses, since the whole point is to see where a moving tag (`:latest`, `:3`) points *today*. With no arguments this is the **whole-file forced-bump verb**: every declared tag is re-resolved, even when the lock is already current. An unchanged result rewrites the lock byte-identically. The operation is fully transactional — on any resolution failure nothing is written. Resolution only ever writes `ocx.lock`; it never rewrites the local index at `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index/`.

Pass binding names, `-g/--group`, or both to advance only part of the toolchain instead: every other pin in `ocx.lock` stays frozen. A scoped update advances each named binding's declared tag to today's resolution and carries every other entry forward unchanged. This only moves the resolution the declared tag already points to — it never changes the declaration itself. To pin a new explicit version, edit `ocx.toml` directly; that is a declaration change, not an update.

::: tip `ocx update` vs `ocx lock`
Whole-file `ocx update` always re-resolves every tag against the registry; scoped to `-g`/`NAME` arguments, it re-resolves only those. `ocx lock` only re-resolves when `ocx.toml` drifted (idempotent when clean), and prefers the local index like other project-tier commands. To advance versions, use `ocx update`. To reconcile a changed config, use `ocx lock`.
:::

::: tip The update family
Four verbs share the name; each refreshes exactly one record. [`ocx index update`](#index-update) refreshes the local index at `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index/` — [`ocx index sync`](#index-sync) is `ocx index update` over a whole registry's catalog rather than a named list. [`ocx self update`](#self-update) refreshes the managed ocx installation. [`ocx config update`](#config-update) refreshes the managed-config snapshot. `ocx update` refreshes `ocx.lock`. Under [`--frozen`](#arg-frozen), `ocx update` caps discovery at that local index — a declared tag it doesn't already know exits [`81`](#exit-codes). Under [`--offline`](#arg-offline), no network call is made at all, which also exits `81` when resolution is required.
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
| [`--lazy-mode <MODE>`](#arg-lazy-mode) | — | Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. `pull` composes nothing, so `always` changes *what* is pre-warmed instead of what reaches `PATH`: a tool the ladder resolves to `always` gets its metadata, its dependency closure's config blobs, and its generated shim launchers — no content. The content downloads the first time one of those launchers runs, in whatever environment a later `ocx run` or `ocx env` composes. | *(inherit from `ocx.toml` / `OCX_LAZY_MODE`)* |
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

Outside `--dry-run`, plain output is a three-column `Package` / `Kind` / `Path` table, one row per pulled tool; `--format json` is a matching object keyed by pinned identifier, `{"path": "...", "kind": "package"|"shim"}`. A tool the `lazy-mode` ladder resolved to `never` reports its materialized package root and `kind: "package"`; a tool resolved to `always` reports the generated shim directory this run created and `kind: "shim"` — no package root exists for it yet. See [Deferred Tools][in-depth-lazy-loading].

One reserved key sits beside the identifier keys: `advisories`, the same array [`ocx env`](#env-root) and [`ocx package env`](#package-env) publish — `{"kind": "...", "package": "...", "key": "...", "message": "..."}` objects, one per deferred tool whose declared metadata could not be fully validated. Always present, empty unless a tool composed with `--lazy-mode always` raised one; warning-only, and written to stderr as well so the plain channel carries it too. No pinned identifier can collide with the key, since every other key is a `registry/repository@sha256:...` string.

#### Dry-run preview {#pull-dry-run}

`ocx pull --dry-run` resolves each locked tool through the local index
(cache-first, like the real pull does) and reports whether it is already in the
store. The store is never modified. Combine with [`--offline`](#arg-offline) to
forbid the cache-miss network probe entirely.

```shell
$ ocx pull --dry-run
Package                                     Status
localhost:5000/cmake@sha256:1f4a9c2e7b03    cached
localhost:5000/ripgrep@sha256:8d2b60fa1c95  would-fetch
```

Plain output shortens each locked leaf to a 12-hex digest; the full pin rides
out under [`--format json`](#arg-format), which also carries a `path` field. That
`path` matches the contract of [`ocx package which`](#package-which): it is the
**package root** (parent of `content/` and `entrypoints/`), not the `content/`
subdirectory, and it is populated only for `cached` rows. Consumers traverse into
`<path>/content/` for files or prefer [`ocx env`](#env) to compose `PATH` and
friends.

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
| [`--lazy-mode <MODE>`](#arg-lazy-mode) | — | Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. `always` composes a shim for every tool the ladder resolves to `always`; its content downloads the first time the child process invokes it. | *(inherit from `ocx.toml` / `OCX_LAZY_MODE`)* |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` → `a=b`. Only the segment before that first `=` is checked for a `:TYPE[:SEP]` qualifier — an environment variable name can never contain `:`, so a Windows-style value with its own colon (`--env PATH:path=C:\tools\bin`) is read correctly, and `--env FOO:constant=a=b` sets `FOO` to `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends) — the same three kinds [`[env]`][config-project-env] uses. `SEP` qualifies `list` only: the string a `list` contribution is joined to the existing value with (`--env GODEBUG:list:,=gctrace=1`); omitted, the key inherits whatever separator another contributor already declared, or a single space if none did — see [Env Composition][env-composition-list]. A relative `path` value resolves against the **current directory** the flag was invoked from, not the project root [`[env]`][config-project-env] resolves against: a checked-in file must mean the same thing from any subdirectory, while a flag is composed by whatever script invokes `ocx`, and the current directory is the one base that script can compute. Highest-precedence stage: wins over ambient, package, patch, and project/group [`[env]`][config-project-env] (see [Project Environment][env-composition-project-env]). A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). | — |
| `--help` | `-h` | Print help information. | — |

::: tip Target the global toolchain
Pass `--global` **before** the subcommand: `ocx --global run -- cmake --version`. The global file must exist (no auto-init for read commands). See [`--global`][global-flag].
:::

::: warning `--env PATH=...` replaces the composed `PATH`, it does not extend it
`--env` with no `:TYPE` is a `constant` — it replaces the key outright, the same as a bare-string [`[env]`][config-project-env] entry. `--env PATH=/opt/tools/bin` therefore overwrites the composed `PATH`, silently dropping every package's `bin/` and `entrypoints/` directory. There is no name-based special case for `PATH`; write `--env PATH:path=/opt/tools/bin` to prepend instead of replace.
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
| 64 | `--` missing; empty argv; empty `-g` segment; no `ocx.toml` found; unknown `-g` group; unknown binding NAME; ambiguous NAME across groups with conflicting identifiers; `--global` combined with `--project`; a bare `--env FOO` with no `=`; an `--env` `TYPE` that names no modifier or is empty (`--env X:bogus=v`, `--env X:=v`); or `--env` sets an `OCX_*`/`__OCX_*` key. (OCX remaps clap's default exit 2 to 64.) |
| 65 | `ocx.lock` is stale — run `ocx lock`; or two contributors to one env key declared conflicting list separators (see [Separator agreement][env-composition-list-separator]); or a policy-covered binding's Sigstore bundle is tampered (auto-verify). |
| 69 | Registry unreachable during auto-install of a missing package. |
| 75 | Transient registry failure during auto-install (connect failure, timeout, 429/502/503/504) — rerunning may succeed. |
| 77 | A policy-covered binding's certificate identity or OIDC issuer does not match (auto-verify). |
| 78 | `ocx.lock` absent — run `ocx lock`; or `ocx.toml` parse error — including a tool binding declared directly under `[group.<name>]` instead of `[group.<name>.tools]`, or an `[env]`/`[group.<name>.env]` entry with an `OCX_*`/`__OCX_*` key (e.g. `[group.all]` declared); or no leaf digest for the host platform at the locked version (no `"any"` fallback key in `[tool.platforms]`) — run `ocx update <tool>` to re-resolve; or a policy-covered binding's trust root/policy is misconfigured (auto-verify). The host-leaf check fires only for tools actually composed: the named subset when `NAME` is given, or every tool in scope when it is omitted. |
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

# One-off override — wins over ambient, package, and project/group [env]
ocx run --env CI=1 --env SOURCE_DATE_EPOCH=0 -- task build

# Prepend a project-local directory to PATH for this invocation only
ocx run --env PATH:path=node_modules/.bin -- eslint .
```

::: tip Project-tier vs OCI-tier
`ocx run` requires `ocx.toml` and `ocx.lock`. If you do not have a project file, use [`ocx package exec`](#package-exec) with an OCI identifier instead.
:::

See [Project Toolchain In Depth → Running tools][in-depth-project-running] for composition order, PATH precedence, the `all` keyword, and worked examples.

### `remove` {#remove}

Removes one or more tool bindings from `ocx.toml`, rewrites `ocx.lock`, and uninstalls the tools.

Each argument accepts either a bare binding name (`cmake`), a name with a tag (`kitware/cmake:3.28`), or a fully-qualified identifier (`ocx.sh/kitware/cmake:3.28`). An identifier form is reduced to the repository basename — the tag and registry are used only to locate the correct entry and the installed package; the key match is against the TOML map key. A binding added under an explicit name ([`ocx add glab=ocx.sh/gitlab/cli`](#add-binding-names)) is matched only by that name — remove it with `ocx remove glab`, not its identifier. Fails with exit code 79 if any argument matches no binding; the removals are staged together, so a fail-fast leaves `ocx.toml` unchanged.

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

- `<IDENTIFIER>...`: One or more binding names or fully-qualified identifiers to remove (e.g. `cmake`, `kitware/cmake:3.28`, or `ocx.sh/kitware/cmake:3.28`). A binding added under an explicit [`NAME=`](#add-binding-names) is addressed by that name only.

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
| 1 | The in-place `ocx.toml` edit could not be expressed safely (rare); the command aborts rather than falling back to a lossy rewrite. |
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

The second gate is **at consumption time**, invoked whenever `ocx env` or `ocx package exec` is given two or more roots. The compose-time check projects each root's interface surface (bundle entry points plus interface-visible TC entries) and reports the same `EntrypointCollision` error if two roots claim the same name. This catches conflicts that the per-package install gate cannot see — two roots installed independently without `--select` and combined only at exec time.

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
| `--managed-config REF` | — | Adopt (or clear) the corporate [managed-config][config-managed] tier. `REF` is resolved as an OCI reference, synchronously fetched and persisted, then the `[managed]` seed fence in `config.toml` is written only on success — a fetch failure leaves no partial state. Pass `--managed-config ""` to clear an existing seed and delete the snapshot. Omitting the flag does not skip resolution: it falls back to [`OCX_MANAGED_CONFIG`][env-ocx-managed-config], then the existing seed. Every run reconciles whichever source resolves — a wiped or mismatched snapshot self-heals (hard-fail on a fetch error, same as first adoption), and an already-adopted seed is re-synced to whatever the registry serves now, so a newer published config is picked up without a separate [`ocx config update`](#config-update). That re-sync is best-effort once a matching snapshot already exists on disk: a fetch failure warns on stderr and keeps the existing snapshot (exit 0) instead of failing the run. | *(resolved: env, then existing seed)* |
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
| `already_adopted` | Yes | The resolved ref matches the existing seed AND a matching snapshot is on disk. Either the registry was checked and still serves the same content (verified, not assumed), or the check was skipped outright — a digest-pinned seed (content-addressed, cannot drift), `--offline`, or an in-force [`config update --pause`](#config-update). The digest is the existing snapshot's digest. A wiped or mismatched snapshot self-heals instead: the run re-fetches and reports `adopted`. |
| `adopted` | Yes | A new or changed ref was fetched, persisted, and the seed fence written. Also covers self-heal of a wiped or mismatched snapshot behind a fence that was already current. |
| `refreshed` | Yes (+ `previous_digest`) | The resolved ref matched the existing seed, but the registry now serves newer content than the on-disk snapshot: the snapshot was replaced in place — the fence itself is untouched, only rewritten on an `adopted` transition. `digest` is the new content; `previous_digest` is what the snapshot carried going in. |
| `refresh_unavailable` | Yes (+ `reason`) | The re-sync of an already-adopted seed could not reach the registry. The existing snapshot is kept and the run still exits 0 — `reason` carries the fetch error, and the same message is written to stderr as a warning. Re-run, or run [`ocx config update`](#config-update) directly, to retry. |
| `cleared` | No | `--managed-config ""` removed the seed fence and deleted the snapshot. |
| `dirty` | No | The `[managed]` fence carries user edits; left untouched without `--force` — drives root `status: skipped` (exit 82). |
| `would_adopt` | No | `--dry-run`: a first adopt, a self-heal of a wiped or mismatched snapshot, or a clear would run, but nothing was fetched or written. |
| `would_refresh` | Yes | `--dry-run` against an already-adopted seed: a re-sync would run, but nothing was fetched or written — dry-run never touches the network, so this does not confirm the registry actually has newer content. |

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

Where codes 65, 69, 74, 79, and 80 above concern the `[managed]` tier, they apply to the fetch that establishes the seed — first adoption, self-heal of a wiped or mismatched snapshot, or an explicit clear; the same codes also arise from the bootstrap phase (installing the pinned binary), independent of managed config. The re-sync of an *already-adopted* seed is best-effort instead: a failed re-sync *fetch* (or a published payload that fails validation) reports `managed_config.status: "refresh_unavailable"` and still exits 0, rather than failing the whole run — a failure while *writing* the refreshed snapshot to disk still errors (74), since the on-disk state may no longer be the retained one.

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

Version discovery queries the published index and registry live for the newest release — self update exists to reach the freshest upstream ocx, so it does not read the (possibly stale) [local index][fs-index]. This matches [`ocx self setup`](#self-setup) and the background update notice ocx prints on other commands. Under [`--offline`][arg-offline] the check is skipped and the running binary is left unchanged; [`--remote`][arg-remote] is redundant (already the default) but still accepted.

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

#### `announce` {#package-announce}

Observes an owner-curated set of registry tags for one package and publishes the rebuilt entry into the index: written to a local directory (`--out`), or opened as a pull request against the index repository.

The pull request comes from a fork with `--fork`. Omit `--fork` and the announce branch is pushed to `--index-repo` itself — for a publisher whose credential can already push there, which is the only working path when the publishing repository and the index share an owner, since a repository cannot be forked into the namespace that already owns it. Either way the change arrives as a pull request; `announce` never commits to the index's default branch.

**GitHub and GitLab are both supported**, on their public hosts and on self-hosted instances. On GitLab the change arrives as a **merge request**; everything else in this section reads the same. Which forge a run talks to comes from `--index-repo`:

| `--index-repo` | Forge | API base |
|---|---|---|
| `ocx-sh/index` (no host) | GitHub | `https://api.github.com` |
| `github.com/acme/index` | GitHub | `https://api.github.com` |
| `gitlab.com/acme/index` | GitLab | `https://gitlab.com/api/v4` |
| `github.example.com/acme/index` + `--forge github` | GitHub Enterprise Server | `https://github.example.com/api/v3` |
| `gitlab.example.com/acme/index` + `--forge gitlab` | Self-managed GitLab | `https://gitlab.example.com/api/v4` |

A self-hosted host **must** name its forge with `--forge`; a run without it exits 64 and says so. A hostname carries no reliable signal of which forge runs behind it, and a wrong guess would send your credential to the wrong API — so `ocx` asks instead of guessing. `--forge` also overrides the inferred forge for `github.com` and `gitlab.com`, for an instance sitting behind a proxy under one of those names.

GitLab groups nest, and so does the coordinate: `--index-repo gitlab.example.com/acme/platform/tooling/index` names the project `index` inside the group path `acme/platform/tooling`. `--fork` takes the same form. On GitHub, where organizations do not nest, a nested namespace is rejected before any request is made.

Because a host and a group are both just a leading segment, a **dotted top-level group** is ambiguous: `acme.team/platform/index` reads as the host `acme.team`, and the run stops asking for `--forge` rather than guessing where to send your credential. Write the host out to say what you meant — `gitlab.com/acme.team/platform/index` names the group `acme.team/platform` on gitlab.com.

One self-hosted shape is **not** supported: an instance mounted under a path prefix, where the API lives at `https://example.com/gitlab/api/v4` rather than at the host root. The `[HOST/]NAMESPACE/PROJECT` grammar has nowhere to put the prefix, and GitLab's own `glab` has the same open gap. Instances served at the host root — the default for every standard install — work.

Both forges use the same credential variable, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] — a GitHub personal access token or a GitLab personal access token, depending on which forge the run targets.

A run that produces no change — the rebuilt entry is byte-identical to the one already committed — makes no commit, and the report's `status` reads `unchanged` instead of `updated`. Running `announce` again for a package already announced from the same branch updates the existing pull request in place rather than opening a second one.

An unchanged run normally opens no pull request either. The one exception is a run whose announce branch still carries commits the index repository does not have: an earlier run's update reached the branch but never reached a pull request, so the unchanged run opens (or reuses) one and reports it, rather than leaving that work stranded.

`--out` is unaffected by all of that: it writes the whole entry every run, unchanged included, so `announce --out dir` followed by a step that consumes `dir` never sees an empty directory. Only `status` reports that nothing moved.

Every run also observes the package description published by [`ocx package describe`][cmd-package-describe]. When its artifact has moved since the last announce, the entry's description block is rebuilt — title, summary, keywords, and content-addressed copies of the README and logo — and the report's `desc_status` reads `updated`. An unmoved description costs one request and writes nothing. A description recorded in the index that the registry no longer serves stops the run rather than clearing it silently.

Publishing tags for a package that has no entry in the index yet is out of scope for `announce` — a first-time claim goes through a manual pull request against the index repository.

A tag that is not a version — the OCX-internal `__ocx` namespace, or a canonical `sha256.<hex>` tag from [`--canonical-tag`][cmd-package-push] — is dropped from the curated set rather than failing the run, and reported in the JSON report's `reserved_tags_dropped`. The one exception: `--tags-from-registry` filters a reserved tag out of its listing silently, before it reaches that report, since canonical tags are pushed by default and reporting one per published version would drown a real drop. A reserved tag already committed in the index root is still reported, from any mode. A curated set that resolves to nothing but reserved tags exits 64.

**Usage**

```shell
ocx package announce --package <NAMESPACE>/<NAME> (--tags <TAGS> | --tags-from-file <PATH> | --tags-from-registry | --refresh) [--out <DIRECTORY> | --fork <REPOSITORY>] [OPTIONS]
```

**Options**

| Name | Description | Default |
|------|-------------|---------|
| `--package <NAMESPACE>/<NAME>` | Package to announce, e.g. `acme/widget` (required). | — |
| `--tags <TAGS>` | Comma-separated tag list that replaces the currently-committed curated set. A committed tag not named here is dropped. Mutually exclusive with `--tags-from-file`/`--tags-from-registry`/`--refresh`; exactly one is required. | — |
| `--tags-from-file <PATH>` | Add the tags in this file (comma- or newline-separated) to the already-committed curated set. Never removes a committed tag. | — |
| `--tags-from-registry` | Add every tag the package's registry repository currently holds to the already-committed curated set. Never removes a committed tag; a yanked tag stays yanked. Reserved tags are filtered out of the listing before the union. | — |
| `--refresh` | Re-observe every already-committed tag, picking up a digest that moved (e.g. `latest`) without changing which tags are curated. | — |
| `--out <DIRECTORY>` | Write the rebuilt index entry under this directory instead of opening a pull request. Written on every run, including one that changes nothing. Mutually exclusive with `--fork`, and the one mode that needs no credential. | — |
| `--fork <REPOSITORY>` | Open (or update) the pull or merge request from this fork, as `[HOST/]NAMESPACE/PROJECT`. Omit it to push the announce branch straight to `--index-repo`, which needs push access on that repository. Requires [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. | — |
| `--index-repo <REPOSITORY>` | Index repository the pull or merge request targets, as `[HOST/]NAMESPACE/PROJECT`. Give the host for a self-hosted instance; the namespace may be a nested GitLab group path. | `ocx-sh/index` |
| `--forge <FORGE>` | Which forge hosts the index: `github` or `gitlab`. Inferred for `github.com` and `gitlab.com`; **required** for a self-hosted host. | inferred |
| `--yank <TAG>` | Mark a tag as yanked — a publisher signal that content should no longer be installed, not a delete. Repeatable. Requires `--yank-reason`; only applies to a tag already in the curated set. | — |
| `--unyank <TAG>` | Clear the yanked marker from a tag. Repeatable. | — |
| `--yank-reason <TEXT>` | Reason recorded on every tag named by `--yank` in this run. | — |
| `-h`, `--help` | Print help information. | — |

**Exit codes**

| Condition | Exit code |
|---|---|
| A curated tag's physical host resolves to a private, loopback, link-local, or metadata address — add it to that namespace's [`trusted_hosts`][config-registries-trusted-hosts] to allow | 78 |
| Any mode other than `--out` run without [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] set, the token was rejected (401/403), or — without `--fork` — the token cannot push to `--index-repo`. The last is checked before anything is written and names the repository and the missing permission | 80 |
| The physical registry could not be resolved (DNS failure), or the forge is unreachable or returned a 5xx | 69 |
| A curated tag does not resolve on the physical registry — check for a typo | 79 |
| The namespace is unclaimed — no committed root exists for the package yet. Claiming one is a human-lane action, never something announce performs | 79 |
| The forge rate-limited the run (429), or a concurrent announce kept winning the branch — retry | 75 |
| The curated set resolved to nothing but reserved tags — nothing left to announce | 64 |
| `--index-repo` names a self-hosted host and no `--forge` was given, or `--fork` names the namespace that already owns the index (fork it into itself — omit `--fork` instead) | 64 |
| `--fork` names a different host than `--index-repo`, or either coordinate's host is malformed. A fork lives on the same instance as its upstream, and a host that is not a hostname is refused rather than interpreted | 64 |
| `--index-repo` or `--fork` names a nested namespace on GitHub, which has no nested organizations. Checked before any request | 64 |
| The description recorded in the index no longer exists on the registry — republish it, or ask for it to be cleared in the index | 65 |

**JSON report**

```json
{
  "package": "acme/widget",
  "status": "updated",
  "pull_request_url": "https://github.com/ocx-sh/index/pull/42",
  "pull_request_number": 42,
  "fork": "forkuser/index",
  "desc_status": "updated",
  "written_paths": [],
  "reserved_tags_dropped": []
}
```

`status` and `desc_status` are each `unchanged` or `updated`; `desc_status` reports the package description separately from the tags. `pull_request_url`/`pull_request_number`/`fork` are always `null` for `--out`; otherwise `pull_request_url`/`pull_request_number` are `null` only when the run made no pull request, so an unchanged run that ensured one still reports it, and `fork` is `null` whenever `--fork` was not given. `written_paths` lists the files written under `--out` — the whole entry on every run, `unchanged` included — and stays empty in every mode that opens a pull request. `reserved_tags_dropped` names the tags this run dropped for not being a version — always an array, empty rather than absent — except a reserved tag `--tags-from-registry` observed straight from the registry listing, which never enters it (see above).

::: tip
[`ocx package push --announce-file`][cmd-package-push] appends the tag it just pushed (and any cascade tags) to a file in the same comma/newline format `--tags-from-file` reads, so a publish pipeline can feed one straight into the other:

```shell
ocx package push -i acme/widget:1.2.3 -c --announce-file tags.txt widget.tar.xz
ocx package announce --package acme/widget --tags-from-file tags.txt --fork myuser/index
```

Announce to a GitLab index, opening a merge request from a fork:

```shell
ocx package announce --package acme/widget --tags 1.0.0 \
  --index-repo gitlab.com/acme/index --fork gitlab.com/myuser/index
```

Announce to a self-managed GitLab in a nested group, from a fork in another group:

```shell
ocx package announce --package acme/widget --tags 1.0.0 --forge gitlab \
  --index-repo gitlab.example.com/acme/platform/tooling/index \
  --fork gitlab.example.com/contrib/team/index
```

Announce to a GitHub Enterprise Server index, pushing the branch to the index itself:

```shell
ocx package announce --package acme/widget --tags 1.0.0 --forge github \
  --index-repo github.example.com/acme/index
```
:::

#### `cascade check` {#package-cascade-check}

Diffs a package's cascade tag graph — the rolling aliases (`latest`, `3`, `3.28`, …) [`ocx package push --cascade`][cmd-package-push] maintains — against the state a fold over every published concrete version says it should be. Nothing re-checks the cascade graph at publish time if a cascade push is interrupted partway through (see [Cascades][in-depth-versioning-cascades]); `check` is how that drift gets found again after the fact, without republishing anything. It never writes to the registry or to any local index.

`check` accepts both **logical** and **physical** identifiers. A logical identifier (a namespace with an [`index`][config-registries-index] configured, e.g. `ocx.sh/kitware/cmake:3.28`) resolves to its physical registry location the same way [`ocx package install`][cmd-package-install] does, and additionally fetches that namespace's live [index][in-depth-indices] root — a **third finding layer**, index staleness, on top of the registry-graph findings every identifier gets, comparing each observed alias digest against the digest actually committed in the index. A physical identifier (a bare registry path, e.g. `ghcr.io/ocx-contrib/cmake:3.28`) skips that layer: there is no logical name to look an index root up under. A digest-pinned identifier (`pkg@sha256:…`) is a usage error — a digest names one immutable artifact, not a tag with alias ancestors to diff.

Each identifier's own tag selects how much of the graph a run covers:

| Identifier form | Scope |
|---|---|
| Tagless (`acme/cmake`) | Every alias tag in every variant track — `latest`, `3`, `3.28`, and any `debug`/other variant's aliases too |
| `:latest` (or any bare default-variant alias) | The default variant's track only — never `debug` or another variant's |
| A rolling tag (`:3.28`, `:debug-3`) | That tag's subtree plus the path from it up to its own root (`3.28` → `3` → `latest`) |
| A fully build-tagged leaf (`:3.28.1_20260216120000`) | The path up to root only — the leaf itself is the published source of truth and is never a write target |
| A bare variant name (`:debug`) | Usage error (exit 64) — whether `debug` names a track root depends on the package's other tags, which this call has not read; scope a tag under it instead (`:debug-3`) |
| A digest reference (`@sha256:…`) | Usage error (exit 64) — a digest has no tag graph to diff |

Multiple identifiers naming the same package union their scopes into a single report; different packages each get their own. `check` authenticates for **pull only** — it never probes push credentials.

**Usage**

```shell
ocx package cascade check <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more package identifiers, logical or physical, each optionally carrying a tag that narrows scope (see the table above). Required.

**Options**

- `-h`, `--help`: Print help information.

**Exit codes**

| Condition | Exit code |
|---|---|
| Every alias in scope matches the fold-expected state — nothing to report | 0 |
| At least one finding: a stale or missing alias entry, a duplicate entry shadowing another for the same platform, an orphaned tag, or (logical identifiers only) an index root behind the registry | 65 |
| A digest-pinned identifier, or any tag that does not name a node in the version graph — junk, a reserved or canonical `sha256.<hex>` tag, a bare variant name, or a version nothing published (`:9.99`) | 64 |

**JSON report**

```json
{
  "reports": [
    {
      "identifier": "acme/cmake",
      "logical": "ocx.sh/acme/cmake",
      "aliases": {
        "latest": { "state": "present" },
        "3": { "state": "present" },
        "3.28": { "state": "present" }
      },
      "rows": [
        {
          "tag": "3.28",
          "platform": { "architecture": "arm64", "os": "linux" },
          "status": "stale",
          "observed": "sha256:aaaa…",
          "expected": "sha256:bbbb…",
          "source": "3.28.1_20260216120000",
          "observed_source": null
        },
        {
          "tag": "3.28",
          "platform": { "architecture": "amd64", "os": "linux" },
          "status": "duplicate",
          "observed": "sha256:dddd…",
          "expected": "sha256:bbbb…",
          "source": "3.28.1_20260216120000",
          "observed_source": "3.28.0_20260101000000"
        }
      ],
      "index_findings": [
        { "finding": "stale", "tag": "3.28", "committed": "sha256:cccc…", "live": "sha256:bbbb…" }
      ],
      "ignored_tags": ["sha256.aaaa1111"],
      "unrepairable": []
    }
  ]
}
```

Every report is nested under one top-level `reports` array — the whole JSON contract is that one wrapper key. Each row's `status` is one of `ok`, `missing`, `stale`, `orphan`, or `duplicate`; `source`/`observed_source` name the published version a digest was folded from or recognized as belonging to, `null` when there is none. `duplicate` marks a platform for which the alias's index carries two descriptors: only the last one resolves, so the earlier entry is published but invisible to every consumer — `repair` collapses the pair back to one entry the same way it rebuilds any other stale slot. `index_findings` carries two shapes: a committed tag whose registry digest has moved past what the index still records (`stale`, shown above), and an alias tag observed live on the registry that the index has never committed at all (`{ "finding": "not-committed", "tag": "…" }`). `unrepairable` names aliases that need new content published before anything can fix them — `{ "reason": "child-manifest-missing", "tag": "…", "digest": "…" }` (a referenced manifest is gone), `{ "reason": "child-digest-unaddressable", "tag": "…", "digest": "…" }` (a digest algorithm this build cannot check), or `{ "reason": "would-empty-index", "tag": "…" }` (repairing it would leave the alias with no entries at all). Field names above are representative of the shipped report's shape, not a frozen wire contract — what a script branches on is `rows[].status` and the finding classes, not a specific key spelling.

#### `cascade repair` {#package-cascade-repair}

Recomputes and writes the whole alias index for every tag [`cascade check`](#package-cascade-check) would report as broken. `repair` writes by default — the same convention as every other `--dry-run` flag in this reference: `check` is already the read-only preview, so `repair` needs no separate opt-out to be safe to run. Pass `--dry-run` to compute and report the same plan without touching the registry.

Identifier forms and scope selection are identical to [`cascade check`](#package-cascade-check) — see the table there. An index-staleness finding on a logical identifier is never something `repair` writes: fixing the registry graph and re-publishing the index are different hops (see below), and `repair` only ever authenticates for registry push.

For each broken alias, `repair` rebuilds the whole platform index entry from the same fold `check` diffs against, preserving every observed entry the fold does not itself supersede — an [OCI annotation][oci-annotations] already on the index, a non-platform entry like an attestation, or an orphaned alias tag whose child manifest still exists on the registry. An orphan is preserved while its child is resolvable and dropped only once it provably is not: before writing, every referenced platform manifest is checked to still exist, and a missing one is dropped **only if every entry naming that digest is an orphan slot**. If the same digest also backs a slot the fold expects (a manifest shared across two platforms, a Rosetta-style alias) or an entry with no platform at all (an annotation or attestation), the **whole alias** is refused instead (reported, not silently skipped) rather than quietly losing content, while every other alias in the run still writes. Writes are batched — nothing reaches the registry until the whole run's plan is built — and proceed concurrently per tag. After each write, `repair` re-reads the tag it just wrote and warns (does not fail) if the digest disagrees with what was pushed, which is evidence of a concurrent publisher racing the same tag rather than something `repair` can safely resolve — the write itself still landed, so the outcome is reported `raced` only when nothing was written at all (the tag moved between this run's read and its write), never when the write landed but a read-back disagreed. There is no [conditional-request][mdn-if-match] guard on the write itself — avoid running `repair` against a repository with a publish in flight.

`repair` only ever touches the **registry** side of the tag graph — reaching the public [index][in-depth-indices] with the fix is a second, separate hop through [`ocx package announce`][cmd-package-announce]. `--announce-tags <PATH>` writes one bare alias-tag name per line, in the same comma/newline format [`--tags-from-file`][cmd-package-announce] reads, so a pipeline can chain the two directly:

```shell
ocx package cascade repair --announce-tags tags.txt acme/cmake
ocx package announce --package acme/cmake --tags-from-file tags.txt --fork myuser/index
```

The flag accepts **exactly one package per invocation** (usage error, exit 64, nothing written) — the follow-up `announce --package` names a single package, and a second package's tags landing in the same flat file would give it no way to tell whose they were. What a real run records is exactly the tags whose write **landed**: any alias a `raced`, `refused`, or `failed` outcome moved is left out, since announcing it would commit a digest this run never wrote. Landed tags are unioned with every tag an index-staleness finding names — the one class of drift a repair cannot close itself, announced even when the same run wrote nothing at all — so one file still covers both hops. `--dry-run` writes nothing to the registry, so its file records the whole computed plan instead — every tag it *would* repair — since that is the only content a preview has to report. Either way the file is written on every run, including one that changes nothing (an empty file) — a workflow can always feed it into `--tags-from-file` unconditionally, the same way [`ocx package push --announce-file`][cmd-package-push] chains into `announce`. `--tags-from-file`'s union semantics matter here: it never drops an already-committed tag, and it adds a tag that was never committed at all — an alias `repair` had to create from scratch — so one follow-up command covers a re-pointed alias and a brand-new one alike. When a run found index staleness on a logical identifier but had nothing of its own to repair, warming a particular machine's local copy is [`ocx index update`][cmd-index-update]'s job, not `repair`'s or `announce`'s — the report names that third hop when it applies.

**Usage**

```shell
ocx package cascade repair [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more package identifiers, logical or physical, each optionally carrying a tag that narrows scope (same rules as [`cascade check`](#package-cascade-check)). Required.

**Options**

| Name | Description | Default |
|---|---|---|
| `--dry-run` | Compute and report the repair plan without writing to the registry. | off |
| `--announce-tags <PATH>` | Write this run's alias-tag handoff to [`ocx package announce --tags-from-file`][cmd-package-announce], one bare tag per line. One package per invocation only — a second package's tags in the same file has no owner to attribute them to (usage error, exit 64, nothing written). A real run records the tags whose write landed, unioned with any tag an index-staleness finding names; `--dry-run` records its whole computed plan instead. Written on every run; empty when there is nothing to hand off. | — |
| `-h`, `--help` | Print help information. | — |

**Exit codes**

| Condition | Exit code |
|---|---|
| Every registry write this run attempted succeeded (an index-staleness finding from a logical identifier may remain — that is `announce`'s job, not a failure here) | 0 |
| At least one finding remains after the run — a write failed, an alias raced by a concurrent publisher before it could write (rerun the repair), or an alias could not be repaired without new content (its only remaining reference to a needed platform manifest is gone, or repairing it would leave the index empty) | 65 |
| `--dry-run` computed a non-empty plan — a preview that still names repairs is not a clean run, even though nothing was written | 65 |
| A digest-pinned identifier, or any tag that does not name a node in the version graph — junk, a reserved or canonical `sha256.<hex>` tag, a bare variant name, or a version nothing published (`:9.99`) | 64 |

**JSON report**

```json
{
  "entries": [
    {
      "report": {
        "identifier": "acme/cmake",
        "logical": "ocx.sh/acme/cmake",
        "aliases": {
          "latest": { "state": "present" },
          "3": { "state": "present" },
          "3.28": { "state": "present" }
        },
        "rows": [
          {
            "tag": "3.28",
            "platform": { "architecture": "arm64", "os": "linux" },
            "status": "stale",
            "observed": "sha256:aaaa…",
            "expected": "sha256:bbbb…",
            "source": "3.28.1_20260216120000",
            "observed_source": null
          }
        ],
        "index_findings": [],
        "ignored_tags": [],
        "unrepairable": []
      },
      "planned": [
        {
          "tag": "3.28",
          "index": {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
              {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:bbbb…",
                "size": 1234,
                "platform": { "architecture": "arm64", "os": "linux" }
              }
            ],
            "artifactType": "application/vnd.sh.ocx.package.v1"
          },
          "observed_digest": "sha256:aaaa…",
          "referenced_digests": ["sha256:bbbb…"],
          "reasons": [
            {
              "tag": "3.28",
              "platform": { "architecture": "arm64", "os": "linux" },
              "status": "stale",
              "observed": "sha256:aaaa…",
              "expected": "sha256:bbbb…",
              "source": "3.28.1_20260216120000",
              "observed_source": null
            }
          ]
        }
      ],
      "outcomes": [
        {
          "tag": "3.28",
          "outcome": {
            "outcome": "written",
            "digest": "sha256:bbbb…",
            "verified": true,
            "dropped": ["sha256:dead…"]
          }
        },
        {
          "tag": "3",
          "outcome": {
            "outcome": "raced",
            "expected": "sha256:eeee…",
            "live": "sha256:ffff…"
          }
        }
      ],
      "announce_tags": ["3.28"]
    }
  ],
  "dry_run": false,
  "announce_tags_path": "tags.txt"
}
```

`entries[].report` is the same [`cascade check`](#package-cascade-check) report shape for this package — findings this run is repairing, not a separate schema. `planned` carries the whole replacement index computed for each broken alias, `reasons` echoing the exact rows that justified it; `outcomes` is empty for a preview (`dry_run: true`), since nothing was attempted. `written`'s `verified` is `false` when this run's post-write read-back found a different digest than it just pushed — evidence of a concurrent publisher, not a failure: the write still landed, and the plain-text table shows it as `written-unverified` rather than a distinct JSON outcome. `dropped` names the dead orphan-only digests this write removed before it went on the wire — omitted, never an empty array, when nothing was dropped. `raced` means a concurrent publisher moved the tag between this run's read and its write, so nothing was written for it (`expected`/`live` are each `null` when that side of the race never held the tag); rerunning the repair re-reads the new state. A `refused` outcome's `outcome` object nests the same tagged shape as `unrepairable` above (`{ "outcome": "refused", "reason": "child-manifest-missing", "tag": "…", "digest": "…" }`, and so on for the other two reasons) — this alias was never attempted; a `failed` outcome's is `{ "outcome": "failed", "message": "…" }`, a write the registry itself rejected. `entries[].announce_tags` is this package's contribution to the top-level `--announce-tags` file — `3.28` here, not `3`, because the raced write never landed. `announce_tags_path` is the path `--announce-tags` was given, `null` when the flag was not passed. Representative shape, as with `cascade check` above — the shipped report's exact key spelling is not a frozen contract, only the finding classes a script branches on.

#### `create` {#package-create}

Bundles a local directory into a compressed package archive ready for publishing. If the package
metadata includes [dependencies][ug-dependencies], the declared dependency graph is
validated for cycles at this stage — catching errors before the package reaches the registry.

When `--metadata` is given, `create` is also the compiler for [dependency pins][reference-manifest-pins]:
it validates the sidecar and always rewrites it, in canonical form, next to the output bundle — never a
byte copy of the input file.

`--platform` is **required** whenever `--metadata` is given (else usage error, exit 64). It declares the
platform the packaged content runs on. That answer cannot come from the build host: the host describes
what the build machine *supplies*, while `--platform` states what the artifact *demands* — a static musl
binary cross-built on a glibc host demands neither the host's libc nor its architecture. Every dependency
is pinned for the platform you name, and whatever you name is the label the package is published under.

The platform never enters the metadata sidecar itself — `create` writes it instead to a build receipt
(`<stem>-receipt.json`) next to the output bundle, a build artifact with no schema that is never pushed
to a registry. [`ocx package push`][cmd-package-push] and [`ocx package test`][cmd-package-test] read the
receipt back for a `--platform` you did not give them — see [Build receipt](#package-create-receipt)
below.

- `--platform <PLATFORM>` (a concrete platform): each dependency without a digest is resolved
  against the selected index to the one manifest [compatible][reference-platforms-compatibility]
  with `<PLATFORM>`. Zero compatible candidates fails with exit 65 (lists what is available); more
  than one is ambiguous (exit 65). The sidecar is rewritten with the resolved digest pinned
  directly on each dependency's identifier.
- `--platform any`: every unpinned dependency must itself offer an `any` manifest — an `any`
  requirement is satisfied only by an `any` offer, so a dependency with no `any` build fails
  `create` (exit 65), naming it. The resolved digest is pinned bare on the identifier — the same
  single-pin shape a concrete platform gets. A leaf manifest carries no platform descriptor of its
  own, so [`ocx package push`][cmd-package-push] later re-verifies the pin against the dependency's
  own image index rather than trusting the sidecar's word for it (see [Multi-Platform
  Packages][authoring-multi-platform]). `create` also rejects a direct digest pin anywhere in an
  `any`-targeted bundle's dependency list, including one already present before `create` ran (exit
  65) — it resolves against an index and has no registry evidence to verify a pin it did not
  resolve itself.

Resolution honors [`--remote`][arg-remote], [`--offline`][arg-offline], and [`--frozen`][arg-frozen]
exactly like every other tag resolution: the default checks the local index first and fetches on a
miss; `--offline`/`--frozen` refuse to resolve a dependency tag not already cached (exit 81); a
dependency tag absent from the selected index fails with exit 79. See
[Resolving Dependency Pins][authoring-building-pushing-dependency-pins] for the full workflow and the
[Dependencies reference][reference-dependencies] for the sidecar field shapes.

`create` is also the compiler for the [`binaries`][reference-binaries] claim: `--bin-scan` /
`--no-bin-scan` control whether it scans the content tree for executables the package puts on
`PATH` to fill or verify that field. `--bin-scan` and `--no-bin-scan` are a paired, last-wins flag (same
shape as every other `--X`/`--no-X` pair in this reference) with a **tri-state** resolution —
neither flag given is its own mode, not simply "default off":

| Mode | `binaries` absent in the sidecar | `binaries` declared (including `[]`) |
|---|---|---|
| **Auto** (neither flag — default) | Scans; writes the discovered names into the resolved sidecar. | Scan not run; the declared list passes through verbatim. |
| `--bin-scan` | Same as Auto — verification needs a declaration to check against. | Scans; a discovered executable missing from the declared list fails with exit 65 (`UndeclaredBinary`); a declared name present on disk but not executable fails with exit 65 (`DeclaredNotExecutable`); a declared name simply absent from disk is legal. On success the declared list passes through verbatim. |
| `--no-bin-scan` | No scan; the field stays absent. | No scan; the declared list passes through verbatim. |

`--bin-scan` requires `--metadata`/`-m` — a usage error otherwise (exit 64): the flag exists to
verify a declaration, and there is nothing to verify without a metadata sidecar. `--no-bin-scan`
needs no sidecar either way, since it never scans.

Filling or verifying the field needs a scan, and a scan needs a host that can evaluate the
**target platform's** executable-file convention. The Windows extension allowlist is pure string
matching — any host can apply it — but the Unix exec-bit convention can only be read on a Unix
host. In practice that means a Windows host targeting anything but Windows: `linux/*`, `darwin/*`
and `--platform any` alike — `any` names no native OS convention of its own, so it is scanned by
the Unix exec bit like the rest. There, every mode that would have scanned fails with exit 65
(`UnsupportedHostScan`) — `--bin-scan`, and the Auto default with `binaries` absent. A host that
cannot check the claim says so rather than publish an unchecked one quietly. Hand-author
`binaries`, or pass `--no-bin-scan` to declare the gap deliberately; the error names both. A Unix
host is unaffected in every mode and for every target.

An unreadable scan-target directory (e.g. permission denied) fails with exit 74
([`IoError`][exit-codes]) rather than silently producing an empty list; only a target directory
that does not exist yields zero candidates.

The scan only ever fills the **resolved sidecar** written next to `-o` — the same rail
[dependency pins][reference-dependencies-authoring] already ride from `create` to `push`; the
authored `-m` input file is never rewritten. It only considers `${installPath}`- or
`${self.installPath}`-rooted (the two are [exact aliases][reference-env-self-alias])
[`path`][reference-env-path] variables carrying [interface visibility][reference-env-visibility]
with no [render modifier][reference-env-render] — a `path` value combined with a `${deps.*}`
segment, or carrying a `:native`/`:posix` modifier, is out of scan scope entirely and is
**silently** excluded from the auto-filled claim, with no diagnostic here; a foreign or reused
layer added later at `push` time was never part of the content tree `create` scanned either. All
three cases need the publisher to hand-author `binaries` instead. See
[Executables][reference-binaries] for the full field semantics, including why a modifier-bearing
interface `PATH` value fails the [libc lint](#package-create-libc-check) on Linux and `any` — the
one place this same exclusion does surface a diagnostic.

##### Build receipt {#package-create-receipt}

`create` writes a second sidecar next to the bundle: `<stem>-receipt.json`, holding
`{"version": 1, "platform": "...", "identifier": "..."}` — both fields optional, each present only
if you gave `create` the matching flag. It is a build artifact, not package metadata: it has no
JSON Schema, is never uploaded to a registry, and exists only to carry what one local build was
told to the two commands that consume its output.

`create` writes it whenever it has something to record — with or without `--metadata`. Give
neither `--platform` nor `--identifier` and no receipt is written, because there is nothing to put
in one.

The receipt is a **fallback, never an authority**. Per value:

| Flag on [`push`][cmd-package-push] / [`test`][cmd-package-test] | Receipt | Result |
|---|---|---|
| given | anything | the value you gave, in silence — the receipt is not consulted for it |
| omitted | records the value | the recorded value |
| omitted | records nothing (or no receipt at all) | usage error (exit 64) — nothing determines the value |

`push` resolves both `--platform` and `--identifier` this way; `test` resolves `--platform` (its
`--identifier` names the local test subject and stays required). One finer gap the receipt also
fills: `push --identifier repo` **without a tag** takes the version the receipt recorded, when the
receipt names the same repository — the flag picks where to publish, the recorded build says which
version it was. A receipt about a different repository contributes nothing and the ordinary
`latest` default applies. The file is opened only when something is missing, so an invocation that
states everything never touches it.

##### Checking the declared libc {#package-create-libc-check}

Whenever `--metadata` is given and the target is a Linux one or `any`, `create` also checks the
[`os.features`][reference-platforms] the
`--platform` value declares against what the packaged binaries actually need. Under subset
matching an **empty** feature list is a positive claim that the artifact demands nothing of the
host — so a glibc-linked binary published without `libc.glibc` resolves on a musl-only host and
then fails to start with a bare `No such file or directory`, the kernel reporting a missing ELF
interpreter for a file that is plainly there.

The check reads the ELF `PT_INTERP` header of every file the package puts on an interface
`PATH` directory — the same scan scope as `--bin-scan`, but every regular file rather than only
the executable ones, since the libc a file needs is a fact about its bytes. It is not gated on
`--bin-scan`: that flag governs the `binaries` claim, this governs the `os.features` claim.

| Condition | Result |
|---|---|
| Statically linked (no `PT_INTERP`) | Needs no declaration |
| Needs a libc the declared platform requires | Passes |
| Needs a libc the declared platform does not require | Exit 65; the message names the file, the loader, and a paste-ready `--platform` value |
| Dynamically linked, but the platform is `any` | Exit 65 — `any` claims every host can run it |
| Carries an ELF header but will not parse, or names an unrecognised loader | Exit 65 — an undeterminable requirement is never treated as "needs nothing" |
| Not an ELF object (scripts, data, docs) | Not a subject of the check |

The check runs before the archive is written, so a refusal leaves no bundle on disk.

It reads only the dynamic loader. A binary that needs `libicu`, `libstdc++` or any other shared
library still passes, as does one built against a newer glibc than the host provides —
`os.features` carries libc *family*, not version. Targets other than Linux are not checked: macOS
has a single C library, and OCX defines no `libc.*` feature for the Windows CRTs.

`--no-libc-lint` skips the check entirely, including its scan-scope refusal. It is an escape
hatch rather than a convenience: the check reads bytes off disk, and a wrong answer from it would
otherwise block every `create` for a Linux target with no way through. Skipping it leaves the
declared `os.features` unverified — an artifact this section would refuse can then be published,
and it will resolve on hosts that cannot execute it — so a warning naming the declared platform is
printed to stderr. The warning follows the check's own scope — `--metadata` with a Linux target or
`--platform any`. Anywhere the check never inspects (a bare `create` with no sidecar, or a
non-Linux *concrete* target) the flag suppresses nothing, so nothing is said. Passing it on every
leg of a per-platform matrix therefore stays quiet except where it actually skipped something.
Nothing else changes: the same metadata and the same layers are written either
way.

**Usage**

```shell
ocx package create [OPTIONS] <PATH>
```

**Arguments**

- `<PATH>`: Path to the directory to bundle.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier the bundle will be published under. Used to infer the output filename when `--output` is a directory, and recorded in the [build receipt](#package-create-receipt) for [`ocx package push`][cmd-package-push] to fall back to.
- `-p`, `--platform <PLATFORM>`: Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content) — see [Platforms][reference-platforms] for the grammar. Required whenever `--metadata` is given, with no host default (see above); optional otherwise, where it only shapes the inferred output filename. With `--metadata`, a Linux target or `any` also has its `os.features` checked against the packaged binaries — see [Checking the declared libc](#package-create-libc-check).
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform. The file extension controls the compression algorithm: `.tar.xz` (LZMA, default), `.tar.gz` (Gzip), or `.tar.zst` (Zstandard).
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to a `metadata.json` sidecar to validate, resolve, and write alongside the output bundle. Requires `--platform` (see above); dependencies without a digest are pinned to that platform's manifest digests, and the resolved sidecar is written next to the output bundle in canonical form. If omitted, no metadata sidecar is written; the [build receipt](#package-create-receipt) is written either way, since it records the invocation rather than the sidecar.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`. Applies to whichever algorithm is selected.
- `-j`, `--threads <N>`: Number of compression threads. `0` (default) auto-detects from available CPU cores (capped at 16). `1` forces single-threaded compression. Affects LZMA (`.tar.xz`) and Zstandard (`.tar.zst`) compression; Gzip is always single-threaded.
- `--bin-scan`, `--no-bin-scan`: Scan the content tree for executables the package puts on `PATH` to fill or verify the [`binaries`][reference-binaries] metadata claim — see the mode table above. Paired, last-wins flags; neither given (the default) fills an absent claim and passes a declared one through untouched.
- `--no-libc-lint`: Skip the libc check on the packaged binaries — see [Checking the declared libc](#package-create-libc-check). The escape hatch for a false refusal: the declared `os.features` then go unverified and a warning naming the platform is printed wherever the check would have run, but nothing about what gets written changes.
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

`push` makes no dependency-resolution decisions — it is a gate. If the metadata sidecar declares [dependencies][ug-dependencies], every one of them must already carry a manifest digest pin for the platform this invocation publishes ([`ocx package create`][cmd-package-create] is what resolves them; see [Resolving Dependency Pins][authoring-building-pushing-dependency-pins]). `push` fails before uploading anything if:

| Condition | Exit code |
|---|---|
| No `--platform`, and no platform in the build receipt beside the bundle (see [Build receipt](#package-create-receipt)) | 64 |
| No `--identifier`, and no identifier in the build receipt | 64 |
| A dependency is not digest-pinned | 65 |
| A dependency's pin resolves to an OCI Image Index instead of a manifest | 65 |
| A dependency of an `any`-targeted push pins a digest the dependency's own image index does not advertise as `any` | 65 |
| A dependency's pinned manifest does not exist in its registry | 79 |
| Authentication to a dependency's registry fails | 80 |

**Usage**

```shell
ocx package push [OPTIONS] <LAYERS>...
```

**Arguments**

- `<LAYERS>...`: Zero or more layers, in order (base layer first, top layer last). Each layer is either:
  - a path to a pre-built archive file (`.tar.gz`, `.tgz`, `.tar.xz`, `.txz`, `.tar.zst`, `.tzst`, or `.tar.zstd`), or
  - a digest reference of the form `sha256:<hex>.<ext>` (e.g. `sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08.tar.gz`) pointing at a layer that already exists in the target registry. The `<ext>` suffix is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. Bare digests are rejected.
  - Extension aliases: `.tgz` is accepted as an alias for `.tar.gz`, `.txz` for `.tar.xz`, and `.tzst` / `.tar.zstd` for `.tar.zst`. The canonical forms `tar.gz` / `tar.xz` / `tar.zst` are what ocx emits internally — aliases are normalized on parse.
  - To force file interpretation of a pathological filename that happens to match the digest shape, prefix it with `./` (e.g. `./sha256:abc….tar.gz`).
  - Omitting all layers produces a config-only OCI artifact with `layers: []`, valid for referrer-only / description-only manifests. `--metadata` is required in that case.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier including the tag, e.g. `kitware/cmake:3.28.1_20260216120000`. Omit it to publish under the identifier the [build receipt](#package-create-receipt) beside the bundle recorded; with neither, exit 64.
- `-p`, `--platform <PLATFORM>`: Target platform to publish — see [Platforms][reference-platforms] for the grammar. Single-valued: passing more than one exits 64. Omit it to publish for the platform the [build receipt](#package-create-receipt) beside the bundle recorded; a value given here is used as given, and the receipt is not consulted for it. With neither, exit 64. Every dependency is projected for this platform (see the gate table above).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `kitware/cmake:3.28.1_20260216120000` automatically re-points the rolling ancestors (`kitware/cmake:3.28.1`, `kitware/cmake:3.28`, `kitware/cmake:3`, and `kitware/cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided (all layers are digest references, or the layer list is empty).
- `--build-timestamp [<FORMAT>]`: Append a UTC build-metadata segment to the published tag. `datetime` (default when flag passed bare) appends `_YYYYMMDDhhmmss`, `date` appends `_YYYYMMDD`, `none` is a no-op. The identifier's tag must already be `X.Y.Z` (optionally with a variant prefix or pre-release suffix) and must not already carry build metadata. Use this in continuous-deploy pipelines that publish rolling pre-release versions like `dev.ocx.sh/ocx/cli:0.3.0-dev_20260514120000`. The wire-format tag uses `_` (OCI tags forbid `+`); semver `+` is accepted on input and normalized. When the flag is omitted entirely, no build-metadata segment is appended. Passing `--build-timestamp=none` is the explicit equivalent.
- `--canonical-tag` / `--no-canonical-tag`: `--canonical-tag` (default) also pushes a digest-named `sha256.<hex>` tag for each platform manifest pushed in this invocation; `--no-canonical-tag` skips it. This is a pure registry-side deletion safety net — a stray tag delete cannot orphan a digest still referenced by a lock, since the canonical tag itself keeps the manifest reachable. It has no effect on [`index.ocx.sh`][in-depth-indices-public] resolution, which ignores canonical tags entirely.
- `--announce-file <PATH>`: After a successful push, append the pushed tag and any cascade tags to this file (creating it if absent), so [`ocx package announce --tags-from-file`][cmd-package-announce] can pick them up. This is a scratch file for one pipeline run, not a persistent list — a stale file left over from an earlier run could re-add a tag that was deliberately dropped from a later announce.
- `--annotation <KEY=VALUE>`: Record an [OCI annotation][oci-annotations] on the published [image index][oci-image-index]. Repeatable; see [Annotations](#package-push-annotations) below.
- `--sbom <PATH>`: Attach the file at `PATH` as a CycloneDX SBOM on the manifest this push just wrote — sugar for running [`ocx package attest --type cyclonedx`][cmd-package-attest] against the pushed digest immediately afterward, including its polarity: a signing identity visible in the environment (an identity-token override, or an ambient CI platform) means a signed [DSSE][dsse] attestation; nothing visible means the SBOM is attached raw, typed by its own media type, with no signature at all. See [Attestations][ug-attestations-attach] for when each shape applies. The predicate is read and every offline/policy check runs *before* the push itself; a refusal there means nothing is uploaded. A failure *after* the push (Fulcio, Rekor, or the referrer write) does not roll the push back — the push report is printed first, and only then does the attest failure become the process's exit code. See [`attest`][cmd-package-attest] for the predicate-type vocabulary, the identity-token precedence, and the size limit.
- `-h`, `--help`: Print help information.

::: tip Layer reuse
Digest-referenced layers are not re-uploaded — ocx only HEADs the registry to verify they exist. This is the foundation of the [layer dedup model](../user-guide.md#file-structure-layers): a base layer pushed once can be referenced from any number of subsequent packages by digest.

```shell
# Push a fresh base + tool combination
ocx package push -p linux/amd64 -i acme/mytool:1.0.0 base.tar.gz tool.tar.gz

# Reuse the same base by digest in a later release.
# The digest is the full 64-char sha256 hex written verbatim —
# the ellipsis is shown here only to keep the example short.
ocx package push -p linux/amd64 -i acme/mytool:1.0.1 sha256:<hex>.tar.gz newtool.tar.gz
```
:::

::: warning Bring your own archives
`ocx package push` does not bundle a directory for you. Each file layer must be a pre-built archive. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy) and defeats layer reuse — use [`ocx package create`](#package-create) to produce a stable archive once, then push and reference it by digest from later commands.
:::

#### Annotations {#package-push-annotations}

`--annotation KEY=VALUE` records an [OCI annotation][oci-annotations] on the [image index][oci-image-index] of every tag the push writes — the primary tag and, under `--cascade`, each rolling tag it re-points. The flag is repeatable, splits at the first `=` (so values may contain `=`), and keeps the last value for a repeated key. A key that is empty, or an argument with no `=` at all, is a usage error (exit 64).

Values are written verbatim: OCX never derives an annotation from the environment or from the repository path. Omitting the flag writes nothing and leaves whatever the index already carries untouched, so an earlier annotation survives a later plain push. Supplying a key overwrites that one key and leaves the rest of the index's annotations alone.

The annotation that matters in practice is `org.opencontainers.image.source`. It is the documented mechanism by which a registry links a package back to its source repository — on [GHCR][ghcr-repo-link] this is what produces the repository link on the package page and what makes the package inherit that repository's permissions. Registries derive nothing from the repository path, so a package published to `ghcr.io/acme/tools/widget` links to nothing until the annotation says otherwise:

```shell
# In GitHub Actions, the runner already knows the answer.
ocx package push -c -p linux/amd64 -i ghcr.io/acme/tools/widget:1.2.3 \
  --annotation org.opencontainers.image.source=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY \
  widget-1.2.3-linux-amd64.tar.xz
```

Any other annotation key works the same way — `org.opencontainers.image.revision` for the commit that produced the build, `org.opencontainers.image.licenses` for the SPDX expression. The [OCI annotation spec][oci-annotations] lists the pre-defined keys and the reverse-domain convention for custom ones; OCX does not validate keys beyond rejecting an empty one.

::: tip Catalog display lives elsewhere
Title, description, and keywords shown in a catalog come from [`ocx package describe`](#package-describe), which publishes them on the separate `__ocx.desc` tag. `--annotation` is for facts about a *published build* that a registry reads off the index itself.
:::

#### Layer layout {#package-push-layout}

Any layer argument to [`push`](#package-push) or [`test`](#package-test) accepts an optional `:strip=N,prefix=P,from=REPO` suffix that overrides [`strip_components`][metadata-strip-components] for that one layer and/or requests cross-repository blob reuse:

```
<ref>:strip=<N>,prefix=<P>,from=<REPO>
```

All three fields are optional, input order is free, and each may appear on its own or combined — `prefix=share,strip=1` parses the same as `strip=1,prefix=share`. `<N>` is a `u8`. `<P>` is a relative, non-escaping path — no leading `/`, no `..`, no Windows drive or UNC prefix, bounded to 32 components / 4096 bytes total — under which the layer's post-strip tree is placed in the assembled `content/` directory instead of at the package root. `<REPO>` names a repository in the same registry (lowercase letters, digits, `.`, `_`, `/`, `-`; no leading or trailing `/`) that `push` attempts a registry-side blob mount from before falling back to a normal upload — useful for reusing a layer already pushed elsewhere without re-uploading its bytes. A registry may decline a mount for any reason, and most decline unless the credential carries pull scope on the source repository; a decline is not an error. For a file layer the fallback simply uploads the bytes, so `from=` is a pure optimization. For a `sha256:<hex>.<ext>` digest layer there are no local bytes to fall back on, so a declined mount fails the push unless the blob already exists in the target repository. The JSON push report's `layers` object (`{mounted, uploaded, verified}`) records how each layer was resolved; run with `-l debug` to see individual declines.

```shell
# base.tar.gz ships wrapped in a `1.2.3/` directory; strip it and relocate the
# remainder under `share/`. tool.tar.gz keeps the default (root, no strip).
ocx package push -p linux/amd64 -i acme/mytool:1.0.0 \
  base.tar.gz:strip=1,prefix=share \
  tool.tar.gz
```

The resolved values are carried in the manifest layer descriptor's `annotations` as `sh.ocx.layer.strip-components` and `sh.ocx.layer.prefix` — never in `metadata.json`. Each field falls back independently at read time: an explicit annotation wins; a missing `strip` annotation falls back to the package-wide `strip_components` from [metadata][metadata-strip-components], then to `0`; a missing `prefix` annotation falls back to the package root. A push with no `:strip=`/`:prefix=` on any layer writes no `sh.ocx.layer.*` annotations at all, so the manifest stays byte-identical to a pre-layout publish.

**Constraints**

- Layers are a flat merge, not an overlay stack. OCI whiteout entries (`.wh.*`, `.wh..wh..opq`) — e.g. inside a foreign layer reused by digest from a Docker/BuildKit build — pass through as ordinary files; OCX never interprets them as deletions.
- A deep `prefix` combined with a deep layer tree can approach the legacy Windows `MAX_PATH` limit. Keep `prefix` shallow for packages that install on Windows.
- `<P>` cannot contain a comma — the layout suffix is a comma-separated `key=value` list with no escaping. This constrains the (typically short) prefix value itself, not the file paths inside the layer.
- A layer filename that literally contains `:strip=`, `:prefix=`, or `:from=` cannot be pushed as a plain path — there is no escape hatch, unlike the `./` disambiguation used above for digest-shaped filenames. Avoid such filenames.
- `from=REPO` is push-only — it is never carried into the manifest or its annotations, unlike `strip`/`prefix`.

Malformed layout syntax at publish (`strip` not a `u8`, an unknown key, a duplicate key, an empty value, an escaping/oversized `prefix`, or a `from` value outside `[a-z0-9._/-]` or with a leading/trailing `/`) is a CLI usage error (exit 64). The same `prefix` bound is re-validated when a layer is read back — manifests are third-party-writable — and a malformed annotation there is a data error (exit 65).

#### `test` {#package-test}

Materializes a package locally without a registry round-trip and runs a command or script in its composed env. Mirrors the argument shape of [`package push`][cmd-package-push]: identifier as `-i/--identifier`, then layers, then `--platform`. Either a trailing `-- CMD [ARGS...]` or a `--script PATH` is required; the two forms are mutually exclusive.

::: warning Commands resolve against the package first
A bare command name is looked up in the package's own directories before the host `PATH`, and the package's copy of a name is never skipped:

- **The package ships an executable of that name** — it runs. This is the ordinary case.
- **The package ships the name but the file is not executable** — the command fails with exit code 65, naming the path and its permission bits. It does *not* fall back to a same-named binary on the host, because that would test something the package does not contain and report a pass.
- **The package does not ship the name at all** — it resolves on the host `PATH` as usual (`sh`, `grep`, and friends keep working), with a warning naming the directories that were searched.

A name carrying a path separator (`./tool`, an absolute path) addresses a file directly and is unaffected.

Catch a missing executable bit at publish time instead with [`ocx package create --bin-scan`][cmd-package-create].
:::

**Usage**

```shell
# Trailing-command form
ocx package test [OPTIONS] --identifier <IDENTIFIER> [LAYERS]... -- <CMD> [ARGS]...

# Script form
ocx package test [OPTIONS] --identifier <IDENTIFIER> [LAYERS]... --script <PATH|->
```

**Arguments**

- `<LAYERS>...`: Zero or more layers, in order (base first, top last). Same syntax as `package push`, including the optional [`:strip=N,prefix=P,from=REPO` layout suffix][cmd-package-push-layout]: a path to a `.tar.gz`/`.tar.xz`/`.tar.zst` archive, or a `sha256:<hex>.<ext>` digest reference to a layer already in the registry. Digest refs are fetched on demand; missing digest blobs when a local policy (`--offline` or `--frozen`) is active produce exit code 81 (`PolicyBlocked`). `from=REPO` is meaningless for `test` (there is no registry push) and is simply ignored if present.
- `-- <CMD> [ARGS]...`: Command to run inside the composed env. Required unless `--script` is given.

**Options**

| Name | Short | Description | Default |
|------|-------|-------------|---------|
| `--identifier <IDENTIFIER>` | `-i` | Package identifier in tag form (`repo:tag`) — required. An explicit `@digest` suffix is rejected (the digest is computed locally from the supplied layers). | — |
| `--platform <PLATFORM>` | `-p` | Target platform. Omit it to take the platform the [build receipt](#package-create-receipt) beside the bundle recorded; a value given here is used as given, and the receipt is not consulted for it. With neither, exit 64. | build receipt |
| `--script <PATH\|->` | — | Path to a [Starlark][starlark-lang] test script, or `-` to read the script source from stdin. Mutually exclusive with the trailing `-- CMD` form. | — |
| `--metadata <PATH>` | `-m` | Path to the metadata JSON file. Defaults to a sibling of the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided. | auto-detected |
| `--keep` | — | Preserve the temp build directory after the command exits. Path is printed to stderr. Default temp root is `$OCX_HOME/temp/test/`. Mutually exclusive with `--output`. | false |
| `--output <DIR>` | `-o` | Materialize into `DIR` instead of an auto-managed temp dir. `DIR` must not exist or must be empty. Implies keep. Must reside on the same filesystem as `$OCX_HOME/layers/`. On Windows, must point under `$OCX_HOME/`. Mutually exclusive with `--keep`. | — |
| `--self` | — | Compose the package's private env surface (default: interface surface). Same semantics as [`ocx package exec --self`][cmd-exec-self]. | false |
| `--clean` | — | Strip ambient parent env before composing — only `OCX_*` config and composed package vars reach the child. Mirrors [`ocx package exec --clean`][cmd-exec-clean]. | false |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` -> `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends); `SEP` qualifies `list` only (`--env GODEBUG:list:,=gctrace=1`) and, if omitted, inherits whatever separator another contributor to the key already declared, or a single space if none did. A relative `path` value resolves against the **current directory**. Applied last, so it overrides every package-declared variable. This is a per-invocation override, not project configuration -- it does **not** make this command read `ocx.toml`. A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). See the `PATH` override warning under [`ocx run`](#run). | — |
| `--help` | `-h` | Print help information. | — |

**Examples**

```shell
# Run the binary in its composed env (trailing-command form).
ocx package test -p linux/amd64 -i acme/mytool:1.0.0 mytool.tar.xz -- mytool --version

# Run a Starlark test script against the package.
ocx package test -p linux/amd64 -i acme/mytool:1.0.0 mytool.tar.xz --script smoke.star

# Read a Starlark script from stdin.
printf 'r = ocx.run("mytool", "--version")\nexpect.ok(r)\n' \
  | ocx package test -p linux/amd64 -i acme/mytool:1.0.0 mytool.tar.xz --script -

# Keep the temp dir for inspection on failure.
ocx package test -p linux/amd64 --keep -i acme/mytool:1.0.0 mytool.tar.xz -- mytool --version

# Materialize to a named directory.
ocx package test -p linux/amd64 --output ./build -i acme/mytool:1.0.0 mytool.tar.xz -- mytool --version

# Explicit metadata path + digest base layer.
ocx package test -p linux/amd64 -m metadata.json -i acme/mytool:1.0.1 \
  sha256:<hex>.tar.xz ./newtool.tar.xz -- mytool --version
```

::: tip Tempdir lifecycle
Without `--keep` or `--output`, the temp directory is deleted on any exit — success or failure. Use `--keep` to opt in to preservation on failure. Re-run with `--keep` to inspect.
:::

**Exit codes — trailing-command branch**

The child's own exit code propagates verbatim, so any value can appear. Two codes are produced by `ocx` itself before the child starts: 64 for a usage error, and 65 either when the package ships the named command but the file is not executable (see the warning above) or when composing the environment finds two contributors to one key declaring conflicting list separators (see [Separator agreement][env-composition-list-separator]).

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
- **`--closure`**: walks the declared [dependencies][reference-dependencies] to compute the metadata-only dependency **closure** — every package reachable from the reference, read from cached or fetched metadata alone, without installing anything — plus two **surface** projections: `interface` (what would land on `PATH` for a consumer installing the reference) and `private` (what the reference's own runtime sees). On an image-index reference, `--closure` platform-selects first (honoring `-p`/`--platform`, the host platform otherwise) exactly like `--resolve` does, because the walk needs a concrete manifest to read declared dependencies from — `--closure` alone therefore never returns the metadata-less candidates listing.

Unlike [`package test`][cmd-package-test], the identifier accepts an explicit `@digest` (a tag or digest both resolve).

`--closure` fails closed: if any dependency's manifest or metadata can't be loaded, the whole request fails rather than rendering a partial closure (see **Exit codes** below). A script deciding whether `--closure` ran should check for the `closure` key in JSON output, not for `resolution` — `resolution` reflects the shape of the reference (whether it needed platform selection), not whether `--closure` was requested.

**Usage**

```shell
ocx package inspect [OPTIONS] <IDENTIFIER>...
```

**Arguments**

- `<IDENTIFIER>...`: One or more package identifiers to inspect. Each is a tag (`repo:tag`) or `@digest`.

JSON output is one envelope — `{ platform?, packages, env }` — whose `packages` array carries one entry per requested identifier, in request order, each naming itself in its `name` field. Plain output renders each package's tree in the same order.

**Options**

- `-p`, `--platform <PLATFORM>`: Platform to select. Applies with `--resolve` and `--closure`; ignored in default mode (the candidate list always shows every platform).
- `--resolve`: Platform-select through the index and emit the resolution chain — the pinned identifier and the walk-order chain blob descriptors (index → platform manifest → config blob, each with its `role`, media type, and size) — alongside the metadata and layers (the layers are shown for the selected manifest in both default and `--resolve` mode).
- `--closure`: Compute the metadata-only dependency closure without installing. Adds one `closure` object to JSON output — `deps` (the transitive dependencies, in transitive-closure order) and `surface` (the `interface` and `private` projections) — and a matching `closure` branch to the plain-text tree. Combining `--closure` with `--resolve` on an image-index reference is redundant but harmless — the platform selection `--closure` already performs is the same one `--resolve` performs. A non-empty `closure.conflicts` exits 65 while still reporting the conflict.
- `--env <KEY[:TYPE[:SEP]]=VALUE>`: Set an environment variable for this invocation, surfaced in the report's `env` array. Repeatable. Per-invocation only — this command still reads no `ocx.toml`; for a project's declared environment use [`ocx inspect`](#inspect).
- `-h`, `--help`: Print help information.

Honors the global [`--offline`][arg-offline], [`--remote`][arg-remote], and [`--format`][arg-format] flags. JSON is the primary consumer surface.

**JSON shape**

The top level is one envelope, shared verbatim with [`ocx inspect`](#inspect):

```json
{
  "platform": "linux/amd64",
  "packages": [ { "name": "mytool:1.0.0", "…": "…" } ],
  "env": []
}
```

- `platform` — the platform the run selected. Present only with `--resolve` / `--closure`: default mode selects none, so `-p` stays inert there and no platform is reported.
- `packages` — one entry per requested identifier, **in request order**. An array rather than an object keyed by identifier, because order is part of the contract and JSON object key order is not. Each entry's `name` is the identifier exactly as it was requested.
- `env` — the per-invocation `--env` overrides, in flag order. Always present, empty when none were passed. Package-declared environment is not here: it lives inside each entry's `closure.surface.env`, attributed per package and without values (those are `${installPath}`-templated and only concrete after install).

Each `packages` entry carries `name` and `identifier`, plus `pinned_identifier` (the identifier with its digest already attached) and `pinned_digest` wherever one artifact was selected, plus one of the shapes below.

Default, image index — candidate listing:

```json
{
  "identifier": "registry/repo:tag",
  "pinned_identifier": "registry/repo:tag@sha256:…",
  "pinned_digest": "sha256:…",
  "candidates": [
    {
      "digest": "sha256:…",
      "pinned": "registry/repo:tag@sha256:…",
      "platform": "linux/amd64",
      "media_type": "…",
      "size": 123
    }
  ]
}
```

A candidate's `pinned` is that child as a pullable reference — the entry's identifier with the child's digest attached — so naming one platform never means splicing a reference by hand. It is spelled `pinned`, not `pinned_identifier`: a candidate has one digest, so there is nothing to disambiguate against — the same reason `resolution.pinned` is spelled that way. The entry's own `pinned_identifier` names the index the candidates came from.

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
  "platform": { "os": "linux", "architecture": "amd64", "os.features": ["libc.glibc"] },
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

`--closure` — adds one `closure` object on top of whichever body the reference already produces (the **metadata** body shown here for a single manifest, or the **resolution** body above when the reference needed platform selection):

```json
{
  "identifier": "registry/cmake:3.28",
  "pinned_digest": "sha256:cccc…",
  "metadata": { "type": "bundle", "version": 1, "env": [], "dependencies": [], "entrypoints": {} },
  "layers": [{ "digest": "sha256:…", "media_type": "…", "size": 123 }],
  "closure": {
    "deps": [
      {
        "name": "zlib",
        "identifier": "registry/zlib@sha256:bbbb…",
        "effective_visibility": "public",
        "entrypoints": ["zfmt"],
        "integrations": ["com.jetbrains"],
        "dependencies": []
      },
      {
        "name": "gcc",
        "identifier": "registry/gcc@sha256:dddd…",
        "effective_visibility": "private",
        "binaries": ["gcc", "g++"],
        "entrypoints": [],
        "integrations": [],
        "dependencies": [
          { "identifier": "registry/zlib@sha256:bbbb…", "visibility": "public", "name": "zlib" }
        ]
      }
    ],
    "surface": {
      "interface": {
        "binaries": [],
        "entrypoints": [
          { "name": "cc", "package": "registry/cmake:3.28@sha256:cccc…" },
          { "name": "zfmt", "package": "registry/zlib@sha256:bbbb…" }
        ],
        "env": [
          { "key": "PATH", "type": "path", "package": "registry/cmake:3.28@sha256:cccc…" },
          { "key": "ZLIB_ROOT", "type": "constant", "package": "registry/zlib@sha256:bbbb…" }
        ],
        "integrations": [
          { "namespace": "com.microsoft.vscode", "package": "registry/cmake:3.28@sha256:cccc…" },
          { "namespace": "com.jetbrains", "package": "registry/zlib@sha256:bbbb…" }
        ],
        "binaries_complete": false
      },
      "private": {
        "binaries": [
          { "name": "gcc", "package": "registry/gcc@sha256:dddd…" },
          { "name": "g++", "package": "registry/gcc@sha256:dddd…" }
        ],
        "entrypoints": [
          { "name": "zfmt", "package": "registry/zlib@sha256:bbbb…" }
        ],
        "env": [
          { "key": "PATH", "type": "path", "package": "registry/cmake:3.28@sha256:cccc…" },
          { "key": "GCC_HOME", "type": "constant", "package": "registry/gcc@sha256:dddd…" },
          { "key": "ZLIB_ROOT", "type": "constant", "package": "registry/zlib@sha256:bbbb…" }
        ],
        "integrations": [],
        "binaries_complete": false
      }
    },
    "conflicts": { "entrypoints": [], "repositories": [] }
  }
}
```

`closure.deps` lists every package transitively reachable from the reference's declared [dependencies][reference-dependencies], in transitive-closure order — dependencies before the packages that depend on them. The inspected reference itself is never listed here; it is named by the top-level `identifier` and contributes to both surface projections below. A dependency reached through two different paths (a diamond) still appears once, carrying the merge of every path that reaches it. Each entry carries:

- `name` — the dependency repository's short display name (its final path segment).
- `identifier` — the dependency's resolved identity, always digest-pinned. A closure node is a resolved artifact and never a bare tag, so there is no separate `digest` key repeating the tail of this one.
- `effective_visibility` — the entry's [visibility][reference-visibility] as composed from the root, down every path that reaches it.
- `binaries` — the same tri-state as [Executables][reference-binaries]: the key is absent when the publisher never declared the field, `[]` when the publisher declared zero, and a populated array when names are declared.
- `entrypoints` — the entry's own declared [entry-point][guide-entry-points] launcher names. Independent of `binaries`: a package may declare either, both, or neither. A binary is a raw executable the package puts on `PATH`; an entry point is a named launcher that runs one with a fixed argument prefix — the two are separate axes, not a 1:1 pairing. Above, `zlib` declares an entry point but no binaries; `gcc` declares binaries but no entry point.
- `integrations` — the entry's own declared [integration namespace][reference-integrations] keys, lexicographically ordered, `[]` when it declares none (absent and empty are the same state here, unlike `binaries`'s undeclared/declared-empty tri-state). Keys only, no payload — a closure node is not installed, the same reason `env` above carries no values.
- `dependencies` — the entry's own declared edges (as authored, not composed), each carrying its `visibility` and dependency `name` — enough to reconstruct the DAG from the flat list.

`closure.surface` projects the same node set two ways — the two environments the package participates in, each equal to what the runtime composer emits:

- `interface` is the **consumer view**: what a package depending on this reference inherits. It admits the reference itself plus every dependency whose `effective_visibility` reaches the interface axis. This is the surface [`ocx env`][cmd-package-env] composes for a downstream consumer.
- `private` is the **self-execution view**: what the reference runs with when it runs itself — its own `bin/` plus every dependency reaching the private axis. This is the surface [`ocx env --self`][cmd-package-env] composes.

The two surfaces overlap by design, and the overlap is deliberate, not redundant:

- A `public` dependency reaches both axes and appears in both surfaces (`zlib` above). A dependency reached only through a `private` edge appears in `private` only (`gcc` above); an `interface`-only dependency, in `interface` only.
- Entry points carry an implicit `interface` visibility: a launcher exists so a *consumer* can invoke the package, while the package's own runtime bypasses its launchers and calls `bin/` directly. The reference's own entry points therefore appear under `interface` only (`cc` above) — `ocx env --self` never puts its `entrypoints/` on `PATH` — while a **dependency's** entry points cross the edge like any interface-side carrier and appear on whichever surfaces admit the dependency (`zfmt` above): they are how this package invokes that dependency.
- Binaries carry an implicit `public` visibility — raw executables serve consumers and the package's own shims alike — so the reference's own binaries, like its `public` env vars, appear in both surfaces.
- Env crossing is asymmetric, matching the composer. The reference's own vars cross on the surface's axis (its `interface`/`public` vars on `interface`, its `private`/`public` vars on `private`). A **dependency**, however, contributes only its *interface-side* vars on either surface — a dependency's own `private` var is that dependency's internal detail and never crosses the edge into this package, so it appears on neither surface.

Each surface carries four attributed arrays plus a completeness flag:

- `binaries` / `entrypoints` — what lands on `PATH` on that axis, each entry `{ name, package }` naming the declaring package.
- `env` — the environment keys exposed on that axis, each entry `{ key, type, package }` plus a `separator` field present only when `type` is `list`. `type` is `path`, `constant`, or `list`; the value is omitted because it is `${installPath}`-templated and only concrete once the package is installed — the summary answers *which* keys would be set, not *to what*.
- `integrations` — the [integration namespaces][reference-integrations] each admitted package declares, each entry `{ name, package }` — `name` holds the namespace, never the payload (a closure node is not installed, so there is nothing concrete to interpolate a payload against). This reuses the same `{ name, package }` shape `binaries`/`entrypoints` use above, not the `{ namespace, package, payload }` shape the flat [`package env`][cmd-package-env] array carries — the two field names for the same concept belong to two different envelopes. One entry per (namespace, package) pair, never merged. **Interface surface only**: `private.integrations` is always `[]`, and a dependency still needs an interface-reaching edge to contribute at all — `gcc`'s `private` edge above contributes to neither surface, while `zlib`'s `public` edge and the reference's own root position admit their namespaces to `interface` (`cmake` contributes `com.microsoft.vscode` the same way it contributes its own `cc` entrypoint).
- `binaries_complete` — `false` iff some admitted node on that axis left `binaries` **undeclared** (the key absent from its metadata). A declared-empty claim (`"binaries": []`) is the opposite of a gap — the publisher asserts *zero* binaries — and keeps the aggregate complete; an unknown claim never silently counts as zero. Above, both surfaces admit `zlib` (undeclared) so both read `false` even though `gcc` declared its own claim.

`closure.conflicts` names install/compose conditions detected over the interface projection: `entrypoints` (two or more packages claiming the same entrypoint name) and `repositories` (one repository resolving to two or more distinct digests). Both arrays are always present; empty means the surface is realizable.

A non-empty `conflicts` exits **65** (`DataError`) while still reporting the condition in full — the payload is what a caller reads to act, the exit code is what a pipeline branches on. 65 is the same code install/compose already returns when it hard-rejects the identical condition, so `inspect --closure` exits exactly where the corresponding `ocx run` would.

**Examples**

```shell
# List the platforms a multi-platform tag offers.
ocx --format json package inspect mytool:1.0.0 | jq '.packages[0].candidates'

# Inspect several packages at once — one array entry each, in input order.
ocx --format json package inspect mytool:1.0.0 othertool:2.0.0 | jq '.packages[].name'

# Pick one entry by the name it was requested under.
ocx --format json package inspect mytool:1.0.0 othertool:2.0.0 \
  | jq '.packages[] | select(.name == "othertool:2.0.0")'

# Inspect one platform child by digest (same repo, online or cached).
ocx package inspect acme/mytool@sha256:abc…

# Platform-select and include the OCI resolution chain.
ocx --format json package inspect --resolve -p linux/arm64 mytool:1.0.0 | jq '.packages[0].resolution'

# What would land on PATH without installing it?
ocx --format json package inspect --closure mytool:1.0.0 | jq '.packages[0].closure.surface.interface'

# The exact artifact, without splicing identifier and digest together.
ocx --format json package inspect --resolve mytool:1.0.0 | jq -r '.packages[0].pinned_identifier'
```

**Plain output**

With `--format plain` (the default) the report renders as a tree rooted at the pinned identifier — the one place a full `sha256:` digest is spelled out, because it is what the command was asked for. The candidate listing shows one node per platform child; the single-manifest view shows the `metadata` branch (`env`, `dependencies`, `entrypoints`, and — when declared — [`binaries`][reference-binaries]) followed by a `layers` branch listing each layer by index, annotated with the discriminating tail of its media type (`tar+xz`) and a human-readable size. Under `entrypoints`, an entry whose dispatch command diverges from its invocable name carries a `→ <command>` annotation; entries whose command matches the name (the common case) are shown without annotation. `binaries` renders the same [undeclared vs. declared-empty][reference-binaries-none-vs-empty] distinction the field carries on the wire: an undeclared claim produces no `binaries` node at all, a declared-but-empty claim renders a `binaries (none declared)` leaf, and a non-empty claim renders a `binaries` branch listing each name. JSON output is unaffected by this rendering split — the `metadata` field is the full metadata document, so the `binaries` key is present or absent exactly as declared:

```text
registry/repo@sha256:…
├── metadata
│   ├── entrypoints
│   │   ├── fmt → cargo-fmt
│   │   └── build
│   └── binaries
│       ├── build
│       └── cargo-fmt
└── layers
    └── [0] · sha256:… · tar+xz · 192 B
```

Only the tail of the media type is shown because every layer of a package repeats the same `application/vnd.oci.image.layer.v1` prefix — 30 characters that push the size past the right edge of a narrow terminal without telling two layers apart. `--format json` carries the full media type.

Here `fmt` dispatches to the `cargo-fmt` binary while `build` dispatches to a binary named `build`;
`binaries` lists both underlying names the entry points wrap. A package that never declared the
field renders no `binaries` node at all; one that declared it empty renders a single
`binaries (none declared)` leaf instead of the branch shown above.

With `--resolve`, a `resolution` branch is added alongside `metadata` and `layers`. It opens with the `platform` the walk selected against, then a `chain` listing each blob by its `role` (`index`, `manifest`, `config`) with a human-readable size. The layers stay under the manifest — they are content the manifest references, not steps in the walk:

```text
registry/repo:tag@sha256:…
├── metadata
│   └── …
├── layers
│   └── [0] · sha256:… · tar+xz · 192 B
└── resolution
    ├── platform linux/amd64+libc.glibc
    └── chain
        ├── index · sha256:… · 429 B
        ├── manifest · sha256:… · 448 B
        └── config · sha256:… · 244 B
```

The `platform` leaf is the answer whenever `--platform` drove the selection: a libc refinement like `+libc.glibc` is chosen during the walk and is visible nowhere else in the tree. The chain carries no media-type column — the `role` label already names it (an `index` role *is* `application/vnd.oci.image.index.v1+json`), and there is no `pinned` leaf because it would repeat the tree root byte for byte.

With `--closure`, a `closure` branch is added alongside `metadata` and `layers` (and `resolution`, on a multi-platform reference — `--closure` platform-selects the same way `--resolve` does). The branch holds a flat `deps` list — one leaf per transitive dependency, in transitive-closure order, labeled by its short name with the whole identifier annotated as a digest-inked span and its composed visibility tagged after — and a `surface` branch with `interface` and `private` sub-branches, each rendering its admitted binaries/entrypoints/env the same way the JSON `surface` object does. A dependency reached through two different paths already merges into one entry before rendering (see [Visibility][reference-visibility]), so `deps` needs no repeat-visit marker:

```text
registry/cmake:3.28@sha256:cccc…
├── metadata
│   └── …
├── layers
│   └── …
└── closure
    ├── deps
    │   ├── zlib · registry/zlib@sha256:bbbb… · public
    │   └── gcc · registry/gcc@sha256:dddd… · private
    └── surface
        ├── interface
        │   ├── entrypoints
        │   │   ├── cc · cmake
        │   │   └── zfmt · zlib
        │   ├── env
        │   │   ├── PATH · path · cmake
        │   │   └── ZLIB_ROOT · constant · zlib
        │   ├── integrations
        │   │   ├── com.microsoft.vscode · cmake
        │   │   └── com.jetbrains · zlib
        │   └── binaries incomplete: at least one admitted package leaves binaries undeclared
        └── private
            ├── binaries
            │   ├── gcc · gcc
            │   └── g++ · gcc
            ├── entrypoints
            │   └── zfmt · zlib
            ├── env
            │   ├── PATH · path · cmake
            │   ├── GCC_HOME · constant · gcc
            │   └── ZLIB_ROOT · constant · zlib
            └── binaries incomplete: at least one admitted package leaves binaries undeclared
```

Surface entries attribute each claim to its owning package by short name — the same name the `deps` branch above uses as its label, so `deps` reads as the legend for the whole surface. The full pinned identifier appears once per dependency there rather than once per claim; a three-dependency, five-binary closure would otherwise repeat an 89-character pin thirty times. `--format json` attributes by full identifier in both places.

Here `gcc`'s direct edge is `private` — it never reaches the interface surface, so `cc` (the reference's own entrypoint) and `zfmt` (`zlib`, reachable by a separate `public` edge) are the only entries under `interface`, while `private` drops `cc` (the reference does not go through its own launcher) and gains `gcc`'s two binaries. Both surfaces flag `binaries incomplete`: the reference itself and `zlib` never declare a `binaries` claim on either axis they're admitted to, and completeness requires every admitted node to have declared — `gcc`'s own `["gcc"]` claim does not offset it.

`integrations` renders only under `interface` here — `cmake` and `zlib` both contribute a namespace and both reach the interface axis, while `private` never gets a `integrations` branch at all (an empty array renders no branch, the same convention every other section here follows). `gcc` declares no integrations in this example, but the point holds even if it did: its `private` edge would keep them off both surfaces.

An entrypoint or repository conflict, when present, renders as its own branch directly under `closure` with one child per colliding party — the one place the view spends vertical space, because it fires exactly when there is a decision to make:

```text
└── closure
    ├── deps
    │   └── …
    ├── surface
    │   └── …
    ├── entrypoint 'fmt' claimed by multiple packages
    │   ├── registry/zlib:1.3
    │   └── registry/gcc:13
    └── repository 'registry/zlib' resolves to multiple digests
        ├── sha256:bbbb11223344
        └── sha256:eeee55667788
```

Entrypoint conflicts name the colliding packages without their digests — *which* packages collide is the answer, and the digest is not. Repository conflicts are the inverse: one repository, several digests, each shortened to twelve hex characters.

**Exit codes**

- `79` (`NotFound`) — the tag or digest does not resolve; with `--closure`, also a dependency in the closure that is genuinely absent from the registry (a source was consulted and it said no).
- `81` (`PolicyBlocked`) — a local policy (`--offline` or `--frozen`) refused the resolution: the manifest or config blob is absent from the local cache, or an unpinned tag was not in the local index. With `--closure`, the same code covers a dependency's manifest or metadata blob missing from the local cache under `--offline` — run the same `--closure` inspection online once (or `ocx package pull` the dependency) to warm the cache, then retry offline.
- `65` (`DataError`) — the resolved metadata is malformed, fails validation, or exceeds the metadata size cap; with `--resolve -p <platform>`, also a platform feature mismatch or an ambiguous dual-libc selection (see [exit codes](#exit-codes)). With `--closure`, the same checks apply to every dependency in the closure — one bad dependency fails the whole request rather than a smaller closure.

#### `copy` {#package-copy}

Promotes an already-published package to another registry or repository without rebuilding it.

The platform manifests and their blobs are copied **verbatim**, so every digest stays the same. That is the whole point: a [Sigstore signature][in-depth-signing]'s subject *is* the platform manifest digest, and an [`ocx.lock`][cmd-lock-file] entry pins it. Rebuilding the package for production would produce a different digest — orphaning the signature you verified in staging and invalidating every lock pinned against it — while looking like it worked. See [Promoting packages][ug-promoting] for the dev → staging → prod walkthrough.

Three kinds of object, three different rules, because only one of them is content:

| Object | Treatment |
|---|---|
| Platform manifest + its blobs | Copied byte for byte; the digest never changes. |
| The tag's [image index][oci-image-index] | Merged one platform at a time. Copying `linux/amd64` never removes a `darwin/arm64` the target already offers. |
| Rolling tags (`1.4`, `1`, `latest`) | Recomputed against the **target**'s tag list under `--cascade`, never carried over from the source's. |

**Usage**

```shell
ocx package copy [OPTIONS] <SOURCE>
```

**Arguments**

- `<SOURCE>`: The published package to promote, as `registry/repository:tag` or `registry/repository@sha256:<hex>`. A tag names an image index (or, for a single-platform package, a bare manifest); a digest names one platform manifest and then `--platform` is required, because a platform manifest carries no platform of its own — OCX records the platform in the index entry, never in the manifest.

**Options**

- `--to <REGISTRY>`: Rewrite only the registry host, keeping the repository path and the tag. `dev.example.com/team/tool:1.4.2 --to prod.example.com` lands at `prod.example.com/team/tool:1.4.2`. Mutually exclusive with `--identifier`.
- `-i`, `--identifier <IDENTIFIER>`: The full target reference, for when the repository path or the tag changes too. Required when `<SOURCE>` names a digest — a digest carries no tag for `--to` to preserve.
- `-p`, `--platform <PLATFORM>`: Repeatable. Against a tag it *filters* the source index; omit it to copy every platform the source offers. Against a digest it *declares* the platform, and exactly one is required. See [Platforms][reference-platforms] for the grammar.
- `-c`, `--cascade`: Also re-point the rolling ancestors (`1.4`, `1`, `latest`) at the target. The blocker checks read the target's tag list, so promoting `1.4.1` into a production registry that already publishes `1.4.2` leaves `1.4` where it is.
- `--canonical-tag` / `--no-canonical-tag`: `--canonical-tag` (default) also writes a digest-named `sha256.<hex>` tag for each copied platform manifest at the target — the same registry-side deletion safety net [`push`](#package-push) writes.
- `--referrers` / `--no-referrers`: `--referrers` (default) also copies everything anchored to each manifest — signatures, SBOMs, attestations — following referrer chains recursively. Requires the [OCI Referrers API][oci-referrers-spec] at the target; a registry without it exits 84 rather than accepting a referrer manifest it will never list. `--no-referrers` promotes the package alone.
- `--description`: Also copy the repository description (README, logo, catalog annotations) from the `__ocx.desc` tag. Off by default — a description is repository-level prose rather than part of the version being promoted, and environments legitimately carry different ones. [`ocx package describe --from`](#package-describe) copies it on its own.
- `--annotation <KEY=VALUE>`: Record an [OCI annotation][oci-annotations] on the target's image index. Repeatable, same semantics as [`push`](#package-push-annotations). Platform manifests are never annotated — that would change their digest, which is the one thing a copy must not do.
- `--dry-run`: Report what would be copied and write nothing. The preview covers only the per-platform disposition below — a `--cascade` or `--canonical-tag` promotion's rolling-tag and canonical-tag moves are never computed under `--dry-run`, so those fields report empty regardless of what a real run would write. See **Output** below.
- `-h`, `--help`: Print help information.

**Output**

One row per platform the target offers after the copy, each labelled with what happened to it — this is the result, on stdout:

| Result | Meaning |
|---|---|
| `added` | The target's index had no entry for this platform. |
| `unchanged` | The target already pointed at this exact digest. |
| `replaced` | The target pointed at a different digest for this platform. |
| `kept (not in source)` | The target offers this platform and the source does not, so the merge left it alone. |

The last row is why the report is per platform: a filtered promotion that leaves a mixed index behind is a legitimate outcome and a serious mistake, and only the row list tells them apart.

The `Digest` column means two things, and the `Result` column says which: on an `added`, `replaced` or `unchanged` row it is the digest this copy put there, and on a `kept (not in source)` row it is the digest the target already had and this copy never touched.

Under `--dry-run` the two write results read `would add` and `would replace` in the table. The JSON `disposition` keeps `added` / `replaced` either way — the top-level `status` (`copied` or `planned`) is what a script branches on.

The tags written, the blob traffic and the description outcome go to stderr as one status line, leaving stdout to the table. `--format json` carries all of it: `cascade_tags_written`, `canonical_tags_written`, `referrers_copied`, `blobs` (`present` / `mounted` / `uploaded`), and `description` — `copied`, `absent` when the source publishes none, `skipped-dry-run`, or `null` when `--description` was not passed.

Under `--dry-run` both `cascade_tags_written` and `canonical_tags_written` are always empty, whatever `--cascade` and `--canonical-tag` say: the tag phase is the part a dry run does not run.

**Exit codes**

| Condition | Exit code |
|---|---|
| `<SOURCE>` names a digest and `--platform` is absent, or given more than once | 64 |
| `<SOURCE>` names a digest and `--identifier` is absent | 64 |
| `<SOURCE>` names an [image index][oci-image-index] by digest — name the tag instead | 64 |
| `--to` and `--identifier` together | 64 |
| No platform in the source matches `--platform` | 64 |
| The source tag or digest does not resolve | 79 |
| Authentication to either registry fails | 80 |
| `--referrers` (the default) and the target has no [Referrers API][oci-referrers-spec] | 84 |
| `--offline` is set — a copy always needs network access to both registries | 81 |

::: tip Promotion is safe to re-run
A second identical copy is idempotent in effect — no new content lands and no tag moves — but it is not a no-op on the wire. Every platform still re-verifies: the leaf manifest is re-fetched and re-PUT, and with `--referrers` (the default) its referrer set is re-copied too, because the target's index entry proves the manifest is there, not that every blob it names still is. Only blob *bodies* are skipped, via a HEAD against the target. The index is re-PUT too: each platform's entry is merged into every tag it lands on, and the merge is a read-modify-write that writes even when the entry it would set is already there. Pipelines can re-run a promotion step without special-casing it — the cost is a HEAD per blob, a manifest re-PUT per platform, and an index re-PUT per platform per tag, not a re-upload.
:::

::: warning A copy is not a re-sign
The signature travels with the manifest, so it still names the identity that signed it in the source environment. If your policy requires a production-specific attestation, sign again at the target — promotion preserves provenance, it does not create it.
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
- `--logo <PATH>`: Path to a logo image (PNG or SVG). The file's bytes must be the format its extension names; anything else exits 65 without touching the published description.
- `--title <TITLE>`: Short display title for the package catalog.
- `--description <TEXT>`: One-line summary.
- `--keywords <LIST>`: Comma-separated search keywords.
- `--from <SOURCE>`: Copy the whole description — README, logo and catalog annotations — from another package repository, replacing the target's. Mutually exclusive with the field options above: this is a copy, not a merge, so mixing the two would silently pick a winner. Use it to promote a catalog page reviewed in staging without re-authoring it, or after an [`ocx package copy`](#package-copy) that ran without `--description`. A source that publishes no description exits 79 and the target is left untouched; the same code covers a source repository that does not exist at all.
- `-h`, `--help`: Print help information.

At least one of the above metadata options must be provided, or `--from`.

**Exit codes**

| Condition | Exit code |
|---|---|
| `--from` combined with `--readme`, `--logo`, `--title`, `--description`, or `--keywords` | 64 |
| Neither `--from` nor any metadata option given | 1 |
| `--from <SOURCE>` names a repository with no published description (or whose `__ocx.desc` tag does not resolve — the two are indistinguishable at this point) | 79 |
| A `--logo` file's bytes do not match the format its extension names | 65 |
| `--offline` is set | 81 |
| Authentication fails | 80 |

The "nothing to update" case exits `1` (`Failure`) rather than a more specific code: it raises a plain error with no `ClassifyExitCode` source, so classification falls through to the generic case. "No description to copy" carries the registry's own not-found cause, so it reaches `79` — a script can tell "there was nothing to promote" from "the command was wrong".

#### `sign` {#package-sign}

Publishes a [Sigstore][sigstore] keyless signature for a package manifest as an [OCI Referrers][oci-referrers-spec] artifact. The signing flow uses an ephemeral ECDSA P-256 keypair: [Fulcio][fulcio] issues a short-lived certificate binding the key to your OIDC identity, the manifest digest is signed, and the entry is logged to [Rekor][rekor]. The resulting [Sigstore bundle v0.3][sigstore-bundle] is pushed to the registry as a referrer of the target manifest, discoverable and verifiable by `ocx package verify`. [`cosign verify`][cosign] does not discover it — cosign finds signatures only through its own tag schema, and OCX publishes and reads only through the Referrers API, by design — but the bundle itself is a standard Sigstore bundle either tool can read; see [cosign Interoperability][signing-cosign-interop] in the signing guide.

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
| `--fulcio-url` | — | (`[trust.sigstore].fulcio_url`, else `https://fulcio.sigstore.dev`) | [Fulcio][fulcio] CA endpoint (override for private deployments) |
| `--rekor-url` | — | (`[trust.sigstore].rekor_url`, else `https://rekor.sigstore.dev`) | [Rekor][rekor] transparency-log endpoint (override for private deployments) |
| `--identity-token-file` | — | — | Read the OIDC identity token from this file (highest precedence). File must be owner-readable only (`chmod 600`); world- or group-readable files are rejected with exit 77 (`IdentityTokenFilePermissive`). File must be **owned by the effective user** (uid match required); a foreign-owned file with mode `0600` is still rejected with exit 77 (CWE-732). Symlinks are not followed; a symlink at the supplied path is rejected with exit 77 (CWE-367 mitigation). **Windows:** permission validation is not implemented; use `--identity-token-stdin` or [`OCX_IDENTITY_TOKEN`][env-identity-token] instead (the command exits 77 if `--identity-token-file` is used on Windows). |
| `--identity-token-stdin` | — | — | Read the OIDC identity token from stdin (second precedence). Mutually exclusive with `--identity-token-file` |
| `--no-tty` | — | `false` | Suppress the interactive browser OAuth fallback; ambient token detection must succeed or an override flag must supply a token |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |

**Token precedence**

`ocx package sign` resolves an OIDC identity token from the following sources, in order:

1. `--identity-token-file <PATH>` — read from file (highest precedence)
2. `--identity-token-stdin` — read from stdin
3. [`OCX_IDENTITY_TOKEN`][env-identity-token] environment variable
4. Ambient CI detection — GitHub Actions (`ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN`), GitLab CI (`SIGSTORE_ID_TOKEN`), CircleCI (`CIRCLE_OIDC_TOKEN_V2`)
5. Interactive browser OAuth (suppressed when `--no-tty` is set)

Never pass a raw token on the command line — it would appear in shell history and process listings.

:::info Signing targets Rekor v1
`ocx package sign` runs the full keyless pipeline end-to-end — keypair generation, [Fulcio][fulcio] certificate, [Rekor][rekor] entry, [Sigstore bundle v0.3][sigstore-bundle] assembly, and the referrer push. It writes a Rekor v1 `hashedrekord` entry; a Rekor v2 (tiles) instance is not supported yet. See [Deferred to future work][signing-deferred].
:::

**Exit codes**

| Code | Condition |
|------|-----------|
| 0 | Signature published successfully |
| 64 | `InvalidEndpointUrl` — malformed `--fulcio-url` or `--rekor-url` (must be `https://`, or `http://` on loopback only; no credentials, no unsupported schemes) |
| 65 | `RekorSetMalformed` — Rekor returned the log entry but its Signed Entry Timestamp could not be extracted or parsed |
| 77 | `OidcPreCheckFailed` — OIDC pre-check rejected the token (missing scopes, audience mismatch, expired) |
| 77 | `OfflineSignRefused` — `--offline` is incompatible with `package sign`; Fulcio + Rekor are hard dependencies |
| 77 | `IdentityTokenFilePermissive` — `--identity-token-file` is readable by group/other (must be `0600` or tighter) |
| 78 | Fulcio rejected the certificate signing request as malformed |
| 79 | `TargetNotFound` — no manifest for the requested `--platform` under the target image index |
| 80 | Fulcio rejected the OIDC token (issuer mismatch, expired, wrong audience) |
| 83 | Rekor transparency log unavailable at time of signing, or it returned a log entry with no usable Merkle inclusion proof |
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
    "context": {
      "identifier": "registry.example/pkg:1.0"
    }
  }
}
```

`detail` is omitted when no fine-grained discriminant is available. `context` is always present (may be `{}`). A `remediation` key is reserved in the envelope shape but not currently emitted. The `kind` values are the snake_case `ErrorCategory` variants: `usage_error`, `auth_error`, `permission_denied`, `config_error`, `data_error`, `not_found`, `unavailable`, `temp_fail`, `transparency_log_unavailable`, `referrers_unsupported`, `io_error`, `internal`.

**`detail` discriminants for `package sign`** (frozen contract C-S1-1):

| `detail` value | Exit | Meaning |
|----------------|------|---------|
| `fulcio_bad_request` | 78 | Fulcio rejected the CSR as malformed |
| `oidc_token_rejected` | 80 | Fulcio rejected the OIDC token (issuer mismatch, expired, wrong audience) |
| `fulcio_unavailable` | 75 | Fulcio could not be reached, or answered 429 or 5xx — a transient outage, safe to retry |
| `transparency_log_unavailable` | 83 | Rekor transparency log unavailable at time of signing, or it returned a log entry with no usable Merkle inclusion proof — publishing that bundle would produce a signature OCX itself refuses to verify |
| `rekor_set_malformed` | 65 | Rekor returned the entry but the SET could not be extracted or parsed |
| `referrers_unsupported` | 84 | Registry does not implement the OCI Referrers API |
| `target_not_found` | 79 | No manifest for the requested `--platform` under the target image index |
| `oidc_pre_check_failed` | 77 | OIDC pre-check failed client-side before the token was sent to Fulcio |
| `forbidden_registry_target` | 78 | The target registry is refused by policy before any signing call is made |
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

Verifies a [Sigstore][sigstore] keyless signature attached to a package manifest via [OCI Referrers][oci-referrers-spec]. The command fetches the [Sigstore bundle v0.3][sigstore-bundle] referrer for the target, verifies the [Fulcio][fulcio] certificate chain against a supplied trust root (see `--sigstore-trusted-root` below), verifies the [Rekor][rekor] Signed Entry Timestamp (SET), verifies the signature over the subject manifest digest, and checks the certificate identity and OIDC issuer against the identity you either supply as flags or have pinned in a [`[[trust.policy]]`][config-trust] entry. All five checks must pass for the command to exit 0.

`--offline` (or [`OCX_OFFLINE`][env-offline]) scopes to the Sigstore trust services — the Rekor-key fetch and TUF — not the registry: verify still fetches the target and its signature referrer from the registry in every mode. Offline verify requires a pinned Rekor key from `--sigstore-trusted-root` (or one of the other trust-root rungs) or a fresh trust-root cache entry; see [Offline and Air-Gapped Verification][signing-offline] for the full model.

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
| `--rekor-url` | — | (`[trust.sigstore].rekor_url`, else `https://rekor.sigstore.dev`) | [Rekor][rekor] transparency-log endpoint (override for private deployments) |
| `--sigstore-trusted-root` | — | *(public-good root over TUF)* | Path to a Sigstore [trusted-root][sigstore-tuf] JSON (or a directory holding `trusted_root.json`) — supplies the [Fulcio][fulcio] CA, the certificate-transparency log keys and the pinned [Rekor][rekor] public key together, so no Rekor-key fetch is needed. Equivalent env var: [`OCX_SIGSTORE_TRUSTED_ROOT`][env-sigstore-trusted-root]; the flag wins. Highest rung of the trust-root ladder — see [Self-hosted Sigstore][in-depth-self-hosted-sigstore] for the config-driven alternatives. Required for [`--offline`](#arg-offline) verify unless another rung already supplies a pinned Rekor key |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |
| `--attestation` | — | `false` | Verify an in-toto attestation instead of a signature — same referrer discovery, trust-root and identity pipeline, a different referrer content type. See [Verifying attestations][cmd-package-verify-attestations] below |
| `--type` | — | *(any type)* | Restrict attestation verification to one [predicate type][cmd-package-attest]. Requires `--attestation` — used alone it is a usage error (exit 64) |

#### Identity resolution {#package-verify-identity}

Two ways to tell `ocx package verify` whose signature to accept:

- **Flags** — pass both `--certificate-identity` and `--certificate-oidc-issuer`. This is an exact-match pair that overrides any configured policy, matching the original flag-only behavior byte-for-byte.
- **[`[[trust.policy]]`][config-trust]** — omit both flags. Verify first checks the pooled `config.toml`-tier ("operator") policies against the target's canonical `registry/repository`; if any match, the project `ocx.toml` is not consulted at all. Only when no operator policy matches does verify fall back to the project `ocx.toml`'s policies. See the [configuration reference][config-trust] for scope matching, most-specific-wins resolution, regex identities, and the operator-authoritative precedence rule. Reading `[[trust.policy]]` from `ocx.toml` here is the one documented exception to "OCI-tier commands never consult `ocx.toml`" — trust policy is a security posture, not toolchain-binding resolution.

Supplying exactly one of the two flags is a usage error (exit 64) rejected by the argument parser (clap `requires`) *before* verification runs — a `--certificate-identity` without a matching `--certificate-oidc-issuer`, or vice versa, cannot express a valid match. Because it is caught at parse time it produces a bare usage error with **no** JSON envelope and no `error.detail` (it is not the `no_identity_provided` case). Supplying neither flag with no `[[trust.policy]]` scope covering the target is also exit 64, but *that* one is the `NoIdentityProvided` verify error (it does carry an envelope): there is no identity to check the signature against.

:::warning A bare Fulcio CA is not a trust root
`ocx package verify` runs the full pipeline end-to-end — referrer discovery, [Fulcio][fulcio] chain, SCT, [Rekor][rekor] SET and inclusion proof, subject-digest signature, identity and issuer match. With no trust root supplied by any rung of the ladder and no cached trust material, it fetches the public-good trust root over [TUF][sigstore-tuf].

A Fulcio certificate embeds a Signed Certificate Timestamp that the verifier checks against the CT log's key, so trust material carrying CA anchors alone cannot verify anything — verify refuses it with exit 78 and the message `trust root carries no CT log key`. A Sigstore trusted-root JSON carries the anchors, the CT log keys and the pinned Rekor key together; that is the only shape `--sigstore-trusted-root` accepts. See [Self-hosted Sigstore][in-depth-self-hosted-sigstore].
:::

**Exit codes**

| Code | Condition |
|------|-----------|
| 0 | Signature verified — identity and issuer match, bundle cryptographically valid |
| 64 | `UsageError` — malformed `--rekor-url` (must be `https://`, or `http://` on loopback only; no credentials, no userinfo) |
| 64 | `NoIdentityProvided` — neither `--certificate-identity` nor `--certificate-oidc-issuer` was given and no [`[[trust.policy]]`][config-trust] scope covers the target (a lone flag is instead rejected at parse time as a bare usage error, with no envelope) |
| 65 | Data integrity failure: signature invalid, subject digest mismatch, certificate chain invalid, Rekor SET invalid (bundle tampered), Rekor transparency-log body does not bind to the bundle (spliced SET), the signature candidate examination cap was reached before a valid signature was found, or bundle parse failed. In `--attestation` mode, also: predicate type mismatch, a missing or weak-digest subject, an unrecognized in-toto statement or DSSE payload type, a SLSA provenance builder mismatch, more than one matching attestation with no `--type` to disambiguate, or the attestation exceeded its size or byte-budget limit |
| 77 | Certificate identity or OIDC issuer mismatch |
| 78 | Trust root unavailable or failed to load — includes [`--offline`](#arg-offline) verify with no pinned Rekor key available (no `--sigstore-trusted-root`, no configured trust root, and no fresh trust-root cache entry); the message names the remedy |
| 78 | `TrustPolicyInvalid` — the [`[[trust.policy]]`][config-trust] entry matched for this target sets both `identity` and `identity_regexp`, sets neither, or its `identity_regexp` fails to compile |
| 78 | `ForbiddenRegistryTarget` — the target registry is refused by policy before any verification is attempted |
| 79 | No signatures found for target, no usable Sigstore bundle among referrers, or no manifest for the requested `--platform` under the target image index. In `--attestation` mode: no attestation found for the target (`attestation_not_found`) |
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
| `target_not_found` | 79 | No manifest for the requested `--platform` under the target image index |
| `no_usable_bundle` | 79 | Referrers found but none has a recognized Sigstore bundle artifact type |
| `candidate_limit_exhausted` | 65 | The signature candidate examination cap was reached with unexamined referrers remaining and none of the examined candidates passed; the operator must reduce the referrer count or raise the cap |
| `identity_mismatch` | 77 | Certificate SAN does not satisfy the expected identity, whether supplied via `--certificate-identity` or resolved from a [`[[trust.policy]]`][config-trust] entry |
| `issuer_mismatch` | 77 | Certificate OIDC issuer does not match the expected issuer, whether supplied via `--certificate-oidc-issuer` or resolved from a [`[[trust.policy]]`][config-trust] entry |
| `cert_chain_invalid` | 65 | Certificate chain does not verify against the supplied trust root |
| `signature_invalid` | 65 | Signature does not verify over the subject manifest digest |
| `subject_digest_mismatch` | 65 | The bundle's signed digest does not match the target manifest's digest |
| `rekor_set_invalid` | 65 | Rekor SET does not verify (bundle tampered) |
| `transparency_body_mismatch` | 65 | Rekor transparency-log entry body does not bind to the bundle — a previously-valid SET/body spliced onto a different subject |
| `rekor_inclusion_proof_absent` | 65 | Bundle carries a Rekor inclusion promise but no Merkle inclusion proof. The promise alone is not evidence the entry was published in a signed tree, so verification refuses it. Re-sign against a transparency log that returns an inclusion proof |
| `rekor_set_absent_tsa_present` | 83 | Rekor SET absent but RFC 3161 TSA timestamp present (Rekor v2 transition) |
| `referrers_unsupported` | 84 | Registry does not implement the OCI Referrers API |
| `transparency_log_unavailable` | 83 | Rekor transparency log unavailable during verify |
| `bundle_parse_failed` | 65 | Bundle is not valid Sigstore bundle v0.3 or is corrupted JSON |
| `trust_root_unavailable` | 78 | Embedded TUF trust root asset not present in this build (Slice 1) |
| `trust_root_load` | 78 | Trust root failed to load — malformed trusted-root JSON, no CT log key, TUF fetch failed, or [`--offline`](#arg-offline) verify with no pinned Rekor key available (supply `--sigstore-trusted-root`, or run an online verify first to populate the cache) |
| `forbidden_registry_target` | 78 | The target registry is refused by policy before any verification is attempted |
| `no_identity_provided` | 64 | No identity to verify against: both certificate flags omitted and no [`[[trust.policy]]`][config-trust] scope matched the target. (A lone flag is a clap parse error — still exit 64, but with no envelope and no `detail`.) |
| `trust_policy_invalid` | 78 | A matched [`[[trust.policy]]`][config-trust] entry is malformed — identity XOR violation, or an `identity_regexp` that does not compile |
| `invalid_endpoint_url` | 64 | Malformed `--rekor-url` |
| `attestation_not_found` | 79 | No attestation referrer found for the target (`--attestation` mode) |
| `predicate_type_mismatch` | 65 | The `--type` given does not match any verified attestation's `predicateType` |
| `statement_subject_mismatch` | 65 | The in-toto Statement's `subject` does not name the target manifest digest |
| `statement_subject_absent` | 65 | The in-toto Statement carries no `subject` entry at all |
| `statement_subject_weak_algorithm` | 65 | The Statement's subject digest uses an algorithm weaker than SHA-256 |
| `builder_mismatch` | 65 | The attestation's SLSA provenance `builder.id` does not match the pinned `builder` in a [`[[trust.policy]]`][config-trust] entry |
| `statement_type_unsupported` | 65 | The DSSE payload's `_type` is not a recognized [in-toto][in-toto] Statement type |
| `payload_type_unsupported` | 65 | The [DSSE][dsse] envelope's `payloadType` is not `application/vnd.in-toto+json` |
| `multiple_attestations` | 65 | More than one verified attestation candidate for the target and no `--type` narrowed it to one; the message names every candidate's referrer digest and every distinct predicate type in the set, so `--type` has a value to take — and says outright when a single shared type means `--type` cannot narrow further |
| `unsupported_tlog_entry_kind` | 65 | The Rekor transparency-log entry kind is neither `hashedrekord` nor `dsse` |
| `tlog_binding_mismatch` | 65 | The transparency-log entry does not bind to the DSSE envelope actually being verified |
| `attestation_too_large` | 65 | The attestation referrer exceeds its per-entry size limit |
| `attestation_payload_too_large` | 65 | The DSSE payload inside a verified attestation exceeds its size limit |
| `too_many_attestations` | 65 | More attestation candidates exist for the target than the examination cap allows |
| `attestation_budget_exhausted` | 65 | The cumulative byte budget across all examined attestation candidates was exhausted before a match was found |
| `internal` | 1 | Unexpected internal error |

#### Verifying attestations {#package-verify-attestations}

`--attestation` swaps the referrer content type verify looks for: instead of a Sigstore-bundle signature over the manifest digest, it fetches a [DSSE][dsse]-enveloped [in-toto][in-toto] Statement, verifies the identical five-step pipeline against it (referrer discovery, Fulcio chain, Rekor SET and inclusion proof, then the Statement's signature and subject digest), and additionally checks that the Statement's `subject` names the target digest with a strong algorithm. `--type` narrows which `predicateType` counts as a match — omit it to accept any predicate type carried by a verified attestation.

The success and error JSON envelopes are byte-identical in shape to signature-mode verify (see [JSON output](#package-verify) above) — `data` carries the same five fields regardless of mode, since a verified attestation and a verified signature both reduce to "this subject digest, this certificate, this timestamp." Use [`ocx package sbom`][cmd-package-sbom] when the predicate type or its content is the thing you need back.

**Example — verify a package signed in CI, with flags**

```shell
ocx package verify \
  -p linux/amd64 \
  --sigstore-trusted-root /etc/ocx/trusted_root.json \
  --certificate-identity https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  registry.example/pkg:1.0
```

With no trust root from any rung of the ladder and no fresh trust-root cache entry, verify fetches the public-good trust root over [TUF][sigstore-tuf] — `--sigstore-trusted-root` here pins a self-hosted or private deployment's own trust material instead. Passing it on every invocation is the most expensive rung; [Self-hosted Sigstore][in-depth-self-hosted-sigstore] covers the config-driven alternatives. See also [Current limitations][signing-limitations].

**Example — verify with a `[[trust.policy]]` covering the target, no flags**

```toml
# ocx.toml or config.toml
[[trust.policy]]
scope = "registry.example/pkg"

[trust.policy.keyless]
identity    = "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

```shell
ocx package verify -p linux/amd64 --sigstore-trusted-root /etc/ocx/trusted_root.json registry.example/pkg:1.0
```

See the [configuration reference][config-trust] for the full schema, scope matching, and rotation semantics.

#### `attest` {#package-attest}

Attaches an attestation — an SBOM, a provenance statement, or any other structured predicate — to a package manifest as an [OCI Referrers][oci-referrers-spec] artifact.

The shape depends on whether a signing identity is visible in the environment (see [`sign`][cmd-package-sign] for the same override-token/ambient-CI check). **With one present**, the predicate you supply is wrapped in a [DSSE][dsse]-enveloped in-toto Statement naming the target manifest digest as its `subject`, signed through the identical keyless pipeline `sign` uses — an ephemeral ECDSA P-256 keypair, a [Fulcio][fulcio] certificate bound to your OIDC identity, a [Rekor][rekor] transparency-log entry — and the resulting bundle is pushed as a referrer, discoverable and verifiable by [`ocx package verify --attestation`][cmd-package-verify-attestations] and [`ocx package sbom`][cmd-package-sbom]. **With none visible**, `--type` must resolve to one of the three SBOM media types (`cyclonedx`, `spdx`, `spdxjson`, or a full URI resolving to one of them) — the predicate document is pushed as the referrer payload verbatim, typed by its own media type, with no DSSE envelope, no Fulcio certificate, and no Rekor entry. Any other `--type` with no signing identity present is refused before any network call (exit 64, `unsigned_type_unsupported`) — an unsigned provenance or vulnerability statement carries no attribution worth publishing. See [Attestations][ug-attestations-attach] for the full polarity rule and when each shape applies.

If a signing identity is detected but acquiring a usable token then fails (Fulcio unreachable, an ambient CI token rejected), that is a hard error — the command never falls back to publishing unsigned.

`ocx package push --sbom <PATH>` is sugar for `ocx package attest --type cyclonedx` against the digest a push just wrote, including this same polarity — see [`push`][cmd-package-push]. Use `attest` directly to attach a predicate standalone, attach an SPDX predicate, attach more than one predicate type to the same manifest, or attach an attestation to something other than a package this invocation just published.

Attesting requires registry access regardless of shape — `--offline` is rejected with exit 77, checked before the predicate file is even read.

**Usage**

```shell
ocx package attest [OPTIONS] --predicate <PATH> --type <TYPE> --platform <PLATFORM> <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier to attest (`registry/repo:tag[@digest]`).

**Options**

| Name | Short | Default | Purpose |
|------|-------|---------|---------|
| `--predicate <PATH>` | — | *(required)* | Path to the predicate file — the document the Statement wraps verbatim. Bounded to 15 MiB; a larger file is refused (exit 65, `predicate_too_large`) rather than truncated. The path must not be a symlink — it is opened with `O_NOFOLLOW` on Unix, and a symlink is refused, along with any other I/O error reading the file (exit 74 — see the exit-codes table below) |
| `--type <TYPE>` | — | *(required)* | The predicate's type. One of the cosign-compatible aliases — `cyclonedx`, `spdx`, `spdxjson`, `slsaprovenance1`, `link`, `vuln`, `openvex`, `custom` — or any absolute predicate-type URI, stored byte-exact. `slsaprovenance` and `slsaprovenance02` are recognized aliases but both resolve to SLSA provenance v0.2, which `attest` refuses before any network call (exit 64, `provenance_version_unsupported`) — pass `slsaprovenance1` instead. `custom` wraps the predicate bytes in cosign's `{Data, Timestamp}` envelope before signing; every other alias resolves to its canonical `predicateType` URI and signs the predicate bytes as given |
| `--platform` | `-p` | *(required)* | Target platform — selects the single-platform manifest under the image index to attest |
| `--fulcio-url` | — | (`[trust.sigstore].fulcio_url`, else `https://fulcio.sigstore.dev`) | [Fulcio][fulcio] CA endpoint (override for private deployments) |
| `--rekor-url` | — | (`[trust.sigstore].rekor_url`, else `https://rekor.sigstore.dev`) | [Rekor][rekor] transparency-log endpoint (override for private deployments) |
| `--identity-token-file <PATH>` | — | — | Read the OIDC identity token from this file. Same permission, ownership and symlink checks as [`sign`][cmd-package-sign] |
| `--identity-token-stdin` | — | — | Read the OIDC identity token from stdin. Mutually exclusive with `--identity-token-file` |
| `--no-tty` | — | `false` | Suppress the interactive browser OAuth fallback |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |

Token precedence and the ambient-CI detection order are identical to [`sign`][cmd-package-sign] — neither command has a `--identity-token` value flag; only file, stdin, an environment variable, and ambient CI detection.

::: tip Offline refusal runs before the predicate is even read
`--offline` fails the command before `--predicate` is opened and before any token is resolved — a local policy refusal never depends on what the predicate file contains or whether it exists.
:::

**Exit codes**

| Code | Condition |
|------|-----------|
| 0 | Attestation published successfully |
| 64 | `InvalidEndpointUrl` — malformed `--fulcio-url` or `--rekor-url` |
| 64 | `ProvenanceVersionUnsupported` — `--type` resolved to a SLSA provenance predicate below v1.0 (`slsaprovenance` or `slsaprovenance02`); pass `--type slsaprovenance1` |
| 64 | `UnsignedTypeUnsupported` — no signing identity is visible and `--type` did not resolve to one of the three SBOM media types; supply an identity to attach it signed, or use a `cyclonedx`/`spdx`/`spdxjson` type |
| 65 | `PredicateTooLarge` — the `--predicate` file exceeds 15 MiB |
| 65 | `RekorSetMalformed` — Rekor returned the log entry but its Signed Entry Timestamp could not be extracted or parsed |
| 65 | `PredicateNotJson` — the `--predicate` file did not parse as JSON |
| 74 | An I/O error reading `--predicate` — missing file, permission denied, or the symlink refusal. `error.kind` is `io_error` with **no** `error.detail`; a script must branch on `error.kind` for this one |
| 75 | `FulcioUnavailable` — Fulcio could not be reached, or answered 429 or 5xx; a transient outage, safe to retry |
| 77 | `OidcPreCheckFailed`, `OfflineAttestRefused` (`--offline` is incompatible with `attest`; checked first), or `IdentityTokenFilePermissive` |
| 78 | Fulcio rejected the certificate signing request as malformed |
| 79 | `TargetNotFound` — no manifest for the requested `--platform` under the target image index |
| 80 | Fulcio rejected the OIDC token |
| 83 | Rekor transparency log unavailable, or it returned a log entry with no usable Merkle inclusion proof |
| 84 | Registry does not support the OCI Referrers API |

**JSON output** (`--format json`)

On success, `ocx package attest` emits a success envelope. Signed:

```json
{
  "schema_version": 1,
  "command": "package attest",
  "exit_code": 0,
  "data": {
    "identifier": "registry.example/pkg:1.0",
    "platform": "linux/amd64",
    "subject_digest": "sha256:<64-hex>",
    "predicate_type": "https://cyclonedx.org/bom",
    "bundle_digest": "sha256:<64-hex>",
    "referrer_digest": "sha256:<64-hex>",
    "signed": true,
    "certificate_identity": "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main",
    "certificate_oidc_issuer": "https://token.actions.githubusercontent.com"
  }
}
```

Unsigned — no signing identity was visible, so the three certificate fields are omitted (never emitted empty) and `bundle_digest` is the SBOM document's own digest rather than a Sigstore bundle's:

```json
{
  "schema_version": 1,
  "command": "package attest",
  "exit_code": 0,
  "data": {
    "identifier": "registry.example/pkg:1.0",
    "platform": "linux/amd64",
    "subject_digest": "sha256:<64-hex>",
    "predicate_type": "https://spdx.dev/Document",
    "bundle_digest": "sha256:<64-hex>",
    "referrer_digest": "sha256:<64-hex>",
    "signed": false
  }
}
```

`predicate_type` echoes the **resolved** `predicateType` URI actually written into the Statement (signed) or declared as the referrer's `artifactType` (unsigned) — for the cosign-alias spellings this differs from the `--type` value you passed (e.g. `--type cyclonedx` resolves to `https://cyclonedx.org/bom`); a literal URI passed to `--type` is echoed unchanged.

On error, `ocx package attest` emits the same envelope shape as [`sign`][cmd-package-sign]. The `error.detail` field is a snake_case discriminant for programmatic matching:

**`detail` discriminants for `package attest`** (frozen contract C-S1-1):

| `detail` value | Exit | Meaning |
|----------------|------|---------|
| `predicate_too_large` | 65 | The `--predicate` file exceeds 15 MiB |
| `rekor_set_malformed` | 65 | Rekor returned the entry but the SET could not be extracted or parsed |
| `predicate_not_json` | 65 | The `--predicate` file did not parse as JSON |
| `fulcio_bad_request` | 78 | Fulcio rejected the CSR as malformed |
| `fulcio_unavailable` | 75 | Fulcio could not be reached, or answered 429 or 5xx — a transient outage, safe to retry |
| `oidc_token_rejected` | 80 | Fulcio rejected the OIDC token |
| `transparency_log_unavailable` | 83 | Rekor transparency log unavailable, or returned an entry with no usable Merkle inclusion proof |
| `referrers_unsupported` | 84 | Registry does not implement the OCI Referrers API |
| `target_not_found` | 79 | No manifest for the requested `--platform` under the target image index |
| `oidc_pre_check_failed` | 77 | OIDC pre-check failed client-side before the token was sent to Fulcio |
| `offline_attest_refused` | 77 | `--offline` is incompatible with `package attest` |
| `identity_token_file_permissive` | 77 | Token file has permissive permissions, wrong owner, or is a symlink |
| `forbidden_registry_target` | 78 | The target registry is refused by policy |
| `invalid_endpoint_url` | 64 | Malformed `--fulcio-url` or `--rekor-url` |
| `provenance_version_unsupported` | 64 | `--type` resolved to a SLSA provenance predicate below v1.0; pass `--type slsaprovenance1` |
| `unsigned_type_unsupported` | 64 | No signing identity is visible and `--type` did not resolve to a CycloneDX or SPDX predicate |
| `internal` | 1 | Unexpected internal error |

**Human-readable output** (default format) states the trust class outright rather than leaving it to be inferred from missing rows — a `Signature` field reads `signed` or `unsigned (attached without an identity)`, and the three certificate rows are present only when signed.

**Example — attach a CycloneDX SBOM in CI**

```yaml
- name: Attest SBOM
  run: |
    cyclonedx-cli ... > sbom.json
    ocx package attest \
      -p linux/amd64 \
      --predicate sbom.json --type cyclonedx \
      registry.example/pkg:1.0
```

The same ambient GitHub Actions OIDC token [`sign`][cmd-package-sign] picks up automatically applies here — no `--identity-token-*` flag is needed.

#### `sbom` {#package-sbom}

Lists, or extracts, the SBOM attestations a package manifest carries — the read-side counterpart to [`attest`][cmd-package-attest]. A manifest can carry two kinds: a **signed** attestation, a [DSSE][dsse] bundle with a Fulcio certificate and a Rekor entry behind it, and an **unsigned** attach, a raw referrer with no signature over it at all.

Which of the two you get back, and whether anything is checked, is decided per invocation by one of two modes:

- **`--verify`** — every listed document carries a signature that passed every check [`verify --attestation`][cmd-package-verify-attestations] runs (referrer discovery, the Fulcio/Rekor/identity pipeline, the Statement's subject-digest binding). An unsigned attach is **refused**, never listed: the policy names who must have signed, and this document has no signer (exit 77 when it is all the package carries). This is the default whenever `--certificate-identity`/`--certificate-oidc-issuer` are given, or a [`[[trust.policy]]`][config-trust] covers the package.
- **`--no-verify`** — nothing is checked and no cryptography runs at all. Signed bundles and raw attachments alike are read for their document and reported `verified: false`, with no signer identity, because none was checked. This is the default when no identity source resolves, and it is what makes `ocx package sbom` work with no Sigstore setup: a consumer who has configured no trust policy can still read a published SBOM.

Naming both flags is not an error — the later one wins, as with every `--x`/`--no-x` pair in ocx — but `--no-verify` cannot be combined with `--certificate-identity`/`--certificate-oidc-issuer` (exit 64): supplying an identity while refusing to check it is contradictory rather than overridden. `--verify` with no identity source at all is also exit 64 — verification was demanded and nothing was named to verify against.

An unverified entry is never dressed up as a verified one. It carries `verified: false` in both the plain-text and JSON forms, no certificate fields, and the listing itself reports which mode produced it in `summary.verification` — so a script never has to infer why a row is unverified. See [Attestations][ug-attestations-attach] for when a package carries which kind.

**Usage**

```shell
ocx package sbom [OPTIONS] --platform <PLATFORM> <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier to list SBOM attestations for (`registry/repo:tag[@digest]`).

**Options**

| Name | Short | Default | Purpose |
|------|-------|---------|---------|
| `--platform` | `-p` | *(required)* | Target platform — selects the single-platform manifest under the image index |
| `--type <TYPE>` | — | *(any type)* | Restrict to one [predicate type][cmd-package-attest] |
| `--output <PATH\|->` | `-o` | — | Write the matched predicate's bytes, byte-exact as the publisher wrote them, to `PATH`, or to stdout with `-`. Refuses more than one matching attestation (exit 65, `multiple_attestations`) — naming every candidate's referrer digest and every distinct predicate type in the set, since there is no correct one to pick silently. Under `--no-verify` the document was not checked, so one warning line naming the referrer digest goes to stderr; the written bytes are unaffected. `-` refuses a TTY destination (exit 64) — piped bytes are not something a terminal should render raw |
| `--summary` | — | `false` | Augments the listing rather than replacing it: each plain-text row's Detail column gains component-count context (spec version, component count, top-level component name); each JSON entry gains a `summary` object, which also carries `serial_number` — a JSON-only field, never shown in the plain-text form. Restricted to `specVersion` 1.5-1.7; any other predicate type or an out-of-range CycloneDX version refuses **that entry** — it moves to `refused` with `reason_kind` `sbom_summary_failed`, naming the version it read and the `--type cyclonedx` remedy — never a silently empty summary and never the whole listing, so one unreadable document among five costs you that one |
| `--certificate-identity` / `--certificate-oidc-issuer` | — | *(policy-resolved)* | Same identity-resolution rule as [`verify`][cmd-package-verify] |
| `--sigstore-trusted-root` | — | *(public-good root over TUF)* | Same as [`verify`][cmd-package-verify] |
| `--rekor-url` | — | (`[trust.sigstore].rekor_url`, else `https://rekor.sigstore.dev`) | [Rekor][rekor] transparency-log endpoint |
| `--no-cache` | — | `false` | Bypass the per-registry referrers-capability cache for this invocation |
| `--verify` | — | *(when an identity source resolves)* | Require a verified signature; refuse unsigned attachments. Exit 64 when no identity source resolves — nothing was named to verify against |
| `--no-verify` | — | *(when no identity source resolves)* | List every document without verifying anything. Conflicts with `--verify` and with the certificate flags (exit 64) |

`--output` and `--summary` are mutually exclusive with each other and with the default listing mode. `--summary` works in both verification modes, on whatever the mode listed.

**Exit codes**

Shares [`verify`][cmd-package-verify]'s exit-code taxonomy under `--verify` — 79 when nothing verifies, 65 for any data-integrity failure, 78 for a trust-root or policy problem, 83/84 for Rekor/Referrers unavailability. Under `--no-verify` the trust-material codes — 78 (trust root or policy), 77 (identity), 83 (Rekor), and the 65 signature classes — are unreachable, because no trust material is consulted; 84 remains reachable (the referrers capability is still probed), and so does 64 for an invalid `--rekor-url`, which is validated before the mode is resolved. Four `sbom`-specific additions:

| Code | Condition |
|------|-----------|
| 77 | `unsigned_rejected_by_policy` — an unsigned attach was found and this run demands a signature. Listed in `refused` when a signed attestation was also found; when unsigned attachments are all the subject carries, the refusal is promoted to the command's own error. `--no-verify` lists the same document instead |
| 65 | `MultipleAttestations` under `--output` — more than one attestation matches and none was named by `--type` |
| 65 | `sbom_media_type_unsupported` — a raw referrer's payload layer declares a media type outside the SBOM set. Reachable under `--no-verify` only, since `--verify` refuses raw referrers before reading them. Listed in `refused` when the scan found anything else on the subject; when it is the only candidate, the refusal is promoted to the command's own error |
| 64 | `--output -` requested on a TTY, or `--summary` combined with `--output`. `--no-verify` combined with a certificate flag is a clap parse error, no envelope. `no_identity_provided` — `--verify` demanded with no identity source to verify against: no certificate flags and no matching [`[[trust.policy]]`][config-trust] |

A scan that finds nothing at all — no signed attestation and no unsigned attach — is `AttestationNotFound` (79), the same as an unqualified [`verify --attestation`][cmd-package-verify-attestations] with no matching referrer. Under `--summary` an empty `entries` array is reachable at exit 0 — every document refused the summariser, so each one is reported in `refused` with `summary.status` `partial_failure`. The distinction is what was found, not what was listed: 79 means nothing at all was found, exit 0 with empty `entries` means every candidate was found but none could be read.

**JSON output** (`--format json`) — default listing mode

`--output` bypasses this envelope entirely, regardless of `--format`: the destination (a file, or stdout via `-`) receives the matched predicate's raw bytes and nothing else. Combining `--output <file>` with `--format json` leaves stdout empty — the bytes went to the file, and there is no listing to wrap in an envelope.

A verifying run (`--verify`, or an identity source resolving) over a manifest carrying one signed attestation:

```json
{
  "schema_version": 1,
  "command": "package sbom",
  "exit_code": 0,
  "data": {
    "summary": {
      "status": "success",
      "verification": "verified",
      "exit_code": 0,
      "total": 1,
      "verified": 1,
      "unverified": 0,
      "refused": 0
    },
    "entries": [
      {
        "predicate_type": "https://cyclonedx.org/bom",
        "verified": true,
        "subject_digest": "sha256:<64-hex>",
        "referrer_digest": "sha256:<64-hex>",
        "certificate_identity": "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main",
        "certificate_oidc_issuer": "https://token.actions.githubusercontent.com",
        "signed_at": "2026-04-19T12:00:00Z"
      }
    ],
    "refused": []
  }
}
```

The same manifest under `--no-verify`, which checks nothing and reads the bundle's payload anyway:

```json
{
  "summary": {
    "status": "success",
    "verification": "unverified",
    "exit_code": 0,
    "total": 1,
    "verified": 0,
    "unverified": 1,
    "refused": 0
  },
  "entries": [
    {
      "predicate_type": "https://cyclonedx.org/bom",
      "verified": false,
      "subject_digest": "sha256:<64-hex>",
      "referrer_digest": "sha256:<64-hex>"
    }
  ],
  "refused": []
}
```

`summary.verification` is `verified` or `unverified` and names the mode the whole listing was produced under. Branch on it, not on the rows: an `unverified` row means "nothing was checked" under `verification: "unverified"`, and cannot occur at all under `verification: "verified"`, where an unsigned attach is refused rather than listed.

A manifest carrying a signed attestation *and* a raw unsigned attach, read under `--verify` (or a matching policy): the signed document lists, the unsigned one moves to `refused`, and the whole command still exits 0 — a refusal beside a match is reported, not raised:

```json
{
  "summary": {
    "status": "partial_failure",
    "verification": "verified",
    "exit_code": 0,
    "total": 2,
    "verified": 1,
    "unverified": 0,
    "refused": 1
  },
  "entries": [
    {
      "predicate_type": "https://cyclonedx.org/bom",
      "verified": true,
      "subject_digest": "sha256:<64-hex>",
      "referrer_digest": "sha256:<64-hex>",
      "certificate_identity": "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main",
      "certificate_oidc_issuer": "https://token.actions.githubusercontent.com",
      "signed_at": "2026-04-19T12:00:00Z"
    }
  ],
  "refused": [
    {
      "referrer_digest": "sha256:<64-hex>",
      "reason": "SBOM referrer is attached without a signature, and verification is required; pass --no-verify to list it unverified",
      "reason_kind": "unsigned_rejected_by_policy"
    }
  ]
}
```

When the unsigned attach is the *only* candidate on the subject, there is nothing for the refusal to sit beside — it is promoted to the command's own top-level error instead of a `refused` row, exit 77.

Every entry carries `predicate_type`, `verified`, `subject_digest` and `referrer_digest`. `certificate_identity`, `certificate_oidc_issuer` and `signed_at` are present only when `verified: true` — omitted, not `null`, on an unverified entry, so an empty identity is never mistaken for a rendering failure. `summary.verified` and `summary.unverified` partition `entries`; `summary.total` is `verified + unverified + refused`.

A scan that examined and rejected candidates reports them in `refused`, never silently — `summary.status` is `partial_failure` whenever `refused` is non-empty, `success` otherwise. Plain-format listings truncate `refused` to the first 20 with a `... and N more (see --json)` trailer; `--json` is never truncated. Each `refused` entry carries `reason` (prose) and `reason_kind` (a frozen slug, e.g. `unsigned_rejected_by_policy`, `multiple_attestations`, `bundle_parse_failed`, `sbom_media_type_unsupported`) — scripts branch on `reason_kind`, never on `reason`. The verify pipeline's own refusals come first; a `--summary` document that could not be read follows them with `reason_kind` `sbom_summary_failed`, which is deliberately outside that slug set: the document was found (verified or not), and only the reading of its payload failed.

Under `--summary`, each entry gains a `summary` object:

```json
"summary": {
  "spec_version": "1.6",
  "serial_number": "urn:uuid:...",
  "component_count": 42,
  "top_level_component": "acme/widget"
}
```

`serial_number` and `top_level_component` are omitted, not `null`, when the document does not carry one.

**Example — list every SBOM a package carries**

```shell
ocx package sbom -p linux/amd64 registry.example/pkg:1.0
```

**Example — extract the CycloneDX SBOM to a file**

```shell
ocx package sbom -p linux/amd64 --type cyclonedx --output sbom.json registry.example/pkg:1.0
```

**Example — pipe a CycloneDX SBOM straight into another tool**

```shell
ocx package sbom -p linux/amd64 --type cyclonedx --output - registry.example/pkg:1.0 | jq .
```

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

Pass `--no-verify` (below), or set [`OCX_NO_VERIFY`][env-no-verify] for a CI-wide opt-out, to skip a policy-covered package's verification; the flag wins when both are set, and the bypass logs a single `WARN` per invocation. Under [`--offline`][arg-offline] (or [`OCX_OFFLINE`][env-offline]), verification reuses whatever trust material is already local — a trust root from any rung of the ladder ([`OCX_SIGSTORE_TRUSTED_ROOT`][env-sigstore-trusted-root], [`[trust.sigstore]`][config-trust-sigstore], `$OCX_HOME/sigstore/trusted-root.json`) or a warm `$OCX_HOME/state/trust_root/` cache entry — and fails closed with exit `78` when neither is available, rather than installing an artifact it could not check. See [Verify by default][guide-auto-verify] in the user guide for the full model.

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

Identifiers are OCI references (e.g. `kitware/cmake:3.28`), resolved through the index and auto-installed when missing. Because it auto-installs, a package covered by a [`[[trust.policy]]`][config-trust] is signature-verified before it runs — the same gate as [`package install`](#package-install) (see its auto-verify contract). The full reference body — stdin inheritance, process replacement on Unix, exit codes — is in the [`exec`](#exec) section. For project-tier execution driven by `ocx.toml`, use [`ocx run`](#run).

**Usage**

```shell
ocx package exec [OPTIONS] <PACKAGES>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<PACKAGES>`: OCI identifiers to resolve (e.g. `kitware/cmake:3.28`).
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

| Flag | Short | Description |
|------|-------|-------------|
| `-p`, `--platform` | | Target platform to consider. |
| `--clean` | | Start with a clean environment; only package-declared variables and `OCX_*` config vars reach the child. |
| `--self` | | Use the self view (expose `private` + `public` entries). Default: consumer view (`public` + `interface` only). |
| [`--lazy-mode <MODE>`](#arg-lazy-mode) | — | Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. `always` composes a shim instead of downloading content up front; the requested command's own invocation is what triggers materialization if it names one of the deferred package's entries. Typing `always` together with `--self` is a usage error (exit 64) — a shim is a consumer-facing launcher and `--self` selects the private view that bypasses launchers, so the two ask for contradictory things. An `always` merely *inherited* from `OCX_LAZY_MODE` is not: `--self` outranks it and composes eagerly. | *(inherit from `OCX_LAZY_MODE`; there is no `ocx.toml` to consult on this OCI-tier command)* |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` -> `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends); `SEP` qualifies `list` only (`--env GODEBUG:list:,=gctrace=1`) and, if omitted, inherits whatever separator another contributor to the key already declared, or a single space if none did. A relative `path` value resolves against the **current directory**. Applied last, so it overrides every package-declared variable. This is a per-invocation override, not project configuration -- it does **not** make this command read `ocx.toml`. A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). See the `PATH` override warning under [`ocx run`](#run). | — |
| `-h`, `--help` | | Print help information. |

#### `env` {#package-env}

Print the resolved environment variables for one or more OCI-tier packages.

Output format is controlled by the root [`--format`](#arg-format) flag (default: `plain`). Plain format outputs an aligned table with `Key`, `Type` and `Value` columns. JSON format (`ocx --format json package env`) outputs `{"entries": [...], "binaries": [...], "entrypoints": [...], "integrations": [...], "advisories": [...]}`. `entries` is unchanged from before this field existed. `binaries` and `entrypoints` are top-level sibling arrays — not nested inside `entries` — of `{"name": "...", "package": "..."}` objects: one entry per admitted package's declared [executables][reference-binaries] (`binaries`) or [entry points][entry-points] (`entrypoints`). `package` is the canonical resolved identifier that declared the claim (`registry/repo[:tag]@digest` — the tag may be absent, so a tagless digest-pinned form is legal). Both arrays are always present, possibly empty.

`integrations` is a fourth top-level sibling array of `{"namespace": "...", "package": "...", "payload": ...}` objects — one row per (declaring package, [integration namespace][reference-integrations]) pair, `payload` the interpolated block OCX never interprets or merges. Two packages declaring the same namespace produce two rows, never one merged row — a row count exceeding the distinct-namespace count is the visible proof nothing merged. The array is present, with attribution, even for a single root package — it is never collapsed to a bare object or omitted. Like `binaries`/`entrypoints`, it is always `[]` under `--self` (integrations reach only the interface surface a consumer sees) and never appears in `--shell`/`--ci` output. See [Integrations][reference-integrations] for the field's grammar, size caps, and interpolation rules.

`advisories` is a fifth top-level sibling array of `{"kind": "...", "package": "...", "key": "...", "message": "..."}` objects, one per [deferred tool][in-depth-lazy-loading] whose declared metadata could not be fully validated at compose time (`key` is present only for the two variants that name an environment variable) — always present, empty unless a package composed with [`--lazy-mode always`](#arg-lazy-mode) triggered one; warning-only, never a compose failure.

Use `--shell[=NAME]` for eval-safe shell export lines — the only sourceable form.

In plain format, the `Key`/`Type`/`Value` table itself is unchanged — a hint line follows it summarizing availability whenever any binaries, entry points, or integration namespaces are admitted, e.g. `5 binaries available (cmake, ctest, cpack, ...); 2 integration namespaces (com.jetbrains, com.microsoft.vscode); use --format json for the full list`. The integrations clause names namespace keys only — payloads never render in plain output, for the same reason the `entries` table gained no fourth column. None of the three arrays ever appears in `--shell`/`--ci` output — those channels emit only shell-export lines / CI sink writes.

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
| `--self` | | Self view: emits `private` + `public` entries. Default: consumer view (`public` + `interface`). `integrations` is always `[]` under `--self` — integrations reach only the interface surface, regardless of view. |
| [`--lazy-mode <MODE>`](#arg-lazy-mode) | | Top tier of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder]. `always` composes a shim instead of downloading content up front. Has no effect together with `--candidate`/`--current`, which always resolve a materialized package. Typing `always` together with `--self` is a usage error (exit 64) — a shim is a consumer-facing launcher and `--self` selects the private view that bypasses launchers, so the two ask for contradictory things. An `always` merely *inherited* from `OCX_LAZY_MODE` is not: `--self` outranks it and composes eagerly. |
| `--shell[=NAME]` | | Emit eval-safe shell export lines for the named dialect. Same conventions as root [`ocx env --shell`](#env-root). Mutually exclusive with `--ci`. |
| `--ci[=PROVIDER]` | | Write the resolved environment into the CI system's persistence channel for later pipeline steps. `PROVIDER` ∈ `github` / `github-actions`, `gitlab` / `gitlab-ci`. Bare `--ci` auto-detects. Equals-form required. Mutually exclusive with `--shell`. |
| `--export-file=PATH` | | Write [GitLab CI/CD][gitlab-ci-export-docs] JSON-lines to `PATH`. Requires `--ci=gitlab`; exit 64 for `--ci=github` or without `--ci`. |
| `--show-patches` | | Annotate each entry with its origin. When [`[patches]`][config-patches] is configured, companion overlay entries are appended after the package's own entries; this flag adds a `Source` column to the plain table (a `"source"` object in JSON) naming the descriptor rule and companion that produced each overlay entry. No effect when `[patches]` is not configured. Mutually exclusive with `--shell` and `--ci`. |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` -> `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends); `SEP` qualifies `list` only (`--env GODEBUG:list:,=gctrace=1`) and, if omitted, inherits whatever separator another contributor to the key already declared, or a single space if none did. A relative `path` value resolves against the **current directory**. Applied last, so it overrides every package-declared variable. This is a per-invocation override, not project configuration -- it does **not** make this command read `ocx.toml`. A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). See the `PATH` override warning under [`ocx run`](#run). | — |
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
`patches.snapshot.json` beside `ocx.lock` (or in `$OCX_HOME` under `--global`).

Set [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot] to the file's path so all subsequent
composition prefers the pinned digests over live tag lookups. Adopting a snapshot is a
deliberate opt-in and is independent of [`--frozen`][arg-frozen], which scopes to the
package tier: freeze the patch tier by pointing that variable at this file. Companions are
pinned per `repository:tag`, so a descriptor naming one repository at two tags freezes both
versions independently.

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
| 65 | An existing snapshot on the read path carries a format version this `ocx` does not read — re-run this command to rewrite it. |
| 74 | I/O error writing the snapshot file. |
| 78 | No `ocx.lock` found for the project tier (run `ocx lock` first). |

#### `patch sync` {#patch-sync}

Re-fetches every patch descriptor for all installed packages and the global descriptor. Installs
any newly-referenced companion packages. Requires network access.

This command also picks up patches for packages installed before the `[patches]` tier was
configured. All states are re-checked regardless of what was previously recorded. Running
`patch sync` is equivalent to `ocx index update` for the patch tier — not to the similarly-named
`ocx index sync`, despite the shared verb.

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
| `--companion-archive <PATH>` | | Local archive for a companion package; avoids a registry round-trip. Repeatable for multiple companions. There is no `-i` flag to name the companion — the archive's metadata sidecar (`<archive-stem>-metadata.json`, the same naming [`ocx package test`][cmd-package-test]'s `--metadata` flag defaults to) must carry an `identifier` field matching one of the descriptor's companion entries exactly: registry, repository, and tag. A bare identifier (no registry) qualifies against your configured default registry, not the `[patches]` registry. |
| `--platform <PLATFORM>` | `-p` | Target platform for composing the environment. Defaults to host platform. |
| `--registry <HOST/PATH>` | | Patch registry to compose against, e.g. `registry.corp.example/ocx-patches`. Overrides the configured [`[patches]`][config-patches] tier, so you can preview a descriptor against a new patch registry without a config block. Defaults to the configured registry. |
| `--script <FILE>` | | Starlark test script to run in the composed environment. Mutually exclusive with `-- COMMAND`. |
| `--env <KEY[:TYPE[:SEP]]=VALUE>` | — | Set an environment variable for this invocation only. Repeatable; later occurrences win over earlier ones for the same key. Splits on the **first** `=`, so `--env FOO=a=b` yields `FOO` -> `a=b`. `TYPE` is `constant` (replaces, the default when omitted), `path` (prepends), or `list` (appends); `SEP` qualifies `list` only (`--env GODEBUG:list:,=gctrace=1`) and, if omitted, inherits whatever separator another contributor to the key already declared, or a single space if none did. A relative `path` value resolves against the **current directory**. Applied last, so it overrides every package-declared variable. This is a per-invocation override, not project configuration -- it does **not** make this command read `ocx.toml`. A bare `--env FOO` with no `=`, a `TYPE` that names no modifier or is empty, a `SEP` that is empty, contains `=`, contains a newline or carriage return, qualifies a non-`list` type, or edges a `list` value, an invalid variable name, or an `OCX_*`/`__OCX_*` key is rejected (exit 64). See the `PATH` override warning under [`ocx run`](#run). | — |
| `-h`, `--help` | | Print help information. |

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Environment printed, or the trailing command/script exited 0. |
| *(child's exit code)* | With a trailing command, the child's exit code is forwarded unchanged — a command that exits 7 makes `patch test` exit 7. |
| 64 | No patch registry available — pass `--registry <HOST/PATH>`, configure a `[patches]` tier, or set `OCX_PATCHES` before testing; a `--companion-archive` metadata sidecar has no `identifier` field; or its `identifier` does not match a companion the descriptor names for the base (naming the nearest entry it found). |
| 65 | Descriptor JSON is malformed or the version is unsupported; or two contributors to one env key declared conflicting list separators (see [Separator agreement][env-composition-list-separator]). |
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
| `test` | Validate a candidate config file locally and preview the configuration it would produce, without publishing or adopting anything (operator side). |
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
`$OCX_HOME/config.toml` only on success. A fetch failure during first adoption, or while
self-healing a wiped or mismatched snapshot, leaves no partial state and fails the
command.

A bare re-run against an already-adopted seed reconciles it every time, not just once at
onboarding: the source is re-fetched, and a newer digest replaces the snapshot in place
(`refreshed`) while unchanged content just confirms it (`already_adopted`, now verified
rather than assumed). That re-sync is best-effort — a fetch failure warns on stderr,
keeps the existing snapshot, and still exits 0 (`refresh_unavailable`); first adoption,
the self-heal case above, and a failure writing the refreshed snapshot to disk stay
hard-fail.

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
both commands with one schema. Statuses: `adopted`, `already_adopted`, `refreshed`,
`refresh_unavailable`, `cleared`, `dirty`, `would_adopt`, `would_refresh`.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Adopted, already adopted, refreshed, cleared, or `--dry-run` reported — including a re-sync of an already-adopted seed that could not reach the registry (`refresh_unavailable`; the existing snapshot is kept). |
| 64 | Nothing to set up — no `--managed-config`, no `OCX_MANAGED_CONFIG`, no existing seed. |
| 65 | The fetched managed-config package is malformed — digest mismatch, no `any/any` entry, missing `config.toml`, over 64 KiB, or invalid TOML. |
| 69 | Registry unreachable while fetching the snapshot. |
| 74 | Writing the snapshot or the `[managed]` fence failed. |
| 78 | The reference is not a valid OCI identifier, or a system-locked tier would be redirected or cleared. |
| 79 | The managed-config package does not exist in the registry. |
| 80 | Authentication failed while fetching the snapshot. |
| 82 | The `[managed]` fence carries user edits and `--force` was not passed. Nothing was touched. |

Codes 65, 69, 74, 78, 79, and 80 apply to the fetch that establishes the seed — first
adoption or self-heal of a wiped or mismatched snapshot. The re-sync of an
already-adopted seed is best-effort instead: a failed re-sync reports
`refresh_unavailable` and still exits 0.

::: tip CI recipe
`ocx config setup --managed-config <ref>` persists the seed and reconciles it on every
invocation — a job that re-runs `config setup` each time picks up newly published
content without a separate [`ocx config update`](#config-update) step, and a failed
re-sync keeps the last-known-good snapshot rather than failing the job. For ephemeral
runners where persisting is pointless, the env-var pairing (`OCX_MANAGED_CONFIG=… ocx
config update`) works without writing a seed — see
[`OCX_MANAGED_CONFIG`][env-ocx-managed-config]. Use `ocx config update` directly
whenever a stale or unreachable snapshot must fail the job instead of being silently
tolerated.
:::

#### `config test` {#config-test}

Runs the same checks [`config push`](#config-push) enforces before publishing — parses as
an ocx config, carries no `[managed]` section, stays within 64 KiB — against a candidate
file already on disk, then reports the configuration this machine would resolve if the
payload were adopted: the effective `[registry]` default, `[registries]`, `[mirrors]`, and
`[patches]` tiers, plus the machine's own `[managed]` posture. It touches the network for
nothing — validation and the merge preview are both local — and never publishes, adopts, or
writes anything.

The preview reproduces the adoption fold order exactly: the machine's discovered tiers
(built-in defaults, system, user, `$OCX_HOME`) first, then the candidate payload, then any
explicit [`--config`](#arg-config)/[`OCX_CONFIG`][env-config] overlay — never the machine's
*current* managed snapshot, which the candidate stands in for. An explicit overlay therefore
wins over the candidate in the report, exactly as it would once the payload is adopted, and
only for the keys it actually sets — where the overlay is silent, the candidate's value still
wins over the machine's own.

Past parsing, the merged result is run through the same gates every ocx invocation applies to
its own config: an invalid `[mirrors."<host>"]` entry — an unparseable URL, or a plain-HTTP
scheme not covered by an insecure-registries allowlist — fails the command (any forwarded
[`OCX_MIRRORS`][env-ocx-mirrors] entries are folded in first, same as ordinary resolution),
and so does an empty `[patches] registry = ""`. A payload that parses cleanly can still be one
no machine could actually start under; catching that is the point of previewing. `[patches]`
in the report reflects the same precedence an ordinary command uses: the merged config's own
`[patches]` when it declares one, otherwise the forwarded [`OCX_PATCHES`][env-ocx-patches]
tier.

Keys the config schema does not recognize are listed under `unknown_keys` as warnings, never
a rejection — an unknown key is equally a typo (`registry.defalt`) and a setting a newer ocx
understands, and the report cannot tell the two apart. Coverage is best-effort: a
`[mirrors."<host>"]` table is parsed value-first from raw TOML, so a typo inside one mirror
entry is not caught here — it is not a schema field, so it can never be "unrecognized" the
way `registry.defalt` is.

**Usage**

```shell
ocx config test <CONFIG>
```

**Arguments**

| Argument | Description |
|----------|-------------|
| `CONFIG` | Path to the candidate config file to check. Required. |

**Output** — plain: a `Field`/`Value` table; rows with no payload for this candidate (no
`[patches]`, no configured `[managed]` tier, no unknown keys) are omitted. JSON: a fixed
shape (`candidate`, `valid`, `registry_default`, `registries`, `mirrors`, `patches`,
`managed`, `unknown_keys`) — every field is always present, with `null`/`[]` where a tier is
unconfigured, so a consumer can key on `.valid`/`.unknown_keys` without probing for the
field first.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Valid — report printed. Unknown keys are warnings and do not change this. |
| 74 | Reading the candidate file failed for a reason other than not found or permission denied. |
| 77 | The candidate file could not be read — permission denied. |
| 78 | Payload rejected — not valid config TOML, contains a `[managed]` section, or exceeds 64 KiB (same rejection set as [`config push`](#config-push)); or the merged result fails a resolution gate — an invalid or plain-HTTP [`[mirrors]`][config-mirrors] entry, or `[patches] registry = ""`. |
| 79 | The candidate file does not exist. |

::: tip Learn more
[Managed-configuration walkthrough][user-guide-managed-config] — where `config test` fits between authoring and publishing.
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
setting [`OCX_MANAGED_CONFIG`][env-ocx-managed-config]; re-running either setup command
also refreshes the snapshot, but `ocx config update` is the surface for explicit version
pins, rollback, and `--pause`/`--resume`. See
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
[oci-annotations]: https://github.com/opencontainers/image-spec/blob/main/annotations.md
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[ghcr-repo-link]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#labelling-container-images
[sigstore]: https://www.sigstore.dev/
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[in-toto]: https://github.com/in-toto/attestation
[dsse]: https://github.com/secure-systems-lab/dsse
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
[in-depth-indices-layout]: ../in-depth/indices.md#local-layout
[in-depth-indices-update]: ../in-depth/indices.md#update-modes
[in-depth-signing]: ../in-depth/signing.md
[cmd-lock-file]: ../in-depth/project.md#lock-format
[ug-promoting]: ../user-guide/promoting-packages.md
[in-depth-indices-public]: ../in-depth/indices.md#public-index
[in-depth-indices-servable]: ../in-depth/indices.md#servable
[in-depth-lazy-loading]: ../in-depth/lazy-loading.md
[in-depth-lazy-loading-ladder]: ../in-depth/lazy-loading.md#deferred-tools-ladder
[config-trust-sigstore]: ./configuration.md#keys-trust-sigstore
[signing-limitations]: ../in-depth/signing.md#current-limitations
[signing-offline]: ../in-depth/signing.md#offline-verification
[signing-deferred]: ../in-depth/signing.md#deferred-future-work
[signing-cosign-interop]: ../in-depth/signing.md#cosign-interop
[in-depth-self-hosted-sigstore]: ../in-depth/self-hosted-sigstore.md

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-ocx-lazy-mode]: ./environment.md#ocx-lazy-mode
[env-ocx-lazy-report]: ./environment.md#ocx-lazy-report
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-ocx-frozen]: ./environment.md#ocx-frozen
[env-ocx-patch-snapshot]: ./environment.md#ocx-patch-snapshot
[env-no-config]: ./environment.md#ocx-no-config
[env-config]: ./environment.md#ocx-config
[env-ocx-mirrors]: ./environment.md#ocx-mirrors
[env-ocx-patches]: ./environment.md#ocx-patches
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
[env-sigstore-trusted-root]: ./environment.md#ocx-sigstore-trusted-root
[env-offline]: ./environment.md#ocx-offline
[env-no-verify]: ./environment.md#ocx-no-verify

<!-- external: completions -->
[clap-complete]: https://docs.rs/clap_complete/latest/clap_complete/

<!-- reference -->
[config-ref]: ./configuration.md
[config-mirrors]: ./configuration.md#keys-mirrors
[config-patches]: ./configuration.md#keys-patches
[config-managed]: ./configuration.md#keys-managed
[config-managed-required]: ./configuration.md#keys-managed-required
[config-project-env]: ./configuration.md#project-config-env
[config-project-package]: ./configuration.md#project-config-package
[config-project-groups]: ./configuration.md#project-config-groups
[config-schemas]: ./configuration.md#schemas
[in-depth-versioning-cascades]: ../in-depth/versioning.md#cascades
[env-ocx-managed-config]: ./environment.md#ocx-managed-config
[user-guide-managed-config]: ../user-guide.md#managed-config
[env-composition-project-env]: ./env-composition.md#project-env
[env-composition-list]: ./env-composition.md#composition-order-list
[env-composition-list-separator]: ./env-composition.md#composition-order-list-separator
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
[ug-attestations-attach]: ../user-guide/attestations.md#attestations-attach

<!-- commands (package-test options) -->
[cmd-package-describe]: #package-describe
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
[cmd-package-sign]: #package-sign
[cmd-package-verify]: #package-verify
[cmd-package-verify-attestations]: #package-verify-attestations
[cmd-package-attest]: #package-attest
[cmd-package-sbom]: #package-sbom
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
[reference-platforms-compatibility]: ./platforms.md#compatibility

<!-- reference (package-create --bin-scan / env binaries+entrypoints attribution) -->
[reference-binaries]: ./metadata.md#executables
[reference-binaries-none-vs-empty]: ./metadata.md#executables-none-vs-empty
[reference-integrations]: ./metadata.md#integrations
[reference-env-path]: ./metadata.md#env-path
[reference-env-visibility]: ./metadata.md#env-entry-visibility
[reference-env-self-alias]: ./metadata.md#env-interpolation-self
[reference-env-render]: ./metadata.md#env-interpolation-render
[reference-env-interpolation]: ./metadata.md#env-interpolation
[reference-visibility]: ./metadata.md#dependencies-visibility
[reference-dependencies-authoring]: ./metadata.md#dependencies-authoring-vs-published

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

<!-- commands (package announce) -->
[cmd-package-announce]: #package-announce
[env-ocx-announce-token]: ./environment.md#ocx-announce-token
[config-registries-trusted-hosts]: ./configuration.md#keys-registries-trusted-hosts

<!-- commands (package cascade) -->
[cmd-index-update]: #index-update
[config-registries-index]: ./configuration.md#keys-registries-index
[in-depth-indices]: ../in-depth/indices.md
[mdn-if-match]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/If-Match
