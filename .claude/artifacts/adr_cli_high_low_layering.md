# ADR: CLI High-Level / Low-Level Layering and `ocx run`

## Metadata

**Status:** Proposed
**Date:** 2026-05-08
**Deciders:** Architect (planning agent), Michael Herwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
- [ ] OR deviation justified in Rationale section
**Domain Tags:** api, integration
**Supersedes:** N/A
**Superseded By:** N/A
**Amended By:** [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md) (2026-05-15) — adds the GLOBAL TIER and the strict-isolation rule (see Changelog).

## Context

The OCX CLI has organically grown a two-layer structure but never named it. Half of the commands operate on OCI identifiers as the unit of work (`install cmake:3.28`, `exec node:20 -- node main.js`, `package pull`, `find`, `index update`); the other half operate on the project toolchain declared in `ocx.toml` + `ocx.lock` (`init`, `add`, `remove`, `lock`, `update`, `pull`, `shell hook`, `shell direnv`). Users discover the split anecdotally: by reading sources, by reading the website's user guide, or — most often — by trying the wrong command for their job and reading the error.

Two concrete pain points expose the cost of this implicit split:

1. **Asymmetric naming.** `ocx pull` is project-tier (reads `ocx.lock`, has no positional package args). `ocx package pull` is OCI-tier (takes positional identifiers). `ocx exec` only has the OCI-tier form: there is no project-tier counterpart that consults `ocx.lock`. Users who run `ocx pull` followed by `ocx exec my-tool ...` hit a usability cliff — `pull` understood "my-tool" via `ocx.toml`, but `exec` does not. The asymmetry is invisible from `--help`.
2. **Embedding contract is unwritten.** Backend tools — GitHub Actions, Bazel rules, devcontainer features, CI scripts — embed OCX command lines. Without a declared layering, every change risks accidentally breaking the embedding contract. Demoting `ocx exec` (a primitive) into a project-tier facade would silently break every embedder. Promoting `ocx pull` (a project-tier facade) into a primitive would silently break every CI script that runs `ocx pull` from a checkout to pre-warm the cache.

The trigger for this ADR is the addition of a project-tier `ocx run` command. Adding it requires a stable answer to "which layer is this command in, and what does that imply about its arguments and contract?" — otherwise the command lands in the same fog the rest of the CLI sits in.

The decision splits cleanly into one main decision (formalize the split, add `ocx run` as the project-tier mirror of `ocx exec`) and one sub-decision (reserve `all` as a special group keyword in addition to `default`). Both are scoped to the CLI surface and to the project subsystem; no library-internal API changes are implied.

## Decision Drivers

- **Embedding stability.** Backend tools depend on the OCI-tier surface (`install`, `exec`, `package *`, `index *`). Any change that moves a primitive's argument shape is a breaking change for every embedder. The layering must be explicit so future PRs cannot accidentally break the embedding contract.
- **Symmetric naming for symmetric concepts.** `pull` (project-tier) and `package pull` (OCI-tier) is a working pattern. `exec` (OCI-tier) without a project-tier sibling is not. Users expect every project-tier flow that uses bindings to have a counterpart, just as every project-tier flow that uses lock entries does.
- **No layer-mixing surprises.** A command must not silently switch contracts based on the presence or absence of `ocx.toml`. If the user invokes a project-tier command outside a project, that is an error — not a fallthrough into OCI-tier behaviour. Conversely, an OCI-tier command must never consult `ocx.toml` even if one is present.
- **KISS.** Three commands beat one overloaded command. `exec` (OCI-tier) and `run` (project-tier) each have a tight, predictable contract. Overloading either erodes both.
- **Backend-first.** Per `product-context.md` principle 1, OCX is a tool for tools. JSON output, machine-readable exit codes, no interactive defaults. Layering must serve that audience: every layer change must preserve programmatic discoverability.
- **Clean-env execution.** Per `product-context.md` principle 4, `ocx exec` runs with package-declared variables only. `ocx run` must inherit the same model — clean-env on `--clean`, otherwise inherit-then-overlay — so users moving between the two commands do not encounter surprise environment changes.

