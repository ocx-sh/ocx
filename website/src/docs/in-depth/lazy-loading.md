---
outline: deep
---
# Deferred Tools {#deferred-tools}

A project toolchain can declare a dozen tools and use three of them in any given job — a monorepo's `ocx.toml` might list every compiler, linter, and formatter the team owns, while a single CI step runs `eslint` and nothing else. [`ocx env`][cmd-env-root] and [`ocx exec`][cmd-run] compose the whole declared set by default, which means every tool's content downloads before the job's first command even starts.

`lazy-mode` changes when a tool's content downloads, not what ends up composed. A tool set to `always` still lands on `PATH` immediately — its declared names resolve, `ocx package which` finds it, `ocx exec <name>` sees it in scope — but the bytes behind it stay unfetched until the first invocation of one of its names. A job that never calls `eslint` never pays for it.

## Composing a shim {#deferred-tools-compose}

Every [env-composing command][cmd-run] accepts `--lazy-mode`:

<<< @/_scripts/lazy-loading/lifecycle.sh{sh}

<Terminal src="/casts/lazy-loading/lifecycle.cast" title="Deferring cmake until first use" collapsed />

Under `always`, OCX writes a small generated launcher per declared name into a shim directory and composes that directory onto `PATH` — the same PATH slot an eagerly-materialized [package root's `entrypoints/`][fs-packages] would occupy, just pointed at a directory with no `content/` yet. [`ocx package which`][cmd-which] and [`ocx pull`][cmd-pull]'s JSON report both say so explicitly: every entry carries a `kind` of `package` or `shim`, so a script can tell the two apart without probing the filesystem.

::: info Like containerd's lazy image pulls
[containerd][containerd]'s [stargz-snapshotter][stargz-snapshotter] does the same trade for container images: an `eStargz`-formatted layer lets a container start running before its files finish downloading, fetching each file lazily the first time a process opens it. OCX applies the same idea one layer up — at the tool level instead of the file level, and driven by the first process invocation instead of a filesystem read.
:::

### Resolution ladder {#deferred-tools-ladder}

`--lazy-mode` is a policy, not a one-off flag — it resolves through five tiers, most specific first, with `never` (eager) as the floor:

| Tier | Source |
|------|--------|
| 1 | `--lazy-mode` on the invoked command |
| 2 | `[package."<id>"]` in [`ocx.toml`][config-project-package] |
| 3 | `[group.<name>]` in [`ocx.toml`][config-project-groups] |
| 4 | The toolchain-level `lazy-mode` key in `ocx.toml` |
| 5 | [`OCX_LAZY_MODE`][env-ocx-lazy-mode] |
| — | Floor: `never` |

Each tier is independently optional; an absent tier means *inherit from the next one down*, never *this tier said `never`*. Setting `lazy-mode = "always"` at the toolchain level and `lazy-mode = "never"` on one package's own entry composes that one package eagerly while every other declared tool defers — the package-tier entry is a decision, not a gap.

## First invocation {#deferred-tools-materialize}

A shim's generated launcher is a small script, deliberately similar to the one an [entry point][entrypoints-ref] writes:

```sh
#!/bin/sh
exec "${OCX_BINARY_PIN:-ocx}" launcher shim '<pinned-id>' -- "$(basename "$0")" "$@"
```

It execs the hidden `ocx launcher shim` subcommand, passing the pinned identifier the shim was built for and the name the caller typed. That subcommand runs the ordinary pull — the same three-layer fetch, extract, and assemble pipeline an eager install uses — then resolves the requested name on the freshly materialized package's own `PATH` and executes it. Nothing about the pull is special-cased for laziness: a lazily materialized tool is byte-identical to the same tool installed eagerly.

Once materialized, the tool's real `entrypoints/` directory outranks the shim on `PATH` for every later invocation in that environment, so a second call never re-triggers the shim path. `ocx package which` reflects the same flip — its `kind` reports `shim` before the first use and `package` after.

### Progress during the download {#deferred-tools-report}

`lazy-report` controls whether that first-invocation download renders progress. It cannot be a flag on `ocx env` or `ocx exec`, because the process rendering it — `ocx launcher shim` — is a separate process the shell already exec'd into by the time any content moves; nothing survives from the composing command to tell it what to show. `lazy-report` therefore resolves independently, inside the shim process itself, from its own four-tier ladder — one tier shorter than `lazy-mode`'s, because there is no group to consult once composition is over:

| Tier | Source |
|------|--------|
| 1 | `--lazy-report` (only on `ocx launcher shim` — never typed by a user) |
| 2 | `[package."<id>"]` in `ocx.toml` |
| 3 | The toolchain-level `lazy-report` key in `ocx.toml` |
| 4 | [`OCX_LAZY_REPORT`][env-ocx-lazy-report] |
| — | Floor: `silent` |

`progress` opens a channel on the controlling terminal; where none exists — a Docker build, a CI runner, anything under `setsid` — it silently degrades to `silent` rather than failing. Errors always reach stderr regardless of this setting.

## Advisories {#deferred-tools-advisories}

Some `metadata.json` shapes only substitute cleanly once a package's content is on disk — a `${installPath}`-rooted value concatenated with something else, for instance, or a claim of zero `binaries` on a node that never got a chance to declare any. Composing such a package as a shim can't detect these the way an eager install's file scan does, so [`ocx env`][cmd-env-root] and [`ocx package env`][cmd-package-env] instead emit an `advisories` array in their JSON output — warning-only, never a compose failure — naming the affected package and, where relevant, the key.

## Windows {#deferred-tools-windows}

`lazy-mode` has no effect on Windows in this release: a tool resolved to `always` composes eagerly instead, with a debug-level log line noting why. This is not an error and produces no warning — the composed environment is simply the same one you would get without the flag.

## Garbage collection {#deferred-tools-gc}

A shim directory is kept alive by the same lock-pinned root set that keeps an eagerly-installed package alive — [`ocx clean`][cmd-clean] regenerates a collected shim on the next compose, exactly as it would re-pull a collected package.

[`ocx clean --force`][cmd-clean] is the one case where this differs from an installed package. A deferred tool has no [install symlink][fs-symlinks] pointing at it — only the lock pins reference it — and `--force`'s entire purpose is to suppress the lock-pinned root set for the run. So `--force` collects every shim directory unconditionally, the same way it already collects an unsymlinked eager package. The next `ocx env` or `ocx exec` regenerates whatever shims that composition needs; nothing is lost, but the first post-`--force` invocation of a deferred tool re-materializes it from scratch.

<!-- external -->
[containerd]: https://containerd.io/
[stargz-snapshotter]: https://github.com/containerd/stargz-snapshotter

<!-- in-depth -->
[entrypoints-ref]: ./entry-points.md

<!-- commands -->
[cmd-env-root]: ../reference/command-line.md#env-root
[cmd-run]: ../reference/command-line.md#exec
[cmd-pull]: ../reference/command-line.md#pull
[cmd-which]: ../reference/command-line.md#which
[cmd-package-env]: ../reference/command-line.md#package-env
[cmd-clean]: ../reference/command-line.md#clean

<!-- environment -->
[env-ocx-lazy-mode]: ../reference/environment.md#ocx-lazy-mode
[env-ocx-lazy-report]: ../reference/environment.md#ocx-lazy-report

<!-- reference -->
[config-project-package]: ../reference/configuration.md#project-config-package
[config-project-groups]: ../reference/configuration.md#project-config-groups

<!-- internal -->
[fs-packages]: ../user-guide.md#file-structure-packages
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
