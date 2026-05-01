# ADR: Two-Axis Visibility (Entry + Edge) and Exec Modes

> **Current implementation state** — for the live module map, facade pattern, error model, and task surface, see [`subsystem-package-manager.md`](../rules/subsystem-package-manager.md), [`subsystem-package.md`](../rules/subsystem-package.md), and [`subsystem-cli.md`](../rules/subsystem-cli.md). This ADR is the design rationale record; read it for *why*, not *what is true today*.

## Metadata

**Status:** Accepted
**Date:** 2026-04-29
**Deciders:** mherwig, architect
**Beads Issue:** N/A (open during plan phase)
**Related PRD:** [`prd_package_entry_points.md`](./prd_package_entry_points.md) — re-opens the §"Out of Scope" deferral of entry-level visibility
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in [`product-tech-strategy.md`](../rules/product-tech-strategy.md)
**Domain Tags:** data | api
**Supersedes:** N/A
**Superseded By:** N/A
**Amends:** [`adr_package_dependencies.md`](./adr_package_dependencies.md) §"Dependency visibility", [`adr_package_entry_points.md`](./adr_package_entry_points.md)

**Reversibility Classification:** **One-Way Door High** for the metadata schema additions (visibility on `env[]`, default-value election) and CLI mode flag. Schema fields ship inside published packages; consumers read them across OCX versions, so changing the field name or default value after ship requires a major-version deprecation cycle. The exec-mode default flip is high-impact behavioral change for in-tree metadata (consumer mode hides private — packages relying on union-default behavior break unless re-tagged). Two-Way for the algebra changes (propagation/merge tables already shipped — only adding entry-level participation).

**Cross-model review recommendation:** Per [`workflow-swarm.md`](../rules/workflow-swarm.md), High-reversibility ADRs auto-trigger Codex cross-model plan review. Recommended `--codex` overlay on the eventual `/swarm-plan high` run.

## Context

OCX has a working visibility model on dependency edges (`sealed | private | public | interface`) introduced by [`adr_package_dependencies.md`](./adr_package_dependencies.md). Three gaps surfaced when reasoning about entry-point semantics for a meta-package:

### Gap 1 — Visibility lives only on the edge, not on the publisher's own env

In CMake, `target_include_directories(T <PRIVATE|PUBLIC|INTERFACE> ...)` puts the keyword next to **what the target declares**. Each include directory carries its own visibility. `target_link_libraries(T <PRIVATE|PUBLIC|INTERFACE> dep)` puts the keyword on the **link edge** — controlling how the linked target's INTERFACE+PUBLIC properties forward through `T`. Two orthogonal axes; both required.

OCX has only the edge axis. A publisher cannot say "my `JAVA_HOME` is consumer-facing but my `_OCX_INTERNAL_FLAG` is internal." The consumer's edge is binary over `P`'s whole env — all-or-nothing per dep. This conflates "consumer's intent" with "publisher's contract" and forces packages to over-expose internal env or omit needed runtime env from consumers.

### Gap 2 — `resolve_env` documented behavior diverges from implementation

[`adr_package_dependencies.md` §208](./adr_package_dependencies.md) states:

> "The `resolve_env()` consumer path filters by `is_consumer_visible()` — `public` and `interface` contribute env vars, `private` and `sealed` do not."

The actual implementation at [`crates/ocx_lib/src/package_manager/visible.rs:139`](../../crates/ocx_lib/src/package_manager/visible.rs) filters by `is_visible()` (anything not `Sealed`). Verified by `worker-architecture-explorer`: there is **no call site** for `is_consumer_visible` outside `Visibility::merge()`'s diamond-axis OR. The runtime pipeline includes `Private` env in direct `ocx exec` invocations regardless of the consumer's distance from the package — `private` leaks at every direct exec target.

This means today's `ocx exec META -- mvn` works correctly (interface deps' env loads), but for the wrong reason: the union default also leaks deps marked `private`. The intent was a single-axis filter; the code is currently axis-agnostic. The user-visible behavior is "everything not sealed loads."

### Gap 3 — Three execution contexts collapsed into one verb

Three distinct call shapes today share `is_visible()` semantics:

1. **Direct user invocation** `ocx exec PKG -- cmd` — human or script wants `PKG`'s contract.
2. **Launcher self-invocation** `cmake.sh → ocx exec 'file://...' -- cmake "$@"` — generated launcher needs PKG's full self env (private + public). Recursion-safety today is achieved by PATH order (raw `${installPath}/bin/cmake` shadows `entrypoints/cmake.sh`), not by env filter — see [`visible.rs:347-352`](../../crates/ocx_lib/src/package_manager/visible.rs).
3. **Transitive consumer** — another package depending on `PKG` should see `PKG`'s public + interface env, never private. This case is handled correctly today through the propagation table at install time.

The launcher self-invocation case (2) has identical env semantics to direct user invocation (1) today, despite their conceptual asymmetry. Adding mode distinction lets the launcher request self-axis explicitly without affecting case (1).

### Why now

Issue #61 (entry points) shipped with the explicit deferral "Entry-point visibility levels (sealed/public/interface) — Not in issue scope; env-var visibility already covers this axis" ([PRD §Out of Scope](./prd_package_entry_points.md)). That deferral assumed env-var visibility covered the axis, but env-var visibility is on the edge, not on the publisher's own entries. The deferred problem is wider than the entry-point line item — it is the fundamental shape of OCX's visibility model. Re-opening now, before in-tree metadata accumulates assumptions about the union-default behavior, prevents a much more painful migration later.

This ADR re-opens the deferral with a unified design that closes all three gaps simultaneously.

## Decision Drivers

- **CMake parity** — OCX's visibility model is already explicitly inspired by CMake; matching CMake's two-axis shape removes a known divergence point and lets ecosystem documentation lean on a familiar reference.
- **Reproducibility** — `ocx exec PKG -- cmd` should produce the same env as a transitive consumer would see for `PKG`'s contract. Today they differ (union vs `is_consumer_visible`).
- **Publisher authority** — The publisher knows what's stable contract vs internal. The consumer's edge filter shouldn't override the publisher's intent on individual env entries.
- **Backward compatibility migration cost** — Default values must be chosen so existing metadata behaves predictably under the new model. Breaking is acceptable per the `project_breaking_compat_next_version` budget but should be deliberate.
- **Recursion safety** — The launcher chain must not re-enter itself. Existing solution (PATH order) keeps working under any new mode design.
- **Discoverability** — Mode flag should be invisible to end users in the common path; launcher embeds it; debug case requires explicit opt-in.
- **Testability** — Each axis must be independently testable. Diamond-merge invariants (commutative, associative — already proven) must hold across every change.

