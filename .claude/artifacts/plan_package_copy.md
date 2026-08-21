# Plan: `ocx package copy`

## Status

- **Plan:** plan_package_copy
- **State:** review
- **Active phase:** complete — execution and the review-fix loop are done
- **Step:** /hex-execute → review-fix-loop converged; next `/hex-review .claude/artifacts/plan_package_copy.md`
- **Last update:** 2026-08-22 (after the round-2 panel + cross-model gate; branch `evelynn`, HEAD e428be81)
- **Rounds:** 2 adversarial panels (8 workers then 5) + Codex `terra` gate. Round-1 Block
  findings: 5, all closed. Round-2: 2 Blocks + 4 actionable, all closed. Gate: zero blocking
  on this diff, two CONFIRMED pre-existing defects recorded in
  `review_r2_codex_gate_package_copy.md` and left for the owner.
- **Deferred, owner decision:** five write-backing reads still addressed to a mirror on the
  `push` / `announce` / `publish_gate` paths (Invariant #5) — outside this feature's scope and
  each annotated in place. Listed in `review_r2_security_package_copy.md` F1–F3 and
  `review_r2_quality_package_copy.md` Q3.

---

## Overview

**Status:** Approved
**Author:** architect
**Date:** 2026-08-19
**Related ADR:** [`adr_package_copy.md`](./adr_package_copy.md)

## Objective

Move an already-built, already-signed package between registries without changing its leaf
platform-manifest digest, so a dev → staging → prod promotion carries its signatures and its
`ocx.lock` pins intact.

## Scope

### In Scope

- `ocx package copy` — one package per invocation, tag- or leaf-digest-addressed.
- Byte-identical leaf manifests and blobs; recursive referrer copy; canonical `sha256.<hex>` tags.
- Per-platform index upsert at the target; rolling-tag cascade computed against the target.
- `ocx package describe --from <SOURCE>` for repo-level description promotion.
- `OciTransport::push_blob_from_path` — bounded-memory blob transfer.
- Unit + acceptance suites, reference docs, a use-case walkthrough with asciicasts.

### Out of Scope

- Batch copy of several packages per invocation.
- Whole-repository or whole-registry sync (a replication engine, not this).
- A `promote` alias or an environment model in config.
- Re-signing at the target — [ocx-sh/ocx#198](https://github.com/ocx-sh/ocx/issues/198).
- Index announce — `ocx package announce` already does it and stays a separate step.

## Research

**Research artifact:** N/A — prior art (`oras cp`, `crane copy`, `skopeo copy`,
`regctl image copy`) is summarised in the ADR's *Industry Context* section.

## Technical Approach

See [`adr_package_copy.md`](./adr_package_copy.md). The four fixed decisions:

- **D1** leaf manifests byte-copied, never rebuilt; PUT to the digest-addressed URL (see the
  2026-08-19 amendment in the ADR — `push_manifest_raw` returns a Location URL, not a digest).
- **D2** indexes merged per platform via `Client::merge_platform_into_index`, never byte-copied.
- **D3** rolling tags recomputed from the **target's** tag list via `push_with_cascade`.
- **D4** source reads use canonical addressing, never the mirror path (`subsystem-oci.md` #5).

Two-phase write order: phase 1 writes blobs, leaves and referrers (pure adds); phase 2 does the
index merges, the canonical tag, and the cascade — the canonical tag is derived from the merged
index, so it cannot precede the merge (see the ADR's 2026-08-21 write-order amendment).

Per-platform disposition, decided by comparing the target index entry's digest with the source
leaf digest: `added` / `unchanged` / `replaced` / `kept (not in source)`.

## Work Packages

File-disjoint where marked; WP1 and WP2 block everything downstream.

| WP | Scope | Files | Depends on |
|---|---|---|---|
| **WP1** | Stub-transport capture for referrers and the new blob method | `crates/ocx_lib/src/oci/client/test_transport.rs` | — |
| **WP2** | `push_blob_from_path` on the transport (native + trait) | `crates/ocx_lib/src/oci/client/transport.rs`, `client/native_transport.rs` | — |
| **WP3** | Transfer engine | `crates/ocx_lib/src/oci/copy.rs` (new), `oci.rs` (mod line) | WP1, WP2 |
| **WP4** | Publisher facade | `crates/ocx_lib/src/publisher.rs` | WP3 |
| **WP5** | CLI leaf + report | `crates/ocx_cli/src/command/package_copy.rs` (new), `command/package.rs`, `api/data/package_copy.rs` (new), `api/data.rs` | WP4 |
| **WP6** | `describe --from` | `crates/ocx_cli/src/command/package_describe.rs` | WP4 |
| **WP7** | Error slugs + classification | `crates/ocx_cli/src/error_envelope.rs`, `crates/ocx_lib/src/cli/classify.rs` | WP3 |
| **WP8** | Acceptance harness: second zot | `test/docker-compose.yml`, `test/conftest.py` | — |
| **WP9** | Test suites | `crates/ocx_lib/src/oci/copy.rs` (test mod), `test/tests/test_package_copy.py` (new) | WP1–WP8 |
| **WP10** | Docs + casts | see *Documentation* below | WP5, WP6 |

## Test Contracts

### Rust unit (stub transport)

- Every source-form violation raises a typed error with the recorded `calls` log **empty** —
  that is what makes "rejected before any network write" observable.
- Byte identity: pushed leaf bytes `==` seeded bytes; digest unchanged. Compare bytes, not
  parsed values, or a re-serialization bug passes.
- Disposition: absent → `added`; same digest → `unchanged` **and zero pushes recorded**;
  different digest → `replaced`.
- Duplicate self-heal: an index seeded with two `linux/amd64` entries merges to one.
- Aliased digest: two platforms sharing one digest both survive; one canonical tag written.
- `mount_blob` taken on same-registry copy (`mount_calls`), not taken cross-registry.
- Phase ordering: a scripted failure on the cascade merge must leave every leaf, referrer and
  the primary tag's canonical tag written, and no rolling tag moved. (Corrected 2026-08-21: the
  canonical tag is written in phase 2, after the primary-tag merge, not phase 1 as originally
  stated here — the contract as first written was unwritable, since a failure on "the first
  index merge" is the primary-tag merge itself, which the canonical tag depends on completing
  first. See the ADR's write-order amendment.)
- A spooled blob whose re-hash disagrees with its descriptor → typed error before any upload.
- Cascade blockers read from target-side versions: a newer patch at the target blocks the move.

### Acceptance (`test/tests/test_package_copy.py`)

1. Same registry, different repo — multi-platform; `index_platforms` equal, per-platform digests equal.
2. Cross-registry, multi-platform — same assertions across two zots.
3. Byte identity via `fetch_manifest_raw` on both sides, bytes **and** `Docker-Content-Digest`.
4. **Signature survives the copy** — sign at the source against the real Sigstore stack, copy,
   `ocx package verify` against the target. The load-bearing test of the whole feature.
5. Merge not overwrite — target holds `darwin/arm64`, copy only `linux/amd64`, both present,
   report row `kept (not in source)`.
6. Second copy after a new platform — `unchanged` for the first, `added` for the new one, no re-upload.
7. Cascade against the target — target holds `3.28.2`, copying `3.28.1` must not move `3.28`.
8. Idempotent re-run — exit 0, every row `unchanged`.
9. Error cases, each asserting the one contracted code, never a band:
   digest source without `--platform` → 64 **and the target provably never contacted**;
   without `--identifier` → 64; digest naming an index → 64; `--to` with `--identifier` → 64;
   source tag absent → 79; referrers against `registry:2` → **84**; `--no-referrers` against the
   same target → 0 (proving 84 comes from the capability probe, not from the target differing);
   `--offline` → 81; `--dry-run` → 0 with the target byte-for-byte unchanged.
10. Description — `--description` copies `__ocx.desc`; a default run leaves the target's
    description untouched; `describe --from` copies it alone.

Per `subsystem-tests.md`: compose the existing `ocx` / `unique_repo` / `make_package` fixtures;
assert one exit code, never a range; pair every negative assertion with a positive one that
proves the check can fire.

## Documentation

- `website/src/docs/reference/command-line.md` — `{#package-copy}` section (prose, **Usage**,
  **Arguments**, **Options**, **Exit codes**), plus the `--from` addition under describe.
- `website/src/docs/user-guide/promoting-packages.md` (new) — use-case walkthrough modelled on
  `user-guide/patches.md`: pain scenario, `## How it works`, dev → staging → prod steps each
  anchored by a `<Terminal>` cast, `## In depth` links, reference-style links at the bottom.
- `website/.vitepress/config.mts` — sidebar entry beside "Patching".
- `test/doc_scripts/promote__dev-to-staging.sh`, `promote__staging-to-prod.sh` — one
  `# region cast` block each; captures and assertions stay outside the region (RN9 / RN5b).
- `test/recordings/setups.py` — a `setup:promotion` state provider.
- `.claude/rules/subsystem-cli-commands.md` — add to the "Low-level registry" tier row.
- `.claude/artifacts/handshake_toolchain_cli.md` — dated amendment line, per its own convention.

## Verification

- `task rust:test:unit`
- `task test` once (rebuilds `test/bin/ocx` with `--features ocx/__testing`), then
  `cd test && uv run pytest tests/test_package_copy.py -v`
- `task test:doc-scripts:drift`
- `task website:build`
- `task verify`

---

## Convergence check (/hex-review high, 2026-08-21)

The plan carries no C-/S- requirement IDs, so convergence ran against the Work
Packages table and the Test Contracts section. Appended by the orchestrator;
these are the gaps, not new scope.

| Item | State | Note |
|---|---|---|
| WP7 error slugs | partial | `classify.rs` landed; `error_envelope.rs` untouched. Premise dissolved — `CopyError` is a two-arm transparent wrapper with no `CopyErrorKind`, so there is no `kind_detail()` to emit and `detail` is `skip_serializing_if`. `kind`/`exit_code` are correct. Decide: add the arm, or strike the file from WP7. |
| WP9 unit contract #3 | contradicts | "zero pushes recorded" is not asserted; the test checks only `push_blob:` absence, and `Unchanged` still PUTs the leaf and re-copies referrers. Contract itself corrected 2026-08-21 to match the shipped re-verify behaviour (see the ADR's `unchanged`-row amendment) — assigned to WP-E this fix-loop round; verify at merge. |
| WP9 unit contract #4 | missing | duplicate self-heal (two `linux/amd64` entries → one). Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP9 unit contract #5 | missing | aliased digest (two platforms, one digest, one canonical tag). Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP9 unit contract #7 | missing | phase ordering under a scripted merge failure — and unwritable as stated, because the canonical tag is phase 2, not phase 1. Corrected form recorded 2026-08-21 in the Test Contracts section above; assigned to WP-E this fix-loop round; verify at merge. |
| WP9 unit contract #8 | missing | spooled-blob re-hash mismatch. `verify_spooled_blob` has no test at all. Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP9 acceptance #1 | missing | same-registry, different-repo copy — every acceptance case crosses registries. Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP9 acceptance #6 | missing | second copy after a new platform (`unchanged` + `added`, no re-upload). Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP9 acceptance #9 | partial | `--offline` → 81 absent; the other 7 error cases are delivered. Assigned to WP-E this fix-loop round (2026-08-21); verify at merge. |
| WP10 casts | delivered | one script rather than the planned two; deliberate. |

Unrequested but sound, kept: `test_a_copied_package_is_installable_from_the_target`,
`test_a_non_canonical_manifest_is_copied_byte_for_byte` (the latter is the only
byte-identity test that actually discriminates).

Verdict: **not converged** — caps the review at Needs Work independently of the
Block findings. Next: `/hex-execute .claude/artifacts/plan_package_copy.md`.