## Industry Context & Research

**Research artifact:** [`research_run_command_conventions.md`](./research_run_command_conventions.md) (2026-05-07)

The research surveyed thirteen tools that ship a `run` or `exec` subcommand: npm, pnpm, yarn, cargo, just, task, mise (exec + run), nix (run + develop + shell), direnv exec, devenv, uvx, pipx run, bazel run.

**Trending approaches:**
- The dominant shape is `<tool> run [scope-flag] <name> [-- ARGV]`. Cargo, bazel, mise exec, and nix run all use it. `--` is mandatory in every tool that adopts it.
- Scope/group selection is the highest-divergence axis: pnpm uses repeatable `--filter`, cargo uses `--bin`, nix uses flake attributes, mise uses `--env` for profile switching. There is no convergent norm.
- Reserved keyword names are rare. `default` shows up in just and task as a recipe/task name, not a CLI keyword. No surveyed tool reserves `all`. `pnpm --filter '*'` is the closest analog ("everything") but uses glob syntax, not a reserved word.
- Exit code forwarding is universal. The npm workspace exception is a documented bug, not a design choice.

**Key insight:** The shape `ocx run [-g GROUP[,GROUP,...]]... [NAME...] -- ARGV...` is well within the cargo/bazel/mise convention. It introduces no novel ergonomic. Reserving `all` carries no prior-art conflict — no surveyed tool has claimed the keyword for an incompatible meaning. Introducing `run` as a project-tier mirror of `exec` mirrors the existing `pull` / `package pull` precedent and matches user expectations from cargo (`cargo run` is project-tier; there is no OCI-tier-equivalent in cargo because cargo only has one tier).

The research also validated that `--` must be mandatory: every tool that documents the separator documents it as mandatory. Tools that omit it (just, pnpm, yarn) accept ambiguity in exchange for terseness, but they do not accept flag-prefixed argv. OCX is a backend tool; flag-prefixed argv is the common case (think `ocx run linter -- --format json`), so the separator is not optional.

## Considered Options

### Option 1: Add `ocx run` and document the high-level / low-level split

**Description:** Add a new `ocx run` command at the project tier, structurally mirroring `ocx pull`'s resolution shape and `ocx exec`'s child-spawn mechanics. Concurrently, document the CLI as a two-layer surface: high-level (project-tier, driven by `ocx.toml` + `ocx.lock`, symbols are binding names) and low-level (OCI-tier, driven by registries, symbols are OCI identifiers). Reserve `all` alongside `default` as a special group keyword inside `ocx run` (and consistently across project-tier commands accepting `-g`).

| Pros | Cons |
|------|------|
| Closes the asymmetry: every project-tier flow now has both an env-composition primitive (`exec` / `run`) and a cache-warming primitive (`package pull` / `pull`). | Adds a new top-level command; `--help` surface grows by one entry. |
| Embedding contract becomes explicit: low-level commands promise stable identifier semantics, high-level commands promise lock-file semantics. | Documentation work: user guide, command-line reference, FAQ, and rules catalog all need to name and explain the layering. |
| Each command keeps a single contract — no mode-switching based on `ocx.toml` presence. | Two commands cover the same intent ("run a tool"), so users have to learn the choice. Mitigated by the rule "if you have `ocx.toml`, use `run`; otherwise use `exec`." |
| `all` keyword reservation is a small, contained change. Implementation cost is one constant + two parse-time checks (parse-time + mutate-time). | Existing user `[group.all]` configs (none observed in mirrors corpus, but theoretically possible) would break with a parse error. Migration hint mitigates. |
| Matches industry shape (cargo/bazel/mise) — zero novelty cost for embedders. | — |

### Option 2: Overload `ocx exec` to consult `ocx.toml` first

