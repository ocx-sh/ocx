# Index Routing Semantics — Feedback Capture

**Status:** Working note. Captures user feedback on `ChainedIndex`/`LocalIndex` routing for re-review before `sion` merges to `main`.
**Branch context:** Authored on `main` while investigating the `--remote` catalog bug. Sion changes (pinned identifiers) compound the existing design issues.
**Author:** Claude session, 2026-04-27.

---

## 1. The Spec (User's Mental Model)

The flag → behavior table the implementation must honor:

| Mode | Mutable lookups (catalog, tag list) | Content-addressed lookups (manifest by digest) | Local index writes |
|------|--------------------------------------|------------------------------------------------|--------------------|
| `--remote` (`OCX_REMOTE=1`) | **Source only.** No local read. | Cache-friendly (immutable). Cache miss → fetch from source. | **No.** Query never mutates local index. |
| `--offline` (`OCX_OFFLINE=1`) | **Local only.** No remote index constructed at all. Source is never consulted. | Local only. Miss = `None`. | No. |
| Default (neither flag) | **Local only.** Miss = `None` for pure queries. No auto-fetch. | Local only for queries. | No (for queries). |

Two orthogonal axes the current code conflates:

- **Routing axis** (read from where): determined by mode + immutability of identifier.
- **Write axis** (does this call mutate the local index): determined by *intent* (pure query vs. install/pull/exec/explicit-update). The trait does not encode intent today.

Auto-update happens **only** when:

1. Install / pull / exec (or any path that requires the data to proceed).
2. Explicit `ocx index update <pkg>` invocation.

It does **not** happen when:

- `ocx index catalog`
- `ocx index list <pkg>` (with or without `--platforms`/`--variants`)
- Any other read-only command

> Direct quote: "a query operation should NOT update any local index. The auto-update should only apply if the data is required (install / pull / exec /...) or explicitly asked for (`index update`)."

### Why content-addressed reads are different

Digests pin immutable content. A cache hit can never be wrong — bytes addressed by digest either match the digest or they don't exist. So:

- Caching digest-keyed manifests freely is safe in any mode.
- A cache miss on a digest-addressed read may still need to fetch (because the data may not be local yet) — but this fetch is part of "we need this manifest to proceed", which only happens on install/pull/exec paths.

### Why `index list` is hybrid

`index list <pkg>` enumerates tags. The caller may pass:

- A bare repo (`cmake`) — pure mutable enumeration. Should respect the mode/intent rules above.
- A tag (`cmake:3.28`) — narrows the listing, still mutable.
- A digest (`cmake@sha256:…`) — content-addressed and should never need a remote round-trip beyond what's already cached.

The output therefore mixes "what is locally known" (always) with "what the registry currently has" (only under `--remote` and only via a non-mutating read).

---

## 2. What the Code Does Today (post my `list_repositories` fix)

`crates/ocx_lib/src/oci/index/chained_index.rs` after the in-flight fix:

| Method | Default | Remote | Offline | Spec? |
|--------|---------|--------|---------|-------|
| `list_repositories` | local only | source only, no fallback to local | local only | ✅ matches spec |
| `list_tags` | local only | **`refresh_tags(source)` → writes through to local index, then reads local** | local only | ❌ Remote mutates local during query |
| `fetch_manifest` (digest) | local first, miss → walk chain (write-through) | local first, miss → walk chain (write-through) | local only | 🟡 OK because digest pins immutable content; write-through is a content-addressed cache fill, not a mutable-state mutation |
| `fetch_manifest` (tag) | local first, miss → walk chain (write-through) | skip local, walk chain (write-through) | local only | ❌ Tag write-through happens on every call regardless of intent — query and install paths share the same surface |
| `fetch_manifest_digest` | analogous to `fetch_manifest` | analogous | local only | ❌ same as above for tag-addressed case |

Concrete violations of the spec:

1. **`list_tags` in Remote mode mutates local index.** `chained_index.rs:201-216` calls `LocalIndex::refresh_tags(source)` which persists tag pointers to disk before returning. A pure `ocx index list --remote foo` invocation thereby alters `$OCX_HOME/tags/`.
2. **`fetch_manifest` (tag-addressed) auto-updates local index from any caller.** `chained_index.rs:220-239` walks the chain on cache miss in Default mode, and unconditionally walks the chain in Remote mode. The walk persists to local. `index list --platforms` (pure query, called via `command/index_list.rs:121`) and `package_manager::tasks::resolve.rs:58` (install/pull path) share the same call site, so the trait cannot distinguish them.
3. **Default mode `list_tags` cannot see registry data even when the user did not pass `--offline`.** Combined with #2 above, this is asymmetric: `index list foo` returns local-only tags (no source contact), but `index list --platforms foo` triggers a tag-addressed `fetch_manifest` which auto-fetches and persists. Two adjacent flags on the same command produce different side-effect profiles.

