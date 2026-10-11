---
title: "Environment Composition"
---
<!-- doc_type: reference -->

How OCX assembles environment variables and selects the active toolchain tier. The reasoning behind each rule is in [Environments][in-depth-environments] and [Toolchain activation][explanation-toolchain-activation].

## Strict Isolation {#strict-isolation}

Global packages never compose into, supplement, or fall back into a project's resolved environment. Both env-composition commands are hermetic with respect to the global toolchain: an undeclared package is absent and never filled in.

| Command | Tier | Reads |
|---|---|---|
| [`ocx exec`][cmd-run] | Project | `ocx.toml` and `ocx.lock` |
| [`ocx package exec`][cmd-exec] | OCI | OCI identifiers on the command line, never any `ocx.toml` |

See [Strict isolation][env-strict-isolation].

### PATH precedence {#strict-isolation-shell-hook}

Isolation is PATH order and not PATH stripping: a project's binaries are prepended ahead of the global toolchain's and shadow same-named global binaries. [`ocx direnv export`][cmd-direnv-export] emits only the project's PATH entries, which [direnv][direnv] prepends before the ambient `PATH`.

See [PATH precedence][env-strict-isolation-path].

### Idempotent re-application {#strict-isolation-idempotent}

Every `PATH` prepend OCX emits is idempotent with move-to-front semantics: a directory already present moves from its old position to the front. Re-applying the same output therefore never grows `PATH`.

- Output: self-contained shell statements, with no dependency on an `ocx` process, a guard variable or a helper function
- Shells: bash, zsh, ash, ksh, dash, fish, PowerShell, elvish, nushell, Windows `cmd`
- Version floors: elvish 0.16+ (`str:` module), nushell 0.101+ (auto-list `PATH` conversion)
- `cmd`: rebuilds `PATH` with `%VAR:search=%` substring deletion, with no `FOR /F` and no delayed expansion, and matches segments case-insensitively
- Also applies to: the in-process environment of `ocx exec` and `ocx package exec`, and to CI exports (`--ci=github`, `--ci=gitlab`)

See [Idempotent re-application][env-strict-isolation-idempotent].

### Hermetic `ocx exec` {#strict-isolation-exec}

`ocx exec` reads exactly two files, `ocx.toml` and its sibling `ocx.lock`, and composes only the packages they declare. By default it inherits the spawning shell's environment, prepends the composed `PATH` entries and sets the other composed variables on top.

- `--clean`: drops the inherited environment
- Named bindings: `ocx exec cmake -- …` resolves only the named bindings to a host leaf. Omit the names and the whole scope must resolve
- `-g GROUP`: selects the namespace for name resolution, not a mandate that every binding in it be available
- Shell integration: whatever the per-prompt reconciler applied to the spawning shell is reverted first, using the [`__OCX_ENV_STATE`][env-ocx-env-state] ledger. Your own variables and `PATH` entries are untouched. Without shell integration nothing is removed
- Same revert: `ocx package exec`, `ocx package test` and `ocx patch test`

See [Hermetic `ocx exec`][env-strict-isolation-exec].

### Hermetic `ocx package exec` {#strict-isolation-package-exec}

`ocx package exec` takes one or more OCI identifiers, resolves each, composes the declared environment variables of the resolved packages and spawns the command with that environment. No `ocx.toml` is read, neither the project file nor the global file.

## Patch Opt-Out Scope {#patch-opt-out-scope}

A project's [`no-patches`][config-patches-no-patches] opt-out is read from `ocx.toml`, so it applies only where the project file is directly in scope. A [system-required][config-patches-scopes] tier is never subject to it.

