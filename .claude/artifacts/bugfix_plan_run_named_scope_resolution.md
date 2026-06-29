# Bug Fix Plan: `ocx run NAME` must not resolve the whole group's host leaves

<!--
Bug Fix Plan
Filename: artifacts/bugfix_plan_run_named_scope_resolution.md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer
-->

## Status

- **Plan:** bugfix_run_named_scope_resolution
- **Active phase:** 7 — Commit & Document
- **Step:** /swarm-execute → Review-Fix Loop
- **Last update:** 2026-06-30 (implementation complete; pre-commit gate)

---

## Overview

**Status:** In Progress
**Author:** Michael Herwig (with Claude)
**Date:** 2026-06-30
**GitHub Issue:** N/A
**Severity:** Medium

`ocx run [NAME...] -- ARGV` is the project-tier child-spawn command. Resolution
of the **entire selected scope** happened before narrowing to the requested
bindings, so a `NoHostLeaf` on an unrelated, unnamed sibling aborted a
narrowly-named run.

## Bug Report

### Observed Behavior

`ocx run cmake -- cmake --version` exits 78 (`NoHostLeaf`) when *some other*
default-group tool ships no leaf for the current host — even though the user
only asked for `cmake`.

### Expected Behavior

Named-subset runs succeed as long as the *named* tools resolve. Unnamed
siblings' host-leaf availability is irrelevant. Unnamed runs (`ocx run -- cmd`)
are unchanged: the whole scope is the set and must all resolve.

### Reproduction Steps

1. Build a project whose default group has two tools, where one ships no leaf
   for the current host platform (e.g. a windows-only tool on a linux host).
2. `ocx run cmake -- cmake --version` → today exit 78; expected exit 0.
3. `ocx run -- env` (whole group) → still exit 78 (correct).

Deterministic equivalent at the unit level (no registry): see regression tests.

### Environment

| Factor | Value |
|--------|-------|
| Platform | any host where a scope sibling lacks a host leaf |
| OCX version | HEAD (`main`) |
| Registry | n/a (pre-network resolution error) |
| Configuration | project `ocx.toml` + `ocx.lock` with a host-incompatible sibling |

### Frequency

Always, when the precondition (an unnamed sibling lacking a host leaf) holds.

## Root Cause Analysis

### Investigation Log

1. **Symptom**: exit 78 on `ocx run cmake -- …`.
2. **Proximate cause**: `compose_tool_set` calls `host_leaf_identifier` for
   every entry in scope; the unnamed sibling has no host leaf → `NoHostLeaf`.
3. **Root cause**: `compose_tool_set` couples whole-scope duplicate validation
   with per-entry host-leaf resolution in one pass, and `run.rs` filters NAMEs
   *after* compose — so the filter cannot shrink the resolution/error surface.
4. **Introduced by**: original `ocx run` implementation (resolve-before-filter).

### Root Cause Statement

> A narrowly-named `ocx run` fails because host-leaf resolution runs over the
> whole selected scope *before* the NAME filter, so an unrelated sibling with no
> host leaf errors even though it is never requested.

### Related Code

| File | Role |
|------|------|
| `crates/ocx_lib/src/project/compose.rs` | `compose_tool_set` coupled selection + resolution |
| `crates/ocx_cli/src/command/run.rs` | Phase D compose → Phase E filter → Phase F install |

### Pattern Check

- [x] Searched callers of `compose_tool_set` (`toolchain_env.rs` — unnamed env
  composition, wants every tool; left on `compose_tool_set`, correct).
- [x] `host_leaf_identifier` reused by `ocx pull` / lock materialization —
  untouched (single source of `NoHostLeaf`).
- [x] No other caller filters after compose.

## Fix Approach

### Proposed Change

Split selection from resolution in the lib:

- `SelectedTool` + `ToolSource` (resolution-free selection result).
- `select_tool_set(config, lock, groups, positionals)` — current Step 1+2+3
  logic without `host_leaf_identifier`; duplicate validation at the lock level
  via `resolutions_content_equal` (whole-scope, unchanged).
