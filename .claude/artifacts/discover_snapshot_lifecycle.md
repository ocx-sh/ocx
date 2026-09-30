# Discovery: Snapshot Lifecycle (pre-release prune/delete)

Facts only, for the "snapshot lifecycle" ADR. All paths relative to
`/home/mherwig/dev/ocx-sion` unless a repo prefix is given.

## Claim diff

| Claim | Verdict | Evidence |
|---|---|---|
| a. No DELETE/delete-manifest/delete-tag in `ocx_oci`/`external/rust-oci-client` | **CONFIRMED** | `grep -rniE "fn delete\|DELETE " external/rust-oci-client/src` → 0 hits. `ocx_oci` hits are all `AuthStore::delete` (credential-store delete, `crates/ocx_oci/src/auth/store.rs:118,300`) — unrelated to registry content. No manifest/tag delete verb anywhere in the client. |
| b. `package_announce.rs` modes all additive | **CONFIRMED** | `crates/ocx_cli/src/command/package_announce.rs:65-108` — `--tags` (replace curated set, doc explicitly: a dropped tag is "not a version"/reserved, never a real version removal path), `--tags-file`/`--tags-from-registry` ("Never removes a committed tag"), `--refresh` (re-observes, no removal), `--yank`/`--unyank`/`--yank-reason` (marker toggle, not delete). |
| c. Index tag entry shape `{content, observed, yanked?}`; no branch removes a tag key | **CONFIRMED, with nuance** | Two structs implement the shape at different layers: **write/local-copy** `DerivedTag {content, observed}` at `crates/ocx_index/src/local_index.rs:43-48`, committed via `commit_root_tags` (`local_index.rs:415-483`), which only `doc.tags.insert(...)` (line ~468) — no `.remove()`/`.retain()` on the tags map anywhere in `ocx_index/src`. **Read/wire** `RootTag {content, yanked}` at `crates/ocx_index/src/wire.rs:69-77` — `observed` is present in the JSON fixtures but not modeled in the read struct (no `deny_unknown_fields`, `wire.rs:52`, so it round-trips through the field being ignored on that path). `YankMarker {reason, at}` at `wire.rs:79-85`. |
| d. `ocx.lock` pins digests, lock resolution is index-free | **CONFIRMED (doc)** | `arch-principles.md` Key Concepts, "Index" row: "plays no part in `ocx.lock` resolution (locks are index-free)". No contradicting code found; lock-touched resolution (`resolve_lock_touched`) reads only the registry/lockfile per `subsystem-cli-commands.md` `update` row. |
| e. Prerelease-with-build cascades only to parent prerelease; build sorts lexicographically | **CONFIRMED** | `crates/ocx_package/src/version.rs:304-385` `impl Ord for Version`, line 377: `(Some(lhs_build), Some(rhs_build)) => lhs_build.cmp(rhs_build)` — `String::cmp`, i.e. lexicographic. Cascade test `crates/ocx_package/src/cascade.rs:446-453` `cascade_prerelease_with_build_cascades_to_parent` proves a build-tagged prerelease (`1.7.3-beta+20260216`) cascades only to `1.7.3-beta`, never past it. |
| f. `package push --keep-tag` default pushes `__ocx.keep.<algo>-<hex>` | **CONFIRMED** | `crates/ocx_cli/src/command/package_push.rs:53-56` doc comment: "Push a `__ocx.keep.sha256-<hex>` tag ... Pass `--no-keep-tag` to skip it." Flag wired at lines 363/388/400; default-on per `subsystem-cli-commands.md` `package push` row. |
| g. Docs (D-additive-only, D-yank-not-delete, C7, adr_index_indirection yank, adr_announce_gitlab_forge) | **CONFIRMED, present** | `adr_announce_publisher_surface.md` changelog (2026-07-19) ratifies "additive `--tags-file` union, unchanged⇒no-op". `adr_index_indirection.md` documents yank/deprecation as an *index-level marker* (surfaced via `status`/`RootTag.yanked`), never a delete verb. `adr_announce_gitlab_forge.md` exists (24.0K) covering the GitLab forge path `--transport git`. No registry-DELETE decision recorded in any of the four. |