| Invocation | Honors the opt-out | How |
|---|---|---|
| [`ocx exec`][cmd-run] | Yes | Forwards the opted-out bases' canonical `registry/repository` keys in [`OCX_PATCHES`][env-ocx-patches] and the names it resolved in [`OCX_LAUNCH_IDENTITIES`][env-ocx-launch-identities] |
| [`ocx env`][cmd-env-root], [`ocx direnv export`][cmd-direnv-export] | Yes | Read the project config directly; they and the per-prompt reconciler mark the opted-out package in `OCX_LAUNCH_IDENTITIES` |
| Launcher re-entered from those chains | Yes | Matches the opt-out against the forwarded names |
| Launcher run by absolute path or through [`ocx package exec`][cmd-package-exec] | No | Decodes no forwarded opt-out and composes the companion overlay as if `no-patches` were never set |

See [Per-package opt-out][patches-no-patches-guide].

## Tier Selection {#tier-selection}

OCX has two toolchain tiers, and selection is always explicit: there is no implicit fallback from project to global. `--global` and `--project` are mutually exclusive, and combining them exits with code 64 (`UsageError`).

| Tier | How to activate | File |
|------|----------------|------|
| Project | CWD walk finds `ocx.toml`; or `--project <path>`; or [`OCX_PROJECT`][env-ocx-project] | nearest `ocx.toml` ancestor |
| Global | `--global` flag; or [`OCX_GLOBAL`][env-ocx-global] | `$OCX_HOME/ocx.toml` |

- CWD walk: stops at the first `ocx.toml` it meets. A nested project replaces the enclosing one, and two project files never layer
- No home-tier discovery: a walk that finds nothing means no project tier is active, and the global toolchain is active only when requested

See [Nested projects switch, they do not stack][in-depth-shell-integration-nested].

### Root `--global` commands {#tier-selection-commands}

`--global` is a root flag and must appear before the subcommand, as in `ocx --global add ripgrep:14`. It affects these toolchain-tier commands:

| Command | With `--global` |
|---------|----------------|
| [`ocx add`][cmd-add] | Adds binding to the global file |
| [`ocx remove`][cmd-remove] | Removes binding from the global file |
| [`ocx lock`][cmd-lock] | Re-locks the global file |
| [`ocx update`][cmd-update] | Advances a binding in the global file |
| [`ocx pull`][cmd-pull] | Pre-warms packages declared by the global file |
| [`ocx exec`][cmd-run] | Composes env from the global file + its lock |
| [`ocx env`][cmd-env-root] | Emits composed toolchain env for the global file |

## Toolchain Activation {#toolchain-activation}

Two `ocx.toml` keys decide how a selected toolchain reaches a shell, and both are read per tier from that tier's own file. [`activate`][config-activate] picks the shape, and [`pinned`][config-pinned] picks whether composed paths follow the rendered `links/<group>/<entry>` links or name digest paths from `ocx.lock`.

See [Toolchain activation][explanation-toolchain-activation].

### The `activate` × `pinned` matrix {#toolchain-activation-matrix}

| [`activate`][config-activate] | [`pinned`][config-pinned] | What the project contributes to `PATH` | What a composing emitter composes | What [`ocx pull`][cmd-pull] renders |
|---|---|---|---|---|
| `env` | `false` | its composed entries — each package's own `PATH` directories | through the `links/<group>/<entry>` links | the trampolines and the links |
| `env` | `true` | its composed entries — each package's own `PATH` directories | digest paths from `ocx.lock`, consulting no link | the trampolines only — the link pass writes and prunes nothing |
| `bin` | `false` | `<home>/toolchain/active/bin`, and nothing else | through the `links/<group>/<entry>` links | the trampolines and the links |
| `bin` | `true` | `<home>/toolchain/active/bin`, and nothing else | digest paths from `ocx.lock`, consulting no link | the trampolines only |
| `none` | `false` | nothing | through the `links/<group>/<entry>` links | the trampolines and the links |
| `none` | `true` | nothing | digest paths from `ocx.lock`, consulting no link | the trampolines only |