## Industry Context & Research

**Research artifact:** [`research_visibility_propagation_models.md`](./research_visibility_propagation_models.md) — full per-tool survey, comparison matrix, exec-mode survey, divergence-from-precedent notes. Section below summarizes the load-bearing findings.

### CMake — both axes are public API

[`target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html) takes `<INTERFACE|PUBLIC|PRIVATE>` per directory list — the visibility describes what the target itself publishes. [`target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html) takes the same keyword set, controlling how the linked target's INTERFACE+PUBLIC properties flow through the link edge.

Effect:

- `target_include_directories(T PRIVATE x)` — `x` applies to T's compile only; consumers don't see it.
- `target_include_directories(T INTERFACE x)` — `x` applies to consumers only; T's compile doesn't see it.
- `target_link_libraries(T PRIVATE A)` — A's INTERFACE+PUBLIC apply to T's compile; not forwarded.
- `target_link_libraries(T INTERFACE A)` — A's INTERFACE+PUBLIC forwarded to T's consumers; not used by T.

**Diamond merge in CMake** is implicit at "use" time: when building the consumer's compile, CMake walks the transitive graph and applies all reachable PUBLIC + INTERFACE entries. There's no per-target merge state; the merge is materialized only when needed.

### Nix — `propagatedBuildInputs` is the interface analog

