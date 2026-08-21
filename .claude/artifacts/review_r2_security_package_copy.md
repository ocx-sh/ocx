# Review R2 — Security — `ocx package copy` fix loop

Scope: `git diff dfcdcb98..HEAD` (40 files, +4962/-521). Reviewing THE FIXES.
Focus: security. Rules: `subsystem-oci.md` Invariant #5, `quality-rust/security.md`,
`package-manager-domain.md` (PKG-04..PKG-11), `errors.md` (ERR-03/17), CWE-150/345/367/400.

Status: COMPLETE.

## Summary

**Verdict: Needs Work** — no Block. The three headline claims hold: the
`ReadAddressing` default inversion is complete and over-corrects nothing, the
blob spool is bounded on bytes actually written behind an absolute cap, and the
`#[error(transparent)]` classification trap has exactly one instance in the diff
and it is the one already fixed. What is left is that the inversion stopped three
call sites short of finishing the job it started, and one new doc comment claims a
property the function it documents does not have.

| # | Severity | Subject | Class |
|---|---|---|---|
| F1 | High | Publish gate's anti-forgery evidence read from a mirror | Deferred |
| F2 | High | Cascade tag list read from a mirror while its blocker probe went canonical | Deferred |
| F3 | Warn | Announce records mirror-observed bytes into the published index | Deferred |
| F4 | Warn | "Cap trips are errors, never warnings" contradicted in the same function | Actionable |
| F5 | Suggest | Per-blob spool cap with no aggregate byte budget | Deferred |
| F6 | Suggest | Second-layer digest identity checks cannot go red | Actionable (docs) |

Actionable: 2. Deferred: 4. F1 and F2 are High because each is a live CWE-345
path that the diff's own comments identify, name the invariant for, and then
decline to close.

---

## Findings

### F1 [High] — the publish gate's anti-forgery evidence is still read from a mirror

**Where:** `crates/ocx_lib/src/publisher/publish_gate.rs:137-145` (line changed by this diff).

```rust
// Mirrored is inherited, not chosen: this read gates a publish, which
// Invariant #5 says must be decided on the canonical host. ...
let (digest, manifest) = client
    .fetch_manifest_addressed(dependency_identifier, ReadAddressing::Mirrored)
    .await
```

**Invariant broken:** `subsystem-oci.md` Invariant #5 — "Any read whose answer
decides, gates, or verifies a write ... must never name `ReadAddressing::Mirrored`."
This diff's own `ReadAddressing::Mirrored` doc (`oci/client.rs:150-152`) narrows it
further: "Named explicitly, never implied — only for a read whose answer cannot
decide, gate, or verify a write." The call site now *names* the variant its own
docs forbid here.

**Failure scenario.** `verify_any_pin_provenance` is the D5 fail-closed
anti-forgery gate (`publish_gate.rs:107-131`): a dependency pin in a sidecar is a
publisher *claim*, and this fetch is the only registry *evidence* that
contradicts a hand-edited sidecar. Inputs: an operator has
`[mirrors."ghcr.io"]` configured; an attacker (or a stale mirror snapshot) serves,
for `ghcr.io/vendor/dep:latest`, an image index whose entry for
`sha256:<platform-specific-leaf>` declares `platform: {os: "any"}`. Outcome:
`advertised_as_any` is `true`, the gate passes, and `ocx package push` publishes
to the **canonical** registry a bundle whose `any`-platform dependency actually
resolves to a linux/amd64-only leaf. Consumers on other platforms install a
binary for the wrong architecture, and the forged pin is now attested by a
published artifact. The mirror never has to fail — it only has to answer.

**Refutation attempted and failed.** I checked whether the `Err` arm makes this
safe: it does not. `AnyPinProvenanceUnavailable` fails closed only on *fetch
failure*; a successful answer is taken at face value (`publish_gate.rs:148-155`),
which is the identical asymmetry the diff itself documents for
`has_blocking_platform` (`package/cascade.rs:305-315`) and fixed there. I also
checked whether push can reach a mirror-only network: it cannot — the push
itself is canonical (`Client::transport_write_reference`), so reading canonically
adds no reachability requirement the push does not already have.