- Session directories: `$OCX_HOME/toolchain/active/bin` and OCX's install directory behind it are on `PATH` in every row, including `none`
- Composing emitters: [`ocx env`][cmd-env-root], [`ocx exec`][cmd-run], [`ocx direnv export`][cmd-direnv-export], the `env`-mode reconciler and the `env`-mode login exporter. Only `ocx env` and `ocx exec` declare [`--pinned` / `--no-pinned`][arg-pinned]
- `env`-mode reconciler and login exporter: read the global tier's `activate`, so under `bin` or `none` neither composes at all
- Link cells: a lock entry whose link is absent, stale or not a link composes on its digest path. Only roots have links, so a dependency's `PATH` contributions and every `${deps.<name>.installPath}` are digest paths in both lanes. See [`--pinned`][arg-pinned-degrade]
- `pinned = true`: suppresses the link pass without deleting a tree, so switching it on and off needs no re-render
- `bin` gate: `<home>/toolchain/active/bin` is added only when the tree's render stamp matches this project and this lock. Otherwise nothing is added and the prompt prints the line below

```text
ocx: /home/dana/work/api: its toolchain has not been rendered for this lock; run `ocx pull` here
```

Within one prompt the entries land in this order, front to back:

1. The project's composed entries
2. The project's `<home>/toolchain/active/bin`
3. `$OCX_HOME/toolchain/active/bin`
4. OCX's own install directory
5. The global tier's composed entries

See [Order on PATH][explanation-path-order].

The trampoline body, its Windows sidecar and the name-collision rule are in [A trampoline composes at call time][explanation-trampolines] and [Name collisions][explanation-collisions].

## Visibility Surfaces {#visibility-surfaces}

Each package declares an interface surface (what consumers see) and a private surface (what its own launchers see). The `--self` flag on `package env`, `package exec`, `package test` and `package deps` selects the surface, and it is OCI-tier only.

| `--self` | Surface emitted | Use case |
|----------|----------------|----------|
| off (default) | Interface surface — vars where `has_interface()` is true | Human or CI script using the package |
| on | Private surface — vars where `has_private()` is true | Generated launchers invoking `ocx launcher exec` internally |

- Launchers: generated launchers force `self_view = true` internally and do not expose `--self` to callers
- Patch companions: a companion's env var is composed on the surfaces of the package it matches, read as if that package had declared it

See [Companions are part of their target][patches-how-part-of-target].

## Self-Referencing Values {#self-referencing}

`${self.env.KEY}` resolves to the resolved value of this package's own earlier-declared `KEY` var. It is legal only in `env` values, never in entry-point `args`.

- Declaration order: `KEY` must be declared strictly earlier in the same `env` array. A forward or self reference, or a reference to a key declared twice, is refused at publish time
- Resolves to: `KEY`'s resolved value, not its unexpanded template
- Surface: independent of `KEY`'s own visibility and of whether `--self` is on
- Failure: a malformed template, a `required` [`path`][metadata-env-path] that does not exist on disk, or an unresolvable `${self.env.KEY}` fails the whole composition with exit 65. This applies to any var of the package, including one not emitted on the surface in play
- Commands: the failure applies to [`ocx env`][cmd-env-root], [`ocx package env`][cmd-package-env], [`ocx exec`][cmd-run] and [`ocx package exec`][cmd-package-exec] on either surface

See [`self.env`][metadata-env-self-env] and [Interpolation Tokens][metadata-env-interpolation].

## Integrations {#integrations}

Package metadata can declare vendor-namespaced [`integrations`][metadata-integrations] blocks, and composition carries them as attributed rows without interpreting them. Two packages that declare the same namespace produce two independent rows, and OCX never merges them.

See [What composition does][explanation-integrations-composition].

### Row Shape {#integrations-row-shape}

`integrations` is a fourth top-level array of the `--format json` envelope, beside `items`, `binaries` and `entrypoints`, and never nested inside `items`. The array is present, with attribution, even when composing a single package.

```json
{
  "items": [ … ],
  "binaries": [ … ],
  "entrypoints": [ … ],
  "integrations": [
    {
      "namespace": "com.microsoft.vscode",
      "package": "kitware/cmake:3.28@sha256:aaaa…",
      "payload": { "extensions": ["rust-lang.rust-analyzer"] }
    }
  ]
}
```

