# Review R1 — spec-compliance, `ocx package copy` (post-implementation)

Focus: spec. Phase: post-implementation. Diff: `main...HEAD` (baseline `0ed4a446`).
Design records: `adr_package_copy.md`, `plan_package_copy.md`, `subsystem-oci.md`
Invariant #5, `adr_cascade_platform_aware_push.md` Bug 2, `arch-principles.md`.

Verdict: **needs work**. Coverage: 29/29 (10 WPs + 9 Rust unit contracts + 10
acceptance contracts). Two contradictions against the design record, one
Invariant #5 violation, one wrong exit code in shipped docs, and 5 declared
unit-test contracts absent.

---

## 1. Traceability — decision by decision

### D1 — leaves byte-copied, never rebuilt

**Implementation: correct.** `oci/copy.rs:126-129` reads through
`fetch_manifest_raw_bytes_addressed` and `oci/copy.rs:155-162` writes
`leaf_bytes` verbatim through `push_manifest_raw`. Nothing in the module
constructs an `ImageManifest` or calls `serde_json::to_vec` on a leaf outside
`#[cfg(test)]` (verified by reading the whole 400-line production half).

**Structural guard: absent.** D1 states "a structural guard test asserts the
module never reaches a manifest builder or re-serializes a leaf." No such test
exists — `grep -n "async fn " crates/ocx_lib/src/oci/copy.rs` lists 9 test
functions, none source-text based, and there is no `include_str!`/`file!()` in
either new module.

**And the Rust unit byte-identity test cannot substitute for it.**
`oci/copy.rs:513 leaf_manifest_bytes_survive_the_copy_verbatim` seeds the source
with `serde_json::to_vec(manifest)` (`oci/copy.rs:474`) and compares the target's
bytes against the source's. An implementation that parsed and re-serialised with
the same `serde_json::to_vec` would produce identical bytes, so the test cannot
go red on the exact defect D1 names — the "unchecked green" shape.

**What does discriminate** is the acceptance test
`test/tests/test_package_copy.py:97 test_a_non_canonical_manifest_is_copied_byte_for_byte`,
which publishes a pretty-printed manifest with `layers` before `config` and
asserts the target serves those exact bytes under the same digest. That is a
genuine red-capable proof of D1. So the property is covered; the guard the ADR
promised is not.

### D1 amendment (2026-08-19) — `push_manifest_raw` returns a Location URL

**Matches the code.** `external/rust-oci-client/src/client.rs:1942-1982`:
`push_manifest_raw` returns `extract_location_header(...)`, falling back to a
locally recomputed `sha256_digest(&body)` only when the registry violates the
spec by omitting `Location`. The ocx wrapper
(`oci/client/native_transport.rs:414-427`) passes that `String` through
unchanged. There is therefore no registry-reported digest to compare, exactly as
the amendment says, and the code comment at `oci/copy.rs:150-153` states the same
reasoning. `verify_spooled_blob` (`oci/copy.rs:300-320`) does re-hash every
spooled blob before upload, as the amendment claims.

### D2 — indexes merged per platform, never byte-copied

**Correct.** `publisher/copy.rs:236-249` calls
`Client::merge_platform_into_index` per platform per tag; nothing in either new
module pushes an index body. `oci/copy.rs:136-140` additionally refuses an
`ImageIndex` reached as a leaf, before any write. `adr_cascade_platform_aware_push`
Bug 2 is closed: `merge_platform_into_index` (`oci/client.rs:562`) does
retain-then-insert, and the target-only platforms are re-listed as
`KeptNotInSource` at `publisher/copy.rs:212-220`.

### D3 — rolling tags recomputed from the TARGET

**Correct in source, wrong in addressing.** `publisher/copy.rs:285` lists tags on
`request.target` and feeds them to `resolve_cascade_tags(client, request.target, …)`
at `:286`. The target is the identifier in both places. Acceptance test
`test_package_copy.py:225 test_cascade_is_computed_against_the_target_not_the_source`
proves the blocker actually fires. See finding **A1** for the addressing defect.

### D4 / Invariant #5 — canonical addressing, read site by read site

Each read site individually:

