# Review R2 — Quality + Diff Integrity — `ocx package copy`

- **Scope**: `git diff dfcdcb98..HEAD` — 39 commits, 40 files, +4962/−521.
- **Focus**: code quality + diff integrity.
- **Rules applied**: `rust-quality/diff-integrity.md`, `rust-quality/reviewing-a-diff.md`,
  `rust-quality/errors.md`, `rust-quality/api-and-idioms.md`, `rust-quality/testing.md`.

> **Tooling note.** `git diff` piped to a filter returns a *reformatted, lossy*
> stream through this environment's proxy — removed lines are dropped entirely,
> so every `^-` detector reports a false NONE. Every detector below was run
> against a raw dump produced with `rtk proxy git diff`, not against a pipe.
> `rg` is emulated and mis-parses `(`; `grep -F` / `sed -n` used for all
> load-bearing line numbers, each re-read before being asserted.

---

## Part 1 — Diff integrity

All nine detectors run. A detector that found nothing says so.

| # | Detector | Result |
|---|---|---|
| 1 | `allow` — `^\+.*#!?\[allow\(` | **Nothing found.** Zero `#[allow]` added. Zero `#[expect]` added either. |
| 2 | `assert` — `^-.*assert` | **8 hits, all justified strengthenings.** Detail below. |
| 3 | `ignore` — `^\+.*#\[ignore\]` | **Nothing found.** No `#[ignore]`; no `pytest.skip`/`xfail`/`skipif` added either. |
| 4 | `stub` — `todo!`/`unimplemented!` | **5 added (`.rs`), all inside `#[cfg(test)] mod tests`.** Not findings. Detail below. |
| 5 | `unsafe` | **Nothing found.** The one textual hit is the word "unsafe" in an artifact heading. |
| 6 | `unwrap` — `^\+.*\.unwrap\(\)` | **15 hits, all inside `#[cfg(test)]`.** No new `unwrap`/`expect` on a library path. |
| 7 | `gate` — taskfiles / `.github/` / `deny.toml` / `clippy.toml` / `rust-toolchain*` / `Cargo.toml` | **Nothing found.** No gate config touched. |
| 8 | `snapshot` — `*.snap` | **Nothing found.** |
| 9 | `lockfile` — `Cargo.lock` | **Nothing found.** No dependency movement. |

### Detector 2 — every removed assertion, adjudicated