**Net: every current-state claim in the decision context holds.** The codebase today has zero registry-DELETE capability, additive-only announce, and index tags that only accrue or get marker-flipped (yank), never removed. `ocx package prune` (deleting from index + registry) is **wholly new capability** — nothing in the current design anticipates a tag disappearing out from under a consumer.

---

## Module map (crates touched by a "prune" design)

```
ocx_oci      [ecosystem]  registry client, transport, auth, endpoint, SSRF guard
  ↑
ocx_index    [ecosystem]  local index collection, wire format, regenerate, chained/local/ocx index impls
  ↑
ocx_package  [ecosystem]  version, cascade, publisher (push/copy/description), metadata
  ↑
ocx_announce [internal]   announce pipeline, forge drivers (GitHub/GitLab), branch CAS
  ↑
ocx_cli      [interface]  package_announce.rs, package_push.rs, package_cascade_{check,repair}.rs, package.rs registry, exit classification
```
`scripts/crate_map.toml` is the enforced edge table (`arch-principles.md` Crate Layout). A `prune` verb would live in `ocx_cli/src/command/package_prune.rs` (or similar), calling into `ocx_package::publisher` (registry delete, new) and `ocx_index` (tag-key removal, new) — both currently-absent capabilities per claims (a) and (c).

### Tag listing + version sorting (reuse candidates for "newest N per channel")
- `crates/ocx_package/src/version.rs:304-385` — `Ord`/`PartialOrd` for `Version` (major/minor/patch/build lexicographic/prerelease) — the sort a "keep last N" policy needs.
- `crates/ocx_package/src/cascade.rs` — `graph::plan_repairs`, `cascade()`, `decompose()` already walk a tag graph and classify build/prerelease relationships; `cascade_repair.rs` calls `graph::plan_repairs(&entry.report, &observation, &expected)` (`crates/ocx_cli/src/command/package_cascade_repair.rs:76`).
- `crates/ocx_cli/src/command/package_cascade.rs:73-93` `audit_all`/`audit_one` — concurrent per-package **live registry** tag-graph reads (`CASCADE_PACKAGE_CONCURRENCY` fan-out via `buffer_unordered`), the existing pattern for "enumerate what a channel currently holds."
- `crates/ocx_index/src/local_index.rs` `refresh_tags` (called from `index update`/`index sync`) is the existing per-tag registry↔local merge loop — but it is additive-only (see Claim c), so a prune policy cannot reuse it for removal without a new branch.

### Signatures/SBOM/attestations next to a manifest (a delete must consider)
- `package sign`/`package verify`/`package attest`/`package sbom` attach via **OCI Referrers API** (`adr_oci_referrers_signing_v1.md`) — referrers are subject-digest-keyed, so deleting a manifest orphans its referrers unless the delete also walks the Referrers index.
- Cosign simplesigning fallback tags: `--signature-format bundle|simplesigning|both` (`subsystem-cli-commands.md` `package sign` row) — simplesigning uses a `sha256-<hex>.sig` convention tag sibling to the subject tag; a tag-level prune that only removes the version tag leaves this sibling orphaned.
- `--keep-tag` (`__ocx.keep.<algorithm>-<hex>`, `package_push.rs:53-56`) is the **existing** registry-side deletion-safety-net tag — explicitly designed against exactly this class of problem (a manifest becoming unreferenced). Any prune design must reconcile with keep-tag: pruning is the first scenario keep-tag was NOT built for (keep-tag prevents accidental GC of content something else pins; prune is deliberate removal).

---

## Interacting commands

Scope: for every command below, what a **vanished/pruned tag** (registry delete + index-tag-key removal, both currently non-existent per Claims a/c) does today, and whether it needs to learn about the new marker/removal.

### `ocx index update` / `ocx index sync`
- **Local collection never removes a tag key.** `commit_root_tags` (`crates/ocx_index/src/local_index.rs:415-483`) is upsert-only: `doc.tags.insert(tag.clone(), DerivedTag{...})` (~line 468), reads the previous tag set only to compute `RootPins{previous, current}` for GC pin advancement (lines 473-481), never to prune. Doc contract, `subsystem-cli-commands.md:204`: "Never deletes a locally-known tag, never fetches anything about a package you did not name."
- **Consequence today:** if a tag is deleted from the registry (hypothetically, since no delete exists yet), the next `index update`/`index sync` for that package simply does not re-observe it — the stale local entry (pointing at a digest the registry no longer serves) survives untouched. This is exactly the documented "index vanished-tag = human anomaly" framing in the decision context (nothing currently distinguishes "never fetched" from "pruned").
- **Needs to learn about ephemeral/prune:** yes — `refresh_tags`/`commit_root_tags` has no code path for "this tag is gone upstream, remove/mark it locally." A prune-aware index sync needs either (1) a companion delete op that removes the local tag key when the registry 404s a previously-known tag, or (2) an explicit "ephemeral, prune on sight" marker so a stale entry is distinguishable from a legitimately-never-synced one.