| Site | Call | Addressing | Verdict |
|---|---|---|---|
| `oci/copy.rs:127` | `fetch_manifest_raw_bytes_addressed` (source leaf) | `Canonical` | ok |
| `oci/copy.rs:250` | `read_reference(source, …)` for blob pull | `Canonical` | ok |
| `oci/copy.rs:341` | `read_reference(source, …)` for `list_referrers` | `Canonical` | ok |
| `oci/copy.rs:357` | `fetch_manifest_raw_bytes_addressed` (referrer) | `Canonical` | ok |
| `oci/copy.rs:203/254/342` | `transport_write_reference(target)` | canonical by construction | ok |
| `publisher/copy.rs:294` | `fetch_manifest_raw_bytes_addressed` (leaf size) | `Canonical` | ok |
| `publisher/copy.rs:308` | `fetch_manifest_raw_bytes_addressed` (source resolve) | `Canonical` | ok |
| `publisher/copy.rs:374` | `fetch_manifest_raw_bytes_addressed` (target index) | `Canonical` | ok |
| **`publisher/copy.rs:285`** | **`client.list_tags(target)`** | **`Mirrored`** | **violation — A1** |

`Client::list_tags` (`oci/client.rs:409-411`) hard-codes
`ReadAddressing::Mirrored`. The canonical variant `list_tags_addressed` exists
and is `pub(crate)`, and `ReadAddressing` is already imported at
`publisher/copy.rs:21`.

### Two-phase write order

**Enforced by code, not convention** — `publisher/copy.rs` has two structurally
separate loops: phase 1 at `:185-208` (blobs, leaf, referrers per platform),
phase 2 at `:222-260` (index merges, then canonical tag). A crash in phase 1
cannot move a tag.

**But the ADR's write-order block is now wrong.** `adr_package_copy.md:314-325`
places the `sha256.<hex>` canonical tag in phase 1; the implementation writes it
at `publisher/copy.rs:250-258`, *after* the index merges, because
`push_canonical_tag` derives the platform's leaf digest from the merged index
(`oci/client.rs:648`). See finding **A2**.

### Idempotency — `added` / `unchanged` / `replaced`

**Dispositions: correct.** `publisher/copy.rs:186-190` computes them from the
target's current index entry against the source leaf digest.

**"`unchanged` with ZERO blob/manifest pushes": contradicted.** The ADR
disposition table (`adr_package_copy.md:305`) says `present, same digest` →
"skip blobs, leaf and merge **entirely**". The code does the opposite: only
`dry_run` short-circuits (`publisher/copy.rs:197-199`), so an `Unchanged`
platform still runs `copy_leaf` (unconditional `push_manifest_raw` at
`oci/copy.rs:155-162`, referrer probe + referrer pushes at `:177-179`), still
merges every tag (`:226-249`, no disposition filter), and still writes the
canonical tag. Only blob *bodies* are skipped, via the target HEAD at
`oci/copy.rs:256-259`. See finding **A3**.

### Source-form contract

| Form | ADR requires | Code | Verdict |
|---|---|---|---|
| `repo@sha256:<leaf>` without `--platform` | 64, target never contacted | `package_copy.rs:100-107`, before `ensure_auth` at `:123` | ok |
| `repo@sha256:<leaf>` without `--identifier` | 64 | `package_copy.rs:108-114` | ok |
| `repo@sha256:<index>` | 64 | `publisher/copy.rs:314-321` (`UsageError`) | code 64 ok, **but after** `ensure_auth(target)` — see **A4** |
| `--to` + `--identifier` | clap conflict 64 | `package_copy.rs:23` `conflicts_with = "identifier"` | ok |
| target has no tag | — | `package_copy.rs:116-118` | extra guard, sound |

Classification is wired: `cli/classify.rs:158` adds `try_downcast!(CopyError)`,
and `CopyError::classify` (`publisher/copy.rs:53-58`) delegates per arm — needed
because `#[error(transparent)]` forwards `source()` *past* the inner error, which
the impl's own doc comment states correctly. `publisher/copy.rs:681` pins both
directions (usage → 64, unreachable source → not 64).

### Cascade recomputed from the target's tag list

Correct — see D3. `publisher/copy.rs:287` filters the target's own tag out of the
cascade list so the report never double-counts it.

