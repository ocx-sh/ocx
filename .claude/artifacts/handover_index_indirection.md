# Handover — Public-Index Indirection & Index-Lock Rework

**Status:** PARKED — waiting on ocx-sh/index structural changes + M-1 gate.
**Date:** 2026-07-17. **Origin:** discussion of issues [#215](https://github.com/ocx-sh/ocx/issues/215) (platform inconsistencies / lock unit) and [#212](https://github.com/ocx-sh/ocx/issues/212) (list of platforms), plus sibling-repo read of `../index`.
**Sibling track:** platform-model unification runs NOW, independently (see "Track split" below).

## Decisions reached (discussion-level, no ADR written yet)

### D-A. Lock unit doctrine + snapshot exemption
- Lock unit = **platform manifest digest, never image-index digest** — uniform across `ocx.lock`, dependency pins, public index. Already an Accepted ADR index-side (`../index/.claude/artifacts/adr_locked_observation_index_format.md` D1).
- **Refinement agreed here:** doctrine applies to *content-less references crossing time/trust* (locks). A **content-carrying snapshot** (local index cache) may point at an index digest, because the bytes travel with the pointer — reference is self-satisfying. Write this exemption into the future ADR explicitly.

### D-B. Local index snapshot = verbatim OCI JSON, root+CAS layout
- Tag → `{digest, observed}` map + content-addressed copy of **whatever the registry served, verbatim** (image index or single manifest, plus child platform manifests — chain already recursed by `persist_manifest_chain`):
  ```
  .ocx/index/{registry}/{repo}/
  ├── tags.json                # tag → { digest, observed }
  └── o/sha256/<hex>.json      # verbatim OCI bytes
  ```
- Rejected: storing observation objects locally (index-repo format). Reasons: invented digest namespace vs registry-CAS verifiability; loses re-push/air-gap capability (Differentiator #9); new format where blob store already does content-addressed OCI. Observation object = pure function of OCI index — **compute on demand** for public-index interop, never store.
- Alignment with ocx-sh/index = **layout convention** (root + `o/sha256/<hex>` CAS) + **in-memory model** (`[(Platform, Digest)]` + one `is_compatible`), NOT byte format.
- Snapshot rule generalizes per source: OCI registry → OCI JSON; index.ocx.sh → root.json + obs objects (its native format, obs objects immutable → cache forever). Cached index files live under `state/registry-index/` (state, not cache, per naming doctrine).

### D-C. Identity is logical; location is routing (overturns stale spec)
- index.ocx.sh is **not a registry** — pointers only, no blobs, no `/v2` (frozen contract index-side). Resolution: `ocx.sh/<ns>/<pkg>` → `/p/<ns>/<pkg>.json` (root: `repository` ptr + tags→content) → obs object CAS (verify hash) → `platforms[].digest` → manifest from physical registry (verify OCI CAS). OCI image index fully bypassed for index-resolved packages.
- **Storage paths, ocx.lock, GC roots key on LOGICAL identity** (`ocx.sh/ns/pkg`), never physical. The stale `design_spec_registry_indirection.md` (2026-07-12, unimplemented) keys storage on `.physical()` — **overturned**: `repository` pointer is volatile by design (human-PR storage migration); physical in lock breaks committed team locks on migration; logical + digests survives (digests verify identity from any location).
- Structural enforcement copies the `[mirrors]` precedent (`adr_oci_registry_mirror.md`): rewrite confined to one seam (`Client::transport_reference`, client.rs:116-135), physical location a distinct type that **cannot compile into storage paths** (blanket Identifier↔transport conversion removed, identifier.rs:304-318). Pipeline order (keep from stale spec D11): `logical → index resolve → physical → mirror_map → fetch`.
- GC hazard verified: reachability recomputes roots via the same `slugify(identifier.registry())` as writes; any identity drift → silent orphan → collected next clean. Logical keying = zero drift by construction.

### D-D. Canonical tags
- `--[no-]canonical-tag` on `ocx package push`, **default ON** (flatten flag style). Digest-named tag `sha256.<hex>` per pushed platform manifest.
- Pure registry-side deletion safety net. **Index ignores canonical tags entirely** — no wire semantics, not required.
- Index-repo ADR-1 D8 says "opt-in" → flip to "default-on, opt-out" there in the alignment pass.

### D-E. Misc settled
- `Any` platform: naturally supported via existing OCI sentinel `{"os":"any","architecture":"any"}`; index side needs only a catalog-UI affordance (icon/badge). No schema work.
- GHCR attestation pollution: not applicable (ocx constructs indices explicitly). Residual hardening: `fetch_candidates` should skip-with-warn unparseable platform entries instead of hard-failing (platform track owns this).
- Signing: deferred entirely, future ADR. Record the mix-and-match threat (unsigned index + signed manifests ⇒ resolve-time platform-set tampering) when that ADR is written.

## Waiting on (index repo)

1. **Structural changes incoming** — index repo not finished; owner expects format churn shortly. Do not bake client code against current schemas.
2. **M-1 gate**: 42-package republish proving format v1 stable. Client-side plan (`plan_registry_indirection_client.md`) explicitly gated on it (`../index/.claude/artifacts/handover_from_ocx.md:12-13,77`).
3. **Client design spec re-derivation**: `design_spec_registry_indirection.md` predates root+CAS wire format AND all decisions above (manifest-unit, `is_compatible`, canonical grammar, logical keying). Full re-derivation needed. Keep: known-registries map (`ocx.sh → index+https://index.ocx.sh`, single baked key, D10), C-46 identity binding (index entry may never override requested tag/digest — security contract), offline/frozen error taxonomy sketch (exit 81 PolicyBlocked path).
4. Unresolved index-side (their backlog, not ours): desc-blob frozen-contract status; pinned-vs-floating anomaly predicate; yank-on-republish; silent tag disappearance; namespace Amendment A1 (`ocx.sh/ocx/cli` first-party reserved segment — we self-publish under it, watch this).
5. `repository`-change handling on already-installed packages = TOFU, accepted stance pending signing ADR.

## Work list when unparked

1. Platform-model ADR must be merged first (platform track — representation feeds everything).
2. ADR "index locking & indirection" covering D-A…D-D (incl. snapshot exemption + logical-keying invariant).
3. Local snapshot rework (TagLock → tags.json + o/ CAS, committed self-contained `.ocx/index/`). Resolves the #215 core complaint. Independent of public index — can start before M-1 if needed.
4. `--[no-]canonical-tag` push flag (small, independent — could also run early).
5. Client indirection: known-registries map, index source in ChainedIndex, two-hop fetch (root mutable / obs immutable cache-forever), typed physical-location seam.
6. Alignment pass in `../index`: ADR-1 D8 opt-in→default-on; Any catalog icon; observation-object canonical serialization must match any ocx-side computation byte-for-byte (`../index/bot/CONTRACTS.md:40` sort order).

## Key files / references

- ocx: `oci/index/chained_index.rs` (chain semantics), `oci/index/local_index/tag_lock.rs` (current TagLock), `oci/client.rs:116-159` (mirror seam), `file_structure.rs:104-116` (slug keying), `project/lock.rs` (V2 shape), `.claude/artifacts/adr_oci_registry_mirror.md` (precedent).
- index repo: `adr_locked_observation_index_format.md` (D1–D9), `adr_public_index_registry_indirection.md` (superseded original), `design_spec_registry_indirection.md` (stale client spec), `bot/CONTRACTS.md`, `schema/root.schema.json`, `schema/observation-object.schema.json`.
