---
layout: doc
outline: deep
---

# Environment Composition

This page is the reference-level specification for how OCX assembles environment variables and selects which toolchain tier is active. For the motivation behind these design decisions, see [Environment composition in the user guide][user-guide-global].

## Strict Isolation {#strict-isolation}

OCX enforces a hard boundary between the global toolchain and project-tier resolution. The rule is unconditional:

> **Global tools never compose into, supplement, or fall back into a project's resolved environment.**

This applies without exception to:

- [`ocx run`][cmd-run] — project-tier env-composition command. Reads `ocx.toml` + `ocx.lock`. The global toolchain (`$OCX_HOME/ocx.toml`) is not consulted, not merged, and not used as a fallback for tools the project does not declare.
- [`ocx exec`][cmd-exec] — OCI-tier env-composition command. Never reads any `ocx.toml`, whether project or global. Takes OCI identifiers directly.

Both commands are hermetic: the environment they produce is determined entirely by their declared inputs. An undeclared tool is absent, never filled from the global set.

:::info Why hard isolation instead of gap-fill?

[Volta][volta] pioneered this model for Node.js: global tools are hidden when a project toolchain is active. The alternative — filling in tools the project does not declare from the global set — produces the reproducibility hole OCX is designed to close: collaborators without the same `$OCX_HOME/ocx.toml` get different resolved environments.

:::

### PATH precedence model {#strict-isolation-shell-hook}

OCX enforces isolation by **PATH precedence**, not PATH stripping. The global toolchain's `current/entrypoints/` directory sits on `PATH` at login time (via `$OCX_HOME/env.sh` sourced from the login profile). When a project toolchain is activated — via `ocx run` or `ocx direnv` — the project tools are **prepended** to `PATH`, shadowing any global tools of the same name.

There is no PATH strip, no `# ocx: global toolchain suppressed` comment, and no `_OCX_APPLIED` fingerprint. The per-prompt shell hook (`ocx shell hook`) has been removed entirely. Isolation is a static consequence of PATH ordering: project tools appear earlier in `PATH` than global tools.

For `ocx direnv`, the `.envrc` evaluates [`ocx direnv export`][cmd-direnv-export] on every directory entry. This emits only the project tools' PATH entries, which [direnv](https://direnv.net/) prepends before the ambient `PATH` — global tools remain reachable for tools not declared by the project, but project-declared tools take priority.

### Idempotent re-application {#strict-isolation-idempotent}

Every `PATH` prepend OCX emits is **idempotent with move-to-front semantics**. Re-applying the same output — direnv re-evaluating `.envrc` on each directory change, a captured snippet re-read from a profile, or a tool re-running [`ocx env --shell`][cmd-env-root] — never grows `PATH`. A directory already present is removed from its old position and placed at the front, so the most recent activation wins lookup and the variable stays a fixed length.

The emitted shell statements are **self-contained**: they depend on no `ocx` process, no guard variable, and no helper function. That makes them safe to capture into a profile —

```sh
ocx package env cmake --shell bash >> ~/.bashrc
```

— where every later shell re-sources the block with `ocx` possibly absent and the directory still lands exactly once, at the front. The same move-to-front dedup applies to the in-process environment ([`ocx run`][cmd-run], [`ocx exec`][cmd-exec]) and to CI exports (`--ci=github`, `--ci=gitlab`).

::: info Shell-specific requirements
All ten supported shells — bash, zsh, ash, ksh, dash, fish, PowerShell, elvish, nushell, and Windows `cmd` — emit idempotent move-to-front output. The `cmd` form rebuilds `PATH` with `%VAR:search=%` substring deletion (no `FOR /F`, no delayed expansion, so `!`-bearing paths stay intact) and matches segments case-insensitively, the way Windows `PATH` lookup does. A couple of shells have version floors: elvish needs the `str:` module (0.16+); nushell needs the auto-list `PATH` conversion (0.101+).
:::

### What "hermetic" means for `ocx run` {#strict-isolation-run}

`ocx run` reads exactly two files: `ocx.toml` and its sibling `ocx.lock`. The resolved environment consists of the tools those files declare — no more. If a tool is not in `ocx.toml`, it is not in the child environment, regardless of what is installed globally or what is on the parent shell's PATH.

Naming a binding subset (`ocx run cmake -- …`) narrows composition further: only the named tools are resolved to a host leaf and installed. A `-g` group selects the *namespace* for name resolution, not a mandate that every tool in it be available — an unrelated tool in scope with no leaf for the current host does not block a narrowly-named run. Omit the names and the whole scope must resolve.

By default `ocx run` **inherits** the spawning shell's environment and merely **prepends** the composed tool `bin/` directories to `PATH` — ambient parent-shell `PATH` entries remain reachable after the project tools. The default is *not* hermetic. Pass `--clean` for a hermetic environment that drops the inherited environment and exposes only the composed tool set, exactly like `exec --clean`.

### What "hermetic" means for `ocx exec` {#strict-isolation-exec}

`ocx exec` takes one or more OCI identifiers on the command line. It resolves each identifier, composes the declared environment variables from the resolved packages, and spawns the command with that environment. No `ocx.toml` is read — not the project file, not the global file. The entire operation is stateless with respect to project configuration.

## Patch Opt-Out Scope {#patch-opt-out-scope}

A project can opt a base package out of the [`[patches]`][config-patches] companion overlay
with [`no-patches`][config-patches-no-patches]. That opt-out lives in `ocx.toml`, so it is
subject to the same explicit-scope rule as every other project setting: it applies only where
the project file is directly read, and nowhere else.