### Referrers copied recursively, probe against the target, 84

**Correct.** `oci/copy.rs:326-398` recurses with a shared `seen` set (cycle
safe), depth cap `MAX_REFERRER_DEPTH = 8`, count cap
`MAX_REFERRERS_PER_LEAF = 256`, and copies each referrer's own blobs before
pushing it. `ensure_target_serves_referrers` (`:196-215`) probes
`ReferrersApiCapability` against `transport_write_reference(target)` — the target
host — and maps `Unsupported` to `ClientError::ReferrersUnsupported`, which
classifies to 84 (`oci/client/error.rs:261`). The probe runs unconditionally when
referrers are requested, not only when the source has some, and the comment at
`:171-176` gives the right reason. Acceptance
`test_package_copy.py:311` asserts 84 and pairs it with a `--no-referrers` run
that exits 0 — the positive half that proves the 84 comes from the probe.

---

## 2. Convergence — Work Packages

| WP | Verdict | Note |
|---|---|---|
| WP1 stub referrers + blob capture | delivered | `test_transport.rs`: `referrers`, `referrers_unsupported`, `blob_locations`, `blob_location_key`, `referrers_key`; `push_blob_from_path` correctly inherits the trait default |
| WP2 `push_blob_from_path` | delivered | trait default at `transport.rs:186-211`, native `BlobBody::{Memory,File}` override; replay-safety unit-tested at `native_transport.rs:203` |
| WP3 transfer engine | delivered | `oci/copy.rs` |
| WP4 publisher facade | delivered | `publisher/copy.rs` (plan named only `publisher.rs`; the split is an improvement) |
| WP5 CLI leaf + report | delivered | see **A6** on the two-table plain output |
| WP6 `describe --from` | delivered | `package_describe.rs`, `conflicts_with_all` on the field flags, replace-not-merge |
| **WP7 error slugs + classification** | **partial** | `classify.rs` landed; `error_envelope.rs` untouched — see **A5** |
| WP8 second zot | delivered | plus an unrequested third (`prod-registry`, 5004) with a stated cast rationale |
| **WP9 test suites** | **partial** | 5 of 9 declared Rust unit contracts and 3 of 10 acceptance contracts absent |
| WP10 docs + casts | delivered | one doc-script instead of the planned two (`promote__dev-to-staging-to-prod.sh`); one wrong exit-code row — see **A7** |

### Rust unit test contracts

| # | Contract | Verdict |
|---|---|---|
| 1 | source-form violation, `calls` log empty | partial — tests assert no `push_` prefix, never an empty log (`oci/copy.rs:654`, `publisher/copy.rs:671`) |
| 2 | byte identity, compare bytes | delivered but non-discriminating — see D1 above |
| 3 | absent→added / same→unchanged+zero pushes / different→replaced | **contradicts** — no zero-push assertion; `a_second_copy_uploads_nothing` (`oci/copy.rs:560`) asserts only `push_blob:` absence |
| 4 | duplicate self-heal (two `linux/amd64` entries → one) | **missing** |
| 5 | aliased digest (two platforms, one digest, one canonical tag) | **missing** |
| 6 | mount same-registry, not cross-registry | delivered (`oci/copy.rs:586`) |
| 7 | phase ordering under a scripted merge failure | **missing** (and unwritable as stated — the canonical tag is now phase 2, see **A2**) |
| 8 | spooled-blob re-hash mismatch → typed error before upload | **missing** — `verify_spooled_blob` has no test; `a_source_digest_mismatch_stops_the_copy` covers the *manifest* fetch, not the blob spool |
| 9 | cascade blockers read target-side versions | missing at unit level; covered by acceptance `test_cascade_is_computed_against_the_target_not_the_source` |

### Acceptance test contracts

