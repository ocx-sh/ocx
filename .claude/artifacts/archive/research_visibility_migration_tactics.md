# Research: Visibility Migration Tactics

**Date:** 2026-04-29
**Driven by:** [`adr_visibility_two_axis_and_exec_modes.md`](./adr_visibility_two_axis_and_exec_modes.md) §618 (Implementation Plan step 8: "Migration audit")
**Companion artifacts:** [`research_visibility_propagation_models.md`](./research_visibility_propagation_models.md), [`research_deprecation_telemetry_patterns.md`](./research_deprecation_telemetry_patterns.md), [`research_launcher_exec_mode_prior_art.md`](./research_launcher_exec_mode_prior_art.md)

## Question

OCX is preparing a v1 → v2 metadata schema bump. The change is "additive optional" (new `visibility` field on `env[]` with default `public`) plus a behavioral default flip (`ocx exec` from union-mode to consumer-mode). The current OCX in-tree mirror catalog has **zero packages declaring any deps today** (Phase 1 finding). What migration tactic should the plan recommend?

## Industry Context and Trends

- **Established:** Default-preservation as the primary migration move (Spack, OCX). When the new field's default reproduces old behavior, migration cost collapses to zero.
- **Established:** Lint-first-then-rewrite (Rust editions). Even when ecosystem rewrite is trivial, a lint pass ships first so authors see exactly what will change before it happens.
- **Established:** Escape-hatch flag, not a migration bridge (npm v7 `--legacy-peer-deps`). The hatch is permanent-but-marked-debug; never framed as a temporary migration aid.
- **Declining:** Community-announcement-only (Nix). Works at 90k-package scale only because Nixpkgs has an army of maintainers to submit bulk PRs. Not a model to replicate at OCX's catalog scale.

## Per-Ecosystem Findings

### Nix — `propagatedBuildInputs` / `mkDerivation` API migrations

