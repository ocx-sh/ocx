# ADR: Toolchain Update Family — rename `upgrade` to `update`, remote-by-default resolution

## Metadata

**Status:** Accepted
**Date:** 2026-07-10
**Deciders:** Michael Herwig
**Related Issues:** #201 (`ocx upgrade` never advances a floating pin without `--remote`)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, thin CLI over `ocx_lib`)
**Domain Tags:** api, cli, oci
**Supersedes:** the default-index routing and the `upgrade` name for this verb in
`adr_scoped_upgrade.md` (scoping contract there stays intact); the Default-mode routing row of
`adr_index_routing_semantics.md` **for this one verb only**
**Superseded By:** N/A

## Context

`ocx upgrade` re-resolves declared tags through the default (local-first) index. A tag already
present in the local snapshot short-circuits to the cached digest
(`chained_index.rs` `walk_chain`, Default arm), so a floating pin (`:latest`, `:3`) never
advances without `--remote` — the exact pain in issue #201, hit daily on the global toolchain:
`ocx --global upgrade` silently reports "up to date" while the registry moved on.

The obvious workaround, `ocx upgrade --remote`, has its own defect: `--remote` + tag +
`Op::Resolve` = "source only, write blobs **+ tag**" (routing matrix in `subsystem-oci.md`).
The upgrade run leaks tag-pointer writes into the shared local index — other projects resolving
against that snapshot see tag movements they never asked for ("other projects may be
compromised", #201).

Both defects share a root: the verb's *intent* ("advance my pins to today's registry state")
was never encoded; it borrowed whatever the context's default index did.

## Decision Drivers

- A verb named "update"/"upgrade" that does not consult the registry by default violates every
  mainstream package manager's contract (`cargo update`, `npm update`, `poetry update` all hit
  the network by default; offline is the flagged exception).
- Registry resolution must not mutate shared state outside the verb's own record — the
  pinned-pull precedent ("`ocx.lock` is the canonical record", `adr_index_routing_semantics.md`)
  already establishes lock-only writes for pinned resolution.
- Policy ceilings (`--offline`, `--frozen`) must stay uniform across verbs — flags cap what any
  verb may do; verbs choose their default *under* those ceilings. No per-verb flag semantics.
- Smallest safe diff: no new resolution engine, no `resolve.rs` API change.

## The Update Family Rule

`update` verbs talk to the registry by definition; each writes **only its own record**:

| Verb | Refreshes | Writes |
|---|---|---|
| `ocx index update` | local index snapshot | `tags/` (+ blobs) |
| `ocx self update` | managed ocx install | managed install + shims |
| `ocx update` (this ADR; formerly `upgrade`) | project/global lock | `ocx.lock` (+ content-addressed blobs) |

The verb rename from `upgrade` to `update` makes the family visible: three verbs, one rule,
three disjoint records. Hard rename, no alias — pre-1.0 breaks are free
(`project_breaking_compat_next_version.md`).

## Considered Options

### Option 1: Route `update` through `ChainMode::Remote` with `Op::Query` read-through