| # | Contract | Verdict |
|---|---|---|
| 1 | same registry, different repo, multi-platform | **missing** — every acceptance case crosses registries |
| 2 | cross-registry multi-platform | partial — no test copies a genuinely multi-platform *source* index; `test_copy_merges_into_the_target_index_instead_of_replacing_it` builds the second platform at the target |
| 3 | byte identity both sides | delivered (two tests, one discriminating) |
| 4 | signature survives | delivered |
| 5 | merge not overwrite + `kept (not in source)` | delivered |
| 6 | second copy after a new platform (`unchanged` + `added`, no re-upload) | **missing** |
| 7 | cascade against the target | delivered |
| 8 | idempotent re-run | delivered |
| 9 | error cases | 7 of 8 delivered; **`--offline` → 81 missing** (the mechanism exists: `context.remote_client()` → `Error::OfflineMode` → `ExitCode::PolicyBlocked`, `error.rs:317`) |
| 10 | description | delivered (both directions) |

Unrequested but sound: `test_a_copied_package_is_installable_from_the_target`,
`test_a_non_canonical_manifest_is_copied_byte_for_byte`.

### WP7 — verified against the code, not the plan's claim

The plan's premise is **half true, and its consequence is overstated**.

- Verified true: `collect_context` (`error_envelope.rs:234-253`) and
  `collect_detail` (`:264-277`) downcast only `SignError`/`VerifyError` and
  `SignErrorKind`/`VerifyErrorKind`. Neither has a `CopyError` arm, and this diff
  does not touch the file.
- Verified false: a copy error does **not** get an "EMPTY detail slug". `detail`
  is `#[serde(skip_serializing_if = "Option::is_none")]` (`:141-142`), so it is
  **omitted**, and the module doc at `:140` documents it as optional.
- Verified overtaken: the part that actually decides scriptable behaviour did
  land elsewhere. `render_error_envelope` derives both `exit_code` and `kind`
  from `crate::app::classify_error` (`:207-208`), not from `collect_detail` — so
  with `try_downcast!(CopyError)` in `cli/classify.rs:158`, a usage refusal
  renders `{"exit_code": 64, "error": {"kind": "usage_error"}}` correctly, with
  no `detail` key.

So the missing `error_envelope.rs` arms leave `CopyError` behaving exactly like
every other non-sign/verify error in the tree (`ClientError`, `PackageError`,
`ProjectError` — none has a `detail` arm either). That is a **scope reduction, not
a defect**: WP7 is `partial`, and whether to add the arm is a design call, not a
bug fix.

---

## 3. Findings

### Actionable

**A1 [Block] `crates/ocx_lib/src/publisher/copy.rs:285` — the target tag listing
that decides which rolling tags get written is mirror-addressed.**
`client.list_tags(...)` resolves to `ReadAddressing::Mirrored`
(`oci/client.rs:409-411`). Its result feeds `Publisher::parse_versions` →
`resolve_cascade_tags`, which decides which tags this command PUTs at the
canonical target. `subsystem-oci.md` Invariant #5 is explicit: "Any read whose
answer decides, gates, or verifies a write must ask for
`ReadAddressing::Canonical`", and names `ocx package cascade check|repair` as the
precedent — `package/cascade/gather.rs:69` uses
`list_tags_addressed(identifier.clone(), ReadAddressing::Canonical)` for exactly
this listing. The module is internally inconsistent about it: the *target index*
read at `publisher/copy.rs:374` is already `Canonical`. A poisoned mirror can
currently choose whether `1.4`/`1`/`latest` move in the production registry.
*Remediation:* replace with
`client.list_tags_addressed(request.target.clone(), ReadAddressing::Canonical).await?`
(both the method and the enum are already in scope at `publisher/copy.rs:21`).
Add a unit test asserting the tag-list read is issued against the canonical host.
(`ocx package push` reaches the same listing through `publisher.list_tags`, which
is the same mirrored path — that call site is pre-existing and out of this diff's
scope, but the same fix applies and should be tracked.)