**Description:** Keep `ocx exec` as the only env-composition primitive. When invoked inside a project (i.e., when `ocx.toml` resolves), parse positional arguments as binding names against the lock; otherwise parse them as OCI identifiers. No new command added.

| Pros | Cons |
|------|------|
| No new command — `--help` surface unchanged. | **Layer mixing.** A single command with two contracts whose selection depends on filesystem state. Embedders must now check for `ocx.toml` to know what their `ocx exec` invocation will do. |
| Discoverable: users already know `ocx exec`. | **Wire ABI breaking risk.** `ocx launcher exec` is an internal subcommand of `exec`'s family; layer-mixing the parent risks polluting the launcher path. |
| — | **KISS violation.** Surprise behaviour — the same command line means different things in different directories. The product principle says "made for machine consumption"; machines hate state-dependent semantics. |
| — | **Composability erosion.** Backend tools embedding `ocx exec` cannot rely on argument semantics being stable; switching CWD silently switches contract. |
| — | **`--clean` and `--self` semantics ambiguous** under bind-name parsing — the flags now apply to bindings or to OCI identifiers depending on context. Doubles the doc burden, not halves it. |

### Option 3: Move `ocx exec` to high-level, demote primitive to `ocx package exec`

**Description:** Make `ocx exec` the project-tier facade (the mirror of `ocx pull`). Move the OCI-tier primitive to `ocx package exec` (the mirror of `ocx package pull`). This rebalances the surface so every tier is uniformly named.

| Pros | Cons |
|------|------|
| Naming becomes perfectly symmetric: `pull`/`package pull`, `exec`/`package exec`, etc. | **Breaks `ocx launcher exec`.** Generated launcher binaries call `ocx launcher exec '<pkg-root>' -- <argv0> ...` (see `subsystem-cli-commands.md` "launcher exec internal subcommand"). The wire ABI is hardcoded into every shim that ocx generates. Renaming/reshaping the family breaks every previously-installed launcher. |
| Aligns `exec` with the project user's mental model ("run my project's tool"). | **Heavy current usage in scripts.** `ocx exec` already appears in user GitHub Actions, Bazel rules, and CI documentation. Demoting it to `ocx package exec` is a hard breaking change with no graceful migration window. |
| — | The fix to point launchers at `ocx launcher exec` was a recent consolidation specifically to keep the OCI-tier `exec` family intact. Reversing that decision now would orphan the consolidation. |
| — | Symmetry is not free if it breaks the embedding contract. The project explicitly does not enforce a stable CLI yet (see `CLAUDE.md` "Current State"), but published packages — including launchers baked into installed packages — *are* stable. |

## Decision Outcome

**Chosen Option:** Option 1 — Add `ocx run` and document the high-level / low-level split.

**Rationale:** Option 1 is the only path that resolves the asymmetry without breaking either the embedding contract (Option 3) or the layer-purity contract (Option 2). The cost is a documentation pass and a single new command file; the benefit is a permanent, named contract for embedders to rely on. Option 3's breakage of `ocx launcher exec` is disqualifying — every previously-installed launcher in the wild would stop working. Option 2's silent contract switching violates the backend-first principle that drives every other CLI design choice in OCX.

The naming choice (`run` rather than `start`, `do`, `with`, etc.) follows cargo and bazel directly. Users moving from those ecosystems read `ocx run` and immediately recognize the shape.

The `all` keyword reservation is a sub-decision rather than a separate ADR because it is purely a parse-time policy on the same surface this ADR introduces. A separate ADR would create the impression that `all` is independently negotiable from the layering decision; it is not — `all` exists because the high-level surface consumes groups as first-class units, and a "every group" alias is necessary the moment groups are first-class.

### Sub-decision: Reserve `all` as a group keyword

