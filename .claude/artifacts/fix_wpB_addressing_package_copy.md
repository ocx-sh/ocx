# WP-B — Addressing and publisher fixes for `ocx package copy`

- **Branch:** `hex/pkgcopy-fix--addressing` (based on `evelynn` @ dfcdcb98)
- **File set:** `crates/ocx_lib/src/oci/client.rs`, `crates/ocx_lib/src/publisher/copy.rs`,
  `crates/ocx_lib/src/package/cascade.rs`
- **Binding rule:** `.claude/rules/subsystem-oci.md` Invariant #5 — a read whose answer
  decides, gates or verifies a write asks for `ReadAddressing::Canonical`.

Finding 4 required editing outside that set — inverting a default is not expressible
without touching its call sites. Six further files were touched **mechanically only**, with
behaviour preserved at every site: `announce/pipeline.rs`, `managed_config/persistence.rs`,
`oci/index/oci_index.rs`, `oci/index/ocx_index.rs`, `publisher.rs`,
`publisher/publish_gate.rs`. None is in another work package's set — WP-A's `oci/copy.rs`,
`oci/client/transport.rs` and `oci/client/native_transport.rs` are untouched, as are
`crates/ocx_cli`, `test/` and `website/`.

Written incrementally: each row is appended the moment the finding is closed.

## Findings

### 1 — [BLOCK] `publisher/copy.rs:285` tag listing decided from a mirror — **fixed**

`target_tags` listed the target's tags through `Client::list_tags`, which is
`ReadAddressing::Mirrored` (`client.rs:409-411`). That listing decides which rolling tags
get re-pointed, and the tags are written canonically — CWE-345/367.

- Changed `crates/ocx_lib/src/publisher/copy.rs:288-292`: the listing now goes through
  `list_tags_addressed(request.target.clone(), ReadAddressing::Canonical)`.
- The `target_tags` doc comment now states that *both* of its registry reads (this listing
  and the blocker probe inside `resolve_cascade_tags`) address the canonical target, and
  why.

**Proof:** new test `publisher::copy::tests::a_promotion_never_reads_the_target_through_a_mirror`
(`publisher/copy.rs`). A cascade-enabled promotion runs against a client with a mirror on
the *target* registry; the test asserts no auth handshake in the whole run names the mirror
host. The assertion is on the auth host, not the answer, because `StubTransport::list_tags`
ignores the reference it is handed (`test_transport.rs:203-216`) — the auth call is where
the host choice is observable.

- Red: reverting the call site to `ReadAddressing::Mirrored` fails with
  `no read a promotion decides from may address the mirror, got ["mirror.invalid"]`.
- Green: restored, `1 test run: 1 passed`.

### 2 — [BLOCK] cascade blocker probe read from a mirror — **fixed, unconditionally**

`resolve_cascade_tags` -> `has_blocking_platform` (`package/cascade.rs:317-330`) fetched
each blocker manifest with `Client::fetch_manifest`, i.e. `Mirrored`. Fixed for **every**
caller, not scoped to the copy path: the push path's probe decides a write too, so scoping
would have left `push --cascade` broken.

- Added `Client::fetch_manifest_addressed(identifier, addressing)` (`oci/client.rs:461-473`),
  with `fetch_manifest` delegating at `Mirrored` — the same split
  `list_tags`/`list_tags_addressed` already uses.
- `has_blocking_platform` now calls it with `ReadAddressing::Canonical`
  (`package/cascade.rs:324-327`).
- The doc comment records the asymmetry that makes this exploitable rather than merely
  wrong: the `Err` arm fails closed (`cascade.rs:219-225`, tag does not move) while a
  successful **under-reporting** answer moves the tag. A hostile mirror does not need to
  fail; it needs to omit a platform.

**Proof:** new test
`package::cascade::tests::orchestration::the_blocker_probe_reads_the_canonical_registry_not_a_mirror`.
The mirror host and the canonical host are seeded with *different* index bodies — the
mirror omits `linux/amd64`, the canonical host carries it — so a mirrored read reports
"nothing blocks". Asserting only that the mirror 404s would have passed for a mirrored
implementation too, via the conservative `Err` arm.