| Site | Removed | Verdict |
|---|---|---|
| `oci/copy.rs` `leaf_manifest_bytes_survive_the_copy_verbatim` | `assert_eq!(target_bytes, source_bytes)` + `copied.size == source_bytes.len()` | **Justified — a self-referential assertion replaced by a discriminating one.** The old form compared two values a re-serialising copy would make equal. The new form seeds `to_vec_pretty` bytes and adds an `assert_ne!` proving the fixture differs from what a re-serialising copy emits (TEST-12's "prove it can go red", inline). |
| `oci/copy.rs` referrer-count test | `push_referrer_manifest` call-count assertion | **Justified — replaced by a strictly stronger by-digest presence assertion** on both the SBOM and the transitively-reached signature, with a comment stating why the count alone could not separate depth 2 from depth 1. `copied.referrers == expected` is retained. |
| `publisher/copy.rs` ×3 | `error.to_string().contains("copy the tag instead" / "--platform" / "exactly one")` | **Justified — string matching replaced by `matches!(error.kind, CopyErrorKind::…)`.** ERR-13 (classify on structure, never on `Display` text). |
| `test/tests/test_package_copy.py` ×2 | `dispositions[host] in {"added","replaced"}`; `== "kept (not in source)"` | **Justified — tolerance band removed** (`== "added"`, with a comment naming why the band admitted a wrong build) and the string updated to the new kebab-case enum spelling. |

No assertion was deleted without a stated reason, and none was weakened.

### Detector 4 — every added `unimplemented!`, adjudicated

| File | Line (HEAD) | `#[cfg(test)]` boundary | Verdict |
|---|---|---|---|
| `oci/client.rs` | 3853 | `mod tests` at 2277 | Test double. |
| `oci/client/transport.rs` | 379 | `mod tests` at 322 | Test double. |
| `oci/referrer/capability.rs` | 255 | `mod tests` at 183 | Test double. |
| `oci/verify/pipeline.rs` | 2934, 4221 | `mod tests` at 2036 | Test doubles. |

All five sit past their file's test-module boundary, match the pre-existing
double style in the same file, and carry a message naming why the path is
unreachable. Non-test doubles took the opposite route — `attest/pipeline.rs`,
`sign/pipeline.rs` and `announce/pipeline.rs` implement the new trait method
by delegating to the real `push_blob_buffered` helper rather than panicking.
**Not findings.**

### Red-then-green ordering

The claims in `fix_wp{A,B,D,E,G}_*.md` are specific (named mutation, named
test, named restore-verification method). Three load-bearing restores were
checked against the shipped tree rather than taken on trust:

- WP-A's `MAX_BLOBS_PER_MANIFEST` guard — present, `oci/copy.rs:536`, with the
  cap constant at `:52` and the boundary test asserting `actual == limit + 1`
  (`:1057-1058`).
- WP-A's traversal caps — `MAX_REFERRER_DEPTH` (`:443`) and
  `MAX_REFERRERS_PER_LEAF` (`:458`) both present and both raising
  `TraversalLimitExceeded`.
- WP-B's cascade restore, claimed as "`cascade.rs:326` reads
  `ReadAddressing::Canonical`" — the *literal* claim is wrong (line 324 reads
  `client.fetch_manifest(&blocker_id)`, the short form), but the **effect** is
  right and the code is correct: the same diff inverts the short form's default
  to `Canonical` (`client.rs:475`). Cosmetic inaccuracy in an artifact, not a
  code defect.

No restore was found to have silently failed. Nothing in the shipped tree
contradicts a claimed red run.

### Scope creep

`git diff --shortstat` is +4962/−521 across 40 files, of which 6 are planning
artifacts. The peripheral source files (`managed_config/persistence.rs`,
`project/resolve.rs`, `package/cascade.rs`, `publisher/publish_gate.rs`,
`command/package_info.rs`, `oci/index/*`, four `*/pipeline.rs`) are each a
direct consequence of two deliberate cross-cutting changes — the
`ReadAddressing` default inversion and the newly-required
`push_blob_from_path` transport method. **No unrelated refactor is bundled.**
No formatting-only churn in untouched files.

The diff is large enough that per-line scrutiny does not scale. Reviewed
exhaustively: `oci/copy.rs`, `publisher/copy.rs`, `oci/client.rs` (the
addressing surface), `api/data/package_copy.rs`, `error_envelope.rs`,
`options/referrers.rs`. Reviewed for shape only: the four pipeline test
doubles, the two index modules, the website docs.

---

## Part 2 — Code quality findings

### Q1 — [WARN] `Client::fetch_manifest_digest` is the one tag-addressed read the inversion missed

**Where**: `crates/ocx_lib/src/oci/client.rs:452-454` (re-read against the committed blob, not the dirty worktree: `git show 779c614e:crates/ocx_lib/src/oci/client.rs` then `sed -n '450,456p'`).

```rust
/// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
pub async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<oci::Digest> {
    let ref_ = self.transport_reference(identifier);   // <- Mirrored, by construction
```

**Rule/invariant**: `subsystem-oci.md` Invariant #5 as restated *by this diff*
(`client.rs:140-149`): "reads address the canonical registry by default … a
mirror is asked for by name through the `*_addressed` variants … nothing in a
call site's shape reveals that it is about to back a write."