Nixpkgs uses `lib.warn` / `builtins.trace` at eval-time to emit structured deprecation warnings (e.g., `warnForBadVersionOverride`, [PR #406535](https://github.com/NixOS/nixpkgs/pull/406535)). These fire when a caller passes a combination that is probably wrong (e.g., overrides `version` without `src`). An escape-hatch attribute (`__intentionallyOverridingVersion = true`) silences the warning for conscious callers. No automated rewrite tooling exists — the 90k-package scale makes per-codemod tooling impractical; bulk manual PRs are the norm. Informal deprecation cadence ≈ one stable release cycle (~6 months). The `allowAliases` attribute handles attribute-path renames, not API-shape changes.

**OCX takeaway:** warning-at-parse is feasible; no automation expectation from the Nix model, but OCX's catalog is 4,000× smaller so automation is cheap.

Sources: [nixpkgs issue #433409](https://github.com/NixOS/nixpkgs/issues/433409), [NixOS Discourse breaking-changes thread](https://discourse.nixos.org/t/breaking-changes-announcement-for-unstable/17574/127)

### Spack — `depends_on(type=)` evolution

When Spack added `type=` to `depends_on`, the migration strategy was entirely default-preservation: omitting `type=` falls through to `("build", "link")`, the historical compiled-package behavior. Zero migration cost. No deprecation period, no tooling, no warnings — old metadata continued to parse and behave correctly under new code. The packaging guide documents the new capability alongside the old without marking the old pattern deprecated.

**OCX takeaway:** This is exactly what choosing `visibility: "public"` as the default achieves. Old `metadata.json` files parse under v2 and their env entries behave as `public` — matching their authored intent. The Spack pattern says: **correct default choice = no migration problem.**

Source: [Spack packaging guide — dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)

### Rust editions — SOTA lint-rewrite-fix model

The Rust edition mechanism is the most mature playbook for additive-with-default-flip migrations:

1. Lints fire as `warn` on the current edition for patterns that will break on the next, giving a preparation window.
2. `cargo fix --edition` auto-rewrites lint-addressable issues — purely mechanical syntax transformations with no semantic ambiguity.
3. Human-judgment cases (macros, doctests, build-script-generated code, `#[cfg]`-gated paths) surface as unfixed warnings, requiring manual update.

The rule: "changes that cannot be automated must be required only in a small minority of crates." A `--broken-code` flag preserves partial rewrite output for manual completion. The opt-in-per-crate model prevents ecosystem fragmentation.

**OCX takeaway:** `task migrate:audit` is the edition-lint analog — read-only scan that reports packages where v2 default-consumer-mode exec produces different behavior than v1 union mode. A `task migrate:rewrite` that stamps `"visibility": "public"` explicitly on every existing env entry is safe (mechanical, no semantic change) and is the `cargo fix --edition` analog. At 15 mirrors this is a 10-line script.

Sources: [Rust Edition Guide — transitioning a project](https://doc.rust-lang.org/edition-guide/editions/transitioning-an-existing-project-to-a-new-edition.html), [Rust Edition Guide — advanced migrations](https://doc.rust-lang.org/edition-guide/editions/advanced-migrations.html), [RFC 2052](https://rust-lang.github.io/rfcs/2052-epochs.html)

### npm v7 — `peerDependencies` behavioral default flip

npm v7 changed peerDependency installation from warning-only to hard-install by default. Migration strategy:

- Weekly beta period before GA gave ecosystem exposure time.
- `--legacy-peer-deps` flag shipped as escape hatch restoring pre-v7 behavior — explicitly framed as potentially temporary ("we might have to have this flag enabled by default for a while as projects gradually update").
- `npm ls` continued to warn about missing peer deps even with the flag active — a persistent nudge that the escape hatch is not a permanent final state.
- No automated migration tooling; lock-file rewrite happened automatically on next `npm install`.
- Behavioral flip announced via blog post explicitly listing it as a semver-major change.

**OCX takeaway:** `--mode=full` is the `--legacy-peer-deps` equivalent. The ADR's guidance to mark it "debug only" and not advertise it in primary docs matches npm's framing. A future `ocx check` warning when packages appear to rely on private-env leak would be the `npm ls`-persistent-nudge analog.

Sources: [npm v7 beta announcement](https://blog.npmjs.org/post/626173315965468672/npm-v7-series-beta-release-and-semver-major), [npm peerDependencies docs](https://docs.npmjs.com/cli/v10/configuring-npm/package-json#peerdependencies)

## Design Patterns Worth Considering

- **Default-preservation as migration strategy** — Single highest-leverage decision is default choice. `visibility: "public"` on `env[]` entries means every existing `metadata.json` parses under v2 and behaves identically to v1.
- **Audit task as the lint pass** — `task migrate:audit` walks 15 `mirrors/*/metadata.json` files and reports packages where consumer-mode default would change observable behavior. Today this list is empty; it ships as a contract guard for future mirror authors.
- **Explicit stamp as the auto-rewrite** — `task migrate:rewrite` reads every `metadata.json` and writes back identical files with `"visibility": "public"` added to every env entry that lacks it. Purely mechanical, zero semantic change, eliminates ambiguity about which files have been reviewed for v2.
- **Escape hatch, not migration bridge** — `--mode=full` documented as permanent debug, not "use this while you migrate." If docs say "use during migration," users will never migrate.

## Recommendation for OCX v2

**No deprecation window. No `migrate` subcommand. Ship `task migrate:audit` and (optionally) `task migrate:rewrite`.**

Rationale:

1. Default-`public` already preserves behavior for all 15 existing mirrors. Zero packages break at v2 cutover on the field-addition side.
2. The exec-mode default flip (union → consumer) is the only behavioral change with break potential. Phase 1 finding (zero packages declare `dependencies`) means there are no packages with `private` deps that could expose private env at direct exec. The behavioral flip has zero current affected packages.
3. A deprecation window is appropriate when an installed base breaks at the flip. That installed base is empty. Adds process overhead for zero benefit.
4. `task migrate:audit` ships as a forward guard: catches the first future mirror that would violate the consumer-mode contract. Doubles as documentation of the migration contract for future maintainers.
5. A `migrate` subcommand targeting end-user metadata files is premature until OCX has external publishers with v1-era packages. `task migrate:rewrite` targeting in-tree mirrors is sufficient at current scale.
6. `test/src/helpers.py` `make_package` is the fixture authority. Update it to produce v2-valid metadata (the explicit stamp or the default-absent shape) and let all acceptance test cases derive from it. This is the Rust `cargo fix --edition` pass on the test corpus.

| Question | Answer | Precedent |
|---|---|---|
| Deprecation window? | No — zero affected packages | Spack (none needed when default preserves) |
| `task migrate:audit`? | Yes — forward guard + contract doc | Rust (`cargo fix` lint pass) |
| `task migrate:rewrite`? | Optional but cheap — 10 lines | Rust (`cargo fix --edition` rewrite) |
| `migrate` subcommand? | Defer — no external publishers yet | npm (lock-file rewrite = implicit, no subcommand) |
| Escape hatch? | `--mode=full`, permanent debug | npm (`--legacy-peer-deps`, framed as temporary) |
| `make_package` update? | Yes — fixture authority must track v2 shape | Rust (run fixer against test code paths) |

## Sources

- [nixpkgs issue #433409 — warnForBadVersionOverride](https://github.com/NixOS/nixpkgs/issues/433409)
- [NixOS Discourse breaking-changes thread](https://discourse.nixos.org/t/breaking-changes-announcement-for-unstable/17574/127)
- [Spack packaging guide — dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)
- [Rust Edition Guide — transitioning a project](https://doc.rust-lang.org/edition-guide/editions/transitioning-an-existing-project-to-a-new-edition.html)
- [Rust Edition Guide — advanced migrations](https://doc.rust-lang.org/edition-guide/editions/advanced-migrations.html)
- [RFC 2052 — Rust epochs](https://rust-lang.github.io/rfcs/2052-epochs.html)
- [npm v7 beta release announcement](https://blog.npmjs.org/post/626173315965468672/npm-v7-series-beta-release-and-semver-major)
- [npm peerDependencies docs](https://docs.npmjs.com/cli/v10/configuring-npm/package-json#peerdependencies)