- `resolve_selected_tools(selected, platform)` — maps `Locked` →
  `host_leaf_identifier`, `Explicit(id)` → verbatim.
- `compose_tool_set` reimplemented as a thin delegate
  (`resolve_selected_tools(&select_tool_set(...)?, platform)`) — external
  contract unchanged.

CLI `run.rs`: Phase D selects (resolution-free), Phase E filters by NAME, Phase
F resolves host leaves for the named subset only. `filter_by_names` retyped
`ResolvedTool → SelectedTool`. `host` platform computed above Phase D.

### Files Modified

| File | Change |
|------|--------|
| `crates/ocx_lib/src/project/compose.rs` | New `SelectedTool`/`ToolSource`; `select_tool_set`/`resolve_selected_tools`; `compose_tool_set` delegate; lock-level dup compare; new regression test + `locked_windows_only` fixture |
| `crates/ocx_lib/src/project.rs` | Re-export new items |
| `crates/ocx_cli/src/command/run.rs` | Phase D/E/F rewiring; `filter_by_names` retyped; docs; integrated regression test |
| `website/src/docs/reference/command-line.md` | `[NAME...]` note + exit-78 row |
| `website/src/docs/reference/env-composition.md` | `ocx run` NAME-subset scoping note |
| `.claude/rules/subsystem-cli-commands.md` | "Semantics & Gotchas" `ocx run` bullet |

### Alternatives Considered

| Approach | Rejected Because |
|----------|-----------------|
| Make NAME a global toolchain lookup widening `-g` scope (Model B) | Changes `-g` semantics; not the agreed fix |
| Revive `RunFilterError::Ambiguous` dead branch | Out of scope; whole-scope dup validation already collapses/rejects clashes |
| Raw `LockedResolution` `==` for dup compare | `resolutions_content_equal` is the existing, V1-tag-insensitive content helper |

### Risk Assessment

| Risk | Mitigation |
|------|------------|
| `compose_tool_set` callers change behavior | Delegate keeps signature + resolve-everything contract; all 14 existing compose tests + `toolchain_env.rs` green |
| Dup-validation order change | Validation now compares `LockedResolution` content before resolution; behaviour-preserving for real locks (one manifest ⟹ identical `platforms` map) |

## Regression Test Specification

> Tests fail-to-compile before implementation (new functions absent); green after.

### Unit Tests

| Test | File | Asserts |
|------|------|---------|
| `named_subset_skips_unnamed_sibling_without_host_leaf` | `crates/ocx_lib/src/project/compose.rs` | select returns both entries (no resolve); resolving cmake-only Ok; resolving whole set Err |
| `named_subset_resolves_while_unnamed_whole_group_errors` | `crates/ocx_cli/src/command/run.rs` | select→filter→resolve: `["cmake"]` Ok; `[]` Err (NoHostLeaf) |

### Existing tests kept green

`compose_errors_when_host_leaf_absent`,
`compose_errors_on_duplicate_binding_across_groups_with_different_content`,
`compose_collapses_duplicate_binding_with_identical_content`, all `filter_*`
tests (retyped to `SelectedTool`).

## Verification Checklist

- [x] Regression tests target the root cause (resolution scope), not symptom
- [x] Fix applied — `cargo test -p ocx_lib project::compose` + `-p ocx --bin ocx command::run` pass
- [x] `task rust:verify` green (fmt, clippy, license, build, 2248 unit tests)
- [ ] `task verify` final gate before commit
- [x] No scope creep — `Ambiguous` branch untouched; dup validation unchanged

## Notes

Manual end-to-end (real project with a host-incompatible sibling) is covered
deterministically by the two unit tests per `subsystem-tests.md` preference for
unit-level bugs. Commit as a single `fix:` on `main` per user instruction; never
push.