**Classification: Deferred.** The comment states this was a conscious deferral
("Moving it is a behaviour change to every publish, so it is a decision of its
own"). It needs a human decision on whether to accept a publish-path latency
change; the code change itself is one argument. Reason: human judgment on
whether every `ocx package push` may now require canonical-registry reachability
for dependency provenance.

---

### F2 [High] — the cascade tag list still comes from a mirror while its blocker probe was moved canonical

**Where:** `crates/ocx_lib/src/publisher.rs:285-289` (line changed by this diff),
consumed at `crates/ocx_cli/src/command/package_push.rs:257` and
`crates/ocx_lib/src/managed_config/publish.rs:390`.

```rust
pub async fn list_tags(&self, identifier: oci::Identifier) -> Result<Vec<String>> {
    self.client.list_tags_addressed(identifier, ReadAddressing::Mirrored).await
}
```

**Invariant broken:** Invariant #5, same clause as F1. The doc comment on the
method concedes it: "callers feed these tags to `push_cascade`, which makes it
the same Invariant #5 case the copy path fixed."

**Failure scenario.** `package_push.rs:257` feeds this list into
`Publisher::parse_versions` → `cascade()`, which computes which rolling tags
(`3.28`, `3`, `latest`) to move at the **canonical** registry. Input: a mirror
that omits `3.28.2` from its tag list while the canonical registry has it.
Outcome: `cascade()` sees no newer blocker, and `ocx package push 3.28.1` moves
`latest` at the canonical registry **backwards** onto 3.28.1 — a silent
downgrade of every consumer resolving `latest`. This is precisely the attack the
new test `the_blocker_probe_reads_the_canonical_registry_not_a_mirror`
(`package/cascade.rs:790-846`) was written to prove is closed, and the diff
closed only the second half of it.

**Refutation attempted and failed.** I checked whether `has_blocking_platform`
re-reads the omitted version and catches the omission: it does not. It iterates
`blockers: &[Version]` — a list *derived from the same mirrored tag list*
(`cascade.rs:316`), so a version the mirror never mentioned is never a blocker
and is never fetched canonically. The canonical read added by this diff only
verifies platforms of blockers the mirror already disclosed. I also checked
whether `parse_versions` filters could reject a short list: it only parses, it
does not cross-check.

**Classification: Deferred.** Same reason as F1 — the code states the deferral
explicitly ("Moving it changes every push"). Reason: human judgment on whether
`ocx package push` cascade computation may require canonical tag listing.

---

### F3 [Warn] — announce records mirror-observed bytes into the canonical published index

**Where:** `crates/ocx_lib/src/announce/pipeline.rs:329` (`observe_curated`), `:399` and `:431` (`observe_desc`) — all three changed by this diff. The tag *selection* that feeds them, `list_registry_tags` at `:224-226`, reaches the mirror too, via `Publisher::list_tags` (F2's method).

**Invariant broken:** Invariant #5. The diff's own comments: "these bytes become
the published index's record of the tag, so Invariant #5 argues for Canonical
here."

**Failure scenario.** `observe_curated` reads a tag's manifest bytes through a
mirror and `observe_desc` probes the `__ocx.desc` digest through a mirror; the
observed digests are then written into the index document announced to
`index.ocx.sh`. A mirror serving a stale digest for `latest` publishes an index
entry pointing at a digest the canonical registry no longer advertises under
that tag — an index that resolves consumers to superseded content, signed off as
current.

**Refutation attempted and failed.** I checked whether the announce pipeline
re-verifies against canonical before publishing: `observe_curated`'s result flows
straight into the index record with no canonical cross-check. I also checked
whether `fetch_manifest_raw_bytes_capped`'s new requested-vs-served digest check
helps: it does not — a *tag*-addressed read carries no `identifier.digest()`, so
the new check is inert for exactly this call (`oci/client.rs:2118`).

Warn rather than High because the announce output is an advisory catalog rather
than an install-decision or a signature subject, and a mirror lag window is the
likely realistic cause rather than an attack.

**Classification: Deferred.** Reason: human judgment on whether announce should
observe the canonical host (the diff says "Changing the host announce observes
from is its own decision").

---
### F4 [Warn] — the new "cap trips are errors, never warnings" contract is violated ten lines below, in the same function

**Where:** doc `crates/ocx_lib/src/oci/copy.rs:433-436` (**added by this diff**,
raw-diff line 2331); the contradicting path `crates/ocx_lib/src/oci/copy.rs:471-484`
(pre-existing, unchanged).

The new doc says:

> Both caps are errors rather than warnings. A promotion that logged "stopping
> here" and then exited zero would leave the target holding an artifact whose
> signature was silently dropped: verifiable at the source, unverifiable at the
> target, and reported as a success (PKG-11).

The same function then does exactly that for a different input:

```rust
let Some((bytes, digest, manifest)) = self.client
    .fetch_manifest_raw_bytes_addressed(&referrer_id, ReadAddressing::Canonical).await?
else {
    log::warn!("Referrer {} listed for {subject} but absent; skipping", descriptor.digest);
    continue;
};
```

**Invariant broken:** PKG-11 as the diff itself restates it — a promotion that
drops a referrer must not exit zero.

**Failure scenario.** Input: a source registry whose Referrers API lists
`sha256:<sig>` for the leaf but answers `MANIFEST_UNKNOWN` for that digest (a
GC race, a partially restored backup, or a source that wants the promotion to
look complete). Outcome: `copy_leaf` returns `Ok`, `copied.referrers` under-counts
by one, `CopyReport.referrers_copied` reports the smaller number, exit code 0,
and the target holds an artifact whose Sigstore bundle stayed behind. A CI job
branching on `--format json` sees `"status":"copied"`. The one signal is a
`log::warn!` line, which `--quiet` and every non-TTY log-level default suppress.

**Refutation attempted and failed.** I checked whether the report makes the loss
visible without reading logs: it does not — `referrers_copied` is an absolute
count with nothing to compare it against, and there is no `referrers_expected`
field in `CopyReport` (`crates/ocx_cli/src/api/data/package_copy.rs:112-113`). I
also checked whether a later `ocx package verify` at the target fails closed and
so contains the damage: it does, but only for a consumer who runs it — the
promotion itself still reports success, which is the exact outcome the new doc
comment says is unacceptable.

**Classification: Actionable.** Remediation, smallest form: count the skips and
return `ClientError::TraversalLimitExceeded`'s sibling — or, if skipping must
stay, add a `referrers_skipped: usize` to `LeafCopy`/`CopyOutcome`/`CopyReport`
so the JSON says it, and narrow the doc comment at `copy.rs:433-436` so it no
longer claims a property this function does not have. Note the code path is
pre-existing; the *claim* is what this diff added.

---

### F5 [Suggest] — the blob spool has a per-blob cap but no aggregate cap

**Where:** `crates/ocx_lib/src/oci/copy.rs:60` (`MAX_COPIED_BLOB_BYTES = 8 GiB`),
`:52` (`MAX_BLOBS_PER_MANIFEST = 512`), `:36` (`MAX_CONCURRENT_BLOB_TRANSFERS = 4`),
enforcement at `:553-559` and `:393`.

The team-lead question ("can a hostile manifest declare 500 GB and get it?") is
**answered no**, and correctly: `blob_set` refuses the whole set before any
transfer when any descriptor declares more than 8 GiB, and `spool` bounds the
bytes actually written with `stream.take(declared + 1)` — so the bound is on
bytes written, not on the declared number (PKG-05/PKG-07 satisfied).

What is not bounded is the product. A source declaring four 8 GiB layers of
garbage causes 4 × 8 GiB = 32 GiB to be written under `$OCX_HOME` before the
first digest check fails, and files are only unlinked after a *successful* push
(`copy.rs:352`) — on the failure path they survive until the `TempDir` guard
drops at the end of `copy_leaf`. On a small root filesystem that is a local
denial of service against a machine that merely attempted a promotion.

**Refutation attempted and failed.** I checked whether the existing pull path
sets a precedent that makes this acceptable: it does, and that is why this is
Suggest and not Warn — `Client::pull_layer` bounds only by `layer.size` with no
absolute ceiling at all (`subsystem-oci.md`, "Decompression-bomb caps"). So the
copy path is already stricter than the shipped pull path, and raising this to a
blocker would be holding new code to a bar the neighbouring code does not meet.

**Classification: Deferred.** Reason: human judgment on whether a
per-promotion aggregate byte budget is wanted, and if so whether it should also
be retrofitted to `pull_layer` — that is a cross-subsystem decision, not a local
fix.

---
### F6 [Suggest] — the second-layer digest identity checks in `oci/copy.rs` cannot go red

**Where:** `crates/ocx_lib/src/oci/copy.rs:195-200` (leaf) and `:488-493` (referrer),
both added by this diff; the first-layer check is
`crates/ocx_lib/src/oci/client.rs:2118-2130`, also added by this diff.

**This answers the review question "verify both actually fire and neither is dead
code" — the answer is that the client-layer check fires and the copy-layer one
cannot, as currently wired.**

Trace: `copy_leaf` builds `source_leaf = source.without_tag().clone_with_digest(leaf_digest.clone())`
(`copy.rs:184`), so `source_leaf.digest() == Some(leaf_digest)`. It then calls
`fetch_manifest_raw_bytes_addressed`, whose `_capped` body returns
`Err(DigestMismatch)` whenever `identifier.digest()` differs from the served
digest (`client.rs:2118-2130`). Therefore any value that reaches `copy.rs:195`
already satisfies `digest == leaf_digest`, and the `if` can never be true. The
same argument applies verbatim to the referrer check at `:488` via `referrer_id`
(`copy.rs:470`).

The test `a_manifest_served_under_the_wrong_digest_is_refused`
(`copy.rs:897-943`) is therefore green because of the *client*-layer check, not
the one in the file it lives in. That is `quality-core.md`'s "Unchecked Green":
a guard whose red state is unreachable is indistinguishable from a guard that
was never wired.

**Refutation attempted and failed.** I looked for a path into `copy_leaf` that
does not pin the digest on the identifier — there is none; `source_leaf` is
constructed unconditionally two statements above. I also checked whether a
case-difference between the two `Digest` values could make them compare unequal
while both pass the client check: it cannot, because both sides derive from the
same `leaf_digest` value.

Not a defect: belt-and-braces against a future refactor of the client layer is a
reasonable thing to keep. But it should not be counted as a second independent
control in the ADR or the review record, and if it is kept it wants a comment
saying it is unreachable-by-construction today.

**Classification: Actionable** (documentation only). Remediation: add one line to
each comment stating the check is a backstop that the client layer makes
unreachable, so a later reader does not mistake an always-green branch for
tested coverage.

---
## Verified sound

Named so silence is distinguishable from not-looked-at. Each line states what was
checked and how.

**1. Addressing default inversion (review item 1) — complete, no over-correction.**
I enumerated every production call site of the four short forms whose default
flipped to `Canonical`, by grepping for the exact one-argument shapes:

| Short form | Production callers after the flip | Judgment |
|---|---|---|
| `Client::list_tags` | `publisher/copy.rs:439` (`target_tags`) | decides which rolling tags to move at the target → Canonical correct |
| `Client::fetch_manifest` | `package/cascade.rs:324` (`has_blocking_platform`) | gates a rolling-tag move → Canonical correct (review item 2) |
| `Client::fetch_manifest_raw_bytes` | `publisher/copy.rs:452` (`resolve_source_leaves`), `:535` (`read_target_entries`) | both back the write → Canonical correct |
| `Client::pull_description` | `publisher.rs:258` → `command/package_describe.rs:82`, `:171`, `command/package_copy.rs:167` | all three write back what they read → Canonical correct (review item 7) |

No read that legitimately wants a mirror was left on the short form. Every
mirror-serving path was explicitly re-addressed: `oci/index/oci_index.rs:51,68,99`
and `oci/index/ocx_index.rs:1086,1144` (the whole pull/install path),
`managed_config/persistence.rs:260,289,433`, `patch/persistence.rs:136` (via
`fetch_single_layer_artifact`, `client.rs:1904`), `announce/pipeline.rs`, and the
new `Publisher::pull_description_mirrored` for `command/package_info.rs:78`.
The three that stayed mirrored *and should not have* are F1/F2/F3.

**No air-gap regression.** I checked whether canonical-by-default adds a
reachability requirement: it does not. Every flipped call site sits on a push or
copy path that already writes canonically (`Client::transport_write_reference`,
`merge_platform_into_index` at `client.rs:530`), so the canonical host was
already required for the operation to complete.

**2. Push path not broken by the cascade change (review item 2).**
`has_blocking_platform` (`cascade.rs:316-330`) was not edited — only its doc
comment and a new discriminating test. It reads canonically because
`Client::fetch_manifest` flipped underneath it. Its only caller chain is
`resolve_cascade_tags` ← `push_with_cascade` and `publisher/copy.rs:441`. Both
write canonically. Correct.

**3. Blob spool bound (review item 4) — the absolute cap exists and is enforced
on bytes written.** `blob_set` (`oci/copy.rs:553-559`) refuses the whole blob set
when any descriptor declares more than `MAX_COPIED_BLOB_BYTES` (8 GiB,
`copy.rs:60`) — before any HEAD, PUT or fetch. `spool` (`copy.rs:393`) then bounds
the read with `stream.take(declared.saturating_add(1))` on the *decompressed
reader's* output, so the bound is bytes actually written, not the declared
number (PKG-05, PKG-07). The `+1` is load-bearing and correct: an over-long body
lands in the digest comparison as a genuine mismatch rather than being truncated
to the cap and hashed as if complete. Completeness (`read < declared` →
`ShortBlobRead`) is checked before content (`actual != blob.digest` →
`DigestMismatch`), matching the ordering `subsystem-oci.md` pins for
`pull_layer`. `MAX_BLOBS_PER_MANIFEST` (512) is checked with `saturating_add` and
`Vec::with_capacity` is sized from the *clamped* count, never the raw declaration
(`copy.rs:535-546`, PKG-04). Residual: no aggregate byte budget — F5.

**4. Referrer cap trips cannot return `Ok` (review item 5).** Both caps return
`Err(ClientError::TraversalLimitExceeded)` — depth at `copy.rs:443-450` (function
entry, before any work) and count at `:458-465` (inside the descriptor loop,
before the `seen.insert`). Both propagate through `copy_leaf`'s `?`
(`copy.rs:244`) with no intervening `.ok()`, `let _ =`, or `unwrap_or_default`. I
grepped the whole added diff for those three shapes and found none on a
production path. `ensure_target_serves_referrers` (`copy.rs:276-291`) likewise
returns `Err(ReferrersUnsupported)`. The new variant classifies to
`ExitCode::DataError` (65) via `oci/client/error.rs:312` and is added to
`project/resolve.rs:850`'s match so the arm stays exhaustive. Separate
inconsistency found on a *different* path: F4.

**Referrer depth is off by one against its own doc**, worth a note but not a
finding: the entry guard is `depth >= MAX_REFERRER_DEPTH`, and `copy_referrers`
recurses unconditionally after each push (`copy.rs:517-518`), so a graph with
exactly 8 levels errors on the recursion into the empty 9th rather than
succeeding. Availability-only, and real chains are depth 2.

**5. CWE-150 (review item 6).** `CopyReport::plain_rows` sanitizes both
registry-sourced columns — `platform` and `digest` —
(`api/data/package_copy.rs:236-249`) and `summary()` sanitizes `target` and every
cascade tag (`:190-202`). The `Result` column is rendered from the typed
`Disposition` enum, not from wire text, so it needs none. The only other
registry-sourced text on this path is `CopyErrorKind::NoMatchingPlatform`'s
`available` list (`publisher/copy.rs:107-114`), which reaches the terminal
through the single error boundary at `main.rs:37`, where
`sanitize_for_terminal` is applied to the whole rendered chain and pinned by a
structural test at `main.rs:78`. The sanitizer strips `Cc`, the bidi `Cf` set and
the zero-width `Cf` set (`api/data.rs:169-203`) — dropping ESC neutralizes CSI
and OSC by removing their introducer. Nothing on the copy path writes a raw
registry field to a terminal.

**6. The `#[error(transparent)]` classification trap — hunted, one instance, and
it is the one already fixed.** `CopyErrorKind::Registry` is the only
`#[error(transparent)]` added by this diff (grep over added lines:
`publisher/copy.rs:118`). `CopyError::classify` delegates explicitly
(`publisher/copy.rs:62-68`) instead of returning `None`, which is the fix.
I then verified the delegation actually resolves rather than bottoming out:
`CopyError` is registered in the downcast ladder (`cli/classify.rs:158`,
second entry); `crate::Error::OciClient(e) => e.classify()` forwards
(`error.rs:336`); and `ClientError::classify` opens with `Some(match self {`
and has **no `None` arm** (`oci/client/error.rs:280-281`; a grep for `None` in
that file returns zero). So 79/80/84 survive.
`error_envelope.rs`'s two collectors were the named suspects and are clear:
`collect_context` downcasts `CopyError` itself, which is the chain head;
`collect_detail` downcasts `CopyErrorKind`, which `CopyError::source()` returns
directly via `#[source]` (`publisher/copy.rs:44-45`) — neither walk has to pass
*through* the transparent arm to reach its target. `cli/classify.rs` gained no
new arm in this diff.

**7. Secrets (ERR-17).** Grepped every added line for `token`, `password`,
`secret`, `credential`, `authorization`, `bearer`, `api_key`: five hits, all
prose in comments or the word "token" meaning a JSON discriminant. No new
`Debug` derive holds a credential; `CopyRequest`/`CopyError`/`CopyReport` carry
identifiers, platforms, paths and counts only.

**8. Diff-integrity detectors — all clean.** Over the raw diff of `crates/`
(4,425 lines): zero added `#[allow(`, zero added `#[ignore]`, zero added
`unsafe`, zero new `todo!`. Five added `unimplemented!()`, every one inside a
`#[cfg(test)] mod tests` stub transport. Forty-six added `.unwrap()`/`.expect(`,
every one in test code (verified by attributing each hit to its file and
surrounding `mod tests`). Six removed assertions, every one replaced by a
*stronger* one: `oci/copy.rs:699-720` swapped a self-comparison for a
non-canonical fixture plus an `assert_ne!` proving the fixture can discriminate;
`publisher/copy.rs` swapped three `error.to_string().contains(...)` prose checks
for `matches!(error.kind, CopyErrorKind::...)`; `test_package_copy.py` narrowed
`except Exception` to `except ValueError` and replaced a
`in {"added", "replaced"}` tolerance band with an exact `== "added"`.

**9. `test_transport.rs` did not weaken any test into vacuous truth.** The change
(`test_transport.rs:370-388`) makes `push_manifest_raw` record into `manifests`
only when the queued outcome is `Ok`, which is strictly more faithful. The one
existing test that queues an `Err` into `push_results`
(`package/cascade/apply.rs:614-657`) asserts on the returned `RepairOutcome`
shape and never reads `manifests`, so its truth value is unchanged.

**10. `push_blob_from_path` losing its trait default is safe and an improvement.**
The default read the whole file into memory, which is the allocation the method
exists to avoid. Removing it makes a transport that forgets to stream a compile
error. There is exactly one production implementor — `NativeTransport`
(`native_transport.rs:275`) — every other of the nine impls is inside a
`#[cfg(test)] mod tests`, and the buffering helper they now call
(`transport.rs:283-295`) is itself `#[cfg(test)]`, so production code cannot
reach it. `test_transport` is `#[cfg(test)]`-gated too (`client.rs:107-108`), so
the gating is consistent and does not break a `__testing`-feature build.

**11. `native_transport.rs:723-729` — `usize::try_from` replacing `total as usize`
is correct (PKG-03).** On a 32-bit target the old cast handed the fork a
truncated length; the fork's `while remaining > 0` loop would have uploaded a
prefix and the failure would have surfaced as a rejected committing PUT rather
than as a size fault. The two `unwrap_or` calls are inside the *error*
construction, are documented as unreachable, and are not panics.

**12. Doc claims check out against shipped code (SEC-32).** `--offline` → 81:
`command/package_copy.rs:112` goes through `context.remote_client()?`, which is
`ok_or(Error::OfflineMode)` (`app/context.rs:592`) → `PolicyBlocked` (81). "OCI
Referrers API at both ends, exit 84": the target is probed explicitly
(`oci/copy.rs:276-291`) and the source fails closed too —
`NativeTransport::list_referrers` maps a 404 on `/v2/<name>/referrers/<digest>`
to `ReferrersUnsupported` rather than an empty list
(`native_transport.rs:553-563`). The new dry-run caveat ("`cascade_tags_written`
and `canonical_tags_written` are always empty under `--dry-run`") matches
`publisher/copy.rs:360` where the whole tag phase sits behind `if !request.dry_run`.
No documented control is absent from the code.

**13. Spool root moved off `$TMPDIR` — correct and load-bearing.**
`command/package_copy.rs:129-133` roots the scratch at
`context.file_structure().temp.root()`. On a memory-backed `/tmp` the byte cap
would have bounded the file and not the medium, turning an 8 GiB declared layer
into 8 GiB of RAM. `copy_leaf` still accepts `None` for library consumers and
tests (`oci/copy.rs:164-176`).

**14. Removing the pre-emptive target `ensure_auth` does not leave a write
unauthenticated.** I checked all four write paths reachable from a copy:
`copy_blob` auths Push before the HEAD (`oci/copy.rs:320-322`); the leaf manifest
PUT auths Push (`:220-223`); `merge_platform_into_index` auths Push
(`client.rs:530`); `push_canonical_tag` auths Push (`client.rs:684-686`).
`push_referrer_manifest` is always preceded in the same iteration by
`copy_blobs`, which auths — and `blob_set` always yields at least the config
descriptor, so that path cannot be empty. The claim that the exit-64 source-form
refusals precede any target contact also holds: `run()` calls
`resolve_source_leaves` before `read_target_entries` (`publisher/copy.rs:294` then `:295`).

**15. Uppercase-digest note (not a finding).** `Digest::try_from` accepts
`[A-F]` and preserves case (`oci/digest.rs:224`), while `PartialEq` is derived on
the hex `String`. The new requested-vs-served comparison at `client.rs:2118`
therefore rejects a user-supplied pin written in uppercase against a
spec-compliant lowercase `Docker-Content-Digest`. This fails *closed*, and OCI
descriptor grammar states `[A-F]` MUST NOT be used, so the behaviour is correct
per DATA-DIG-02 — noted only so it is not later mistaken for a bug.

**16. Checked and deliberately not raised as findings (out of diff scope).**
`Client::push_canonical_tag` (`client.rs:699-709`) re-pulls the platform manifest
and re-PUTs it under `sha256.<hex>` without comparing the re-pulled bytes to
`manifest_digest` — unchanged by this diff, and the attacker would have to be the
target registry the copy is writing to. `oci/copy.rs:471-484`'s listed-but-absent
referrer skip is likewise unchanged code; it is raised as F4 only because this
diff added the doc comment that contradicts it.
