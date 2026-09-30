# Research: Index format compatibility for a per-tag ephemeral marker

## Metadata

**Date:** 2026-09-29
**Domain:** index
**Triggered by:** adr snapshot lifecycle
**Expires:** 2027-03-29

## Direct Answer

Add a boolean `ephemeral: true` to `tagEntry` (not an object, not a sibling
`snapshots` map). Client-side parse compatibility is already solved — no
`deny_unknown_fields` anywhere in the Rust wire types. The real gap is
governance: **tag removal is not gated at all today** — `classify_change`
(index bot) never checks for a tag key disappearing, so
`ocx package announce --tags <replace-list>` can already silently drop any
committed tag and the PR still auto-merges as `refresh` under G-19. The
marker's job is not to *permit* removal (that already exists) but to
*restrict* it to marked tags — land the gate in
`core/diff.py::classify_change` and in the client's existing
`CommittedTagsDropped` guard (`announce.rs:461`), which today only fires for
the additive selections and exempts `Replace`.

## 1. Marker shape options

### Evidence gathered

- Rust wire types have **no `deny_unknown_fields`** on `IndexRoot`/`RootTag`
  (`crates/ocx_index/src/wire.rs:52-53,70-78`); `YankMarker`'s fields are
  `#[serde(default)]` (`wire.rs:83-89`). A new sibling key on `tagEntry`
  parses cleanly on every shipped client — proven by
  `index_root_tolerates_unknown_fields_for_fleet_forward_compat`
  (`wire.rs:334-355`).
- The **index repo's own JSON Schema is strict, the opposite of the client**:
  `root.schema.json`'s `tagEntry` def carries `additionalProperties: false`
  and lists only `content`/`observed`/`yanked`. A PR carrying an unrecognised
  tag key is rejected by G-01 (`check-jsonschema`) before it reaches the bot
  — this is the actual compatibility gate, not the Rust parser.
- **No Rust code writes this shape** — `wire_writer.rs` never touches
  `RootTag`/`tagEntry`; the published root is authored exclusively by the
  Python bot (`_tag_entry_to_dict`, key order `content, observed, [yanked]`).
  "Python ↔ Rust round-trip" is one-directional: Rust only *reads* it.

### Option evaluation

| Option | Cost | Verdict |
|---|---|---|
| **`ephemeral: true` (bool)** | One key in `tagEntry`; `_tag_entry_to_dict` gains one `if`; unmarked fixtures byte-identical; `Option<bool>` + `#[serde(default)]` on the Rust side | Chosen |
| Object `{channel, expires?}` | Same shape one level deeper; `expires` implies a TTL sweep nothing implements today | Scope creep |
| Root-level policy block | Decouples the marker from the tag it governs — status no longer self-describing from the tag's own entry | Wrong altitude |
| Separate `snapshots` map | Doubles every consumer's walk (`ChainedIndex`, dispatch resolution, cascade eviction, site render); a tag moving lanes is two writes not one | Needless duplication |
| Tag-name convention | Silently violable, and the schema's `not:` reservation block already shows this repo rejects convention-only signals for `__ocx*` | No client-side signal |