### `ocx index regenerate`
- `crates/ocx_index/src/regenerate.rs:24-41` (`regenerate_catalog`) — doc comment (lines 4-6, 29-31): "the only operation that clears a catalog entry whose root document is gone." Operates on `c/index.json` (the catalog, package-name → root-digest map) by re-deriving from the `p/` walk on disk — **never touches a root document's `tags` map**, "Writes no `config.json` and removes no root or `o/` object" (regenerate.rs:29). So it is a catalog-level (package-existence) cleanup, orthogonal to tag-level pruning; it would not clean up a stale pruned tag inside a still-extant root.
- **Needs to learn:** no — out of scope by design (package-level, not tag-level). Note for the ADR only if prune ever removes a *whole package's* last root (channel-close semantics for `package prune` removing "a whole channel" could intersect this if the channel *is* the whole package).

### `ocx index catalog` / `ocx index list`
- `subsystem-cli-commands.md:202-203` — pure read commands (`--tags` / `--platforms`/`--variants`) over whatever the local collection or (for `list`, per its "Low-level registry" tier) live registry currently holds. No mutation, so nothing to prune-adapt beyond what the underlying read source (local index vs. live registry) already reflects. A pruned-but-not-yet-`index update`d local entry would still list as present — a display-layer staleness inherited from the "index update" gap above, not a defect of `catalog`/`list` themselves.

### `ocx package announce --tags-from-registry` / `--refresh`
- `crates/ocx_cli/src/command/package_announce.rs:80-91`: `--tags-from-registry` = "Add every tag the package's registry repository currently holds to the already-committed curated set" (doc: "Never removes a committed tag, and a yanked tag stays yanked"); `--refresh` = "Re-observe every already-committed tag, picking up a digest that moved."
- **`--tags-from-registry` cannot resurrect a genuinely-pruned tag**, because it lists tags **live from the registry** — if prune deleted the registry tag, it is absent from what this flag observes. Safe by construction, *given prune deletes the registry side first or atomically*.
- **`--refresh` is the risk path**: it re-observes tags already committed in the **index**, not the registry's current tag list. If prune deletes a tag from the registry but an announce run using `--refresh` still names it as "already committed" (because a prior announce curated it and no announce has dropped it since — recall `--tags` alone can drop a curated tag, `--refresh` cannot), the re-observe step will 404 against the registry and the run errors rather than silently resurrecting anything. So `--refresh` fails loudly on a pruned-but-still-curated tag; it does not resurrect, but it also does not degrade gracefully — **the ADR needs an explicit answer for "curated index tag whose registry manifest is now gone."**
- **Needs to learn:** yes, specifically `--refresh`'s error path needs a designed outcome (auto-drop-from-curated-and-warn, vs. hard-fail) once prune is real.

### `ocx package copy` / `[mirrors]` registry-to-registry sync
- `crates/ocx_package/src/publisher/copy.rs` — `copy()` (line 183) merges the **target** index **per platform**, never byte-copies (doc at `subsystem-cli-commands.md` `package copy` row): "the target index is merged per platform... rolling tags recomputed against the target." Per-platform disposition enum includes `Disposition::KeptNotInSource` (`copy.rs:128,1628`, rendered `"kept (not in source)"`) — i.e. copy already has a first-class notion of "source lacks something the target has" and its default behavior is to **keep** it, not delete it.
- **Consequence:** a mirror `package copy` run against a source whose tag was pruned will simply not see that tag in the source enumeration; anything already copied to the target stays (`KeptNotInSource`), so copy does not propagate deletions and does not need to — it is inherently additive/merge, matching the D-additive-only posture. No resurrection risk: copy reads live source tags, same class of safety as `--tags-from-registry`.
- **Needs to learn:** only if the ADR wants prune semantics to *propagate* across mirrors (i.e. a mirror registry should also drop a tag its upstream pruned) — today's `copy` has no delete verb at all (consistent with Claim a), so cross-registry prune propagation is unbuilt in every direction.