Every sibling short form was inverted in this diff and gained a two-paragraph
doc note naming the `*_addressed` escape hatch — `list_tags` (`:421`),
`fetch_manifest` (`:474`), `pull_description` (`:1758`), `probe_manifest_digest`,
`fetch_manifest_raw_bytes_capped`. `fetch_manifest_digest` gained neither. It
does not call `read_reference` at all, so `ReadAddressing` cannot express it,
and its doc comment is silent on addressing.

I checked the whole surface rather than this one method: `rg`-free census with
`grep -nF 'self.transport_reference(' crates/ocx_lib/src/oci/client.rs` returns
nine sites. Seven are content-addressed (`pull_manifest`, `pull_blob`,
`pull_layer`, `fetch_layer_blob_capped`) or a probe (`head_blob`) — a mirror is
sound there because the digest is verified. Two are the `ReadAddressing` match
arm itself and `ensure_auth`. **`fetch_manifest_digest` is the only remaining
tag-addressed one** — that is, the only one whose job is to turn a mutable tag
into a digest, which is the highest-value decision to take from the canonical
host.

**Failure scenario**: a later change needs "the digest this tag points at" to
decide a write — a cascade eviction, a canonical-tag emission, a re-point guard.
The obvious call is `client.fetch_manifest_digest(&id)`. It compiles, reads as
the canonical-by-default short form the module doc promises, and silently
returns a *mirror's* answer for a tag; the write then lands canonically. That is
CWE-345/367, and the identical defect this diff fixed in `cascade.rs` and
`publisher/copy.rs::target_tags`.

**Refutation attempted, three ways, all failed to clear it**:
1. *Out of diff scope?* No — `client.rs` is the diff's central file, and the
   asymmetry is created by the inversion, not pre-existing. Before this diff
   every short form meant Mirrored, so the method was consistent with its peers.
2. *Live miscall today?* No, and I say so plainly: the single production caller
   is `OciIndex::fetch_manifest_digest` (`oci/index/oci_index.rs:83`), and the
   index path genuinely wants a mirror — its two siblings in the same file were
   explicitly pinned to `ReadAddressing::Mirrored` by this diff. Today's
   behaviour is correct. What is missing is the guard-rail, which is why this is
   Warn and not Block.
3. *Documented elsewhere?* `grep -nF 'fetch_manifest_digest' crates/` — no doc
   or rule names it as an exception.