[Nixpkgs stdenv §Specifying dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies) distinguishes `buildInputs` (build-time, used by self) from `propagatedBuildInputs` (forward to consumers). [Setup hooks](https://nixos.org/manual/nixpkgs/stable/#ssec-setup-hooks) extend the model: a propagated input can install a hook that runs in every consumer's build.

OCX mapping:
- `buildInputs` ≈ edge `private`
- `propagatedBuildInputs` ≈ edge `public`
- (No exact INTERFACE analog — Nix has `propagatedNativeBuildInputs` for the cross-compilation native axis instead)

Default for unmarked dep: included in self, not propagated — matches OCX `private`.

### Guix — `propagated-inputs` parallels Nix

[Guix package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html) defines `inputs` (build-time, used by self), `native-inputs` (host architecture build tools), `propagated-inputs` (forward to consumers' profiles). Same shape as Nix; same OCX mapping.

### Bazel — providers + `runtime_deps` + `exports`

Bazel's transitive-provider model differs structurally: `deps` carries compile + runtime deps used by self; `runtime_deps` carries consumer-only runtime; `exports` re-exports a dep's API to consumers without consuming it directly. Three orthogonal axes via three attribute names rather than one keyword. Equivalent expressiveness to OCX's edge visibility, but without entry-level granularity.

### Spack — depends_on with `type=` tuple

[Spack docs](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types) has `type=('build', 'link', 'run')` per dependency. Closer to OCX edge visibility but with three explicit axes (build-time use, link-time use, runtime propagation). No entry-level visibility on the package's own env.

### devbox / devenv / mise

devbox's `init_hook` and devenv's `enterShell` compose multiple package envs by simply concatenating PATH and re-applying scalar exports. No declared visibility — every package's env leaks to the shell. mise's plugin model is similar: each plugin emits PATH and exports unconditionally.

### Comparison matrix

| Tool | Entry axis | Edge axis | INTERFACE-equivalent | Default propagation | Diamond merge |
|---|---|---|---|---|---|
| CMake | yes (per-property) | yes (per-link) | yes (`INTERFACE`) | private (own); not propagated | implicit at use-time (walk graph) |
| Nix | no | yes (`propagatedBuildInputs`) | no (Native is different axis) | self only | last-defined wins per attribute |
| Guix | no | yes (`propagated-inputs`) | no | self only | last-defined wins |
| Bazel | no | yes (`deps`/`runtime_deps`/`exports`) | yes (`exports`) | self only | provider merge (rule-defined) |
| Spack | no | yes (`type=` tuple) | partial (`run` without `link`) | depends on type | per-axis aggregation |
| OCX (today) | **no** | yes (4-state) | yes (`interface`) | sealed | OR per axis (commutative, associative) |
| OCX (proposed) | yes (3-state) | yes (4-state, unchanged) | yes (`interface` on both) | public on entry, sealed on edge | OR per axis (unchanged) |

### Key insight

Two ecosystems have explicit entry-axis + edge-axis: **CMake** (universal C/C++ reference; OCX already cites it as inspiration) and to a lesser extent **Bazel** (via providers, but lacks per-property entry visibility). Every other surveyed tool collapses entry+edge into a single edge declaration with implicit "all entries propagate per the edge." OCX's proposed shape lands between CMake (full two-axis) and Nix/Guix (edge-only), with a closer affinity to CMake — appropriate given the existing `target_link_libraries` citation in [`adr_package_dependencies.md:152`](./adr_package_dependencies.md).

### Implications for OCX

| Decision support | Source |
|---|---|
| Two-axis (entry + edge) is established in the most mature reference | CMake |
| `interface` is non-trivial — meta-packages forward env without using it | CMake `INTERFACE`, Bazel `exports` |
| Default-private propagation per axis is conventional | Nix, Guix, Spack |
| Diamond merge as OR-per-axis is convention-free; OCX's existing implementation is novel | Already proven (commutative, associative) — keep |
| Per-entry visibility, not just per-edge, addresses publisher's contract concerns | CMake parity |
| Consumer-only `ocx exec` matches CMake's "consume from outside" mental model | CMake |

## Tension 1 — Default visibility for user-declared `env` entries

Adding a `visibility` field to each `env` entry forces a default choice. Two values are coherent; only one preserves migration smoothness for existing metadata.

### Considered Options

#### Option A — Default `private` (RECOMMENDED, post-research)

| Pros | Cons |
|---|---|
| Conservative — matches "no propagation by default" intuition (alignment with edge default `sealed`) | Existing metadata silently loses propagation under consumer mode |
| Forces publishers to think about contract surface | Migration audit required for every package in the catalog |
| Symmetric with CMake's `target_compile_definitions` private default | High break-rate at the v2 boundary (originally) |
| Industry consensus — every mature ecosystem (CMake, Nix, Guix, Spack, Bazel) defaults self-only / not-propagated; only devbox/devenv/mise leak by default and that is the anti-pattern OCX is moving away from (`research_visibility_propagation_models.md` §131) | |
| Eliminates redundant consumer-side `${installPath}/bin` for tool packages with `entrypoints[]` — synth-PATH (Interface) already provides consumer access; bin/ becomes self-mode only for launcher recursion safety | |
| Aligns with stated CMake-inspiration north star (`adr_package_dependencies.md:152` cites `target_link_libraries` parallel) | |

#### Option B — Default `public` (originally recommended; superseded)

| Pros | Cons |
|---|---|
| Existing metadata authored under "env applies at exec" assumption keeps working under consumer mode | Less conservative — env entries propagate by default through `public` edges |
| Matches author intent — entries declared in `env` are typically the package's *contract* (e.g., `PATH`, `JAVA_HOME`) | A publisher who wanted "internal flag" must now mark it `private` explicitly |
| Migration is clean: today's metadata reads as "every env entry public," which is its operative behavior | Future-default-flip would be a second breaking change |
| | Inconsistent with stated CMake inspiration north star — CMake defaults private |
| | Phase 1 finding: 14 of 15 in-tree mirrors carry only `PATH=${installPath}/bin` + `MANPATH` style entries, which are tool-runtime artifacts, not authored consumer contract — "matches author intent" is an unsupported assumption |

#### Option C — No default; `visibility` is required

| Pros | Cons |
|---|---|
| Forces explicit publisher decision per entry | Breaks every existing metadata file at parse time |
| No silent regression risk | Extreme migration cost; every catalog package needs re-tag |
| Self-documenting | Schema change is hostile to manual authoring |

### Decision Outcome

**Chosen Option:** A — default `private`. (Updated 2026-04-29 after `/swarm-plan` research synthesis surfaced industry consensus + `research_visibility_migration_tactics.md` nullified the migration-cost objection.)

**Rationale:** Five mature ecosystems (CMake, Nix, Guix, Spack, Bazel) default to self-only / not-propagated; only the unmanaged-shell tools (devbox/devenv/mise) leak everything by default, which is the anti-pattern OCX explicitly distances itself from. The original Option B rationale rested on two pillars: (a) "matches author intent" — Phase 1 found 14 of 15 in-tree mirrors carry only tool-runtime PATH/MANPATH entries, not authored contract surface, so the intent-matching argument is unsupported; (b) "migration cost" — `research_visibility_migration_tactics.md` confirmed zero affected external packages and 14 in-tree files editable in a single PR, nullifying the cost argument. Default-private is also symmetric with edge default `sealed` (both axes opt-in to expose), eliminates the redundant `${installPath}/bin` consumer leak for tool packages with `entrypoints[]` (the synth-PATH Interface already provides consumer access; bin/ becomes self-mode-only for launcher recursion safety, which is its operative purpose), and aligns with the stated CMake inspiration north star. Publishers explicitly mark contract entries (`JAVA_HOME`, `MANPATH`) as `public` — symmetric with how `entrypoints[]` already requires explicit consumer-surface declaration.

**Migration:** at v2 cutover, the 14 in-tree bare-binary mirrors get explicit `"visibility": "public"` stamps on their PATH/MANPATH entries (mechanical, single-PR). The 1 entrypoint mirror (cmake) leaves PATH default-private (consumer access via synth entrypoints/) and stamps MANPATH explicit `public` if the publisher wants `man cmake` to work for consumers. All in-tree, all atomic with the schema bump.

**Reversibility:** Two-Way at the schema level (additive-optional field with documented default), but One-Way at the behavioral level (every shipped package whose env was authored under "default private" depends on the default not flipping). Flipping later requires a v3 schema bump and a re-tag of every catalog package.

## Tension 2 — Exec-mode default

Today's `is_visible()` filter at `visible.rs:139` is the union (anything-not-sealed). This conflates self-execution with consumer-mode resolution. Three options for the new default.

### Considered Options

#### Option A — Default `consumer` mode (RECOMMENDED)

| Pros | Cons |
|---|---|
| Matches `adr_package_dependencies.md:208` documented intent | Behavioral change for any caller relying on private leak |
| Symmetric with how transitive consumers see the package today (`is_consumer_visible` in `propagate`) | Binaries that need private env at direct exec must now route via launcher |
| User typing `ocx exec PKG` is *consuming* PKG, not *being* PKG | Forcing-function discipline — proper publisher metadata required |
| Closes Gap 2 (impl/intent divergence) by aligning impl with intent | Migration: packages with private deps that bare binaries (not entry points) need either elevation to `public` or entry-point declaration |

#### Option B — Default `full` (preserve today's union behavior)

| Pros | Cons |
|---|---|
| Zero break for existing call sites | Perpetuates Gap 2 (intent/impl divergence) |
| Easiest migration | Loses the proper "consumer-mode" mental model |
| Private env continues leaking at direct exec | Adds a `--mode=consumer` flag nobody uses by default; new mode design has no default driver |

#### Option C — Auto-detect from `cmd` argument

| Pros | Cons |
|---|---|
| "Smart" default based on whether `cmd` matches an entrypoint | Magical behavior; non-obvious failure modes |
| Could route entrypoint-cmd through self-mode automatically | Requires entrypoint metadata at exec time before composing env — chicken-and-egg |
| | Two ways for the same call to behave differently (e.g., `cmake-gui` vs `cmake` with same package) — discoverability collapse |

### Decision Outcome

**Chosen Option:** A — default `consumer` mode.

**Rationale:** Aligns code with documented intent in [`adr_package_dependencies.md:208`](./adr_package_dependencies.md). The user typing `ocx exec PKG -- cmd` is conceptually a transient consumer of `PKG`'s contract, not "PKG itself running." The launcher self-invocation case (the only path where the package "runs as itself") explicitly requests `--mode=self`. Migration cost is real but bounded: packages whose binaries need private env must either declare those binaries as entrypoints (which routes through self-mode launcher) or elevate the env to public. This is a forcing function for proper metadata practice and matches CMake's mental model exactly: a CMake user calling a target from outside doesn't see PRIVATE include directories.

**Reversibility:** One-Way High behaviorally — once shipped, every caller's expectation calibrates to consumer-mode default. Reverting later breaks the inverse direction. The mode flag itself is Two-Way (additive); the default value is One-Way.

## Tension 3 — Adding `visibility` to `env` entries

The schema additions touch every published package's `metadata.json` shape. Two field-shape options for the entry-level visibility marker.

### Considered Options

#### Option A — `visibility: "private" | "public" | "interface"` on each `Var` (RECOMMENDED)

| Pros | Cons |
|---|---|
| Same vocabulary as edge `visibility` — single concept, two surfaces | Field name collision with edge field; readers must distinguish by location |
| Reusable enum (`Visibility::Sealed` rejected at parse for entries — see Tension 4) | Slight ergonomic friction for hand-authored metadata |
| Schema codegen via existing `Visibility` schemars derive | Requires `serde` `default = "public"` annotation on `Var` |

#### Option B — Separate `scope: "self" | "shared" | "consumers-only"` enum

| Pros | Cons |
|---|---|
| Decouples vocabulary from edge-side `visibility` | Two enums to maintain; more cognitive load |
| Could rename axes more intuitively for entries | Loses CMake parallel naming (PRIVATE/PUBLIC/INTERFACE) |
| | Increases test surface (separate axis-mapping tests) |

#### Option C — Boolean pair `consumer_visible: bool, self_visible: bool`

| Pros | Cons |
|---|---|
| Maximum clarity — directly encodes the two axes | Verbose; four-state visibility expressed as 2-bit field |
| | Violates [`quality-rust.md`](../rules/quality-rust.md) Warn-tier "Boolean parameters where enum/literal type clearer" |
| | Author can write `(false, false)` which is sealed (rejected) |

### Decision Outcome

**Chosen Option:** A — same `visibility` vocabulary, restricted to 3 values on entries (`Sealed` rejected at parse).

**Rationale:** Single vocabulary across both surfaces makes the model coherent. Authors learn one concept. The 3-vs-4 state distinction is enforced at deserialization (`Sealed` on an entry rejected with a structured error) and documented in the metadata reference. CMake parallel naming preserved. Schema codegen reuses existing `Visibility` derive, with a wrapper newtype `EntryVisibility` if schemars cannot represent the value-restriction, falling back to a `serde` validator on `Var` post-deserialization.

**Reversibility:** Two-Way at the field level (additive-optional, default `public`), One-Way at the value-set level (rejecting `Sealed` is locked once shipped).

## Tension 4 — Whether `Sealed` is a valid entry visibility

`Sealed` makes no operational sense on a declared env entry — declared but invisible everywhere = dead config. But allowing it preserves enum unity.

### Considered Options

#### Option A — Reject `Sealed` at parse (RECOMMENDED)

| Pros | Cons |
|---|---|
| Catches author mistakes early | Asymmetric visibility set per surface |
| Self-documenting error message | Newtype or runtime validator required |
| | Edge cases for schema-only consumers (the JSON Schema must encode the restriction) |

#### Option B — Accept `Sealed`; treat as "deletion marker"

| Pros | Cons |
|---|---|
| Symmetric vocabulary | No precedent for it; surprising semantics |
| | Author intent unclear |
| | Adds runtime check elsewhere |

#### Option C — Rename per-surface

| Pros | Cons |
|---|---|
| Cleanest semantics | Two enums to learn (see Tension 3 Option B) |

### Decision Outcome

**Chosen Option:** A — reject `Sealed` on entries with structured error `EntryVisibilityError::SealedNotAllowed`. Schema codegen produces `enum: ["private", "public", "interface"]` for `Var.visibility`. The enum type stays unified internally; the validator runs at metadata deserialization.

**Rationale:** Fail-loud beats silent semantics. Rejecting at parse means `metadata.json` author errors don't slip past publish-time validation.

**Reversibility:** Two-Way — relaxing later (accepting `Sealed` with documented semantics) is additive. Tightening further (e.g., rejecting `Interface` too) is breaking and requires schema bump.

## Tension 5 — Mode flag shape

Three exec call shapes need distinct env semantics. How to expose mode selection on the CLI.

### Considered Options

#### Option A — `--mode=consumer|self|full` enum flag, default `consumer` (RECOMMENDED)

| Pros | Cons |
|---|---|
| Single flag, three values, clean discoverability | New flag in CLI surface |
| Launcher emits `--mode=self`; humans rarely see it | One additional shape to test |
| `--mode=full` is the explicit debug escape hatch | Flag value vocabulary must outlive shape changes |

#### Option B — Two boolean flags `--self`, `--full`

| Pros | Cons |
|---|---|
| Maps to two-axis model | Three modes in a 2-bit space — `(--self, --full)` = 4 combos, one nonsensical |
| | Violates Rust quality boolean-flag warning |

#### Option C — Two new verbs `ocx run PKG -- cmd` (consumer), keep `ocx exec` as self-mode

| Pros | Cons |
|---|---|
| Self-documenting | Renames an established verb; breaking |
| Verbs read naturally | Existing scripts using `ocx exec PKG -- cmd` would change semantics overnight |
| | Two verbs for nearly identical behavior — discoverability collapse |

#### Option D — Implicit mode via env var (`OCX_EXEC_MODE`)

| Pros | Cons |
|---|---|
| No new flag in user-facing CLI | Hidden state in env — surprising |
| Launcher sets the var, calls `ocx exec` | Diagnosability collapse — `ocx exec PKG --foo` produces different behavior depending on shell history |

### Decision Outcome

**Chosen Option:** A — `--mode=consumer|self|full` flag.

**Rationale:** Single flag, explicit values, clean default. Launcher embeds `--mode=self` in the generated body — invisible to users in the common path. `--mode=full` covers the rare debug case (`OCX_EXEC_MODE`-style env var rejected as Option D's hidden state). Verb-level split (Option C) was tempting for clarity but breaks established `ocx exec` semantics overnight; an additive flag is strictly better for migration.

**Reversibility:** Two-Way for the flag mechanism (additive). One-Way for the **default value** (per Tension 2). Removing the flag later requires a deprecation cycle and reverts the launcher format — unlikely needed.

## Tension 6 — Entrypoint synthesis: existing implementation vs ADR commitment

The synthetic PATH entry already exists in [`visible.rs:344-373`](../../crates/ocx_lib/src/package_manager/visible.rs). It is currently emitted before declared env entries on purpose — declared `${installPath}/bin` prepends *after* and ends up first in PATH, ensuring `ocx exec file://<pkg>` resolves the raw binary instead of the launcher (recursion-safe by ordering, not by mode filter). The comment at `visible.rs:347-352` documents this as load-bearing.

Two options for how the new mode model interacts with existing synthesis.

### Considered Options

#### Option A — Treat synthetic entry as implicit `Visibility::Interface` on `PATH` (RECOMMENDED)

| Pros | Cons |
|---|---|
| Consumer mode includes synth (interface is consumer-visible) | Synth is a runtime-only concept — `Visibility::Interface` is a metadata concept; coupling them requires care |
| Self mode excludes synth automatically (`is_self_visible` rejects Interface) | Existing test `apply_visible_packages_synthetic_path_before_declared_env` locks order; need parallel test for mode |
| Recursion-safety becomes axis-driven, not order-driven | Existing recursion-safety-by-order is **belt** to the new **suspenders** — keep both |
| Diamond merge applies automatically through visibility algebra | Documentation effort |

#### Option B — Keep order-only recursion safety; don't axis-tag synth

| Pros | Cons |
|---|---|
| No conceptual coupling | Synth must be filtered separately in self mode (extra code path) |
| Existing comment + test stays as primary contract | Two recursion-safety mechanisms in the codebase |

### Decision Outcome

**Chosen Option:** A — synth is `Visibility::Interface` semantically; PATH order rule preserved as defense-in-depth.

**Rationale:** Folding synth into the visibility algebra keeps the model uniform and lets diamond-merge / propagation handle entrypoints "for free." The existing PATH-order recursion-safety is preserved as a redundant safety: even if a future change accidentally exposes synth in self mode, the order rule still places `${installPath}/bin/<name>` ahead of `entrypoints/<name>` and prevents launcher recursion. Two independent mechanisms, both correct.

**Reversibility:** Two-Way — synth tagging is internal to the resolver; can be reverted to a parallel pass without metadata change.

## Tension 7 — Schema migration path

Adding `visibility` to `env[]` entries is an additive-optional field but requires schema codegen update + docs sync.

### Considered Options

#### Option A — Single schema bump v1 → v2 covering all changes (RECOMMENDED)

| Pros | Cons |
|---|---|
| One migration boundary | Aggregates breaking and additive changes |
| Catalog migration (re-tag) happens once | Requires v2 ship before any of the new behavior is observable |

#### Option B — Per-surface staged rollout (entry visibility v1.5, then exec mode v2)

| Pros | Cons |
|---|---|
| Smaller per-step surface | Two migration boundaries |
| | Catalog re-tag burden doubled |
| | "v1.5" is unconventional; tools that pin to v1 don't know what to do |

### Decision Outcome

**Chosen Option:** A — single v2 bump.

**Rationale:** Per the auto-memory `project_breaking_compat_next_version` (no optional fields or fallback paths in the next version), the next version is already the breaking-compat boundary. Bundle all visibility changes into v2 to keep the migration story single-pass. v1 packages continue to parse via a v1 reader (entry visibility absent → defaults to public; mode flag absent → defaults to consumer).

**Reversibility:** One-Way — once v2 ships, v1 metadata is forever the "absent visibility = public" reading.

## Decision Summary

| Tension | Chosen | Rejected alternatives | Close call? |
|---|---|---|---|
| 1 — Default entry visibility | `private` (post-research flip) | `public`, required | Originally close; post-research industry-consensus + CMake-parity decisive |
| 2 — Exec mode default | `consumer` | `full`, auto-detect | Close — break vs documented intent |
| 3 — Entry visibility field shape | unified `visibility` enum | separate enum, boolean pair | No — vocabulary unity wins |
| 4 — `Sealed` on entries | reject at parse | accept, rename per-surface | No — fail-loud beats silent |
| 5 — Mode flag shape | `--mode=consumer|self|full` | two booleans, two verbs, env var | No — single flag clean |
| 6 — Synth as `Interface` | algebra-tagged | order-only | Close — defense in depth approach reconciles |
| 7 — Schema migration | single v2 bump | staged | No — single boundary cheaper |

## Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| Visibility surfaces | 1 (edge only) | 2 (entry + edge) | Entry-level is new |
| Exec modes exposed | 1 (implicit union) | 3 (consumer default, self for launcher, full debug) | Mode flag additive |
| Default at direct exec | Union (`is_visible`) | Consumer (`is_consumer_visible`) | Closes Gap 2 |
| Schema fields added | 0 | 1 (`visibility` on `Var`) | Additive-optional |
| `visible.rs:139` filter call | `is_visible()` | mode-dispatched (`is_self_visible` / `is_consumer_visible` / `is_visible`) | Behavioral |
| Default entry visibility | N/A | `private` | Symmetric with edge `sealed`; CMake parity; industry consensus (research §131) |
| Default edge visibility | `sealed` (unchanged) | `sealed` (unchanged) | No change |

### Consequences

**Positive:**
- Two-axis model matches CMake; documentation can lean on the established mental model.
- Direct `ocx exec` behaves consistently with transitive-consumer view of the same package.
- Publishers gain per-entry control over what's contract vs internal — the missing dial today.
- Meta-packages with `interface` deps work cleanly at direct exec (currently work for the wrong reason).
- Recursion-safety has both axis filter and PATH order — defense in depth.
- Closes the documented divergence between `adr_package_dependencies.md:208` intent and implementation.

**Negative:**
- Behavioral change at direct exec: packages that depend on private-leak today break under consumer mode. Migration audit required for every catalog package whose binaries need private dep env (must elevate to public OR declare as entrypoint).
- Schema additions require docs sync (metadata.md, entry-points.md), schema regen, acceptance test updates.
- New CLI flag adds testing surface (three modes × multiple package shapes = 9+ acceptance test cases minimum).
- Mode flag must outlive future shape changes — pre-launcher tools that bake `--mode=self` are locked to that shape.

**Risks:**
- **Catalog regression at v2 cutover.** *Mitigation:* migration audit script that flags packages whose deps include `private` visibility AND the package has bare binaries (no entrypoint declaration); tag-bump those packages to either declare entrypoints or elevate the dep to `public`.
- **Author confusion between entry vs edge `visibility`.** *Mitigation:* metadata.md doc page must clearly separate the two concepts with the table from the conversation; schemars descriptions on each field must point to the right anchor.
- **Synth-as-Interface coupling fragility.** *Mitigation:* order-based recursion-safety (the existing test contract) stays as defense in depth; new test asserts mode-axis filter rejects synth in self mode.
- **`--mode=full` becomes the de-facto default for users who hit private-leak migration pain.** *Mitigation:* document `--mode=full` as **debug only** in metadata reference; do not advertise it in primary docs; consider adding a deprecation path post-v2 if telemetry shows drift.

## Technical Details

### Schema Shape (v2)

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin",
      "visibility": "public" },
    { "key": "JAVA_HOME", "type": "constant", "value": "${installPath}",
      "visibility": "public" },
    { "key": "_OCX_INTERNAL", "type": "constant", "value": "1",
      "visibility": "private" }
  ],
  "entrypoints": [
    { "name": "java", "target": "${installPath}/bin/java" }
  ],
  "dependencies": [
    { "identifier": "ocx.sh/openssl:3@sha256:...",
      "visibility": "private" }
  ]
}
```

`visibility` on `env[]` entries is optional, default `public`. Accepts `"private" | "public" | "interface"`. `"sealed"` rejected at parse.

`visibility` on `dependencies[]` entries unchanged (4-state, default `sealed`).

### Resolution Algorithm (consumer mode, the new default)

```
fn resolve_env(roots: &[InstallInfo], mode: ExecMode) -> Vec<Entry> {
    let visible = import_visible_packages(roots);  // Phase A: same as today,
                                                   // filters edges by is_visible()
                                                   // (keeps Sealed exclusion)

    let entries = Vec::new();
    for pkg in visible {
        // Synth entrypoint PATH entry — tagged Visibility::Interface
        if pkg.has_entrypoints() && mode.includes(Visibility::Interface) {
            entries.push(synth_path_entry(pkg.entrypoints_dir()));
        }
        // Per-entry filter using mode + entry-level visibility
        for var in pkg.metadata.env() {
            if mode.includes(var.visibility) {
                entries.push(resolve_template(var, &pkg.dep_contexts));
            }
        }
    }
    entries
}