**Chosen:** Reserve `all` alongside `default`. `all` is not a literal group declarable in `ocx.toml` (`[group.all]` rejected at parse time, exit 78); it is not a renamable target for `ocx add --group all` / `ocx remove --group all` (rejected at mutate time, exit 64); when passed to `-g all` it expands to `default + every named group` at the resolution layer.

**Rationale:** Without this keyword, users have to enumerate every group manually (`-g default,ci,release,...`) to compose the full toolchain — directly contradicting the backend-first principle. The natural-language alternatives (`*`, `:all:`, `__all__`) are either taken (pnpm uses `*` for filter glob) or violate `Identifier`/group-name character-class rules. `all` is unclaimed in surveyed tools, reads naturally, and slots into the same place `default` already sits in the codebase (single `internal.rs` constant, single parse-time rejection point, single mutate-time validator).

The reservation must be enforced at both parse-time (`ProjectConfig::from_str_with_path` in `crates/ocx_lib/src/project/config.rs:223`) and mutate-time (`validate_group_name` in `crates/ocx_lib/src/project/mutate.rs:134`) so a `[group.all]` declaration on disk is rejected on read *and* `ocx add --group all` is rejected without ever opening `ocx.toml`. The expansion itself is a CLI-layer transformation (project-tier command receives `-g all`, expands to `[default, named_groups...]` before calling `compose_tool_set`) — see plan §4 for placement rationale.

The decision pair (run + all) is treated as a single sub-decision in this ADR because reserving `all` only matters in service of project-tier commands that take `-g`, and the only such commands are the ones this ADR formalizes (`pull`, `lock`, `update`, `run`).

### Quantified Impact

Not applicable. This is a naming and contract decision; there are no latency, throughput, or cost metrics to quantify. The user-experience impact is binary: before this change, embedders cannot rely on a documented layering; after, they can.

The only measurable surface is the count of CLI commands:

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Top-level commands | 30 | 31 | `run` added |
| Reserved group names | 1 (`default`) | 2 (`default`, `all`) | One additional parse-time + mutate-time check |
| Project-tier env-composition primitives | 0 | 1 | `run` is the first |
| OCI-tier env-composition primitives | 1 (`exec`) | 1 (`exec`) | Unchanged |

### Consequences

**Positive:**
- `ocx run` provides the natural project-tier env-composition flow. Users who maintain `ocx.toml` no longer need to translate binding names to identifiers manually before invoking `ocx exec`.
- The high/low layering becomes documented and enforceable. Future PRs proposing layer-mixing changes (e.g., "what if `ocx install` consulted `ocx.toml`?") have a written rule to point to.
- Reserved keyword set (`default`, `all`) is now closed and discoverable. Any future special keyword (none planned) follows the same enforcement pattern: constant in `internal.rs`, parse-time check in `config.rs`, mutate-time check in `mutate.rs`.
- The `compose_tool_set` machinery (currently orphan scaffolding per the discovery findings) gains its first real consumer, validating its design.

**Negative:**
- Two commands now cover "run a tool" (`exec` + `run`). Users have to learn the selection rule. Mitigation: documentation calls out the rule plainly ("if you have `ocx.toml`, use `run`; otherwise use `exec`") and includes side-by-side examples in the user guide.
- Documentation surface grows: user guide, command-line reference, FAQ, in-depth project doc, and the rules catalog all need to name the layering. Estimate: five doc files, one section each.
- Theoretical risk that an existing project uses `[group.all]` in its `ocx.toml`. Mitigation: the parse-time rejection error message includes the migration hint ("rename `[group.all]` — `all` is now a reserved keyword that selects every group"). No public mirror corpus contains such a config; risk is theoretical.

**Risks:**
- **Embedders currently use `ocx exec` from inside a project context.** They will continue to work — `ocx exec` is unchanged — but documentation must not imply that `exec` is deprecated, otherwise embedders will switch to `run` and break the moment they run outside a project context. Mitigation: positioning text says "`exec` is the OCI-tier primitive, `run` is the project-tier facade — both are stable, neither is deprecated."
- **The `all` keyword may collide with future scope-selection extensions** (e.g., a future `-g ~negation` syntax). Mitigation: a literal-string reservation does not preempt a syntax-based extension; reserved keyword space and syntax space are disjoint.

