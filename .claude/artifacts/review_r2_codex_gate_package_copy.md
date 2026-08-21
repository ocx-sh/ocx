# Review R2 — cross-model gate — `ocx package copy`

- **Status:** Codex ran. `gpt-5.6-terra`, one-shot, scope `code-diff`, `--base dfcdcb98`, exit 0.
- **Diff:** `dfcdcb98..e428be81`.
- **Codex verdict:** `needs-attention`, two findings.
- **Verdict after verification:** **zero blocking findings on this diff.** Both Codex
  findings are real defects and both are pre-existing, byte-identical at the base commit.

## Coverage

Codex reached all six briefed areas. Its trace shows it read `oci/copy.rs` in full,
`publisher/copy.rs`, four regions of `client.rs`, `native_transport.rs`,
`error.rs::classify`, `app.rs`, the vendored `external/rust-oci-client/src/client.rs`,
and ran an independent `ReadAddressing::Mirrored` sweep.

**Not reached by Codex**, checked separately and clean:

- `crates/ocx_cli/src/api/data/package_copy.rs` terminal sanitization — target, cascade
  tags, platform and digest all routed through the sanitizer; `source` is never rendered
  and canonical tags only as `.len()`.
- The referrer traversal caps — `MAX_REFERRER_DEPTH` / `MAX_REFERRERS_PER_LEAF` are
  checked before insert, `seen` spans the whole chain, recursion is boxed.

## Area 1 — no sixth mirrored write-backing read

Codex's independent sweep and a hand enumeration of the 13 reads reachable from
`Publisher::copy` and the CLI agree: no sixth site, direct or transitive.
`fetch_manifest_raw_bytes` (`client.rs:2072`) hardcodes `Canonical`; `list_tags`
(`:421`), `fetch_manifest` (`:487`) and `pull_description` (`:1771`) default to it;
`copy_leaf` / `spool` / `copy_referrers` name it explicitly;
`merge_platform_into_index` (`:542`) and `push_canonical_tag` (`:697`) both use
`canonical_reference()`.

The cascade blocker-probe test discriminates: it seeds the mirror and canonical hosts
with *different* platform lists, so a mirrored implementation fails rather than passing
through the conservative `Err` arm.

## CONFIRMED — both pre-existing, neither introduced here

### C1 — lost-update read-modify-write on the target index

**Where:** `crates/ocx_lib/src/oci/client.rs:531` (read at `:547`, push at `:625`).

**Invariant:** phase-2 convergence — no silent data loss.

**Scenario:** two `ocx package copy` runs promote `amd64` and `arm64` to the same target
tag. Both read the old index at `:547`; each pushes an index carrying only its own
platform at `:625`. The later PUT erases the earlier platform. Both exit 0.

**Refutation attempted:** phase 2 is sequential (`publisher/copy.rs:360-405`) — but that
serializes platforms within one process, not across independent publishers. No
conditional write and no post-write read-back exists (`grep -c 'If-Match|etag|ETag'` over
the function body returns 0).

**Why not a finding against this diff:** the function body extracted at `dfcdcb98` and at
`e428be81` hashes identically (`sha256 4292dcf1117963e9…`, 100 lines both sides). It is
already the write primitive behind `push --cascade`
(`package/cascade/equivalence.rs:150`). Codex's proposed fix — conditional writes with
retry — is not portable either: the OCI distribution spec defines no conditional manifest
PUT. The realistic outcome is a documented single-writer contract, not a code change.

### C2 — unbounded description ingestion

**Where:** `crates/ocx_lib/src/oci/client.rs:1819-1854`.

**Invariant:** bounded ingestion (PKG-04 / PKG-05 / PKG-07).

**Scenario:** `ocx package copy --description` against a hostile source. The layer loop
calls `pull_blob_to_file` (`native_transport.rs:343`) with no declared-size pre-check and
no stream cap, then `tokio::fs::read`s markdown / PNG / SVG layers whole into memory. A
50 GiB markdown layer fills the disk; if it completes, it allocates 50 GiB. There is also
no cap on the layer *count*. Reached at `command/package_copy.rs:167`, **after**
`publisher.copy()` has committed — so the command fails after publishing.

**Correction to Codex's account:** the fork's `pull_blob` *does* digest-verify, against
both the `Docker-Content-Digest` header and the layer descriptor, so content substitution
is caught. The gap is purely the bound — which holds regardless, since an attacker can
serve a genuinely huge blob under a matching digest.

**Refutation attempted:** the promotion caps do not cover it — this is an independent
path. `oci/copy.rs::spool` caps at 8 GiB via `stream.take(declared + 1)`, and
`fetch_single_layer_artifact` (`client.rs:1913`) sits sixty lines below documenting the
exact missing pattern in its own doc comment. Three paths in this feature; one is missing
the standard the other two apply.

**Why not a finding against this diff:** `pull_description` call sites are identical at
`dfcdcb98` and `e428be81`, and the function contains zero `take(` / `MAX_` occurrences at
both commits. `package copy --description` and `package describe --from` both existed at
base. The diff changed only the addressing (mirror → canonical, which *narrows*
exposure) and the `Ok(None)` error.

This is the class `quality-core.md` names as invisible to diff-scoped review: the file
already existed, so no reviewer of the *change* was prompted to question it. That is the
cross-family value of this run.

## PLAUSIBLE

None. Everything Codex reported was verified in source.

## Minor

The gate brief carried the ADR's original phase-1 placement of the canonical
`sha256.<hex>` tag. The code writes it in phase 2 (`publisher/copy.rs:395-405`), which is
correct — the tag is derived from the *merged* index and cannot precede the merge. The
ADR already carries an amendment saying so; its risks bullet and its line citations were
corrected in the same commit as this artifact.

## Follow-ups (not filed — owner decides)

1. Route description layers through the `fetch_single_layer_artifact` cap pattern already
   present in the same file (C2). Higher value of the two: reachable from an untrusted
   source registry.
2. Decide the concurrency contract for `merge_platform_into_index` (C1), scoped to
   `push --cascade` and `package copy` together. Likely a documented single-writer
   contract rather than code.