enum ExecMode { Consumer, Self, Full }

impl ExecMode {
    fn includes(&self, v: Visibility) -> bool {
        match self {
            ExecMode::Consumer => v.is_consumer_visible(),  // public + interface
            ExecMode::Self     => v.is_self_visible(),      // public + private
            ExecMode::Full     => v.is_visible(),           // any non-sealed
        }
    }
}
```

The `Visibility::Sealed` case is rejected at parse for entries; runtime never sees it on a `Var`. Edge-level `Sealed` is filtered out at Phase A as today.

### CLI Surface

```
ocx exec [--mode <consumer|self|full>] <package-or-path> -- <cmd> [args...]
```

Default: `--mode=consumer`.

Generated launcher body (Unix):

```sh
#!/bin/sh
exec ocx exec --mode=self 'file:///<pkg-root>' -- "$(basename "$0")" "$@"
```

Generated launcher body (Windows):

```bat
@ECHO off
SETLOCAL DisableDelayedExpansion
ocx exec --mode=self "file://<pkg-root>" -- "%~n0" %*
```

### Worked Examples

#### Example 1 — `cmake-with-cuda`

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin",
      "visibility": "public" },
    { "key": "CMAKE_ROOT", "type": "constant", "value": "${installPath}/share/cmake",
      "visibility": "public" }
  ],
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" },
    { "name": "ctest", "target": "${installPath}/bin/ctest" }
  ],
  "dependencies": [
    { "identifier": "ocx.sh/cuda-rt:12@sha256:...", "visibility": "private" }
  ]
}
```

