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
- [`ocx package exec`][cmd-exec] — OCI-tier env-composition command. Never reads any `ocx.toml`, whether project or global. Takes OCI identifiers directly.

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
ocx package env kitware/cmake --shell bash >> ~/.bashrc
```

— where every later shell re-sources the block with `ocx` possibly absent and the directory still lands exactly once, at the front. The same move-to-front dedup applies to the in-process environment ([`ocx run`][cmd-run], [`ocx package exec`][cmd-exec]) and to CI exports (`--ci=github`, `--ci=gitlab`).

::: info Shell-specific requirements
All ten supported shells — bash, zsh, ash, ksh, dash, fish, PowerShell, elvish, nushell, and Windows `cmd` — emit idempotent move-to-front output. The `cmd` form rebuilds `PATH` with `%VAR:search=%` substring deletion (no `FOR /F`, no delayed expansion, so `!`-bearing paths stay intact) and matches segments case-insensitively, the way Windows `PATH` lookup does. A couple of shells have version floors: elvish needs the `str:` module (0.16+); nushell needs the auto-list `PATH` conversion (0.101+).
:::

### What "hermetic" means for `ocx run` {#strict-isolation-run}

`ocx run` reads exactly two files: `ocx.toml` and its sibling `ocx.lock`. The resolved environment consists of the tools those files declare — no more. If a tool is not in `ocx.toml`, it is not in the child environment, regardless of what is installed globally or what is on the parent shell's PATH.

Naming a binding subset (`ocx run cmake -- …`) narrows composition further: only the named tools are resolved to a host leaf and installed. A `-g` group selects the *namespace* for name resolution, not a mandate that every tool in it be available — an unrelated tool in scope with no leaf for the current host does not block a narrowly-named run. Omit the names and the whole scope must resolve.

By default `ocx run` **inherits** the spawning shell's environment and merely **prepends** the composed tool `bin/` directories to `PATH` — ambient parent-shell `PATH` entries remain reachable after the project tools. The default is *not* hermetic. Pass `--clean` for a hermetic environment that drops the inherited environment and exposes only the composed tool set, exactly like `exec --clean`.

### What "hermetic" means for `ocx package exec` {#strict-isolation-exec}

`ocx package exec` takes one or more OCI identifiers on the command line. It resolves each identifier, composes the declared environment variables from the resolved packages, and spawns the command with that environment. No `ocx.toml` is read — not the project file, not the global file. The entire operation is stateless with respect to project configuration.

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
| [`ocx update`][cmd-update] | Advances a binding in the global file |
| [`ocx pull`][cmd-pull] | Pre-warms packages declared by the global file |
| [`ocx run`][cmd-run] | Composes env from the global file + its lock |
| [`ocx env`][cmd-env-root] | Emits composed toolchain env for the global file |

## Visibility Surfaces {#visibility-surfaces}

Each OCX package declares two environment surfaces: the **interface surface** (what consumers see) and the **private surface** (what the package's own launchers see).

The `--self` flag on `package env`, `package exec`, `package test`, and `package deps` switches which surface is emitted. It is OCI-tier only — the project-tier commands do not accept it, because a toolchain is a consumer of every tool it declares and the self view leaves those tools' `entrypoints/` off `PATH`:

| `--self` | Surface emitted | Use case |
|----------|----------------|----------|
| off (default) | Interface surface — vars where `has_interface()` is true | Human or CI script using the package |
| on | Private surface — vars where `has_private()` is true | Generated launchers invoking `ocx launcher exec` internally |

Generated launchers force `self_view = true` internally; they do not expose `--self` to callers.

## Self-Referencing Values {#self-referencing}

A package's `env` values can reference each other, not just `${installPath}`. `${self.env.KEY}` resolves to the resolved value of this package's own earlier-declared `KEY` var, so a computed path or value is named once and reused instead of repeated in every `value` template that needs it — the same reuse an earlier [GitHub Actions][github-actions-docs] step's output, or a [Bazel][bazel-rules] `--define`, gives a workflow, applied to one package's own metadata.

```json
{
  "env": [
    { "key": "TOOL_HOME", "type": "constant", "value": "${self.installPath}/sdk" },
    { "key": "TOOL_CFG",  "type": "constant", "value": "${self.env.TOOL_HOME}/etc" },
    { "key": "TOOL_BIN",  "type": "path",     "value": "${self.env.TOOL_HOME}/bin" }
  ]
}
```

`TOOL_CFG` and `TOOL_BIN` both build on `TOOL_HOME` without repeating `${self.installPath}/sdk`. If the SDK subdirectory ever moves, one edit to `TOOL_HOME` propagates to every var built from it.

`${self.env.KEY}` may only reference a `KEY` declared **strictly earlier** in the same package's `env` array — a forward or self reference, or a reference to a key declared twice, is refused at publish time (see [`self.env`][metadata-env-self-env] on the metadata reference for the full resolution rules and the generator-order hazard). It resolves to `KEY`'s **resolved** value, not its unexpanded template, and resolution is **surface-independent**: it reads the same bytes regardless of `KEY`'s own [visibility](#visibility-surfaces) or which surface (`--self` on or off) is being composed. It is legal only in `env` values, never in entry-point `args`. See [Interpolation Tokens][metadata-env-interpolation] for the full token grammar shared by both.

::: warning A template fault anywhere in a package's `env` can fail the whole composition
Surface-independent resolution (above) means OCX cannot resolve only the vars a surface is
about to emit — a `public` var's `${self.env.KEY}` might name a `private` `KEY`, so every
declared var, regardless of its own visibility, is resolved before the composer decides what
crosses. A malformed template, a `required` [`path`][metadata-env-path] that does not exist on
disk, or an unresolvable `${self.env.KEY}` reference on any var — including one that will not
itself be emitted on the surface in play — fails the whole composition with exit 65. This holds
for [`ocx env`][cmd-env-root] / [`ocx package env`][cmd-package-env] and [`ocx run`][cmd-run] /
[`ocx package exec`][cmd-package-exec] alike, on either surface: a package's own metadata either
resolves in full or the composition refuses, independent of who is asking or which surface they
asked for.
:::

## Composition Order {#composition-order}

When multiple packages contribute to an environment (via `ocx run -g GROUP1,GROUP2` or `ocx package exec PKG1 PKG2`), env entries are **prepended** — the last tool walked has its `PATH` entries placed **first** in the resolved `PATH`. In `-g` argument order, groups listed **later** win PATH lookup.

For `ocx run`, the full order rule is:

> First by group-selection order (the order of `-g` flags, after `all` expansion, deduplicated); then alphabetical by binding name within each group.

See [In Depth — Project Toolchain → Composition order rule][in-depth-project-composition] for the worked example with `-g ci,all,release`.

### List Variables {#composition-order-list}

The prepend rule above is for `path` entries. A [`list`][metadata-env-list] entry does the opposite: each contribution is **appended**, matching how the surveyed option-list consumers themselves resolve duplicates — `GODEBUG` scans its setting list backward, and a repeated `NODE_OPTIONS` scalar flag takes the last occurrence. Ordering a `list` value the same way a `path` value is ordered would put the wrong contribution first for consumers like these.

Every fold — `path` or `list` — is a **render, not accumulated state**: the composer recomputes a variable's value from the ambient value plus every applicable contribution, in application order:

```
[prepend-zone: vector reversed] [ambient value] [append-zone: vector order]
```

`path` contributions land ahead of the ambient value, most-recently-applied first (move-to-front). `list` contributions land after it, in the order applied (move-to-back) — the last package or project stage to contribute a `list` value lands at the very end, which is what a last-wins consumer resolves to.

::: tip Idempotence, the same guarantee as `path`
Re-running the fold with a value already present removes it from its old position and re-appends it at the back, so a repeated `ocx run`, a re-evaluated `.envrc`, or a launcher re-entry never grows a `list` variable — the same [move-to-front idempotence](#strict-isolation-idempotent) `path` entries guarantee, mirrored for the opposite end.
:::

#### Separator agreement across a composition {#composition-order-list-separator}

A `list` entry's `separator` is settled **per key**, not per entry: every contribution to one key over one composition must agree. The first entry that declares an explicit separator establishes it for that key; a later entry that omits `separator` inherits the established one, and a key nobody gives an explicit separator falls back to a single space. Two entries for the same key with *different* explicit separators fail the whole composition closed (exit 65), naming the key and both separators, Rust debug-quoted (e.g. `","` and `";"`) — a package declaring `GODEBUG` with `,` plus a project `[env]` entry that omits the separator inherits the comma, keeping `GODEBUG` parseable; the same package plus a project entry that explicitly writes `;` is a conflict, not a silent second delimiter.

This agreement runs once every contributing entry for the composition is known — package-composed, patch-companion, project, group, and `--env` alike — so it sees the whole picture before any fold happens. See [`separator` is required][metadata-env-list-separator] on package metadata and the [`[env]` value grammar][config-project-env] for where each surface may or must spell the separator out.

Settling a separator also re-checks every entry's value against it, because a parse-time check can only compare a value to the separator that entry itself declared — an entry that inherits a separator from another contributor was never checked against it. A value edged by the separator it inherits therefore fails the composition closed (exit 65) even though its own parse-time check passed.

::: warning `cmd.exe` cannot export a `list` entry
`ocx env --shell=cmd` and `ocx package env --shell=cmd` skip every `list`-typed entry with a `# ocx:` note on stderr, naming the key. `cmd.exe`'s only string-replacement primitive, `%VAR:search=replace%`, matches case-**insensitively** with no case-sensitive form — and list elements are opaque option strings where `-DFOO=1` and `-Dfoo=1` are different options, so a case-blind removal would delete the wrong one. Every other type (`path`, `constant`) still exports normally under `cmd`. This is a text-export limitation only: the in-process environment `ocx run` and `ocx package exec` build for a child process on Windows is unaffected — only a captured `--shell=cmd` script loses the `list` lines.
:::