**Description:** Give the command a Remote-mode index and resolve with `Op::Query`, which never
persists (per `adr_index_routing_semantics.md`'s query-never-writes invariant).

| Pros | Cons |
|------|------|
| Reuses existing no-write path, zero new mechanism | `Op::Query` miss under `--offline`/`--frozen` returns `None` → surfaces as `TagNotFound` (79), regressing the documented `PolicyBlocked` (81) contract |
| — | `resolve_lock*` passes `Op::Resolve` internally; switching op per caller means threading `IndexOperation` through the heavily-tested `resolve.rs` public API |

### Option 2: Thread `IndexOperation` through `resolve_lock*`

**Description:** Extend `ResolveLockOptions` (or the signatures) with an operation/mode knob so
the resolver itself can be told "resolve live but do not persist tags".

| Pros | Cons |
|------|------|
| Explicit intent at the resolver layer | Public-API change to `resolve.rs`, the most heavily tested project-layer surface; every caller re-audited |
| — | Conflates index routing (an index-construction concern) with lock resolution (a project concern) — layer violation |

### Option 3: `ChainedIndex.suppress_tag_commit` flag + dedicated `Context::update_index()` (chosen)

**Description:** Add a `suppress_tag_commit: bool` to `ChainedIndex` gating the single
tag-pointer write in `fetch_and_persist_chain`. The CLI context grows an `update_index()`
helper that builds a chained index with `mode = offline ? Offline : frozen ? Frozen : Remote`
(the standard precedence ladder minus the `Default` arm) and the suppress flag set. The
command uses it in place of `default_index()`.

| Pros | Cons |
|------|------|
| Keeps `Op::Resolve` everywhere → `--offline`/`--frozen` still raise `PolicyResolutionBlocked` (exit 81); no 81→79 regression | One more field on `ChainedIndex` (carried in `box_clone`) |
| Zero changes to `resolve.rs`'s public API | Suppression is index-construction state, invisible at the resolve call site (mitigated: constructor is explicitly named) |
| Manifest blobs still persist — content-addressed, immutable, pre-warms materialization | — |

## Decision Outcome

**Chosen Option:** Option 3 — suppress flag + `update_index()`.

**Contract for `ocx update` (all scoping forms of `adr_scoped_upgrade.md` unchanged):**

| Invocation | Resolution | Local index writes |
|---|---|---|
| `ocx update` | live registry (Remote-style) | blobs only — **never a tag pointer** |
| `ocx update --remote` | same (redundant, accepted) | same |
| `ocx update --frozen` | local snapshot only; unknown tag → exit 81 | none |
| `ocx update --offline` | local snapshot only, no network; unknown tag → exit 81 | none |

- **Remote by default.** Tags resolve live, like `ocx self update` already does. `--remote`
  becomes redundant-but-accepted (mirrors `self update` doc wording).
- **No tag-store writes.** Resolution lands in `ocx.lock` only. Blob caching stays —
  content-addressed and immutable, it cannot shadow anything and pre-warms the materialize
  step. Extends the pinned-pull "lock is the canonical record" precedent to tag resolution
  performed on behalf of the lock.
- **Policy ceilings unchanged.** `--frozen` = discovery capped at the local snapshot;
  `--offline` = no network. Both compose with `update` uniformly — flags are policy ceilings,
  verbs are intent defaults under those ceilings.
- **Rename surface.** `upgrade` → `update` across CLI, error remedies, docs, tests. No alias.

### Explicitly Rejected

- **Per-toolchain index snapshot** — redundant with the lock (which already pins digests per
  project) and a layer violation (project state inside the index tier).
- **Config knob in `ocx.toml`** (e.g. `update.remote = false`) — two behaviours to document
  and test for a verb whose meaning should not be configurable.
- **Changing `add`/`lock`/read-path routing** — out of scope; those verbs keep the default
  local-first index. Only the `update` verb re-defaults.

### Consequences

**Positive:**
- `ocx --global update` advances floating pins with zero flags — the #201 daily pain gone.
- Shared local index can no longer be mutated as a side effect of a project's update run.
- Verb family coherence: `index update` / `self update` / `update` — one rule, three records.

**Negative:**
- Breaking rename (accepted: pre-1.0, no alias, no migration prose in docs).
- `ocx update` now needs network by default; snapshot-only flows must say `--frozen`
  (deliberate: that is what the flag is for).

**Risks:**
- *Risk:* a future constructor reuses the suppressing index for a verb that SHOULD commit
  tags. *Mitigation:* the constructor name states the suppression; acceptance test asserts
  the tag snapshot is byte-unchanged after `ocx update`.
- *Risk:* suppress flag lost in `box_clone`, silently re-enabling tag writes on cloned
  indices. *Mitigation:* flag carried explicitly in `box_clone`; covered by unit test.

## Technical Details

```rust
// chained_index.rs — the single gated write site (fetch_and_persist_chain):
if !self.suppress_tag_commit && identifier.tag().is_some() && identifier.digest().is_none() {
    self.local_index.commit_tag(identifier, &digest).await?;
}

// app/context.rs — verb-intent index (near default_index()):
pub fn update_index(&self) -> oci::index::Index { /* Offline ▸ Frozen ▸ Remote ladder,
    suppress_tag_commit = true, sources from remote_index like try_init */ }
```

`command/update.rs` (renamed from `upgrade.rs`) swaps `context.default_index()` for
`context.update_index()`. Scoped dispatch, `resolve_lock_touched`, `resolve_lock`, `--check`,
`select_touched` — all unchanged.

## Validation

- [ ] Unit: suppress flag gates tag commit; carried through `box_clone`.
- [ ] Acceptance NEW 1: bare `ocx update` advances a tag moved only upstream (no prior
      `ocx index update`) — proves remote-by-default.
- [ ] Acceptance NEW 2: local tag snapshot byte-unchanged after `ocx update` — proves
      no-tag-pointer-write.
- [ ] Acceptance NEW 3: `--frozen` update resolves against snapshot only; unknown tag → 81.
- [ ] Renamed suite `test_update.py` green; CLI help gates pass.
- [ ] Manual: `ocx --global update <tool>` advances a floating pin without `--remote`;
      `~/.ocx/tags/` diff before/after empty.

## Links

- [#201](https://github.com/ocx-sh/ocx/issues/201) — floating pin never advances / tag leak
- [`adr_scoped_upgrade.md`](./adr_scoped_upgrade.md) — scoping contract (intact; name + routing superseded here)
- [`adr_index_routing_semantics.md`](./adr_index_routing_semantics.md) — `IndexOperation`, lock-is-canonical precedent
- `crates/ocx_lib/src/oci/index/chained_index.rs` — `suppress_tag_commit` gate
- `crates/ocx_cli/src/app/context.rs` — `update_index()`
- `crates/ocx_cli/src/command/update.rs` — the renamed verb

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-10 | Michael Herwig | Initial draft — update family rule; rename `upgrade` → `update`; remote-by-default, lock-only writes |