| Caller | Mode | Sees |
|---|---|---|
| `ocx exec cmake-with-cuda -- cmake --version` | `consumer` (default) | `PATH` (public) + `CMAKE_ROOT` (public) + synth `entrypoints/` (interface). cuda-rt **not** loaded directly. `cmake` resolves via launcher → `ocx exec --mode=self file://...` → cuda-rt's exported env loads through `private` edge filter → cmake runs with full self env. |
| Launcher `cmake.sh` (transparent to user) | `self` | `PATH` (public) + `CMAKE_ROOT` (public) + cuda-rt's exported env (filtered through edge `private` to self). No synth (interface filtered out). PATH order: `${installPath}/bin` first → raw cmake binary, no recursion. |
| Build script depending on `cmake-with-cuda` (edge=`public`) | `consumer` (transitive) | cmake-with-cuda's public env (`PATH`, `CMAKE_ROOT`) + synth entrypoints (interface). cuda-rt's env **not** propagated (edge `private` blocks consumer-axis at the cmake-with-cuda → cuda-rt edge). |
| Debug `ocx exec --mode=full cmake-with-cuda -- env` | `full` | Everything non-sealed including cuda-rt's exported env for inspection. |

#### Example 2 — `java-toolchain` meta-package

```json
{
  "type": "bundle",
  "version": 1,
  "env": [],
  "entrypoints": [],
  "dependencies": [
    { "identifier": "ocx.sh/jdk:21@sha256:...",     "visibility": "interface" },
    { "identifier": "ocx.sh/maven:3.9@sha256:...",  "visibility": "interface" },
    { "identifier": "ocx.sh/gradle:8.5@sha256:...", "visibility": "interface" }
  ]
}
```