The `--remote` query path violation is the most user-visible: a user running `ocx index list --remote foo` to peek at the registry without committing anything to their local index will silently get local-index mutations.

---

## 3. The Sion Pivot: Pinned Identifiers Remove the Last Reason for Auto-Update on Query

Pre-sion: tags were the only durable selector. After install, `exec` had to re-resolve `foo:3.28` → digest at every invocation. To make that resolution work offline, `ChainedIndex::list_tags` and `fetch_manifest` auto-populated the local cache during install so future resolutions could be answered locally.

Post-sion: install records a **pinned identifier** (`foo:3.28@sha256:…`) into the lock. `exec` reads the pinned digest directly. Tag→digest resolution at exec time is gone.

What still requires source contact post-sion:

- **Install / pull**: resolve tag → digest, fetch manifest chain, write through to local. Same as before.
- **Multi-platform manifest selection during install**: image index is content-addressed (digest); per-platform child manifests are content-addressed. Cache freely.
- **Explicit `index update`**: explicitly contact source, persist to local.

What no longer requires source contact post-sion:

- **`exec`, `env`, `find`, `shell env`**: resolve from pinned identifier (digest). Pure local read of content-addressed data.
- **All `index list` / `index catalog` queries**: read what the local index has, optionally peek at the registry under `--remote`, never mutate local.

This is a clean separation: the *only* paths that should write the local index are install/pull and `index update`. Everything else is read-only.

---

## 4. Recommendations

### 4.1 Main (urgent — required for the catalog/list bug fixes to be coherent)

These changes are independent of sion and align main with the spec. They can land before sion merges.

**M1. Stop write-through on `ChainedIndex::list_tags`.**
Replace `self.cache.refresh_tags(identifier, source)` with a non-mutating `source.list_tags(identifier)` call when in Remote mode. Return the source's tags directly to the caller. Mirror the shape of the new `list_repositories`: in Remote mode, source-only, no fallback, no write-through.

**M2. Make Default mode `list_tags` truly local-only.**
Today it's already local-only by virtue of `if self.mode == Remote` being the only branch that talks to sources. Keep it. Document the contract: pure queries never trigger a fetch. If the user wants fresh data they run `ocx index update` or pass `--remote`.

**M3. Stop tag-addressed write-through on `fetch_manifest` / `fetch_manifest_digest`.**
The hardest change. Today these methods serve two callers — `index list --platforms` (pure query) and `package_manager::tasks::resolve` (install path). Two options:

- **Option A (preferred):** Split the read path from the write path on the `IndexImpl` trait. `fetch_manifest` becomes pure read (Default/Offline → local; Remote → source, no write-through). Add a separate `ensure_manifest_chain(identifier)` method that install/pull/exec call to explicitly populate local before reading. This codifies "intent" in the type system rather than hoping callers do the right thing.
- **Option B:** Keep one method but route through a `Mode::ReadOnly`-vs-`Mode::Update` flag at call time. Less type safety, but smaller diff.

Either way, content-addressed (digest) reads continue to cache freely — they cannot serve stale data.

**M4. Audit and rename the `cache` field on `ChainedIndex`.**
The field is named `cache` but it is the *local index* — a curated, persisted subset, not a cache. Renaming to `local` (or `local_index`) clarifies that:

- Default behavior is "read the local index", not "consult a cache that I'll happily refill from upstream".
- Write-through is a deliberate side effect of install/update, not a transparent cache-fill on every miss.

(The misnomer is part of why the current behavior drifted: "cache-first" reads naturally to "cache miss → fetch". A local index is not a cache; misses just mean "not present", not "fetch and fill".)

**M5. Update the `ChainMode` doc comment in `chained_index.rs:21-25`.**
Reflect M1–M3: Remote = source-only for mutable lookups, no write-through, no local fallback. Digest-addressed reads cache freely in all modes. Local-index writes happen only via explicit update paths.

**M6. Update `subsystem-oci.md`** so the table at "ChainMode" matches reality after M1–M3. The current table claims write-through in Remote mode, which is exactly the behavior we are removing.

**M7. Acceptance test on main** that asserts a pure `--remote` query of `index list` and `index catalog` does **not** mutate `$OCX_HOME/tags/` — diff the directory before and after.

### 4.2 Sion (incorporate user feedback into the in-flight pin work)

Sion already carries the pin lock change. Layer the routing cleanup on top.

**S1. Remove auto-update from exec/find/env paths.**
These commands resolve from the pinned identifier (digest). They should never reach a code path that contacts a source, even on cache miss. If the pinned digest's content is missing locally, the correct behavior is `Err(NotFound)` with a hint to run `ocx install` or `ocx package pull` — not silent network fetch.