### `ocx package cascade check` / `ocx package cascade repair`
- Both read **live registry state**, not the local index: `audit_all`/`audit_one` (`crates/ocx_cli/src/command/package_cascade.rs:73-93`) enumerate "every named package's tag graph" via concurrent registry reads (`buffer_unordered(CASCADE_PACKAGE_CONCURRENCY)`), and for a logical `ocx.sh/…` identifier additionally reads "the live public index root" (`subsystem-cli-commands.md` `package cascade check` row).
- **`repair` cannot re-create a pruned rolling tag pointing at pruned content**: its doc (`crates/ocx_cli/src/command/package_cascade_repair.rs:17-29`) states repair "publishes nothing new, it re-points," and "every manifest it references is checked to still exist, so a run that cannot be completed writes nothing at all." Since the plan (`graph::plan_repairs`, line 76) is built from the **currently observed** registry graph, a pruned version tag is simply absent from `observation` — repair naturally re-points rolling tags at whatever concrete versions still exist, not at ghosts. This is prune-safe by construction (reads current reality, doesn't trust stale state).
- **Needs to learn:** no functional change needed — already correctly derives "expected" state from live registry content. Worth noting as a **positive precedent** for how the prune ADR's other consumers (index update, announce --refresh) should also behave: read live, never trust a cached "this should still exist."

### `ocx update` / `ocx lock` / `ocx add` resolution against a pruned tag
- Lock resolution is index-free (Claim d) — a project's `ocx.lock` pins a **digest**, not a tag. `ocx update` (re-resolves advisory tags against the **live** registry, `subsystem-cli-commands.md` `update` row: "Re-resolve advisory tags in lock against the LIVE registry by default... unknown tag exit 81 under `--frozen`") would, on a pruned tag, get a registry 404 — same failure class as any other missing-tag resolve, not something update-specific needs to add: `--frozen` already has an "unknown tag" exit path (81, `PolicyBlocked`), which is the natural fit for "this tag no longer exists."
- `ocx add`/`ocx lock` resolve a **fresh** tag→digest binding from the live registry at the moment of the call; a pruned tag simply cannot be added (fails as "not found," same as any nonexistent tag today — no special-casing needed).
- **Needs to learn:** no new mechanism — the existing tag-resolution-against-live-registry path already fails correctly on absence. The only gap is diagnostic quality: a plain 404/not-found error doesn't distinguish "never existed" from "pruned by policy," which the ADR may want to improve (e.g. surfacing prune metadata in the 404 response) but doesn't require new *logic*.

### `ocx package pull` / install of a digest whose registry manifest is gone
- Resolution is local-store-first, content-addressed: `crates/ocx_package_manager/src/tasks/find_or_install.rs:42,46` — "Package not found in package store; attempting offline re-assembly from cache" then "not found locally, pulling" (only on a genuine local miss). Since the store is keyed by digest (not tag), a package **already materialized locally** resolves with **zero registry contact** regardless of whether its origin tag/manifest was later pruned from the registry.
- **Consequence:** a pruned digest is fully install/exec-usable forever on any host that already has it cached — pruning the registry does not retroactively break existing installs. A **fresh** `pull`/`install` of that exact digest on a host with no cache hit would 404 against the registry (no special "manifest not found" enum was located in `ocx_package_manager`'s pull path beyond generic registry-error propagation — `crates/ocx_package_manager/src/tasks/verify.rs:186` references `ManifestNotFound` only for a referrers-fallback-to-empty-index case, not a general pull failure).
- **Needs to learn:** no — this is the expected, desired content-addressed behavior (a pinned lock keeps working from cache even after upstream prune). The only ADR-relevant point: prune must NOT touch the local blob/layer/package CAS on any consumer machine — it is a registry+index-side operation only, and the existing local-store-first path already provides the isolation.

### `crates/ocx_package_manager/src/tasks/garbage_collection.rs` (local GC)
- Pure local-tree reachability walker (`GarbageCollector::build`/`unreachable_objects`/`purge`, lines 27-114) over `refs/{symlinks,deps,layers,blobs}` — has **no network component and no knowledge of the registry or index at all**. It answers "what does this host still reference," never "what does the registry still serve."
- **Needs to learn:** no — orthogonal by design. A locally-GC'd (unreferenced) package is unrelated to a registry-pruned one; the two lifecycles (local disk reachability vs. registry retention) are already cleanly separated and should stay that way per `arch-principles.md`'s ref-separation design principle.

### ocx-mirror (`/home/mherwig/dev/ocx-mirror`) — registry sync / announce flow
- Repo confirmed present (`ls` — full Rust project, `src/command/package/pipeline/{push,cascade,notify,patch}.rs`, CI-template generator under `pipeline/generate/templates/`).
- **`pipeline/generate/templates/announce-from-registry.yml`** (lines 1-52): generates a GitHub Actions workflow whose step comment states "the nightly push job announces what its own run published; this one announces **everything the registry already holds**" — runs `ocx-mirror package pipeline announce{SPEC_ARG}` (wrapping, per its purpose, `ocx package announce --tags-from-registry` semantics). Since this reads **live** registry tags at run time (same class as `--tags-from-registry` above), a pruned tag is simply absent from what it re-announces — **no resurrection risk**, provided registry-side prune has already landed before this job runs.
- `pipeline/cascade.rs` + `templates/cascade.yml` mirror `ocx package cascade check/repair`'s live-registry-read pattern (same concurrency-group comment explicitly says cascade and push "re-point the same rolling aliases" and therefore share a concurrency group, unlike the from-registry announce job) — same prune-safety argument applies: cascade repair here also reads live state.
- **Needs to learn:** the main open risk is **ordering/race**, not resurrection logic: if `announce-from-registry` or `cascade` runs *between* the registry-side delete and the index-side delete of a prune operation (if prune is not atomic across both), a from-registry re-announce could catch the tag mid-deletion (registry still serving it, e.g. eventual consistency) and re-commit it to the index moments before prune's own index-delete step — a TOCTOU window the ADR should close by making prune's registry-delete-then-index-delete (or vice versa) atomic/locked against concurrent announce, or by having prune itself be the sole writer during its run (same per-package index lock `commit_root_tags` already takes, `local_index.rs` `lock_source("index-root", ...)`).

**Summary of what needs new design work** (vs. already prune-safe by construction):
- **Needs new logic:** `ocx index update`/`sync` (no removal path at all today — Claim c blocks this structurally); `package announce --refresh` (undefined behavior on a curated-but-now-404 tag — needs a decided outcome, not just an error).
- **Prune-safe already (reads live state, no caching-induced staleness):** `cascade check`/`repair`, `package copy`, `announce --tags-from-registry`, `ocx add`/`lock`/`update`, ocx-mirror's `announce-from-registry`/`cascade` jobs.
- **Orthogonal, no change needed:** `index regenerate` (catalog-level, not tag-level), local `garbage_collection.rs` (disk reachability, not registry retention), `package pull`/install of an already-cached digest (content-addressed, registry-independent).
- **New cross-cutting concern:** ordering/atomicity between prune's registry-delete and index-delete steps, to avoid a race with any live-reading announce/cascade/mirror job.

---

## Index repo governance (`/home/mherwig/dev/index`, read-only)

- `schema/` — the canonical root/catalog JSON Schema the local `ocx_index` wire types must stay compatible with (`RootTag`, `YankMarker` shapes above); a prune design changing the tag wire shape (e.g. adding a `pruned`/`ephemeral` marker) is a schema change here first.
- `bot-tools/` — governance/validation checks the index bot runs against submitted roots; relevant if `ocx package prune`'s index-delete step is expected to go through the same bot/PR path `announce` uses (per the decision context: prune "deletes from both index and registry" — if the index-repo *hosted* copy is bot-governed, a prune delete against it needs a bot-tools rule permitting tag-key removal, which today's additive-only posture (Claim g) does not have).
- ADR-6 FP-3 anomaly: per the decision context, a vanished tag is currently classified as a **human anomaly** (false-positive class 3) by the index bot's own drift detection — this classification itself needs revisiting once vanishing is a legitimate, policy-driven outcome rather than always operator error.
- Golden fixtures + the Python serializer/reference implementation: any wire-shape change (new marker field) needs a paired fixture + serializer update here, mirroring the Rust-side `wire.rs` structs.

## Prior decision records (relevance/conflict)

| Record | Relevance |
|---|---|
| `adr_announce_publisher_surface.md` | Ratifies additive-only `--tags-file` union (D-additive-only lineage); the change series this ADR would need to amend for any announce-side prune interaction. |
| `design_spec_announce_initiative.md` (C7) | Source of "yank never automatic" — prune must not silently reuse the yank mechanism as a delete trigger. |
| `adr_index_indirection.md` | Defines the index's dispatch-object/root model this whole tag-removal design must fit inside (`o/` CAS, root document, catalog) — a tag-key delete must also handle the now-possibly-orphaned `o/<algo>/<hex>.json` dispatch object. |
| `adr_oci_index_only_dispatch.md` | Reserved-tag namespace (`__ocx*`) rules — a prune policy must not treat `__ocx.keep.*`/`__ocx.desc` as prunable version tags. |
| `adr_announce_gitlab_forge.md` | GitLab transport/forge path prune's index-delete would need to route through, if index writes stay forge-mediated (PR/MR) rather than direct. |
| `adr_announce_diverged_branch_rebuild.md` | Existing precedent for how a stale/diverged announce branch is *rebuilt*, not deleted — relevant model for how a prune-driven index change should interact with an open announce PR touching the same root. |
| `adr_cascade_platform_aware_push.md` | Per-platform version filtering + index merging — the mechanism `package copy`'s `KeptNotInSource` disposition reuses; a prune-aware cascade would extend this, not replace it. |
| `adr_index_sync_performance.md` | Coalescing/retry design for `index update`/`sync` — the file that would carry a "removal" branch if one is added, since it already owns the per-package root-fetch/commit machinery. |

## Constraints (rule excerpts)

- **`subsystem-oci.md`** (not fully read this pass — flagged for the ADR author): governs `ocx_oci`/`ocx_index`/`ocx_sign` paths; any new registry-DELETE call is squarely in its scope (SSRF guard, transport, auth) — read before implementing.
- **`subsystem-cli-commands.md`**: a new visible verb (`ocx package prune`) needs a `@pytest.mark.smoke` acceptance test + `command` marker (enforced by `test/lint/test_smoke_coverage.py` / `task test:rows:check`), and `SUITE_FLOOR` rises in the same commit.
- **`quality-rust-exit_codes.md`**: prune's failure modes need mapping onto the shared `ExitCode` enum — likely `NotFound (79)` for "nothing to prune," `PolicyBlocked (81)` for a frozen/offline refusal, `DataError (65)` for an inconsistent/partial prune state; no existing code fits "registry accepted delete, index write failed" (a new tool-specific code above 84 may be warranted — see `adr_announce_diverged_branch_rebuild.md`'s pattern for partial-failure handling).
- **`arch-principles.md`**: any new crate-crossing call (e.g. `ocx_cli` → new delete verb in `ocx_oci`) must respect `scripts/crate_map.toml` edges; `ocx_index`'s three-tier CAS / ref-separation-for-GC design principle is the template for how a registry-side prune should be reflected (or deliberately not reflected) in local GC roots.

## Test infra (reusable)

- **Fake forge** (announce acceptance tests) — GitHub/GitLab forge drivers already have test doubles per `ocx_announce::forge` (used by `announce`/`claim`/`cascade repair --tags-file` flows); a prune-into-index-PR design reuses this directly.
- **Local registry (zot)** — `docker-compose.yml`/`zot-config.json` under `test/` back the acceptance suite's OCI-tier tests (`package push`, `package copy`, `cascade`); a registry-DELETE capability test would run against this same local registry, not a live one.
- **`StubIndexTransport`** (`crates/ocx_index/src/ocx_index.rs:1283` and sibling test modules) — the existing stub-transport pattern for unit-testing `allow_yanked`/tag-resolution behavior; the natural place to add "pruned tag" stub scenarios at the unit-test tier before an acceptance test exists.
- **`test_windows_shim.py`**-style `UNCACHED_MODULES` note: irrelevant here (Windows-specific), not a prune concern.