- Red: mutating the call to `ReadAddressing::Mirrored` fails with
  `the probe must see the canonical registry's platform list; ...`.
- Green: restored (`cascade.rs:326` reads `ReadAddressing::Canonical`), `1 test run: 1 passed`.

### 3 — [BLOCK] pinned manifest read verified self-consistency, never identity — **fixed**

`fetch_manifest_raw_bytes_capped` built a `Digest` from the registry-supplied
`Docker-Content-Digest` header and verified the body against *that*. Nothing compared the
served digest to the digest the caller **requested**, so a registry answering
`GET /manifests/A` with B's bytes under B's own header passed every check and the pin
silently resolved to whatever the registry served (CWE-345).

- `crates/ocx_lib/src/oci/client.rs:2075-2093`: when the identifier is digest-addressed, the
  requested digest is compared to the served one before the self-consistency check, and a
  mismatch returns `ClientError::DigestMismatch { expected: <requested>, actual: <served> }`
  — the existing variant, which already classifies to `DataError` (65) and already means
  "the registry served wrong content". No new variant was minted.
- Identity is checked *before* self-consistency deliberately: if you asked for A and got
  B's self-consistent bytes, the identity failure is the security-relevant attribution.
- Tag-addressed reads are unaffected (`identifier.digest()` is `None`).

**Proof:** new test
`oci::client::tests::fetch_manifest_raw_bytes_rejects_a_manifest_served_under_another_digest`.
The stub answers a digest-pinned request with a *different* manifest under that manifest's
*own* correct digest — internally consistent, so `verify_raw_bytes_digest` cannot see it,
and only the requested-vs-served comparison can.

- Red: disabling the new condition returns `Ok(Some((.., "sh.ocx.substituted": "yes", ..)))`
  — the substituted manifest handed back as if it were the pinned one.
- Green: restored, `11 tests run: 11 passed` across the whole `fetch_manifest_raw_bytes` family.

This is the general defence; WP-A is adding the same check at the copy call site.

### 5 — [WARN] `read_target_entries` swallowed every read failure — **fixed**

The blanket `Err(_) => return Ok(entries)` made an auth denial, a 5xx, an SSRF refusal, a
digest mismatch on the target's own index and a malformed index indistinguishable from
"the target has nothing" — every platform then reported `Added` and every
`KeptNotInSource` row vanished (ERR-19).

- `crates/ocx_lib/src/publisher/copy.rs:378-403`: the fetch is now `?`-propagated; only
  `Ok(None)` returns an empty list.
- Genuine absence is unaffected, and this is the part worth stating: the transport folds
  `MANIFEST_UNKNOWN`, `NOT_FOUND` **and `NAME_UNKNOWN`** into `ClientError::ManifestNotFound`
  (`oci/client/native_transport.rs:154-170`), which `fetch_manifest_raw_bytes_capped` maps
  to `Ok(None)`. A target repository that does not exist yet — the first-promotion case —
  therefore still takes the empty-list path and does not become a hard failure.
- The doc comment names why `--dry-run` is where the old behaviour lied loudest: it writes
  nothing, so the "anything worse surfaces on the first write" defence never fires.

**Proof:** new test
`publisher::copy::tests::an_unreadable_target_index_fails_instead_of_reporting_an_empty_target`
— the target's index is present but served under a digest that does not match its bytes,
under `--dry-run`.

- Red: restoring the swallow (`fetched.unwrap_or(None)`) fails the test.
- Green: restored (no `unwrap_or(None)` remains), `1 test run: 1 passed`.

### 6 — [WARN] phase 2 re-fetched a leaf size phase 1 had already measured — **fixed**

`copy_leaf` returns `LeafCopy.size`, documented as "for the index entry's descriptor", and
`run()` read only `.blobs` and `.referrers`. `leaf_size` then issued a full manifest GET per
platform to recompute exactly that number.