**S2. Confirm that install/pull is the only path that mutates the local index** beyond `ocx index update`. Audit `package_manager::tasks` and the command dispatch to make sure no read path (resolve-for-find, resolve-for-deps, resolve-for-env) reaches `LocalIndex::write_chain_and_commit_tag` or `refresh_tags`.

**S3. Reconsider whether install needs auto-update for pinned-identifier installs.**
If the user installs by pinned identifier (`foo:3.28@sha256:…`), the manifest chain is fully content-addressed and no tag-pointer write to the local index is needed. The pin lock owns the resolution; the local index becomes a manifest blob store, not a tag pointer store, for these installs. Open question — needs a design conversation with the pin-lock author. Flag for pre-merge review.

**S4. Defense-in-depth on sion's `unset OCX_INDEX` workaround.**
Once M1 lands on main and sion rebases, the website catalog task can stop unsetting `OCX_INDEX`. Recommendation in the prior plan was to keep it as defense-in-depth until the `TagStore` robustness follow-up lands. Re-evaluate after M1 and M3 are in: if the only thing the workaround was hiding is auto-update on query, and queries no longer auto-update, the workaround is obsolete.

**S5. Acceptance test on sion**: install a package, then run `exec` with the network removed (e.g. `--offline` and a deliberately broken DNS). Should succeed because the pinned digest resolves locally.

### 4.3 Cross-cutting (post-merge)

**X1. ADR.** Once M1–M6 + S1–S3 are agreed, promote this note to an ADR: `adr_index_routing_semantics.md`. Cite the spec from §1, the pre/post-sion difference, and the trait split (M3 Option A or B) chosen.

**X2. Rename `--remote` flag rationale in user docs.** The flag's effect is "consult registry, don't trust local". Today it doubles as "and persist what you see" — that side effect goes away. Update `website/src/docs/reference/command-line.md` once M3 lands so users don't expect their `index update` cache to fill from `--remote` queries.

---

## 5. Open Questions

1. **Should `index list --remote foo` show *only* what the registry has, or the union of registry + local?** Spec says source-only for mutable lookups, so the answer is registry-only. But `index list` already mixes content-addressed (digest) and floating (tag) data. Decide: when the identifier carries a digest, does `--remote` round-trip to verify, or trust local?

2. **Does `index update` without arguments make sense post-sion?** Pre-sion it pulled the tag-set for every locally-known repo. Post-sion, with pin locks, the local index stops being the source of truth for "what tags exist for this repo" — it becomes a snapshot that may be stale by design. A no-arg `index update` may want to refresh everything in scope of the lockfile, not everything in the local cache. Out of scope for the routing fix, but flag it for sion review.

3. **What does `--remote` mean in combination with `--offline`?** Today these are mutually exclusive at the flag level (one constructs no remote index, the other forces remote use). Confirm that the `Context::try_init` path rejects `--offline --remote` with a usage error rather than silently picking one — needed for the spec to hold.

4. **Should `fetch_manifest` Remote mode be split too?** Per spec, mutable (tag) reads in Remote go to the source and don't write through. Digest reads can still cache. The current code already routes digest separately; the missing piece is "don't write through during query". M3 Option A handles this cleanly; Option B requires care at every call site.

---

## 6. Pre-Merge Checklist (for whoever reviews this before sion → main)

- [ ] Spec in §1 still matches the user's intent (re-confirm with author).
- [ ] M1–M7 landed on main; tests added.
- [ ] S1–S5 incorporated into sion before merge; sion's `unset OCX_INDEX` workaround removed if obsolete.
- [ ] X1 promoted to ADR; X2 user docs updated.
- [ ] Open questions §5 resolved or explicitly deferred with tracking issue.

---

## 7. References

- `crates/ocx_lib/src/oci/index/chained_index.rs:163-268` — current `ChainedIndex` impl (post-`list_repositories` fix).
- `crates/ocx_lib/src/oci/index/local_index.rs:43-77` — `refresh_tags` and `write_chain_and_commit_tag` (the two legitimate write paths).
- `crates/ocx_cli/src/app/context.rs:53-92` — mode selection logic; `Remote` mode currently always pairs with non-empty sources.
- `crates/ocx_cli/src/command/index_catalog.rs`, `index_list.rs`, `index_update.rs` — pure-query and explicit-update commands.
- `crates/ocx_cli/src/command/install.rs`, `package_pull.rs`, `exec.rs` — auto-update consumers (post-sion: only install/pull should remain).
- `.claude/rules/subsystem-oci.md` — needs M6 update.
- `.claude/rules/subsystem-cli-commands.md` — `--remote` flag column needs revisit alongside X2.