**A2 [High] `adr_package_copy.md:314-325` vs `crates/ocx_lib/src/publisher/copy.rs:250-258`
— the ADR's write-order block puts the canonical tag in phase 1; the code writes
it in phase 2.** `push_canonical_tag` (`oci/client.rs:642-682`) derives the
platform's leaf digest from the *merged* index, so it cannot run before the
merge as written. Consequence: a crash between the index merge and the canonical
tag leaves a moved tag with no `sha256.<hex>` safety net — recoverable by re-run,
but not what the ADR describes, and it makes the plan's phase-ordering unit
contract (#7) unwritable as stated.
*Remediation:* amend the ADR's write-order block to place the canonical tag after
the primary-tag merge and state why (it is derived from the merged index), then
rewrite unit contract #7 to match: a scripted failure on the *cascade* merge must
leave every leaf, referrer and the primary tag's canonical tag written, and no
rolling tag moved.

**A3 [High] `crates/ocx_lib/src/publisher/copy.rs:197-208` and `:226` — an
`Unchanged` platform is not skipped, contradicting the ADR disposition table.**
`adr_package_copy.md:305` promises "skip blobs, leaf and merge **entirely**", and
the plan's unit contract #3 promises "zero pushes recorded". Actual: only
`dry_run` skips; `Unchanged` still runs `copy_leaf` (one unconditional
`push_manifest_raw`, `oci/copy.rs:155-162`; a referrers capability probe and a
full recursive referrer re-copy, `:177-179`), still merges every tag
(`publisher/copy.rs:226-249` iterates `source_leaves` with no disposition
filter), and still writes the canonical tag. The in-code comment at
`publisher/copy.rs:201-203` argues for re-running the *blob* check, which is a
defensible reason for the blob HEADs — it does not justify the manifest PUT, the
referrer re-copy, or the merge.
*Remediation:* pick one and make record and code agree. Either (a) keep the
current behaviour and amend the ADR row to "re-verify blobs; re-PUT the leaf and
re-merge (idempotent)", dropping the plan's "zero pushes" contract; or (b)
implement the ADR: skip the merge and the referrer copy for `Unchanged`, keep the
blob HEAD sweep, and add the unit test the plan asked for
(`assert!(calls.iter().all(|c| !c.starts_with("push_")))` on the second run).
Option (a) is the smaller change and is what the shipped docs already describe;
option (b) is what the design record says. This needs an explicit decision
because it changes what a re-run costs against a large signed package.

**A4 [Warn] `crates/ocx_cli/src/command/package_copy.rs:122-124` — the target
registry is authenticated before the library-level source-form refusals run.**
`adr_package_copy.md:375` validates "Every source-form violation exits 64 with
the target registry provably never contacted." Two of the four violations are
caught in the CLI before `ensure_auth` (`:100-115`) and satisfy that. The
index-by-digest refusal is raised inside `Publisher::copy`
(`publisher/copy.rs:314-321`), i.e. after `publisher.ensure_auth(&target)` has
already performed a real token exchange (`publisher.rs:107` →
`native_transport.rs:276` → `authenticate`). Acceptance
`test_an_image_index_named_by_digest_is_a_usage_error` asserts only the exit
code, so nothing catches this.
*Remediation:* either move `ensure_auth` to after `resolve_source_leaves` (it is
only needed by phase 1, which runs later), or narrow the ADR validation line to
the two argv-shaped violations it actually holds for. If the first, extend the
index-by-digest acceptance test with the same `not _target_has_tag(...)`
assertion the `--platform` test already carries.

**A5 [Warn] WP7 half-delivered: `crates/ocx_cli/src/error_envelope.rs` is not in
the diff.** `collect_detail` has no `CopyError` arm, so `--format json` copy
failures carry no `detail` key. As analysed above this is behaviourally
consistent with every other error family in the tree and the `kind`/`exit_code`
fields are correct, so it is a scope reduction rather than a bug.
*Remediation:* either give `CopyError` a `kind_detail()` and a `collect_detail`
arm (matching `SignErrorKind`), or strike `error_envelope.rs` from WP7 in the
plan and record that copy errors intentionally emit no `detail` slug. Do not
leave the plan claiming a file the diff never touched.

**A6 [Warn] `crates/ocx_cli/src/api/data/package_copy.rs:88-128` — two
`print_table` calls in one `Printable::print_plain`, and the second has six
columns.** `subsystem-cli-api.md` "Single-Table Rule": "Each
`Printable::print_plain()` impl produce exactly one table. Multiple dimensions →
encode as columns, not separate tables." Plain-Mode Column Budget caps at 5
columns. A sweep of `crates/ocx_cli/src/api/data/*.rs` shows this is the **only**
report in the tree with two tables — no precedent to lean on.
*Remediation:* keep the per-platform table on stdout (it is the result) and move
the summary line to `context.ui().status(...)` on stderr per the Channel Rules
("Receipts and steps-along-the-way are diagnostics → stderr"), or fold `Status`
into the per-platform rows and drop the constant columns. The JSON shape must not
change — the acceptance suite reads `blobs.uploaded`, `status`,
`referrers_copied`.

**A7 [Warn] `website/src/docs/reference/command-line.md` exit-code table — "No
platform in the source matches `--platform` | 64" is wrong; the code exits 65.**
`publisher/copy.rs:333-335` raises `ClientError::InvalidManifest`, and
`oci/client/error.rs:262-273` classifies `InvalidManifest` to
`ExitCode::DataError` (65). No test covers this row, which is how it drifted.
*Remediation:* this is genuinely an invocation fault — the caller named a
platform the source does not offer — so prefer changing
`publisher/copy.rs:333-335` to `UsageError::new(...)` (the `CopyError::Usage`
arm exists for exactly this class per `publisher/copy.rs:27-33`), keeping the
documented 64. Then add the acceptance case; if instead the docs are corrected to
65, add the case anyway so the row is pinned.

**A8 [Warn] `test/conftest.py:113-136` — the `target_registry` fixture skips
without a readiness retry, so the entire copy acceptance suite can vanish
silently.** `pytest_sessionstart` (`:30-68`) waits up to 5 s for
`mirror_registry` but nothing waits for `target-registry`; the fixture calls
`registry_is_reachable` once and `pytest.skip`s. If the third zot binds slowly,
all 14 `test_package_copy.py` cases skip and CI is green — the shape
`subsystem-tests.md` "Unfalsifiable Greens" names ("A whole file skipping itself
away is indistinguishable from a pass"). The identical pattern exists for
`mirror_registry`/`legacy_registry`, but those back one module each, not the
entire acceptance surface of a new feature.
*Remediation:* extend the existing `pytest_sessionstart` retry loop to cover
`target_registry` (and `prod-registry`, which the recorded cast needs), reusing
the 10 × 0.5 s loop already at `test/conftest.py:61-66`.

**A9 [Warn] Five declared Rust unit contracts absent (plan "Test Contracts →
Rust unit" #4, #5, #7, #8, and the zero-push half of #3), plus three acceptance
contracts (#1 same-registry-different-repo, #6 second-copy-after-a-new-platform,
#9's `--offline` → 81).** Contract #8 is the one with real risk: the
`verify_spooled_blob` CWE-345 guard (`oci/copy.rs:300-320`) has no test at all,
so nothing proves it can go red.
*Remediation:* add, in priority order — (1) a `verify_spooled_blob` unit test
seeding `blobs[digest]` with content that does not hash to `digest` and asserting
`DigestMismatch` with no `push_blob:` call recorded; (2) the duplicate-self-heal
and aliased-digest unit tests (both are pure `StubTransportData` seeding, no new
harness); (3) the `--offline` → 81 acceptance case (one `_copy(..., "--offline")`
run). Contracts #1 and #6 are cheap additions to the existing acceptance file.

### Deferred

**D1 [Warn] `crates/ocx_lib/src/oci/client/test_transport.rs` `referrers_key`
(added in this diff) ignores the registry, so a cross-registry copy test whose
source and target share a repository name reads and writes one map.**
`referrers_key` is `format!("{}@{}", image.repository(), subject_digest)` while
its sibling `blob_location_key` deliberately includes the registry with a comment
saying why ("a promotion routinely copies `team/demo` on one host to `team/demo`
on another"). In `oci/copy.rs:699
referrers_are_copied_recursively_and_only_when_asked` source and target are both
`team/demo`, so a referrer pushed to the *target* lands in the same bucket the
*source* is listed from. The test happens to stay correct — it counts
`push_referrer_manifest` calls, and the descriptor list is snapshotted before the
loop — but the harness cannot express "the target got it and the source did not".
*Why a human decides:* making the key registry-scoped is a one-line change with
no production consumer, but it may be deliberate (a referrers index is
repository-scoped in the OCI spec, and the comment argues that case). Someone has
to say whether the stub is modelling the spec or modelling a registry.