Without an explicit forwarding step, a launcher re-entry would silently break that promise. A
package's generated entrypoint re-enters ocx through the hidden `ocx launcher exec`
subcommand, resolving its own base from a synthetic content-addressed identifier rather than
`ocx.toml` — so on its own it has no way to know the parent project opted this base out, and
would re-apply the companion the parent just suppressed.

[`ocx run`][cmd-run] closes that gap by forwarding the opt-out to the child process over
[`OCX_PATCHES`][env-ocx-patches]: alongside the resolved `[patches]` tier, it includes the
opted-out bases' canonical `registry/repository` keys **and** the content digest of each one
actually resolved that run. The digest leg is what a launcher's re-entry matches against,
since it has no repository path to compare. [`ocx env`][cmd-env-root] and
[`ocx direnv export`][cmd-direnv-export] read the same project config and honor the opt-out
directly in the environment they compose — they have no child launcher to forward it to.

A launcher invoked outside this chain — standalone, or re-entered through the OCI-tier
[`ocx package exec`][cmd-package-exec] — decodes no forwarded opt-out from its environment and
composes the companion overlay as if `no-patches` were never set. A
[system-required][config-patches-scopes] tier is unaffected either way: enforcement is not
subject to the opt-out at all. See [Per-package opt-out][patches-no-patches-guide] in the
Patching packages guide for the full walkthrough.

## Tier Selection {#tier-selection}

OCX has two toolchain tiers. Selection is always explicit — there is no implicit fallback from project to global.

| Tier | How to activate | File |
|------|----------------|------|
| Project | CWD walk finds `ocx.toml`; or `--project <path>`; or [`OCX_PROJECT`][env-ocx-project] | nearest `ocx.toml` ancestor |
| Global | `--global` flag; or [`OCX_GLOBAL`][env-ocx-global] | `$OCX_HOME/ocx.toml` |

The two flags are mutually exclusive — combining `--global` with `--project` exits with code 64 (`UsageError`).

**No implicit home-tier discovery.** Earlier versions of OCX fell back to `$OCX_HOME/ocx.toml` when the CWD walk found nothing. That behavior has been removed. The global toolchain is only active when explicitly requested. A CWD walk that finds nothing means no project tier is active — the command operates without a project context.

### Root `--global` affects these toolchain-tier commands {#tier-selection-commands}

`--global` is a root flag — it must appear before the subcommand (e.g. `ocx --global add ripgrep:14`). The following toolchain-tier commands are affected when `--global` is set:

| Command | With `--global` |
|---------|----------------|
| [`ocx add`][cmd-add] | Adds binding to the global file |
| [`ocx remove`][cmd-remove] | Removes binding from the global file |
| [`ocx lock`][cmd-lock] | Re-locks the global file |
| [`ocx upgrade`][cmd-upgrade] | Advances a binding in the global file |
| [`ocx pull`][cmd-pull] | Pre-warms packages declared by the global file |
| [`ocx run`][cmd-run] | Composes env from the global file + its lock |
| [`ocx env`][cmd-env-root] | Emits composed toolchain env for the global file |

## Visibility Surfaces {#visibility-surfaces}

Each OCX package declares two environment surfaces: the **interface surface** (what consumers see) and the **private surface** (what the package's own launchers see).

The `--self` flag on `exec`, `run`, `package env`, `package exec`, and `package deps` switches which surface is emitted:

| `--self` | Surface emitted | Use case |
|----------|----------------|----------|
| off (default) | Interface surface — vars where `has_interface()` is true | Human or CI script using the package |
| on | Private surface — vars where `has_private()` is true | Generated launchers invoking `ocx launcher exec` internally |

Generated launchers force `self_view = true` internally; they do not expose `--self` to callers.

## Composition Order {#composition-order}

When multiple packages contribute to an environment (via `ocx run -g GROUP1,GROUP2` or `ocx exec PKG1 PKG2`), env entries are **prepended** — the last tool walked has its `PATH` entries placed **first** in the resolved `PATH`. In `-g` argument order, groups listed **later** win PATH lookup.

For `ocx run`, the full order rule is:

> First by group-selection order (the order of `-g` flags, after `all` expansion, deduplicated); then alphabetical by binding name within each group.

See [In Depth — Project Toolchain → Composition order rule][in-depth-project-composition] for the worked example with `-g ci,all,release`.

<!-- external -->
[volta]: https://volta.sh/

<!-- commands -->
[cmd-add]: ./command-line.md#add
[cmd-exec]: ./command-line.md#exec
[cmd-env-root]: ./command-line.md#env-root
[cmd-lock]: ./command-line.md#lock
[cmd-pull]: ./command-line.md#pull
[cmd-remove]: ./command-line.md#remove
[cmd-run]: ./command-line.md#run
[cmd-upgrade]: ./command-line.md#upgrade
[cmd-direnv-export]: ./command-line.md#direnv-export
[cmd-package-exec]: ./command-line.md#package-exec

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-ocx-project]: ./environment.md#ocx-project
[env-ocx-patches]: ./environment.md#ocx-patches

<!-- configuration -->
[config-patches]: ./configuration.md#keys-patches
[config-patches-no-patches]: ./configuration.md#keys-patches-no-patches
[config-patches-scopes]: ./configuration.md#keys-patches-scopes

<!-- internal -->
[user-guide-global]: ../user-guide.md#global-toolchain
[in-depth-project-composition]: ../in-depth/project.md#running-composition-order
[patches-no-patches-guide]: ../user-guide/patches.md#patches-no-patches