- `crates/ocx_lib/src/publisher/copy.rs`: phase 1 now collects
  `copied_leaves: Vec<(Platform, Digest, i64)>`; phase 2 iterates *that* instead of
  `source_leaves`, and `leaf_size` is deleted.
- No new branch was needed: phase 1 `continue`s under `--dry-run` and phase 2 is skipped
  under the same flag, so the producer and the consumer were already gated together.
- Side effect worth stating: phase 2 can now only name a platform in a tag whose content
  phase 1 actually wrote, because the list it walks *is* phase 1's output.

**Proof:** new test `publisher::copy::tests::the_index_entry_carries_the_leaf_manifest_s_real_size`
asserts the pushed index entry's `size` equals the source leaf manifest's byte length — the
load-bearing property, since a client uses that number to bound the manifest read.

- Red: perturbing the captured value (`copied.size + 1`) fails the test.
- Green: restored, `10 tests run: 10 passed` across the whole `publisher::copy` module.

### 9 — `Unchanged` re-verifies: comment sharpened, code unchanged — **no-change-needed (by decision)**

Owner decision: the code stays. `Unchanged` continues to HEAD the blobs, re-PUT the leaf and
re-copy referrers rather than becoming a true no-op.

- `crates/ocx_lib/src/publisher/copy.rs:201-212`: the rationale now names the failure a no-op
  would produce — an index entry proves the *manifest* is present under that digest and says
  nothing about whether every blob it names still is, so a target that was garbage-collected,
  partially pushed, or restored from an incomplete backup carries an entry whose blobs are
  gone, and a skip would report `unchanged` over a package that cannot be pulled. The comment
  states the cost (one HEAD per already-present blob, one deduplicated re-PUT) and says
  outright not to "optimise" it into a skip.
- The comment also records that the ADR and the user docs are being amended to match
  (WP-G owns both; not edited here).

### 8 — [WARN] the primary-plus-cascade tag fold was written twice — **fixed**

Phase 2 built the "primary tag then the rolling tags" set twice: once as a filter
populating `cascade_tags` for the report (with a redundant `tag != primary` guard that
`target_tags` already applies), and once as `std::iter::once(primary).chain(tags)` for the
merge loop (arch F-5).

- `crates/ocx_lib/src/publisher/copy.rs:238-250`: one `merge_tags` list is built, primary
  first; the merge loop walks it and the report takes its tail
  (`merge_tags.iter().skip(1)`). The duplicate `tag != primary` guard is gone —
  `target_tags` already filters the primary out, so the second guard was a second copy of
  one property.
- `.iter().skip(1)` rather than `&merge_tags[1..]`: the slice index would panic on an empty
  list, and the invariant that it is never empty does not belong in an indexing expression.
- Behaviour-preserving refactor; no test changes. `234 tests run: 234 passed` across
  `publisher::copy` and `cascade`.

### 7 — [WARN] `CopyError` could not fill the `--json` envelope — **fixed**

`CopyError` was a two-arm `#[error(transparent)]` wrapper with no discriminant and no
identifier, so `error_envelope.rs`'s `collect_detail` and `collect_context` had nothing to
read and ADR implementation item 6 was undone.