| Caller | Mode | Sees |
|---|---|---|
| `ocx exec java-toolchain -- mvn` | `consumer` | JDK + Maven + Gradle public/interface env via interface edges. `mvn` resolves via Maven's synth `entrypoints/` → Maven's `mvn.sh` → `ocx exec --mode=self file://maven` → Maven's own self env. |
| Launcher chain inside Maven | `self` | Maven's private + public env. Toolchain's contribution: nothing (toolchain has empty `env`). |
| `echo $PATH` inside `ocx exec java-toolchain` | `consumer` | PATH includes synth `entrypoints/` for JDK + Maven + Gradle. Their `${installPath}/bin` is included only if their own env declared it as `public` (typical for tools). |

#### Example 3 — Diamond with mixed visibility

```
toolchain
├── deps[0]: jdk (edge=interface)
└── deps[1]: build-helpers
              └── deps[0]: jdk (edge=public)
```

JDK reachable via two edges: `interface` (from toolchain) and `public` (via build-helpers).

`Visibility::merge`:
- Path A: toolchain → jdk via interface = `Interface` (self=false, consumer=true)
- Path B: toolchain → build-helpers → jdk: `public.propagate(public-of-jdk)` = `Public` (self=true, consumer=true) — assuming jdk's own env is public, which is the propagation child shape
- Merged: `merge(Interface, Public)` = `Public` (self=true||false=true, consumer=true||true=true)