**Chosen: boolean `ephemeral: true`** — the direct sibling of `yanked`'s
"presence marks the row" idiom (`wire.rs:75-77`; schema doc: *"Presence marks
the row yanked"*). One schema line, one serializer line, one Rust field, and
removability stays legible from the tag's own entry, which is exactly what
`classify_change` (below) needs to read at decision time.

## 2. Index bot governance

### Today: nothing flags a removed tag

`core/diff.py::classify_change` (`.../ocx_indexbot/core/diff.py:86-128`) is
the sole classifier behind G-04/G-05/G-19 auto-merge. It compares
`before.tags.items()` against `after.tags.get(tag)`, but the loop body only
runs `if after_entry is not None and after_entry.yanked != before_entry.yanked`
— a tag whose key **disappears** (`after_entry is None`) never hits that
condition and falls through to `"refresh"`, the auto-merge-eligible class.
`governance-contracts.md` G-05 confirms the human-review-required key set is
`repository, owners, status, deprecated_message, superseded_by`, plus
*mutation* of an existing tag's `yanked` field — deletion is absent. ADR-6
FP-2/FP-3 (`core/verify_claims.py`) governs a different question — a tag
still *present* in the root but no longer served by the *registry* — and
doesn't apply to a key removed from the committed JSON.

**Conclusion:** the premise "removal needs a governance carve-out" is
backwards. Silent, ungated removal via `ocx package announce --tags
<replace-list>` (client-side `Replace`, authoritative per
`resolve_curated_tags`, `announce/pipeline.rs:126`) already exists and
already auto-merges. The marker's job is to **narrow** that hole, not widen
a restriction.

### Minimal rule change

In `classify_change`, add a branch beside the existing yanked-mutation check:

```python
for tag, before_entry in before.tags.items():
    after_entry = after.tags.get(tag)
    if after_entry is None:
        if not before_entry.ephemeral:
            return "human-review-required"
        continue  # legitimate: marked on the BASE ref, removal stays refresh-eligible
    if after_entry.yanked != before_entry.yanked:
        return "human-review-required"
```

Also add "mutation of an existing tag row's `ephemeral` field" to the G-05
human-review-required key set — symmetric with `yanked` — so *setting* the
marker needs a human, while *removing* an already-marked tag does not.

### Tamper path — already closed structurally

Can a PR flip a tag to `ephemeral` and remove it in one step? **No, by
construction:** `before` in `classify_change` is always the **base-ref** root
(`diff.py:86-89`), a value the PR's own diff cannot influence, so a same-diff
mark-and-remove still reads `before_entry.ephemeral` as absent and falls
through to `human-review-required` — the same base-vs-head separation that
already makes G-19's ownership check tamper-resistant. What is *not* free: a
PR that only *sets* the marker (tag stays present) would otherwise classify
`refresh` and auto-merge unreviewed, hence adding `ephemeral` to G-05's key
set is required. True one-way immutability (no `true → false`) isn't free
either — `classify_change` treats both directions as identical mutation
today; that needs an explicit third arm if the ADR wants no path back,
mirroring how `yanked` is one-way only by convention, not by schema.

## 3. Announce pipeline — C4 branch CAS / concurrent announce

`adr_announce_diverged_branch_rebuild.md` (D1) already carries a tag-level
merge policy for a `Diverged` branch under an open PR — **base entry wins on
a shared key**, to protect `yanked` from a stale-branch revert. The same
tie-break covers removal for free: the merged root (base) is what
`regenerate` is parented on, and a name absent from `base.tags` never enters
`union_onto_committed`'s input under `UnionFile`/`FromRegistry`
(`pipeline.rs:106-114`) — a stale branch cannot resurrect a tag the base
already removed. No new ref primitive needed.

The real race is a **stale `Replace` run**: a publisher script holding an old
`--tags-file`/argv list that still names an already-removed tag, re-run
after the removal PR merged. `dropped_committed_tags` (`pipeline.rs:148-153`)
already *detects* this class of loss but only fires under the three additive
selections (`announce.rs:461-471`) — `Replace` is exempt, since
replace-deletion is its documented purpose. Extend the guard: under
`Replace`, a name in `dropped_committed_tags(...)` that is
`!base_root.tags[tag].ephemeral` should still raise `CommittedTagsDropped` —
defense in depth; the server gate stays authoritative.

## 4. A separate dev index

`.github/index-policy.json` is the seam: a per-deployment `governance` block
(today just `{"auto_merge": "owners"}`, read by `core/policy.py::IndexPolicy`,
dialled in `cli/governance_check.py:140-181`). Two independent, composable
levers:

- **Schema-level opt-out, free:** a deployment that never adds `ephemeral` to
  its own `root.schema.json` gets `additionalProperties: false` rejecting the
  field outright at G-01 — opt-in per deployment by construction, no flag.
- **Governance-level dial, if finer control is wanted:** add
  `"governance": {"ephemeral_tags": "allowed"|"disabled"}`, read like
  `auto_merge`, so `classify_change` refuses the key when disabled — useful
  for matching upstream's schema without opting into removal semantics yet.

The schema opt-out alone suffices for compatibility; the dial only matters
for "parseable but policy-off" versus "outright rejected."

## 5. Precedent

| Index | Model | Source |
|---|---|---|
| npm | `dist-tags` (`latest`, `next`) are a separate mutable map from immutable `versions`; a dist-tag deletes freely, a version does not (short of `npm unpublish`'s 72h window). Solves "a pointer," not "this row may vanish." | [dist-tag docs](https://docs.npmjs.com/cli/v10/commands/npm-dist-tag) |
| crates.io | Per-version `yanked: bool`; never deletes the line, only excludes from fresh resolution. No removal primitive. | [Cargo book — Registry Index](https://doc.rust-lang.org/cargo/reference/registry-index.html) |
| Homebrew | No per-formula marker; deletion is outright, tombstone-free, history lives in git only. | [Deprecating/Disabling/Removing Formulae](https://docs.brew.sh/Deprecating-Disabling-and-Removing-Formulae) |
| Helm | `index.yaml` is additive-only forever; only whole-chart `deprecated: true`, no per-version marker. | [Chart Repository Guide](https://helm.sh/docs/topics/chart_repository/#the-index-file) |
| Maven | `<snapshotVersions>` regenerates wholesale per deploy; ephemerality is a property of the `-SNAPSHOT` name, not a field. | [Resolver `snapshotVersion` docs](https://maven.apache.org/resolver/apidocs/org/apache/maven/repository/snapshotVersion.html) |

Best fit: **Maven's name-implies-ephemeral** (removal is routine, expected)
combined with **crates.io's presence-marks-the-fact field shape** (a plain
marker on the entry, not a name convention, not a second map).

## Recommendation

1. **Marker: `ephemeral: true` (bool) on `tagEntry`**, sibling of `yanked`.
   No object, no second map, no root-level block, no naming convention.
2. **Do not gate removal only at the client** — it already deletes
   unconditionally under `Replace`. The load-bearing gate is
   `classify_change` plus a G-05 key-set addition; ship both together, since
   a client-only gate is bypassable by anyone scripting the forge API directly.
3. **Reuse `dropped_committed_tags`** as the client-side fail-fast rather than
   inventing a new comparison — it already computes exactly the needed set.
4. **Tamper resistance is structural**, not an added check, as long as
   `classify_change` always reads the marker off `before` (base ref) — add a
   test mirroring `pipeline.rs:1603`'s
   `dropped_committed_tags_names_a_loss_and_stays_silent_otherwise`, asserting
   the opposite: same-PR mark-and-remove still `human-review-required`.
5. **Dev/private index compatibility is free** via existing
   `additionalProperties: false` strictness — no opt-in flag required.

## Sources

| Source | Relevance |
|---|---|
| `crates/ocx_index/src/wire.rs`; `ocx_announce/src/{announce.rs,announce/pipeline.rs,announce/request.rs}` | Client parse compat, `RootTag`/`YankMarker`, `TagSelection`, `dropped_committed_tags`, `CommittedTagsDropped` |
| `.claude/artifacts/adr_announce_diverged_branch_rebuild.md` (2026-09-04) | C4 base-wins merge on `Diverged` branches |
| `/home/mherwig/dev/index/schema/root.schema.json` | `tagEntry`/root `additionalProperties: false` |
| `ocx_indexbot` 0.6.0 `core/{diff.py,verify_claims.py,validate_entry.py}` (`bot-tools/.venv`) | `classify_change`, ADR-6 FP-2/FP-3, no-removal-check finding |
| `/home/mherwig/dev/index/site/src/docs/reference/governance-contracts.md` | G-01..G-20 table, G-05 key set |
| `/home/mherwig/dev/index/.github/index-policy.json`, `ocx_indexbot/core/policy.py` | Per-deployment governance dial |
| [npm dist-tag](https://docs.npmjs.com/cli/v10/commands/npm-dist-tag), [Cargo index format](https://doc.rust-lang.org/cargo/reference/registry-index.html), [Homebrew removal](https://docs.brew.sh/Deprecating-Disabling-and-Removing-Formulae), [Helm index.yaml](https://helm.sh/docs/topics/chart_repository/#the-index-file), [Maven snapshotVersion](https://maven.apache.org/resolver/apidocs/org/apache/maven/repository/snapshotVersion.html) | Precedent table §5 |