### Hidden One-Way Doors

Beyond the obvious irreversibles (command name `run`, keyword `all`, stated layering), this decision commits to several library-API and stylistic shapes that are costly to change later:

| Commitment | Reversibility cost |
|---|---|
| `ProjectErrorKind::ReservedGroupName { name, hint }` field shape | Library API break for any future mirror/SDK consumer that pattern-matches the variant. Plan parameterizes the variant rather than adding a new one — if a third reserved keyword arrives, this shape stays. |
| Helper module path `crates/ocx_cli/src/app/project_context.rs` | Once `pull.rs` and `run.rs` both consume it, renaming the module costs every caller and every doc cross-reference. Decision frozen by this ADR (see plan §4). |
| Composition order rule (group-selection order, then alphabetical within group) | Once documented in user guide + tests, changing it is a behavioural break for any user relying on observed PATH precedence. |
| `ProjectContext` struct field set | Adding fields is non-breaking; reordering or removing fields breaks any cross-crate consumer. The plan freezes the four-field shape (config, lock, config_path, lock_path). |
| `pub fn expand_all_keyword` library export in `crates/ocx_lib/src/project/compose.rs` | Removing or renaming would break any future programmatic consumer (mirror tool, SDK). Plan exposes this as a public lib helper rather than CLI-only so non-CLI consumers can also expand `all`. |

These commitments are accepted because the alternative (deferring them) just relocates the same one-way door — there is no version of this work that does not freeze them somewhere.

## Technical Details

### Architecture

The two-layer model is a clean split for **read-side** consumption (commands that *resolve* something to act on it). Some commands are honestly mixed: they accept OCI-identifier inputs but their effect is project-tier (writing `ocx.toml` or `ocx.lock`); others query a local store rather than a registry. The diagram below names this explicitly rather than papering over it.

```
┌─────────────────────────────────────────────────────────────────────┐
│                    OCX CLI — Two-Layer Surface                       │
├─────────────────────────────────────────────────────────────────────┤
│ HIGH-LEVEL READ (project tier, lock-driven)                          │
│   Driven by:    ocx.toml + ocx.lock                                  │
│   Symbols:      binding names (TOML keys)                            │
│   Contract:     lock-pinned digests, declaration_hash gate           │
│   Commands:     pull, run [NEW]                                      │
├─────────────────────────────────────────────────────────────────────┤
│ PROJECT MUTATORS (cross-tier: identifier in, lock/toml out)          │
│   Input symbol: OCI identifier (introducing a new binding)           │
│   Output:       writes ocx.toml and/or ocx.lock                      │
│   Commands:     add, remove, lock, update                            │
├─────────────────────────────────────────────────────────────────────┤
│ BOOTSTRAP / MIXED                                                    │
│   init (writes a fresh ocx.toml, reads neither),                     │
│   info, version, shell completion                                    │
├─────────────────────────────────────────────────────────────────────┤
│ SHELL ACTIVATION — [SUPERSEDED → handshake_toolchain_cli.md]         │
│   shell hook / shell init / shell env REMOVED (exit 64 at runtime). │
│   ocx env [--global] [--shell[=NAME]] is the new exporter.          │
│   Activation: $OCX_HOME/env.sh sourced from login profile.          │
│   (Kept for historical reference only — do not implement.)           │
├─────────────────────────────────────────────────────────────────────┤
│ LOW-LEVEL — REGISTRY OPS                                             │
│   Driven by:    OCI registries                                       │
│   Symbols:      OCI identifiers (registry/repo:tag@digest)           │
│   Commands:     install, uninstall, package pull, package push,      │
│                 package describe, package info, package create,      │
│                 index update, index list, index catalog              │
├─────────────────────────────────────────────────────────────────────┤
│ LOW-LEVEL — LOCAL-STORE QUERIES (OCI-id addressed, no registry)      │
│   Driven by:    install/symlink store under $OCX_HOME/               │
│   Symbols:      OCI identifiers                                      │
│   Commands:     which, deps, clean (also reads project registry),    │
│                 launcher exec,                                       │
│                 ocx package {install,uninstall,select,deselect,      │
│                              exec,env}  [SUPERSEDED names removed:   │
│                  root install/uninstall/select/exec/deselect → pkg;  │
│                  ci export, shell env → REMOVED (exit 64)]           │
└─────────────────────────────────────────────────────────────────────┘

         ┌───────────────────────┐         ┌───────────────────────┐
         │  ocx pull (high-lvl)  │  ◀──▶   │ ocx package pull (low)│
         └───────────────────────┘         └───────────────────────┘
                  pre-warm cache,                pre-warm cache,
                  bindings only                  identifiers only

         ┌───────────────────────┐         ┌───────────────────────┐
         │  ocx run [NEW]        │  ◀──▶   │ ocx exec              │
         └───────────────────────┘         └───────────────────────┘
                  spawn child with               spawn child with
                  binding-derived env            identifier-derived env
```