Reshaped into the three-layer form `SignError`/`VerifyError` already use. See
[Handoffs](#handoffs) for the exact API WP-D wires against.

- `run()` now returns `CopyErrorKind`; `Publisher::copy` attaches the two identifiers once
  at the boundary, so every `?` inside `run` stays on the bare kind. That is what kept the
  old type a pass-through: threading two identifiers through each conversion is the cost
  that makes wrapper errors get written flat.
- The three structural refusals stopped being `UsageError::new(format!(...))` strings and
  became named variants; the fourth (`NoMatchingPlatform`) was a `ClientError::InvalidManifest`
  string. **Exit codes are unchanged** — the three usage refusals stay 64, and
  `NoMatchingPlatform` stays 65 (`DataError`), which is what `InvalidManifest` already
  classified to. No CLI contract moved.
- Message wording is preserved verbatim minus the leading `{source}`, which the outer
  `Display` (`copying {source} to {target}`) now supplies — so any assertion on
  `copy the tag instead`, `pass --platform`, `pass exactly one --platform` or
  `offers no platform matching the request` still matches the rendered chain (ERR-06: the
  kind does not repeat what the outer error says).
- `CopyErrorKind::Registry` returns `None` from `CopyError::classify`, deferring to the
  chain walker so a registry 401/503 keeps its own code instead of being flattened —
  the same shape as `SignErrorKind::Internal`.

**Proof:** two new tests.

- `every_copy_error_kind_has_a_frozen_slug_and_an_exit_code` — written as an exhaustive
  `match` rather than a list of asserts, so adding a variant is a compile error rather than
  a silently absent slug. Red on renaming one slug (`platform_required` -> `platform_needed`),
  green on restore.
- `the_failure_kind_is_reachable_by_a_chain_walk` — reproduces what `error_envelope.rs`
  actually does (walk `source()`, `downcast_ref`) and asserts both identifiers. A kind that
  is not on the `source()` path leaves `detail` empty however good the enum is.

`cargo check --workspace` is clean: `crates/ocx_cli/src/command/package_copy.rs` only
`?`-propagates the error, so WP-D's file needed no change to keep compiling.

### 4 — [WARN] `ReadAddressing`'s default was on the unsafe side — **fixed**

`Client::list_tags`, `fetch_manifest` and `fetch_manifest_raw_bytes` delegated at
`ReadAddressing::Mirrored`, so a read that backed a write got the wrong host unless its
author knew to ask. Nothing in a call site's shape reveals that a read is about to decide
a write — findings 1, 2 and 5 are three instances of exactly that miss in one file. The
three short forms now delegate at `Canonical`, and a mirror is asked for by name through
the `*_addressed` variants. Clean rename, no alias, no shim.

**Call sites moved — 20 in total, 13 production and 7 in `client.rs`'s own tests.**
The enumeration is the evidence, so it is written out in full rather than counted.

*Now explicitly `ReadAddressing::Mirrored` — behaviour byte-identical to before:*

| Site | Method | Why a mirror is right here |
|---|---|---|
| `oci/client.rs:1868` `fetch_single_layer_artifact` | `fetch_manifest_raw_bytes` | Pulls an artifact for local use; nothing is written back |
| `oci/index/oci_index.rs:52` | `list_tags` | The registry-backed index's listing, for resolution and cache |
| `oci/index/oci_index.rs:67` | `fetch_manifest` | Same, the manifest behind a resolved tag |
| `oci/index/oci_index.rs:96` | `fetch_manifest_raw_bytes` | Same, verbatim bytes for the local index copy |
| `oci/index/ocx_index.rs:1086` | `fetch_manifest` | Physical leaf fetch behind an `index.ocx.sh` resolve |
| `oci/index/ocx_index.rs:1140` | `fetch_manifest_raw_bytes` | Same, verbatim leaf bytes |
| `managed_config/persistence.rs:259` | `fetch_manifest_raw_bytes` | Fetch-and-apply of an operator config; no registry write |
| `managed_config/persistence.rs:288` | `fetch_manifest_raw_bytes` | The index-entry child of the same fetch |
| `managed_config/persistence.rs:432` | `probe_manifest_digest` | Drift probe for the background refresh |

*Also explicitly `Mirrored`, and these four are the ones the inversion exposes —
each is a read that backs a write, so Invariant #5 argues for `Canonical`:*

| Site | Method | What the answer decides |
|---|---|---|
| `publisher.rs:259` `Publisher::list_tags` | `list_tags` | Callers feed the tags to `push_cascade` — the same defect finding 1 fixed for copy |
| `publisher/publish_gate.rs:136` | `fetch_manifest` | Gates a publish (`verify_any_pin_provenance`) |
| `announce/pipeline.rs:325` `observe_one_tag` | `fetch_manifest_raw_bytes` | The bytes become the published index's record of the tag |
| `announce/pipeline.rs:393` `observe_desc` | `probe_manifest_digest` | The digest is written into the published index |

Behaviour is preserved at all four and each carries a comment saying the mirror is
inherited rather than chosen. Moving them changes the host every push, publish and
announce reads from — a decision with its own blast radius, its own test, and its own
commit. They are listed again under `## Handoffs`; after this change they are greppable
(`rg 'ReadAddressing::Mirrored'`), which is what the inversion buys.

*Now the plain short form, having been explicitly `Canonical`:* `package/cascade.rs:324`
(`fetch_manifest`, finding 2), `publisher/copy.rs:395` (`list_tags`, finding 1),
`publisher/copy.rs:408` and `:476` (`fetch_manifest_raw_bytes`, finding 5).

*Tests moved:* three `*_routes_through_mirror` cases onto the explicit form (their subject
moved, not their assertion), and `probe_manifest_digest_routes_through_mirror` likewise.

**`probe_manifest_digest` has no canonical short form.** All three of its callers want a
mirror, so a canonical wrapper would have had no caller — `cargo check` reported it as
dead the moment the default flipped. CLAUDE.md forbids keeping an unused form, so the
explicit `probe_manifest_digest_addressed` is the only one, and its doc comment says why
it is the odd one out. The next caller must name a host rather than inherit one, which is
stricter than the other three, not looser.

**Proof the default is now pinned, not merely current.** Three renamed tests —
`list_tags_defaults_to_the_canonical_host`, `fetch_manifest_defaults_to_the_canonical_host`
(new), `fetch_manifest_raw_bytes_defaults_to_the_canonical_host` — call the short form on a
client with a configured mirror and assert the transport was handed the *upstream* host with
the repository unrewritten. Their positive control is the `*_routes_through_mirror` trio on
the same client and the same identifier, where naming the host is the only difference.

Red: reverting the three delegations to `ReadAddressing::Mirrored` fails all three
(`3 tests run: 0 passed, 3 failed`). Restore verified by grep at `client.rs:417`, `:470`
and `:2024`, then green: `16 tests run: 16 passed` across the defaults, mirror-routing and
bypass families, and `4587 tests run: 4587 passed` for the crate.

**How the call-site list was built.** Not by grep — `list_tags`, `fetch_manifest` and
`fetch_manifest_raw_bytes` are also method names on `Index`, `IndexImpl` and `Publisher`,
so a textual search cannot tell a `Client` receiver from the others. The short forms were
temporarily renamed and `cargo check --workspace --all-targets` was made to enumerate every
caller by type error, twice: once before the change to find what had to move, and once
after to prove nothing was left behind. The second run named only `cascade.rs`, `copy.rs`
and `client.rs`'s own tests — in particular **zero call sites in `crates/ocx_cli`**, so no
CLI behaviour flipped silently under WP-D.

### 10 — [tests] four declared contracts with no test — **fixed, four tests added**

Each is red-then-green against a mutation that reproduces the defect it guards, and each
restore was verified by grep before the green run rather than assumed.

**a. Duplicate self-heal** — `a_tag_carrying_one_platform_twice_comes_back_carrying_it_once`
(`oci/client.rs`, `mod merge_platform`). Seeds a tag whose index lists `linux/amd64`
twice plus a `linux/arm64` entry, merges `linux/amd64`, and asserts exactly one
`linux/amd64` entry survives carrying the new digest — with `linux/arm64` still present as
the control, since a merge that "healed" by rebuilding the index from one entry would
satisfy the first assertion alone. A duplicated platform makes `select_best` pick by
position, so which binary a user gets would depend on entry order.
Red: `retain` replaced by a `position`-then-`remove` (delete the *first* match only).
Restore grep-verified, green.

**b. Aliased digest** — `two_platforms_sharing_one_leaf_survive_as_two_entries_and_one_canonical_tag`
(`publisher/copy.rs`). One leaf manifest named by two platforms — what a publisher produces
when a build is valid on both, and what a dedup pass produces from two byte-identical
builds. Asserts both platform rows carry that one digest, the target's index ends up with
both entries, and `canonical_tags` has exactly one element. The two halves fail in opposite
directions: collapsing by digest drops a platform, while deriving the canonical tag
per-platform rather than per-manifest reports two tags for one artifact.
Red: `copied_leaves.dedup_by(|a, b| a.1 == b.1)` before phase 2 — the plausible
"optimisation". Restore grep-verified, green.

**c. Phase ordering** — `a_promotion_that_dies_before_the_merge_leaves_every_tag_where_it_was`
(`publisher/copy.rs`). Fails at the phase 1 / phase 2 seam and asserts both halves: the leaf
manifest and its referrer are already at the target, and the target's own tag still holds
byte-identical bytes with no other tag written.

*The plan states this contract backwards, so the test is written against the code.* The
canonical `sha256.<hex>` tag is a **phase 2** write, not a phase 1 one, and it cannot be
anything else: `push_canonical_tag` takes the *merged index* as its subject
(`client.rs:667`), which does not exist until the primary merge has run. Phase 1 is leaves,
blobs and referrers; phase 2 is every tag, canonical included.

*The injection is not the one the finding suggested, and the reason is a stub limitation
worth recording.* Failing the first merge's push cannot be modelled here:
`StubTransport::push_manifest_raw` writes the pushed bytes into `manifests` **before** it
consults `push_results`, so a "failed" push still lands the merged index at the target tag
and the no-tag-moved assertion would fail for a test-double reason. Failing the merge's
*pull* is equally unavailable — `read_target_entries` reads the same key one phase earlier,
so the run would die before phase 1. And the blocker probe cannot carry it either: an `Err`
there is caught and fails the cascade closed (`cascade.rs`), so it suppresses rolling tags
without failing the copy. What is left, and is exact, is `--cascade` to a target tag that is
not a version: `target_tags` refuses on its first line, which is the first statement of
phase 2. It doubles as a pin for a real usage bug — that copy uploads everything before
saying it cannot plan the tags.
Red: hoisting the `target_tags` call above the phase-1 loop — the defect itself — leaves the
leaf absent and fails the test. Restore grep-verified, green.

**d. Target-side blockers** — `a_newer_version_at_the_target_holds_the_rolling_tags_back`
(`publisher/copy.rs`). Two arms sharing every input but the target's own tag list. With
`3.28.2` published at the target and offering the same platform, `cascade_tags` is empty;
without it, `3.28` moves. Promotion is exactly where the two registries' version lists
diverge — a staging registry runs ahead of production — and re-pointing `3.28` at the copied
version would be a downgrade visible only to whoever pulls next.
Red: `has_blocking_platform`'s `Ok(true)` flipped to `Ok(false)`, so the blocker stops
blocking and the first arm sees `["3.28", "3", "latest"]`. Restore grep-verified, green.

### 11 — [HIGH] a mistyped platform blamed the manifest and exited 65 — **fixed**

A `--platform` the source does not publish raised `ClientError::InvalidManifest`, so the
user read `invalid manifest: <source> offers no platform matching the request`. Two defects
in one sentence: it sends the reader to inspect an artifact that is perfectly well formed,
and it withholds the only fact that ends the session — what the source *does* offer. The
exit code followed the same mistake: `InvalidManifest` classifies to 65, while the doc table
says 64.

Fixed as one change, because it was one mistake. `available` is now collected while walking
`index.manifests` **before** the `requested.contains` filter, and the failure names both
sides:

    copying dev.example.com/team/demo:3.28.1 to prod.example.com/team/demo:3.28.1:
    offers no platform matching linux/arm64; available: linux/amd64, darwin/arm64

Exit code is 64. The orchestrator's decision was 64 raised as `UsageError`; raising
`UsageError` here would have gone through `CopyErrorKind::Registry` and thrown away the
`no_matching_platform` slug finding 7 established and WP-D is wiring, so the same decision is
implemented on the kind itself: `NoMatchingPlatform { requested, available }` classifies to
`ExitCode::UsageError`. The slug is unchanged, so nothing downstream moves except the number.

Both fields are pre-joined `String`s rather than typed lists — the only consumer is the
message, and a `Vec<Platform>` cannot be interpolated by `thiserror` without a hand-written
`Display`. The two degenerate cases are named rather than rendered as an empty gap: a request
that named nothing reads `any platform`, and an index that declares no platforms at all
reads `none, the index declares no platforms`.

**Proof:** `a_platform_the_source_does_not_publish_is_a_usage_error_naming_what_it_does`
asks for `linux/arm64` against a source publishing `linux/amd64` and `darwin/arm64` — the
confusable typo, not an invented one. It asserts the kind, `classify() == UsageError`, that
the rendering names the request *and* both offered platforms, and that it does not contain
`invalid manifest`.

Red twice, each half separately. Restoring `ExitCode::DataError` fails this test and
`every_copy_error_kind_has_a_frozen_slug_and_an_exit_code`; dropping `available` from the
`#[error]` string fails only the "must name what is on offer" assertion. Both restores
grep-verified, then green.

### 12 — [HIGH, lib half] a prose sentence had become a wire value — **fixed**

`Disposition`'s `Display` renders `kept (not in source)` — a space and two parentheses —
and the CLI pre-formatted it into a `String`, so `--format json` consumers matched on
English. `test/tests/test_package_copy.py:206` pins it, which makes it a *tested* wire
contract; `subsystem-cli-api.md` "Typed Enums Over Strings" forbids exactly this.

`Disposition` now derives `Serialize` with `#[serde(rename_all = "kebab-case")]`, and the
hand-written `Display` is untouched — the same two-rendering shape as
`crates/ocx_cli/src/api/data/path_kind.rs`. The CLI report struct is WP-D's and was not
touched.

**Serialized variant names — the interface WP-D and WP-E key off:**

| Variant | JSON | Terminal (`Display`, unchanged) |
|---|---|---|
| `Added` | `added` | `added` |
| `Unchanged` | `unchanged` | `unchanged` |
| `Replaced` | `replaced` | `replaced` |
| `KeptNotInSource` | `kept-not-in-source` | `kept (not in source)` |

Only the fourth row moves. The first three were already identical in both renderings, which
is why the drift went unnoticed.

**Proof:** `a_disposition_serializes_as_a_token_and_displays_as_prose` asserts both
renderings for all four, written as an exhaustive `match` so a new variant is a compile
error rather than a wire value nobody chose. Red on `rename_all = "snake_case"` (which
yields `kept_not_in_source`); restore grep-verified, then green.

### Merge — WP-A's engine work integrated at `bd0f604f`

`evelynn` moved to `89c83134` mid-flight. Merged into this branch; one conflict, in
`publisher/copy.rs`'s phase-1 `copy_leaf` call, where WP-A's new `scratch_root` parameter
landed on the same lines this branch had used to sharpen the `Unchanged` re-verify comment
(finding 9). Both survive: the call passes WP-A's `None` and carries both notes.

Two things were checked after resolving rather than assumed, because each is a pair of
changes that had to *coexist* rather than merge:

- **The digest-identity defence is intact at both layers.** WP-A's served-vs-requested
  comparison guards the copy call site in `oci/copy.rs:197`; this branch's general one guards
  every caller from inside `fetch_manifest_raw_bytes_addressed` (`oci/client.rs:2085`).
  Neither is redundant and neither was dropped.
- **`ClientError::TraversalLimitExceeded` still reaches its classification arm.**
  `CopyErrorKind::Registry` returns `None` from `CopyError::classify`, so a registry error
  keeps travelling to the chain walker that reads WP-A's new arm.

WP-A's mechanical additions to `oci/client.rs` — the `#[cfg(test)] pub(crate) use
transport::push_blob_buffered` and the two test-double `push_blob_from_path` impls — merged
without conflict and were kept.

`cargo check --workspace --all-targets` and `cargo nextest run -p ocx_lib` are both green on
the merge commit (4595 passed at the time), and green again after findings 11 and 12
(4597 passed).

## Handoffs

**Finding 7's API, for WP-D's `--json` envelope arm.** In `crates/ocx_lib/src/publisher/copy.rs`:

```rust
pub struct CopyError {
    pub source_identifier: oci::Identifier,
    pub target_identifier: oci::Identifier,
    pub kind: CopyErrorKind,          // #[source]
}
// Display = "copying {source_identifier} to {target_identifier}"

#[non_exhaustive]
pub enum CopyErrorKind {
    IndexNamedByDigest,   // "index_named_by_digest",  exit 64
    PlatformRequired,     // "platform_required",      exit 64
    PlatformAmbiguous,    // "platform_ambiguous",     exit 64
    NoMatchingPlatform,   // "no_matching_platform",   exit 65
    Registry(crate::Error),  // "registry", defers to the chain walker
}
```

`CopyError` implements `ClassifyExitCode` (returns `None` for `Registry` so a 401/503 keeps
its own code) and `CopyErrorKind` implements `ClassifyErrorKind` (`exit_code()` plus a
frozen snake_case `kind_detail()`). For the envelope: `collect_detail` downcasts
`CopyErrorKind` and calls `kind_detail()`; `collect_context` downcasts `CopyError` and reads
`source_identifier` / `target_identifier`. Both are on the `source()` path — a test
(`the_failure_kind_is_reachable_by_a_chain_walk`) pins that, because a kind that is not
reachable by a chain walk leaves `detail` empty however good the enum is.
`crates/ocx_cli/src/cli/classify.rs` already registers `try_downcast!(CopyError)`, so no
change is needed there.

**Four Invariant #5 candidates, now explicit and greppable, not fixed here.** Each is a
read whose answer backs a write but which still addresses a mirror. Behaviour was preserved
deliberately (see finding 4); each carries a comment at the call site saying the mirror is
inherited rather than chosen. All four are outside this work package's file set and each
changes the host a different user-facing operation reads from:

| File | What breaks if a mirror lies |
|---|---|
| `crates/ocx_lib/src/publisher.rs:259` | `Publisher::list_tags` feeds `push_cascade`; a stale mirror walks a rolling tag backwards on **every push** — the exact defect finding 1 fixed for copy |
| `crates/ocx_lib/src/publisher/publish_gate.rs:136` | `verify_any_pin_provenance` gates **every publish** on a dependency's `any` pin |
| `crates/ocx_lib/src/announce/pipeline.rs:325` | The observed bytes become the published index's record of a tag |
| `crates/ocx_lib/src/announce/pipeline.rs:393` | The observed desc digest is written into the published index |

**Two reads with no addressing seam at all.** `Client::fetch_manifest_digest`
(`client.rs:441`) and `Client::pull_description` (`client.rs:1727`) build their reference
from `transport_reference` directly, so neither has an `_addressed` variant and neither took
part in finding 4. `pull_description` matters: `package_copy.rs:144` and
`package_describe.rs:161` read a description in order to *write* it to another repository,
which is Invariant #5. Giving it a `ReadAddressing` parameter is a small change; both call
sites are WP-D's.

**One stale doc claim.** `.claude/rules/subsystem-oci.md:544` still says "reads are
mirror-aware by default (`transport_reference`)". After finding 4 that is false for
`list_tags`, `fetch_manifest` and `fetch_manifest_raw_bytes`: the default is canonical and a
mirror is named. The invariant's *conclusion* is unchanged, only its statement of the
default. Not edited here — the file is outside this work package.

**One test-double gap.** `StubTransport::push_manifest_raw` records the pushed bytes into
`manifests` before consulting `push_results`, so a queued `Err` produces a push that "failed"
and landed. No test can currently model a failed manifest push. Closing it is a two-line
change in `oci/client/test_transport.rs` (consult `push_results` first, store only on `Ok`)
and would let finding 10(c) fail the merge itself rather than the statement before it.
