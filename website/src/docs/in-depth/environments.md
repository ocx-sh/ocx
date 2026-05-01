---
outline: deep
---
# Environments {#env-composition}

Every package declares a flat list of environment variables in its [`metadata.json`][metadata-ref]. When you run [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env] against one or more root packages, the composer reads those declarations and builds a single runtime environment. This page is the canonical reference for how that environment is assembled — and for the single boolean (`--self`) that selects which surface of that environment is emitted.

## Two Surfaces {#two-surfaces}

Every package has two distinct environment surfaces:

- **Interface surface** — what the outside world sees. The variables a consumer needs when depending on or running this package. Contributed to `$PATH` lookups, `JAVA_HOME`, library hints, and anything that crosses the package boundary outward.
- **Private surface** — what the package's own runtime needs. Internal compiler paths, runtime libraries, and shim infrastructure that a package's own [entry-point launchers][entry-points] use but should not leak to consumers.

The default `ocx exec PKG -- cmd` selects the **interface surface** of PKG — the public contract. Passing `--self` selects the **private surface** — the full self-env including `private`-tagged entries.

Generated launchers always embed `--self` so the package's own binary runs with its complete internal environment. You should never need to pass `--self` directly for interactive use.

::: info CMake vocabulary — a memory aid, not a contract
[CMake][cmake-tll] uses the same three-state vocabulary (`PRIVATE`, `PUBLIC`, `INTERFACE`) on both [`target_compile_definitions`][cmake-compile-defs] (what the target publishes per declaration) and [`target_link_libraries`][cmake-tll] (how a dep's properties flow through a link edge). OCX entry visibility shares that vocabulary but governs a different axis: it partitions a publisher's own env entries across two **runtime surfaces** (private = self-only, public = both, interface = consumer-only). CMake's `target_link_libraries` keyword controls compile-time dep reachability for downstream build consumers; OCX's dependency-edge `visibility` controls runtime env propagation through the TC.

Use the CMake vocabulary as a memory aid — the directional intuition transfers — but do not assume behavioral parity. The two are analogous, not identical.
:::

## Visibility Views {#visibility-views}

A single boolean `--self` flag selects which environment surface is emitted by `ocx exec` and its sibling commands. The default is **off** — the consumer view a human or script sees when invoking a package from the outside. Generated [entry-point launchers][entry-points] embed `--self` so the launched binary sees the package's full self env.

::: tip Previously documented as "exec modes"
Earlier OCX releases exposed a `--mode <consumer|self>` flag on these commands. The two-axis [`Visibility`][metadata-dep-visibility] struct now collapses both into a single boolean toggle — `--mode=consumer` → `--self` off (default); `--mode=self` → `--self` on. Passing `--mode=…` exits 64 (`UsageError`).
:::

### Consumer view (default — `--self` off) {#view-consumer}

Selects the **interface surface**: only entries whose `visibility.has_interface()` is true are emitted — that is, `public` and `interface` entries. `private` entries are hidden.

Use it for:

- `ocx exec PKG -- cmd` direct invocations from a shell or CI script
- `ocx env PKG` to inspect what a package exposes to consumers
- Any context where you are *using* a package's output, not *being* the package

### Self view (`--self` on, launcher-embedded) {#view-self}

Selects the **private surface**: only entries whose `visibility.has_private()` is true are emitted — that is, `private` and `public` entries. `interface` entries are hidden, matching the publisher's declared semantics ("`interface` = consumer-only, not for self runtime").

`private` entries are visible in this mode. That is intentional: when a package's own launcher runs, it needs the internal paths — compilers, runtimes, shared libraries — that the publisher deliberately kept off the consumer surface.

::: tip Self view is automatic for launchers
The `ocx launcher exec` subcommand — used by every generated launcher script — forces the self view internally. You do not need to pass `--self` to `ocx exec` yourself; rely on the default consumer view for direct invocations.
:::

### Visibility Truth Table {#truth-table}

| Var visibility | `--self` off (interface surface) | `--self` on (private surface) |
|---|---|---|
| `private` | No | **Yes** |
| `public` | **Yes** | **Yes** |
| `interface` | **Yes** | No |
| `sealed` (rejected at parse) | No | No |

`sealed` is not a valid value for `env` entries — it is rejected at metadata parse time. Only [dependency-edge visibility][metadata-dep-visibility] uses all four levels.

### Commands That Accept `--self` {#commands}

| Command | Default | Notes |
|---|---|---|
| [`ocx exec`][cmd-exec] | off | The primary entry point; launchers embed `--self` |
| [`ocx env`][cmd-env] | off | Inspect the resolved env for a package |
| [`ocx shell env`][cmd-shell-env] | off | Generate shell export statements |
| [`ocx shell profile load`][cmd-profile-load] | off | Load profiled packages into shell |
| [`ocx ci export`][cmd-ci-export] | off | Export env to CI system runtime files |
| [`ocx deps`][cmd-deps] | off | Show dependency tree with visibility annotations |

## Composition Order {#composition-order}

The composer iterates each root in the order supplied to the command. For each root, the composition sequence is:

1. **TC entries in topological order** — transitive closure dependencies, deepest first (deps before dependents). The TC for each root is pre-computed at install time and stored in `resolve.json`; the composer reads it in one pass, no recursive walk at exec time.
2. **Root's own env-var declarations** — emitted after all TC contributions so the root's `PATH` entries prepend on top of its dependencies' contributions.
3. **Root's own entrypoints synth-PATH** — if the package declares entrypoints, a synthetic `PATH ⊳ <pkg-root>/entrypoints` entry is appended last.

When multiple root packages are composed (e.g. `ocx exec pkg1 pkg2 -- cmd`), a shared seen-set spans all roots. A package encountered as a transitive dep of both roots is emitted only once, the first time it is encountered.

## Edge Filter {#edge-filter}

The TC for a root assigns each transitive dep an **effective visibility** — the result of inductive composition along every path from root to that dep, computed via [`Visibility::through_edge`][visibility-through-edge] and deduplicated via [`Visibility::merge`][visibility-merge] at install time.

At exec time the composer applies a surface gate:

| Surface | Predicate | Passes |
|---|---|---|
| Interface (`--self` off, default) | `tc_entry.visibility.has_interface()` | `public` and `interface` |
| Private (`--self` on) | `tc_entry.visibility.has_private()` | `public` and `private` |

A dep whose effective visibility from the root is `sealed` contributes nothing to either surface. A dep with effective visibility `public` contributes to both surfaces.

The gate applies only to **whether** a dep contributes. When a dep does contribute, only its own **interface-tagged** `env` entries (`public` and `interface` on the `Var.visibility` field) cross the dep edge into the consumer's surface. `private` entries of a dep are always the dep's own internal matter and are never forwarded, regardless of edge visibility.

::: details Why does the dep contribute only its interface vars, not its full private surface?
Each package has two surfaces, and they are orthogonal. The edge visibility (`Visibility` on the `Dependency` entry) controls whether a dep's env is **accessible at all** from the root's perspective. But what arrives is always the dep's **outward-facing** interface — its `public` and `interface` env entries — not the dep's internal runtime environment. The dep's `private` entries are for the dep's own launchers; they never cross edges. This matches the CMake model exactly: a `PUBLIC` dependency forwards its `INTERFACE` headers to consumers, not its `PRIVATE` build flags.
:::

## Last-Wins Scalar Semantics {#last-wins}

Two composition types:

- **`path` entries** (e.g. `PATH`, `LD_LIBRARY_PATH`) — **prepended**. Each dep's `bin/` directory is inserted before the existing value. Because root contributions come after TC entries, the root's `bin/` wins lookup over any transitive dep's `bin/`.
- **`constant` entries** (e.g. `JAVA_HOME`) — **last-writer-wins**. The last package in topological order that sets a constant variable determines the final value. Root is always last for its own env-var declarations. When two unrelated TC entries both set the same constant, a warning is emitted (see [conflicting scalars][ug-conflicts]).

## Worked Example {#worked-example}

Three packages: `R` (root), `A` (public dep of R), `B` (interface dep of A).

<Tree>
  <Node name="R" icon="📦" open>
    <Description>root package</Description>
    <Node name="A" icon="📦" open>
      <Description>visibility: public</Description>
      <Node name="B" icon="📦">
        <Description>visibility: interface</Description>
      </Node>
    </Node>
  </Node>
</Tree>

At install time, `R`'s `resolve.json` contains the TC:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/b:1@sha256:bbb...", "visibility": "public" },
    { "identifier": "ocx.sh/a:1@sha256:aaa...", "visibility": "public" }
  ]
}
```

B is listed first (dep before dependent, topological order). A's edge is `public`, and B's edge from A is `interface` — `public.through_edge(interface) = public` — so B's effective visibility from R is `public`.

When you run `ocx exec R -- my-cmd` (default, interface surface):

1. **B** — `visibility.has_interface()` → true. Emit B's `public` and `interface` env entries. B declares `PATH += ${installPath}/bin` (`public`). Entry emitted: `PATH ⊳ ~/.ocx/packages/ocx.sh/b:1/.../content/bin`.
2. **A** — `visibility.has_interface()` → true. Emit A's `public` and `interface` env entries. A declares `PATH += ${installPath}/bin` (`public`) and `MY_TOOL_HOME = ${installPath}` (`public`). Entries emitted.
3. **R's own env** — R declares `PATH += ${installPath}/bin` (`public`). Emitted last, so R's `bin/` prepends on top of A and B.

Final PATH (shown left-to-right in prepend order, last-prepended wins):

```
~/.ocx/.../R/content/bin : ~/.ocx/.../A/content/bin : ~/.ocx/.../B/content/bin : $original_PATH
```

R's `bin/` is found first in PATH lookup.

::: tip Visibility in deps
Run [`ocx deps --flat PKG`][cmd-deps] to inspect the effective visibility for each entry in PKG's transitive closure. The flat view shows the exact evaluation order and effective visibility column — the primary debugging tool when environment variables are not what you expect.
:::

## Conflicting Constant Variables {#conflicting-constants}

When two packages in the composition both declare a `constant` variable with the same key, the last-writer-wins rule determines the final value. The root package is always last for its own declarations, so it wins over any transitive dep.

A concrete case: a package `P` declares `JAVA_HOME = ${installPath}` as `public`, and its dependency `J` also declares `JAVA_HOME = ${installPath}` as `public`. In the standard TC walk, `J` is emitted before `P` (dep before dependent). The composition proceeds:

1. **J** — emits `JAVA_HOME = ~/.ocx/.../J/content` (`constant`, last-write so far)
2. **P** — emits `JAVA_HOME = ~/.ocx/.../P/content` (`constant`, overwrites J's value)

Final value: `~/.ocx/.../P/content` — P wins, because it appears later in topological order.

::: warning Last-wins for constants is silent
No error is raised when two packages set the same constant key. The later package in topological order wins silently. OCX emits a warning to stderr only if two *unrelated* TC entries set the same constant — that is, when neither is an ancestor of the other in the dependency graph. If the conflict is expected and intentional, mark the lower-priority dep's entry as `private` or `sealed` so it does not enter the shared surface at all.
:::

::: info Contrast with CMake's dedup-by-value model
[CMake's][cmake-tll] `target_compile_definitions` and `target_include_directories` deduplicate entries by value across the propagated set, keeping only the first occurrence of each duplicate. OCX does not dedup constants by value — it always applies the full composition walk and lets last-writer win. This divergence is intentional: OCX constants carry install-path substitutions that are identity-unique per package, so value-equality across two packages is impossible in normal cases. The warning-on-conflict gate handles the pathological case.
:::

## Launcher Embedding {#launcher-embedding}

When `ocx install` generates launchers for a package's declared [entrypoints][entry-points], each launcher delegates to the internal `ocx launcher exec` subcommand with the package-root path baked at install time.

Unix (POSIX sh):

```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
exec "${OCX_BINARY_PIN:-ocx}" launcher exec '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123…' -- "$(basename "$0")" "$@"
```

Windows (cmd.exe batch):

```bat
@ECHO off
SETLOCAL DisableDelayedExpansion
IF DEFINED OCX_BINARY_PIN (
    "%OCX_BINARY_PIN%" launcher exec "C:\Users\alice\.ocx\packages\ocx.sh\sha256\ab\c123…" -- "%~n0" %*
) ELSE (
    ocx launcher exec "C:\Users\alice\.ocx\packages\ocx.sh\sha256\ab\c123…" -- "%~n0" %*
)
```

`ocx launcher exec` forces the self view internally — no flag needs to be baked into the script. The stable wire ABI is the `launcher exec` subcommand name pair and positional shape.

See the [Entry Points][entry-points] in-depth page for the full launcher generation pipeline, character rejection rules, and cross-platform launcher behavior.

## OCX Configuration Forwarding {#ocx-forwarding}

Whenever ocx spawns a subprocess (most commonly the child process under [`ocx exec`][cmd-exec], which may itself invoke a generated [entrypoint launcher][entry-points] that re-shells back into ocx), it materializes the running ocx's resolution-affecting policy as `OCX_*` env vars on the child. This is the chokepoint that keeps configuration coherent across the chain.

| Variable | Source | Purpose |
|---|---|---|
| [`OCX_BINARY_PIN`][env-ocx-binary-pin] | Resolved path of the running ocx executable | Pins the inner ocx to the same binary that installed the package |
| [`OCX_OFFLINE`][env-ocx-offline] | `--offline` flag on the outer invocation | Child ocx stays offline if outer was offline |
| [`OCX_REMOTE`][env-ocx-remote] | `--remote` flag on the outer invocation | Child ocx uses the remote index if outer did |
| [`OCX_CONFIG`][env-ocx-config] | `--config` flag on the outer invocation | Child ocx loads the same explicit config file |
| [`OCX_INDEX`][env-ocx-index] | `--index` flag on the outer invocation | Child ocx reads the same local index directory |

The running ocx's parsed flags are authoritative — they overwrite any inherited value from the parent shell, so a stale exported `OCX_OFFLINE=1` cannot override an outer `ocx` invoked without `--offline`. The same rule holds under [`ocx exec --clean`][cmd-exec]: the child env starts empty, then `OCX_*` keys are written explicitly from the outer ocx's parsed state. No ambient shell export can bypass this.

::: warning Presentation flags do not propagate
`--log-level`, `--format`, and `--color` are outer-presentation choices and never flow into a child ocx via env. This ensures that running `cmake --version` through a launcher produces only cmake's output, not ocx logs.
:::

`OCX_AUTH_<REGISTRY>_*` is not part of the forwarded set: launcher children re-enter via `ocx launcher exec` using a local package root rather than a registry, so they never need credentials. Future deliberate ocx-spawns-ocx call sites that *do* need auth will declare that forwarding rule at their own boundary.

## Migrating from Other Tools {#migration}

::: info mise / devbox / devenv users
Migrating from [mise][mise], [devbox][devbox], or [devenv][devenv]? Those tools use everything-leaks env composition — every dep's env reaches every consumer. OCX defaults entry visibility to `private` — only entries explicitly tagged `public` or `interface` propagate to consumers. Expect a smaller default consumer surface; mark vars as `public` if they were globally exported in your old setup.

Example: a package that sets `JAVA_HOME` with default (`private`) visibility will not appear in a consumer's environment. Set `"visibility": "public"` to forward it.
:::

## Cross-References {#cross-refs}

- [Entry Points][entry-points] — how launchers are generated, the synth-PATH mechanism for transitive deps, and entrypoint name collisions
- [Entry Visibility in the metadata reference][metadata-entry-visibility] — field-level documentation for `Var.visibility`
- [Dependency Visibility in the metadata reference][metadata-dep-visibility] — the separate edge-level `Dependency.visibility` field
- [Visibility section of the user guide][ug-visibility] — conceptual explanation of the two-surface model

<!-- external -->
[cmake-tll]: https://cmake.org/cmake/help/latest/command/target_link_libraries.html
[cmake-compile-defs]: https://cmake.org/cmake/help/latest/command/target_compile_definitions.html
[mise]: https://mise.jdx.dev/
[devbox]: https://www.jetify.com/devbox/
[devenv]: https://devenv.sh/

<!-- in-depth -->
[entry-points]: ./entry-points.md

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#exec
[cmd-env]: ../reference/command-line.md#env
[cmd-shell-env]: ../reference/command-line.md#shell-env
[cmd-profile-load]: ../reference/command-line.md#shell-profile-load
[cmd-ci-export]: ../reference/command-line.md#ci-export
[cmd-deps]: ../reference/command-line.md#deps

<!-- reference -->
[metadata-ref]: ../reference/metadata.md
[metadata-entry-visibility]: ../reference/metadata.md#env-entry-visibility
[metadata-dep-visibility]: ../reference/metadata.md#dependencies-visibility
[visibility-through-edge]: ../reference/metadata.md#dependencies-through-edge
[visibility-merge]: ../reference/metadata.md#dependencies-merge

<!-- environment -->
[env-ocx-binary-pin]: ../reference/environment.md#ocx-binary-pin
[env-ocx-offline]: ../reference/environment.md#ocx-offline
[env-ocx-remote]: ../reference/environment.md#ocx-remote
[env-ocx-config]: ../reference/environment.md#ocx-config
[env-ocx-index]: ../reference/environment.md#ocx-index

<!-- user guide -->
[ug-conflicts]: ../user-guide.md#dependencies-environment
[ug-visibility]: ../user-guide.md#dependencies-visibility