### API Contract — Layer Promises

**Low-level commands promise:**
- The first positional argument is parsed as an `Identifier` (parsed via `Identifier::parse_with_default_registry` against `OCX_DEFAULT_REGISTRY`). Result: stable across CWD changes, project presence, and lock state.
- Argument shape will not change without a CLI version bump.
- Behaviour is identical inside and outside a project. `ocx.toml` is never consulted.
- Suitable for embedding in CI scripts, GitHub Actions, Bazel rules, devcontainer features.

**High-level commands promise:**
- The first positional argument (when present) is parsed as a binding name. The lock provides identifier resolution.
- A missing `ocx.toml` is a usage error (exit 64). The command does not silently degrade to OCI-tier behaviour.
- A missing `ocx.lock` (when one is required) is a config error (exit 78).
- A stale `ocx.lock` (declaration_hash mismatch) is a data error (exit 65).
- Reserved group names (`default`, `all`) cannot be redeclared.

**Project mutators** (`add`, `remove`, `lock`, `update`) accept OCI identifiers as input (when introducing a binding) but their effect is project-tier — they write `ocx.toml` and/or `ocx.lock`. Their identifier-input shape is part of the low-level promise (stable parsing, registry-aware); their output mutation is part of the high-level promise (lock semantics, declaration_hash discipline).

**Shell activation commands** — [SUPERSEDED → handshake_toolchain_cli.md] `shell hook`, `shell direnv`, `shell init` have been **removed** (exit 64). The replacement is `ocx env [--global] [--shell[=NAME]]` (toolchain-tier env exporter) and `$OCX_HOME/env.sh` sourced from the login profile. Do not implement the commands below.

**Bootstrap / mixed commands** (`init`, `info`, `version`, `shell completion`) make no claims about either tier. `init` writes a fresh `ocx.toml` from nothing; `info`/`version`/`shell completion` report tool-level state.

**Local-store query commands** (`find`, `select`, `deselect`, `deps`, `env`, `shell env`) take OCI identifiers but query the local install/symlink store, not a registry. They are addressed by identifier (low-level promise) but never produce network traffic.

**Asymmetry note: `default` vs `all` reservation.** The two reserved group keywords are deliberately enforced asymmetrically:

- `default` is **literal** — it is the on-disk name for the `[tools]` table. It is reserved at parse-time and mutate-time so a user cannot declare `[group.default]` (which would collide with the implicit default), and so `ocx add --group default` cannot be used to write `[group.default]`. `default` is **always** in the resolution scope (when no `-g` is specified, scope = `[default]`; when `-g all` is specified, scope = `[default, *named_groups]`). At the CLI, `-g default` is not magic — it is the literal name of the default group.
- `all` is a **CLI expansion alias**, not a literal group. Reserving it at parse-time prevents collision (a user cannot declare `[group.all]` and then have the CLI ambiguously interpret `-g all` as "expand" vs "the literal `all` group"). Reserving at mutate-time prevents `ocx add --group all`. The CLI expansion (`-g all` → `[default, *named_groups]`) is a transformation applied before `compose_tool_set`.

This is asymmetric by design: `default` is a name; `all` is a verb. The shared reservation pattern (constant in `internal.rs`, parse-time + mutate-time check) is what makes the reservation safe — neither keyword can leak into a TOML file or a CLI argument with conflicting semantics.

### Data Model — Reserved Names

```rust
// crates/ocx_lib/src/project/internal.rs
pub const DEFAULT_GROUP: &str = "default";
pub const ALL_GROUP: &str = "all";  // NEW
```

Both names are shared single-source-of-truth across `config.rs`, `mutate.rs`, and the project resolve / compose paths. The parse-time check in `crates/ocx_lib/src/project/config.rs:223` extends from a single-name rejection into a small reserved-set check (or two ordered checks — pick whatever reads cleanest in `quality-rust-errors.md` chains; the plan settles this). The mutate-time check at `crates/ocx_lib/src/project/mutate.rs:134` extends `validate_group_name` to refuse `default` and `all` as user-supplied group names.

The `all` expansion is exposed as a library-level helper `pub fn expand_all_keyword(groups: &[String], config: &ProjectConfig) -> Vec<String>` in `crates/ocx_lib/src/project/compose.rs` (alongside `compose_tool_set`). The library `compose_tool_set` itself is not extended to know about `all` — keeping pure composition pure — but the keyword expansion is a pure transformation with no policy, so exposing it as a sibling helper lets non-CLI consumers (mirror tool, future SDK) expand `all` consistently with the CLI. `crates/ocx_cli/src/command/run.rs` calls `expand_all_keyword` before calling `compose_tool_set`. Other project-tier callers (`pull.rs`, `lock`, `update`) adopt the same pattern when they accept `-g`.

## Implementation Plan

Implementation lives in [`plan_cli_run_layering.md`](../state/plans/plan_cli_run_layering.md). Summary of phases:

1. **Stubs** — `command/run.rs` skeleton, `ALL_GROUP` constant, parse-time rejection, mutate-time rejection, helper extraction shape.
2. **Architecture review** — verify stubs match this ADR's contract.
3. **Specification tests** — unit tests for compose extension + parse + mutate; acceptance tests in `test/tests/test_project_run.py`.
4. **Implementation** — fill bodies; extract `load_project_with_lock` helper from `pull.rs`.
5. **Review + documentation** — update user guide, command-line reference, FAQ, in-depth project doc, env-composition cross-link, rules catalog (named layering), `subsystem-cli.md`, `subsystem-cli-commands.md`, `arch-principles.md` (where it lists feature landing locations).

## Validation

- [ ] `task verify` passes after implementation lands.
- [ ] Acceptance test `test_project_run.py::test_run_outside_project_exits_usage_error` confirms the layer-purity rule (no `ocx.toml` → exit 64, not silent passthrough).
- [ ] Acceptance test `test_project_run.py::test_exec_unaffected_by_project_presence` confirms `ocx exec` semantics are unchanged.
- [ ] Acceptance test confirms `all` keyword expands to `default + every named group`.
- [ ] Parse-time rejection of `[group.all]` confirmed by unit test in `config.rs`.
- [ ] Mutate-time rejection of `--group all` confirmed by unit test in `mutate.rs`.
- [ ] Documentation review confirms the layering is named and explained in user guide, FAQ, command-line reference.

## Links