Effective at toolchain root: JDK contributes as if directly `public` from toolchain. `is_consumer_visible` true → JDK env loads in consumer mode. Same algebra as today; no change.

## Implementation Plan

Plan artifact lives at `.claude/state/plans/plan_visibility_two_axis.md` (created by `/swarm-plan` after this ADR is approved).

High-level phases:

1. [ ] **Schema + types** — Add `visibility: Visibility` field to `Var` (default `public`); reject `Sealed` at parse via post-deserialization validator; add `EntryVisibilityError` variant. Regenerate JSON schema.
2. [ ] **Mode enum + flag** — Add `ExecMode { Consumer, Self, Full }` to `ocx_lib::cli` (or inline in `ocx_cli::command::exec`); add `--mode` flag to `Exec` clap struct; default `Consumer`.
3. [ ] **Filter dispatch** — Replace `dep.visibility.is_visible()` at `visible.rs:139` with mode-dispatched filter. Replace synthetic-PATH-emit gate to also check mode (`mode.includes(Interface)`). Add per-entry visibility check inside `common::export_env` loop.
4. [ ] **Launcher emission** — Update `entrypoints.rs` Unix + Windows launcher bodies to embed `--mode=self`. Update byte-exact tests for new shape.
5. [ ] **Schema regen** — `task schema:generate`; verify schema reflects new `visibility` field on `Var` with restricted enum.
6. [ ] **Docs** — Update `website/src/docs/reference/metadata.md` (add Entry Visibility subsection mirroring Edge Visibility), `website/src/docs/guide/entry-points.md` (rewrite intro per Finding 2 from the prior conversation; clarify mode interaction), `adr_package_dependencies.md` §208 (correct stale claim or annotate as superseded by this ADR).
7. [ ] **Acceptance tests** — Three exec modes × three package shapes (cmake-with-cuda private dep, java-toolchain interface deps, diamond merge case). Cover migration story explicitly.
8. [ ] **Migration audit** — Scan in-tree mirror metadata for packages declaring `private` deps that ship bare binaries (no entrypoints). Document the elevate-or-declare decision per package.
9. [ ] **Codex cross-model review** — Per Reversibility Classification, opt in `--codex` overlay during `/swarm-plan` for this ADR's plan artifact.

## Validation

- [ ] Acceptance tests cover three modes × diamond merge × mixed entry visibility — all pass.
- [ ] Schema gen output verified by hand: new field on `Var`, `Sealed` excluded from value enum.
- [ ] Docs site builds; cross-references resolve.
- [ ] Migration audit lists every package needing re-tag; each item triaged.
- [ ] `task verify` passes locally.
- [ ] Cross-model review (`--codex`) finds no blocking concerns.

## Open Questions

- [ ] **Per-variable overrides on edge visibility** — [`adr_package_dependencies.md:210`](./adr_package_dependencies.md) flagged this as future direction (`expose: [PATH]` on a `sealed` dep). Does entry-level visibility supersede that future direction, or do they compose? Likely supersede: entry visibility on the **dep's** env entries is the proper home for "expose only PATH"; the edge visibility filter then receives a refined contract from the dep. Defer concrete answer to a follow-up ADR if the use case surfaces.
- [x] **`--mode=full` deprecation** — Resolved: `ExecModeFlag::Full` removed before ship. Two user-facing modes (`consumer`, `self`) cover both real runtime axes; the union (full) added nothing actionable and invited reaching for "show me everything" to paper over publisher-metadata gaps. The lib retains an internal-only `ExecMode::Full` variant used by the install-time entrypoint collision check (it must cover the union of every later runtime filter); that variant is unreachable from argv. Passing `--mode=full` now exits 64 (`UsageError`) like any other unknown value. Removes the `OCX_NO_MODE_FULL_WARNING` env var, the `[OCX-WARN]` stderr marker, and `ExecMode::warn_if_full` along with it.
- [ ] **Synth entry's per-entry visibility** — Could a future feature let publishers mark individual entrypoints as `private` (select-time only, don't propagate)? YAGNI per conversation; revisit only if a real publisher asks.
- [ ] **`ocx env PKG` mode** — Same dispatch as `ocx exec`? Likely yes, same `--mode` flag, same default. Acceptance test required either way.

## Links