- `namespace`: the declared key
- `package`: the canonical resolved identifier of the declaring package. The tag may be absent, and a digest-pinned identifier is legal
- `payload`: that package's block, [interpolated][metadata-integrations-interpolation] against that package's own `${installPath}`, never the composing root's
- Filter: `.integrations[] | select(.namespace=="…")`

### Ordering {#integrations-ordering}

Rows follow the admitted-set walk order of [Composition Order](#composition-order): for each root, its admitted dependencies first in topological order, then the root itself. Roots keep the order the package set produced them in, and namespaces within one package's contribution are ordered lexicographically.

- Diamond: a dependency reached by two roots contributes once, at its first-seen position

### Interface Surface Only {#integrations-interface-surface}

Integrations reach only the [interface surface](#visibility-surfaces), at every depth: `--self` always composes `"integrations": []`, including a generated [launcher's][cmd-launcher-exec] forced self view. An integrations block has no `visibility` field, unlike `binaries` and `entrypoints`, whose membership follows the two-axis [visibility][reference-visibility] of each declared entry.

- Private edge: a dependency reached only through a `private` edge contributes nothing on either surface

### Patch Companions Contribute Too {#integrations-companions}

A [patch companion][patches-how] declaring `integrations` contributes them like any package. The row's `package` field names the companion's own identifier and never the base it was admitted for.

- Several bases: a companion matched against several bases contributes once
- Surfaces: a companion's `env` follows the [surfaces of the package it matched](#visibility-surfaces). Its `integrations` stay interface-only, and the interface-surface rule is not evaluated per contributor
- Declining: [`no-patches`][patches-no-patches-guide] drops an optional companion whole, `env` and integrations together. A required companion is required on the same terms as its `env`

### Absent from `--shell` and `--ci` {#integrations-shell-ci}

Neither [`--shell`][cmd-env-root] nor `--ci` output carries an `integrations` representation. Only `--format json` carries the array.

## Composition Order {#composition-order}

When several packages contribute to an environment, via `ocx exec -g GROUP1,GROUP2` or `ocx package exec PKG1 PKG2`, `path` entries are prepended: the last package walked has its `PATH` entries placed first. In `-g` argument order, groups listed later win PATH lookup.

- `ocx exec` order: first by group-selection order (the order of `-g` flags, after `all` expansion, deduplicated). Then alphabetical by binding name within each group

See [In Depth: Project Toolchain → Composition order rule][in-depth-project-composition].

### List Variables {#composition-order-list}

A [`list`][metadata-env-list] entry is appended and not prepended. Each contribution lands after the ambient value, so the last package or project stage to contribute lands at the very end.

```text
[prepend-zone: vector reversed] [ambient value] [append-zone: vector order]
```

- `path`: contributions land ahead of the ambient value, most recently applied first (move-to-front)
- `list`: contributions land after the ambient value, in the order applied (move-to-back), which is what a last-wins consumer resolves to
- Render: every fold, `path` or `list`, is a render and not accumulated state
- Idempotence: re-running the fold with a value already present removes it from its old position and re-appends it at the back. A repeated `ocx exec`, a re-evaluated `.envrc` or a launcher re-entry therefore never grows a `list` variable
- `cmd.exe`: `ocx env --shell=cmd` and `ocx package env --shell=cmd` skip every `list`-typed entry with a `# ocx:` note on stderr naming the key, and every other type still exports
- `cmd.exe` scope: a text-export limitation only. The in-process environment that `ocx exec` and `ocx package exec` build for a child process on Windows is unaffected

See [Rendering list variables][env-last-wins-lists].

Two entries for one key with different explicit separators fail the composition with exit 65. See [Separator agreement][env-last-wins-lists-separator], [`separator` is required][metadata-env-list-separator] and the [`[env]` value grammar][config-project-env].

## Project Environment {#project-env}

`ocx.toml` can declare its own environment on top of what packages provide. [`[env]`][config-project-env] sets project-wide constants, [`[group.<name>.env]`][config-project-env] sets group-scoped ones, and the [`--env`][cmd-run] flag sets a one-off override.

- Materialization: project and group `[env]` entries materialize as ordinary env entries and are appended to the vector [Composition Order](#composition-order) produces

See [Project and group `[env]`][in-depth-project-env].

### Precedence {#project-env-precedence}

| Stage | Source | Notes |
|---|---|---|
| 1 (lowest) | Ambient inherited env | Minus the shell reconciler's own contribution ([above](#strict-isolation-exec)); skipped entirely under [`--clean`][cmd-run] |
| 2 | Package-composed env | [Composition order](#composition-order) above — group-selection order, then alphabetical by binding name |
| 3 | Patch-companion overlay | [`[patches]`][config-patches] — each companion var on the surfaces of the package it matched; applied after all package env, before stages 4-6 |
| 4 | Project [`[env]`][config-project-env] | Constants replace; `path` entries prepend; `list` entries append |
| 5 | Group [`[group.<name>.env]`][config-project-env] | In `-g` selection order — a group listed later wins |
| 6 (highest) | [`--env NAME` or `--env KEY[:TYPE[:SEP]]=VALUE`][cmd-run] | Repeatable; `constant` (default) replaces, `path` prepends, `list` appends; a relative `path` value anchors to the current directory, not the project root stages 4-5 use. The bare `NAME` form carries the invoking process's own value of `NAME` in at this stage, and contributes nothing when `NAME` is unset there |

- `path` across stages: every `path` application is [idempotent with move-to-front semantics](#strict-isolation-idempotent). So a stage 4, 5 or 6 `path` entry lands ahead of a stage 2 package `path` entry for the same key
- `list` across stages: a stage 4, 5 or 6 `list` entry lands behind a stage 2 package `list` entry for the same key, in the same append zone. See [List Variables](#composition-order-list)
- Shadowing: a project constant that shadows a package-declared constant of the same key logs at `debug`, never `warn`

### Where `--env` lives {#project-env-flag-surfaces}

`--env` is a per-invocation override and not project configuration, so it is available on every command that composes an environment, on both tiers. The package tier reads no `ocx.toml`, so stages 4 and 5 do not exist there.

| Tier | Commands | Also available |
|---|---|---|
| Project toolchain | [`ocx exec`][cmd-run], [`ocx env`][cmd-env-root], [`ocx direnv export`][cmd-direnv-export] | `-g/--group` selects which groups' `[env]` composes |
| Package (OCI) | [`ocx package exec`][cmd-package-exec], [`ocx package env`][cmd-package-env], [`ocx package test`][cmd-package-test], [`ocx patch test`][cmd-patch-test] | `--self` selects the visibility surface |

- Export and execution agree: `ocx env --env X` composes stages 1 to 6 exactly as `ocx exec --env X` does, and [`ocx package env`][cmd-package-env] pairs with [`ocx package exec`][cmd-package-exec] the same way
- `--clean`: controls only stage 1, what the child inherits from the parent shell. Package env values come from `ocx.lock` and the resolved digests alone, with or without it
- Forwarded under `--clean`: `OCX_HOME` and `OCX_BINARY_PIN` always, and every other resolution-affecting `OCX_*` setting whenever this invocation resolved one, including [`OCX_DEFAULT_REGISTRY`][env-default-registry], [`OCX_INSECURE_REGISTRIES`][env-insecure-registries], [`OCX_NO_PROJECT`][env-no-project] and [`OCX_NO_CONFIG_REFRESH`][env-no-config-refresh]
- Project `[env]`: excluded from the lock's `declaration_hash`, and it has no visibility axis because a project is never a dependency of anything
- `--self`: package vocabulary, absent from the project tier

<!-- external -->
[direnv]: https://direnv.net/

<!-- commands -->
[cmd-add]: ./command-line/commands.md#add
[cmd-exec]: ./command-line/package-consuming.md#package-exec
[cmd-env-root]: ./command-line/commands.md#env-root
[cmd-lock]: ./command-line/commands.md#lock
[cmd-pull]: ./command-line/commands.md#pull
[cmd-remove]: ./command-line/commands.md#remove
[cmd-run]: ./command-line/commands.md#exec
[cmd-update]: ./command-line/commands.md#update
[cmd-direnv-export]: ./command-line/direnv.md#direnv-export
[cmd-package-exec]: ./command-line/package-consuming.md#package-exec
[cmd-package-env]: ./command-line/package-consuming.md#package-env
[cmd-package-test]: ./command-line/package-publishing.md#package-test
[cmd-patch-test]: ./command-line/patch.md#patch-test
[cmd-launcher-exec]: ../explanation/launcher-forwarding.md#launcher-embedding
[arg-pinned]: ./project-file.md#project-config-pinned
[arg-pinned-degrade]: ../explanation/toolchain-activation.md

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-ocx-project]: ./environment.md#ocx-project
[env-ocx-patches]: ./environment.md#ocx-patches
[env-ocx-launch-identities]: ./environment.md#ocx-launch-identities
[env-ocx-env-state]: ./environment.md#ocx-env-state
[env-default-registry]: ./environment.md#ocx-default-registry
[env-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-no-project]: ./environment.md#ocx-no-project
[env-no-config-refresh]: ./environment.md#ocx-no-config-refresh

<!-- configuration -->
[config-activate]: ../explanation/toolchain-activation.md#activate
[config-pinned]: ../explanation/toolchain-activation.md#pinned
[config-patches]: ./configuration.md#keys-patches
[config-patches-no-patches]: ../guides/patch-packages.mdx#patches-config
[config-patches-scopes]: ../guides/patch-packages.mdx#patches-config
[config-project-env]: ../guides/project-toolchain.mdx#project-env

<!-- metadata -->
[metadata-env-list]: ./metadata/env.md#env-list
[metadata-env-list-separator]: ./metadata/env.md#env-list-separator
[metadata-env-interpolation]: ./metadata/env.md#env-interpolation
[metadata-env-self-env]: ./metadata/env.md#env-interpolation-self-env
[metadata-env-path]: ./metadata/env.md#env-path
[metadata-integrations]: ./metadata/integrations.md#integrations
[metadata-integrations-interpolation]: ./metadata/integrations.md#integrations-interpolation
[reference-visibility]: ./metadata/dependencies.md#dependencies-visibility

<!-- internal -->
[in-depth-environments]: ../explanation/environments.mdx
[env-strict-isolation]: ../explanation/environments.mdx#strict-isolation
[env-strict-isolation-path]: ../explanation/environments.mdx#strict-isolation-shell-hook
[env-strict-isolation-idempotent]: ../explanation/environments.mdx#strict-isolation-idempotent
[env-strict-isolation-exec]: ../explanation/environments.mdx#strict-isolation-exec
[env-last-wins-lists]: ../explanation/environments.mdx#last-wins-lists
[env-last-wins-lists-separator]: ../explanation/environments.mdx#last-wins-lists-separator
[explanation-toolchain-activation]: ../explanation/toolchain-activation.md
[explanation-path-order]: ../explanation/toolchain-activation.md#path-order
[explanation-trampolines]: ../explanation/toolchain-activation.md#trampolines
[explanation-collisions]: ../explanation/toolchain-activation.md#collisions
[explanation-integrations-composition]: ../explanation/integrations.md#composition
[in-depth-shell-integration-nested]: ../explanation/shell-integration.mdx#activation-nested-projects
[in-depth-project-composition]: ../explanation/project-toolchain.md#running-composition-order
[in-depth-project-env]: ../explanation/project-toolchain.md#running-project-env
[patches-how]: ../explanation/patches.mdx#patches-how
[patches-how-part-of-target]: ../explanation/patches.mdx#patches-how-part-of-target
[patches-no-patches-guide]: ../guides/patch-packages.mdx#patches-no-patches