**Remediation** (either, cheapest first): add the same two-paragraph doc note
its siblings carry, stating that this one is Mirrored and why; or rename it
`fetch_manifest_digest_addressed(&self, identifier, addressing)` with no short
form at all — the shape `fetch_layer_blob_capped`'s sibling already uses
(`client.rs:1990-1993`: "Unlike its siblings this one has no short,
canonical-by-default form: every caller today wants a mirror, and an unused
wrapper is not kept. The host is therefore always named at the call site").
That precedent exists *in this diff*, which makes the omission a slip rather
than a decision.

**Classification**: Actionable.

### Q2 — [SUGGEST] a doc comment made false by the inversion

**Where**: `crates/ocx_lib/src/oci/client.rs:1865-1868` (re-read against the committed blob, not the dirty worktree; with
`sed -n '1860,1872p'`), on `fetch_single_layer_artifact`:

> "The read goes through the mirror-aware [`Self::transport_reference`] seam,
> **matching every other artifact fetch on `Client`**."

**Invariant broken**: DOC accuracy — a stale help/doc string that contradicts
shipped behaviour. The hunk immediately below it *is* in the diff
(`fetch_manifest_raw_bytes(identifier)` →
`fetch_manifest_raw_bytes_addressed(identifier, ReadAddressing::Mirrored)`), so
the sentence was read past while the line under it was edited.

**Failure scenario**: a maintainer adding a write-backing artifact fetch reads
"matching every other artifact fetch" as licence to keep `Mirrored`, and lands
exactly the defect Invariant #5 exists to stop. Cheap to fix, no behaviour
change.

**Refutation attempted**: is it still true? No. After this diff
`fetch_manifest`, `fetch_manifest_raw_bytes`, `pull_description` and `list_tags`
all default to `Canonical`; this one is now the minority, not the pattern.

**Remediation**: replace the clause with the reason this particular fetch is
mirrored (nothing is written back — the WP-B artifact already states it at
`fix_wpB_addressing_package_copy.md:584`).

**Classification**: Actionable.

### Q3 — [WARN] the diff establishes Invariant #5 and knowingly leaves five write-backing reads on the mirror

**Where** (each re-read with `sed -n`, and the full census is
`grep -rn 'ReadAddressing::Mirrored' crates/ocx_lib/src crates/ocx_cli/src` —
24 hits, of which 8 are `client.rs` tests and 8 are index/managed-config paths
where a mirror is correct):

| Site | Read | Write it backs |
|---|---|---|
| `crates/ocx_lib/src/publisher.rs:287` | `list_tags_addressed(_, Mirrored)` | `push_cascade` re-points rolling tags canonically |
| `crates/ocx_lib/src/publisher/publish_gate.rs:141` | `fetch_manifest_addressed(_, Mirrored)` | gates a publish |
| `crates/ocx_lib/src/announce/pipeline.rs:329` | `fetch_manifest_raw_bytes_addressed(_, Mirrored)` | the bytes become the published index's record of the tag |
| `crates/ocx_lib/src/announce/pipeline.rs:399` | `probe_manifest_digest_addressed(_, Mirrored)` | that digest is written into the published index |
| `crates/ocx_lib/src/announce/pipeline.rs:431` | `pull_description_addressed(_, Mirrored)` | the description is projected into the published index |

**Invariant**: `subsystem-oci.md` Invariant #5, which this diff *authored* into
the rule file (`+` line at `subsystem-oci.md`, diff line 1920): "Any read whose
answer decides, gates, or verifies a write therefore takes the plain short form
and must never name `ReadAddressing::Mirrored`."

**This is not a caught omission — it is a declared one.** Four of the five carry
the literal comment "Mirrored is inherited, not chosen", and each names the
Invariant #5 argument against itself. `publisher.rs:282-284` is the sharpest:
"callers feed these tags to `push_cascade`, which makes it **the same Invariant
#5 case the copy path fixed**. Moving it changes every push."

**Failure scenario** (concrete, for the worst of the five): `ocx package push
--cascade 3.28.1` against a registry with a configured `[mirrors]` entry. The
rolling-tag set is computed from the mirror's tag list. A mirror lagging one
release does not list `3.28.2`, so `latest` and `3` are re-pointed canonically
at `3.28.1` — walking the rolling tags **backwards** onto an older release. This
is byte-for-byte the failure `cascade.rs`'s new
`the_blocker_probe_reads_the_canonical_registry_not_a_mirror` test
(`package/cascade.rs:808-846`) was added to prevent, on the read one call up.

**Refutation attempted**: is `push --cascade` reachable with a mirror
configured? Yes — `[mirrors]` is a documented per-host config with a
`registry` role covering OCI distribution traffic, and nothing scopes it to
read-only commands. Is the copy path affected? No — `publisher/copy.rs:433-443`
(`target_tags`) was fixed to canonical in this diff and has a test.

**Why this is not Block**: the diff's scope is `ocx package copy`, the copy path
is correct, and each remaining site is annotated rather than overlooked. Fixing
them changes the behaviour of `push`, `announce` and the publish gate, which is
a scope decision, not a review call.

**Classification**: **Deferred** — reason: human judgment needed on whether the
Invariant #5 correction extends to the push, announce and publish-gate paths in
this branch or lands as follow-up work. The engine-level answer (the reads are
on the wrong host) is not in doubt; the release-scope answer is.

### Q4 — [BLOCK for the orchestrator, NOT a finding against the diff] the worktree does not build, and HEAD moved during the review

**Not attributable to `dfcdcb98..HEAD`.** Recorded because the orchestrator
needs it before treating any gate result from this worktree as evidence.

`cargo check --workspace --all-targets --locked` fails. Every error is at one
seam, `CopyRequest::scratch_root`:

```
crates/ocx_lib/src/publisher/copy.rs:340:13: expected `Option<&Path>`, found `&Path`
crates/ocx_lib/src/publisher/copy.rs:715:27: expected `&Path`, found `Option<_>`
crates/ocx_lib/src/publisher/copy.rs:807:28: expected `&Path`, found `Option<&PathBuf>`
```

**Cause**: the worktree is dirty and another agent is editing it live.
`git status --porcelain` shows `M crates/ocx_lib/src/oci/client.rs`,
`M crates/ocx_lib/src/oci/copy.rs`, `M crates/ocx_lib/src/publisher/copy.rs`.
The uncommitted hunk flips the field:

```rust
-    pub scratch_root: Option<&'a std::path::Path>,
+    pub scratch_root: &'a std::path::Path,
```

with the test call sites not yet migrated. Two runs one minute apart returned
19 errors and then 4 — the count fell as the other agent progressed, which is
what settles attribution.

**HEAD also moved mid-review.** My assigned scope ended at `779c614e`; HEAD is
now `4ce836f1`, two commits later (`6a95cba5 docs(describe): correct the exit
code…`, `4ce836f1 test(harness): fail loudly when a secondary registry never
starts`). Every finding above and every detector result is against
`dfcdcb98..779c614e`; the two later commits are unreviewed.

**What I did *not* verify, stated plainly**: whether the committed tree at
`779c614e` compiles. I could not, from this worktree, without either
checking out over another agent's in-flight edit or standing up a second
worktree — neither of which is mine to do. The attribution above is strong
(one seam, all three files dirty, error count falling live) but it is not the
same as a green build on the committed tree, and I am not reporting one.

**Classification**: **Deferred** — reason: human/orchestrator judgment on
worktree serialization. One worktree, one concurrent writer; a review and a
fix pass cannot share it. Re-run the gate on a quiesced tree before trusting
any verification claim from this branch.

---

## Checked and clean

Listed so silence is distinguishable from not-looked. Each was a live
hypothesis, investigated, and dropped.

### The `#[error(transparent)]` sibling hunt — clean

The brief named one shipped instance of the class (a `classify()` returning
`None` to defer to a chain walk that transparent had already skipped past) and
asked for siblings. I traced every chain-walking consumer the diff touches or
adds:

- `error_envelope.rs::collect_detail` (`:277-294`) downcasts `CopyErrorKind` on
  the walk. Reachable: `CopyError`'s `kind` is `#[source]` (`publisher/copy.rs:44`),
  so hop 2 of the walk *is* the kind. Not skipped.
- `error_envelope.rs::collect_context` (`:235-266`) downcasts `CopyError` at hop 1.
- `cli/classify.rs::try_classify` (`:158`) downcasts `CopyError` **second in the
  ladder**, ahead of `crate::Error` and `ClientError`, and `CopyError::classify`
  (`publisher/copy.rs:62-68`) delegates explicitly for the `Registry` arm rather
  than returning `None`. That is the fix, and it is in the right place.

I then chased the one way it could still bite: `CopyError::classify` returns
`None` iff the wrapped `crate::Error::classify` does, and `crate::Error`
(`error.rs:314-356`) has exactly two `None` arms — `LayerLayout` and `Shell`.
Under either, the walk would skip `crate::Error` *and* its inner error (both
transparent) and fall through to `Failure` (1). **Not reachable from a copy**:
`run()`'s `?` sites yield `ClientError` (total `classify`, verified at
`oci/client/error.rs:280-318`) or `crate::package::error::Error` (no `None` arm —
`grep -n 'None' crates/ocx_lib/src/package/error.rs` returns nothing). Latent
shape, no reachable defect, so it is not written up as a finding.