- [`research_run_command_conventions.md`](./research_run_command_conventions.md) — runner-CLI convention survey
- [`plan_cli_run_layering.md`](../state/plans/plan_cli_run_layering.md) — implementation plan
- [`subsystem-cli.md`](../rules/subsystem-cli.md) — CLI subsystem rules
- [`subsystem-cli-commands.md`](../rules/subsystem-cli-commands.md) — quick-reference index
- [`product-context.md`](../rules/product-context.md) — backend-first, clean-env execution principles
- `crates/ocx_cli/src/command/exec.rs` — OCI-tier env-composition primitive
- `crates/ocx_cli/src/command/pull.rs` — project-tier pre-warm primitive
- `crates/ocx_lib/src/project/compose.rs` — orphan compose scaffolding (first consumer = `run`)
- `crates/ocx_lib/src/project/internal.rs` — `DEFAULT_GROUP` constant home

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-08 | Architect | Initial draft — formalize high-level / low-level split, add `ocx run`, reserve `all` |
| 2026-05-08 | Architect (Round 2) | Layer table refined (project-mutators, shell-activation, local-store-queries split out); `default`/`all` asymmetry documented; `expand_all_keyword` moved to lib; Hidden One-Way Doors section added |
| 2026-05-15 | Architect (Opus) | **Amended by `adr_global_toolchain_tier.md`.** New **GLOBAL TIER**: `--global` (global flag on `ContextOptions`, `conflicts_with` `--project`) re-targets project-tier/mutator commands (`add`/`remove`/`lock`/`upgrade`/`pull`/`run`; `install --global` = add+lock+install+select [**pre-refactor — `install --global` removed; per-command `--global` collapsed to root-only `ContextOptions` flag 2026-05-17; canonical form now `ocx --global <subcommand>`**]) to `$OCX_HOME/ocx.toml`. Implicit home discovery (`home_project_path`/Tier-4) removed — the global file is reachable *only* via `--global`. Strict isolation: the global tier never composes into project resolution; `ocx run`/`ocx exec` are hermetic and never read it. Shell activation emits the global `current` set only when no project is in effect (project output supersedes on entry; no merge). |
| 2026-05-15 | Hardening pass | Pre-release CLI name stabilization (breaking, allowed in early phase): `update`→`upgrade` (reserves `update` for the data-refresh `index update`; `upgrade` is the project-toolchain version-bump verb); `find`→`which` (path-lookup intent); top-level `info`→`about` (removes the `info` vs `package info` clash; bare `version` kept). The single-child `generate` group was removed and `shell direnv` relocated: all direnv concerns now live under a dedicated top-level `direnv` group — `direnv init` (writes `.envrc`, bare `ocx direnv` ≡ this) + `direnv export` (the stateless eval target, formerly `shell direnv`). Rationale: direnv is its own ecosystem, not a shell, so `shell` now holds only true shell integration (`env`/`completion`/`hook`/`init`). |
| 2026-05-16 | Gate B reconciliation | **[SUPERSEDED — see `handshake_toolchain_cli.md`]** The SHELL ACTIVATION tier row (commands `shell hook`, `shell init`, `shell env`) and the global-tier `install --global` sugar are **REMOVED** from the implemented CLI. `shell` is reduced to `{completion}` only. OCI-tier primitives (`install`, `uninstall`, `select`, `exec`, `deselect`) are **MOVED** to `ocx package <verb>`. A new root `ocx env [--global] [--shell[=NAME]]` replaces both `shell env` and the global prompt-hook output as the toolchain-tier env exporter. The SHELL ACTIVATION row in the layer table below is no longer live; it remains for historical reference only. Activation is now `$OCX_HOME/env.sh` (sourced from the user's login profile via a block-marker idempotent line written by the in-repo installer). No per-prompt hook, no static `$OCX_HOME/init.<shell>`, no PATH strip — isolation by PATH precedence only. Authority: `handshake_toolchain_cli.md` §2, §4, §5; plan `plan_toolchain_cli.md` C4, C6, C7. |