## Project Environment {#project-env}

`ocx.toml` can declare its own environment on top of what packages provide: [`[env]`][config-project-env] for project-wide constants, [`[group.<name>.env]`][config-project-env] for group-scoped ones, and the [`--env`][cmd-run] flag for a one-off override.

Before this stage existed, the only channel was the ambient shell (`FOO=bar ocx run -- …`). That fails outright on Windows — neither PowerShell nor `cmd.exe` has a per-invocation variable prefix, both mutate session state that persists after the command — and it fails for any caller that builds an argv array rather than a shell command line, which is the shape a [GitHub Action][github-actions-docs], a [Bazel rule][bazel-rules], or a Python subprocess call all use.

Project and group `[env]` entries materialize as ordinary env entries and are **appended** to the same vector [Composition order](#composition-order) already produces — the same uniform channel every consumer (`ocx run`, `ocx env`, `ocx direnv export`, the `--ci=github`/`--ci=gitlab` writers) reads.

### Precedence {#project-env-precedence}

| Stage | Source | Notes |
|---|---|---|
| 1 (lowest) | Ambient inherited env | Skipped entirely under [`--clean`][cmd-run] |
| 2 | Package-composed env | [Composition order](#composition-order) above — group-selection order, then alphabetical by binding name |
| 3 | Patch-companion overlay | [`[patches]`][config-patches] — unaffected by this feature |
| 4 | Project [`[env]`][config-project-env] | Constants replace; `path` entries prepend; `list` entries append |
| 5 | Group [`[group.<name>.env]`][config-project-env] | In `-g` selection order — a group listed later wins |
| 6 (highest) | [`--env KEY[:TYPE[:SEP]]=VALUE`][cmd-run] | Repeatable; `constant` (default) replaces, `path` prepends, `list` appends; a relative `path` value anchors to the current directory, not the project root stages 4-5 use |

A stage-4, 5, or 6 `path` entry therefore lands ahead of a stage-2 package `path` entry for the same key — it is applied later, and every `path` application is [idempotent with move-to-front semantics](#strict-isolation-idempotent). A stage-4, 5, or 6 `list` entry lands the opposite way: *behind* a stage-2 package `list` entry for the same key, in the same append-zone — see [List Variables](#composition-order-list) above. Stage 6's `path` resolution differs from stages 4 and 5 in one respect: a relative value anchors to the directory ocx was invoked from, not the project root — see [`--env`][cmd-run] for why. A project constant that shadows a package-declared constant of the same key logs at `debug`, never `warn`: overriding a package default is the declared purpose of stages 4–6, not a collision to flag.

### Where `--env` lives {#project-env-flag-surfaces}

`--env` is a **per-invocation override, not project configuration**, so it is available on every command that composes an environment — on both tiers:

| Tier | Commands | Also available |
|---|---|---|
| Project toolchain | [`ocx run`][cmd-run], [`ocx env`][cmd-env-root], [`ocx direnv export`][cmd-direnv-export] | `-g/--group` selects which groups' `[env]` composes |
| Package (OCI) | [`ocx package exec`][cmd-package-exec], [`ocx package env`][cmd-package-env], [`ocx package test`][cmd-package-test], [`ocx patch test`][cmd-patch-test] | `--self` selects the visibility surface |

The package tier still reads no `ocx.toml` — see the boundary note above. `--env` there composes only what the caller typed on that invocation; stages 4 and 5 do not exist, because there is no project file to declare them.

::: tip Export what you would execute
`ocx run` never prints — it replaces itself with the child process — so the only way to see a composed environment is to ask the command that emits one. `ocx env --env X` composes stages 1–6 exactly as `ocx run --env X` does, so the export and the execution agree by construction. The same pairing holds on the package tier between [`ocx package env`][cmd-package-env] and [`ocx package exec`][cmd-package-exec].
:::

::: warning `--clean` is not the hermeticity boundary
`--clean` controls only stage 1 — what the child process inherits from the parent shell. It is not what makes the package-composed set (stage 2) reproducible. That comes from the resolver's scope: package env values are computed from `ocx.lock` and the resolved digests alone, identically with or without `--clean`. Project `[env]` (stage 4) is the opposite case by design — it is the user's own file, deliberately allowed to read ambient state, and is excluded from the lock's `declaration_hash` for exactly that reason.
:::

`--self` is package vocabulary and does not exist on the project tier. It selects a package's own private surface — which by construction leaves that package's `entrypoints/` off `PATH`, because launchers exist for a *consumer* to invoke the package while the package's own runtime calls `bin/` directly. A project toolchain is a consumer of every tool it declares, so the self view would compose a strictly worse toolchain, not a fuller one. The flag lives on [`ocx package exec`][cmd-package-exec] and [`ocx package env`][cmd-package-env], where a package's own surface is the thing being asked about; see [Visibility surfaces](#visibility-surfaces) above.

Project and group `[env]` entries have no visibility axis at all — a project is never a dependency of anything, so there is no interface/private edge to gate.

<!-- external -->
[volta]: https://volta.sh/
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules

<!-- commands -->
[cmd-add]: ./command-line.md#add
[cmd-exec]: ./command-line.md#package-exec
[cmd-env-root]: ./command-line.md#env-root
[cmd-lock]: ./command-line.md#lock
[cmd-pull]: ./command-line.md#pull
[cmd-remove]: ./command-line.md#remove
[cmd-run]: ./command-line.md#run
[cmd-update]: ./command-line.md#update
[cmd-direnv-export]: ./command-line.md#direnv-export
[cmd-package-exec]: ./command-line.md#package-exec
[cmd-package-env]: ./command-line.md#package-env
[cmd-package-test]: ./command-line.md#package-test
[cmd-patch-test]: ./command-line.md#patch-test

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-ocx-project]: ./environment.md#ocx-project
[env-ocx-patches]: ./environment.md#ocx-patches

<!-- configuration -->
[config-patches]: ./configuration.md#keys-patches
[config-patches-no-patches]: ./configuration.md#keys-patches-no-patches
[config-patches-scopes]: ./configuration.md#keys-patches-scopes
[config-project-env]: ./configuration.md#project-config-env

<!-- reference -->
[metadata-env-list]: ./metadata.md#env-list
[metadata-env-list-separator]: ./metadata.md#env-list-separator
[metadata-env-interpolation]: ./metadata.md#env-interpolation
[metadata-env-self-env]: ./metadata.md#env-interpolation-self-env
[metadata-env-path]: ./metadata.md#env-path

<!-- internal -->
[user-guide-global]: ../user-guide.md#global-toolchain
[in-depth-project-composition]: ../in-depth/project.md#running-composition-order
[patches-no-patches-guide]: ../user-guide/patches.md#patches-no-patches