- [`adr_package_dependencies.md`](./adr_package_dependencies.md) — original four-level visibility model on edges
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — entry-point launcher mechanism, deferred entry-level visibility
- [`prd_package_entry_points.md`](./prd_package_entry_points.md) — PRD §"Out of Scope" deferral re-opened by this ADR
- [`system_design_composition_model.md`](./system_design_composition_model.md) — four composition mechanisms (deps, layers, patches, variants)
- [`research_launcher_patterns.md`](./research_launcher_patterns.md) — prior art for launchers
- CMake [`target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html), [`target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html)
- Nixpkgs [stdenv §dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies)
- Guix [package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html)
- Bazel [providers documentation](https://bazel.build/extending/rules#providers)
- Spack [dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-29 | architect (opus) | Initial draft from in-conversation reasoning + worker-architecture-explorer findings + worker-researcher fallback (industry context inline) |
| 2026-04-29 | mherwig + orchestrator | **Tension 1 decision flipped B → A (default `private`).** Triggered by user observation that consumer-side `${installPath}/bin` is redundant under `entrypoints[]` synth-PATH-as-Interface. Research synthesis (`research_visibility_propagation_models.md` §131 industry consensus + `research_visibility_migration_tactics.md` zero-cost migration) made the flip decisive. Decision Summary table, Quantified Impact "Default entry visibility" row, and Tension 1 prose all updated. Migration semantics revised: 14 in-tree bare-binary mirrors get explicit `"visibility": "public"` on PATH/MANPATH at v2 cutover (atomic with schema bump). |
| 2026-04-30 | mherwig + orchestrator | **`--mode=full` removed from CLI surface before ship.** `ExecModeFlag::Full` deleted; only `consumer` and `self` reach argv. `ExecMode::Full` survives lib-internal for fetch-time entrypoint-collision traversal. `OCX_NO_MODE_FULL_WARNING` env var, `[OCX-WARN]` stderr marker, and `ExecMode::warn_if_full` removed. Open question §640 closed. Truth-table reduced from 3 columns to 2 across exec-modes / user-guide / command-line docs. |
| 2026-04-29 | orchestrator (doc-writer) | **Implementation landed.** Status flipped `Proposed → Accepted`. Implementation delivered via `plan_visibility_two_axis` (5-phase swarm-execute, max tier). Three One-Way Door commitments now live on disk: (1) `Var.visibility` field in v2 schema with default `private` — changing field name or default requires deprecation cycle; (2) `ExecMode::Consumer` as the default exec mode for 6 commands — flipping breaks every shipped caller of `resolve_env`; (3) `--mode=self` literal string baked into every generated launcher on disk — renaming the flag invalidates all installed packages. |
| 2026-04-30 | mherwig + orchestrator | **Visibility collapsed to a struct of two booleans; `ExecMode` deleted; CLI flag becomes `--self`.** Driven by post-merge implementation review: the 16-cell `propagate`, 16-cell `merge`, 12-cell `ExecMode::includes`, and several 4-arm renderers all spelled out the cartesian product of a two-axis lattice. `Visibility` is now a struct `{ private: bool, interface: bool }` with named constants `SEALED`/`PRIVATE`/`PUBLIC`/`INTERFACE` (custom serde keeps the wire format byte-identical). `propagate` collapses to one if-else; `merge` is OR per axis. `ExecMode` (3 variants) and `ExecModeFlag` (2 variants) are gone — env emission, edge filtering, and synth-PATH gating share the single `var.visibility.intersects(view)` predicate, where `view: Visibility` flows in from CLI / launcher / install code. The CLI exposes one boolean `--self`: off → consumer view (`Visibility::INTERFACE`), on → both axes (`Visibility::PUBLIC`). Generated launcher templates embed `--self` instead of `--mode=self` — breaking wire change to launcher scripts, acceptable in next breaking version. **Tension 6 trade-off (suspenders dropped, belt held)**: in the prior model `Self_` excluded `Interface` so the synth-PATH entry would not re-emit during launcher self-invocation; PATH-order (declared `bin/` prepends after synth-PATH and wins lookup) was the belt, the algebra filter was the suspenders. After this refactor `--self` exposes both axes including `Interface`, so synth-PATH re-emits inside launcher invocations. PATH-order is now the sole recursion guard — exhaustively pinned by `apply_visible_packages_synthetic_path_before_declared_env`, `emit_env_synthetic_path_equals_package_dir_entrypoints`, and the related synth-PATH tests. The user has not been able to identify a real-world case where excluding `Interface` from self-execution is semantically required, so the simplification is judged worth one redundant defence layer. The "One-Way Door" wire-vocabulary commitment for `--mode=self` is replaced by a new commitment to `--self`. |
| 2026-04-30 | mherwig + orchestrator | **Tension 7 walked back: v2 schema bump avoided.** `Bundle.version` field + `Version` enum **kept** (single `V1` variant for now), but `Version::V2` and the v1→v2 dual-reader path are removed. Generated JSON Schema reverts to `schemas/metadata/v1.json`. Driver: next release is breaking anyway (per `project_breaking_compat_next_version` memory); a parallel v2 schema during the same break window is unnecessary ceremony. The `version` field stays so a future second variant can ship without a structural migration. Test fixtures (~30 in `test/`, 19 mirror `metadata.json` files), doc examples, and `subsystem-metadata-schema.md` rule reflect the single-version state. v1 back-compat test in `bundle.rs` reframed as default-visibility test. |
| 2026-04-30 | mherwig + orchestrator | **Tension 6 reverted: self mask flipped `Visibility::PUBLIC` → `Visibility::PRIVATE`.** Re-discussion of the prior simplification concluded the "algebra uniformity" rationale didn't hold — the `intersects` predicate is uniform regardless of mask choice; mask is data, not code. User-facing semantics now symmetric: consumer view = consumer-axis filter (`Visibility::INTERFACE`, emits `public` + `interface`); self view = self-axis filter (`Visibility::PRIVATE`, emits `private` + `public`). Each `EntryVisibility` value maps to exact role: `private` self-only, `interface` consumer-only, `public` both. **Restored**: algebra filter as recursion suspenders alongside PATH-order belt — synth-entrypoints PATH (tagged `Visibility::INTERFACE`) drops automatically from self view via `Visibility::INTERFACE.intersects(Visibility::PRIVATE) == false`. Publisher's explicit `visibility: interface` declaration on entries (e.g. `PKG_CONFIG_PATH`, library include hints) is now honored — no longer leaks into self runtime. Wire commitment unchanged: `--self` flag literal stays. Tests updated: `emit_env_self_includes_private_var` now uses PRIVATE mask; new `emit_env_self_drops_interface_only_var`, `apply_visible_packages_self_drops_synth_path_for_own_package`, `import_visible_packages_self_excludes_interface_only_dep`. The earlier "no real-world case for excluding Interface from self" claim is reversed: publisher-declared interface-only entries are precisely the case, and the symmetric model encodes that intent without runtime cost. |
