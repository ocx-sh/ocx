---
title: "ocx direnv"
---
<!-- doc_type: reference -->

## `ocx direnv` {#direnv}

direnv integration (init writes .envrc; export emits the env block).

Bare `ocx direnv` is shorthand for `ocx direnv init`, the once-per-project setup that writes a `.envrc`. The generated `.envrc` evaluates `ocx direnv export` on every directory entry.

```text
ocx direnv [COMMAND]
```

## `ocx direnv export` {#direnv-export}

Print stateless shell exports for the project toolchain (direnv entry point).

Reads the nearest project `ocx.toml` (no home-tier fallback) and its `ocx.lock`, and resolves the selected bindings. It prints a fresh block of bash export lines on every invocation, leaving diffing and caching to direnv.

You do not normally type it by hand; the generated `.envrc` evaluates it.

The output is always bash because direnv sources `.envrc` in a bash sub-shell and translates the result to the interactive shell itself.

A package missing from the object store is pulled on a miss (skip that with `--no-pull`). The pull is best-effort and only happens when a registry is reachable, so a missing package never blocks the prompt.

Otherwise it is noted on stderr and skipped. A stale lock warns on stderr and its digests are still used, and with no project `ocx.toml` in scope the command exits 0 with no output.

The `.envrc` line is yours to edit, for example `eval "$(ocx direnv export -g ci --env FORCE_COLOR=1)"`.

```text
ocx direnv export [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--group`, `-g` | `GROUPS` | Restrict the composition to the named group(s) |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--no-pull` |  | Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit |
| `--pull` |  | Materialize resolved packages into the object store, installing on a local miss |

## `ocx direnv init` {#direnv-init}

Write a `.envrc` wiring `ocx direnv export` into direnv.

Writes the file in the current directory; run `direnv allow` afterwards to activate it. The generated `.envrc` watches `ocx.toml` and `ocx.lock`, so direnv re-runs the hook whenever either changes, and the hook itself is `eval "$(ocx direnv export)"`.

Because `ocx direnv export` always emits bash, the file works for teams with different interactive shells.

```text
ocx direnv init [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--force` |  | Overwrite an existing `.envrc` in the current directory. Without it, an existing file exits 78 and is left untouched |