### Other hypotheses that did not survive

| Checked | Result |
|---|---|
| Needless `.clone()` added to satisfy the borrow checker (STATE-24) | 25 added clones. All either test code, or forced by an owned-parameter API (`Client::list_tags(identifier: Identifier)`, pre-existing), or across a loop boundary where the borrow genuinely ends (`copied_leaves.push((platform.clone(), …))` while `source_leaves` is still iterated). None where mutating the copy should have been visible to the original. |
| `let _ =` / `.ok()` / `unwrap_or_default()` without a reason (ERR-19) | 13 added. 11 in tests; `oci/copy.rs:352` carries a same-line reason (`// best-effort; the TempDir sweeps it regardless`); `oci/copy.rs:553-559`'s `.ok()` feeds straight into `.ok_or(LayerSizeExceeded)`, so nothing is discarded. Clean. |
| A `Result` both logged and returned (ERR-18) | Two `log::info!` lines added (`command/package_copy.rs:549,551`), both status, neither on an error path. Clean. |
| `#[error("…")]` sentence-case or trailing punctuation (C-GOOD-ERR) | 7 added strings, all lowercase, none punctuated. Clean. |
| Dead code: `leaf_size` deleted, `LeafCopy.size` consumed | `grep -rnF 'leaf_size' crates/` → nothing. `LeafCopy.size` flows `oci/copy.rs:254` → `publisher/copy.rs:345` → `merge_platform_into_index(…, *size, …)` at `:388`. No `#[allow(dead_code)]` anywhere in the touched modules. Claim holds. |
| stdout purity under `--format json` (CLI-01/02) | The receipt goes through `UserInterface::status`, which writes to `printer.cerr()` or `log::info!` (`cli/user_interface.rs:50-61`) — stderr either way. stdout carries only `api().report(&report)`. Clean. |
| Terminal neutralisation of registry text (SEC-31/34) | Every wire-sourced cell routes through `sanitize_for_terminal` (`api/data/package_copy.rs` `plain_rows`, `summary`), with a table-driven test carrying ESC/CSI, `\n`, NUL and U+202E **plus a positive control** proving the assertions cannot pass by emptying the output. |
| A new `ClientError` variant slipping past an exhaustive classifier | `TraversalLimitExceeded` forced arms in both `oci/client/error.rs:309-312` and `project/resolve.rs:853` — the compiler caught it, which is the discipline working. |
| Blob spool path collision under `buffer_unordered(4)` | Real hazard, already closed: `blob_set` (`oci/copy.rs:534-565`) dedups by digest before the fan-out, with the collision spelled out in its doc comment. Referrer blob sets run sequentially after the leaf's, so no cross-set race either. |
| Test/source mixing in one commit | `for c in $(git log --format=%h dfcdcb98..779c614e)` cross-checking each commit's file list for both `test/tests/` and `crates/*/src/` → **zero mixed commits**. Acceptance tests and source never move together, so no commit can have edited a spec to match an implementation. |

---

## Summary

**Verdict: Needs Work** — on Q1 alone (one actionable Warn in the API surface
the diff exists to define). The engine, the error model, the CLI surface and the
test discipline are all in good shape; nothing here is a correctness or security
defect in the copy path.

- Diff integrity: **clean**. Nine detectors, no suppression, no weakened
  assertion, no parked test, no gate edit, no lockfile movement. Every removed
  assertion was replaced by a stronger one. Every added `unimplemented!` is a
  test double.
- Actionable: **2** — Q1 (`fetch_manifest_digest` missed by the addressing
  inversion), Q2 (a doc comment the inversion made false).
- Deferred: **2** — Q3 (five annotated Invariant #5 reads left on the mirror in
  `push`/`announce`/`publish_gate`), Q4 (dirty worktree, moved HEAD, no green
  build obtainable from here).

