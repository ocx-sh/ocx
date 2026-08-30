# WP-3 execution report — verify sidecar reads and the Rekor memo

Plan: `.claude/artifacts/plan_issue_sweep_2026-08-30.md`, contracts C-020 … C-026,
scenarios S-007 / S-008. Branch `hex/issue-sweep--wp3`, base `52eecc15`.

## Contracts

| Contract | Verdict | Evidence |
|---|---|---|
| **C-020** `read_sbom_sidecar_tag` returns every layer | **DONE** | `pipeline.rs` `read_sbom_sidecar_tag` now returns `Result<Vec<UnverifiedSbom>, (Option<Digest>, VerifyErrorKind)>` and walks `manifest.layers` through the new `read_every_layer`; the caller `found.extend(sboms)`. Test `a_multi_layer_sbom_sidecar_tag_lists_every_document` (S-007). |
| **C-021** the first-layer doc comment is replaced | **DONE** | The "only the **first** layer is read … cosign never writes a second layer" paragraph is gone; the replacement states why the reader does not assume its producer. Rides C-020's commit, [`4a686702`](../../..). |
| **C-022** `RekorKeyMemo`, keyed on `log_id_hex`, successes only | **DONE** | New `RekorKeyMemo` in `pipeline.rs`; `verify_rekor_set` and `simplesigning_read::logged_entry` both resolve through it. Three tests, all red-proved below. |
| **C-023** the simplesigning path caches trust material | **DONE** | `cache_sidecar_trust_material` called at both sidecar verified-exits inside `scan` (the `.sig` door and the `.att` door). Tests `a_sidecar_verify_caches_the_one_rekor_key_it_resolved` (mechanism, four states) and `a_keyless_sidecar_verify_writes_the_offline_trust_cache` (wiring, S-008). |
| **C-024** the second `.first()`, `read_unverified_referrer` | **DONE — fixed, not left alone** | Verdict and reasoning below. |
| **C-025** the reciprocal doc comment | **DONE** | `read_unverified_layer`'s "reaches the *same* layer" paragraph now says both doors walk all layers through `read_every_layer`. Rides C-020's commit. |
| **C-026** the stale `Scheme::File` comment (WP-4 handoff) | **DONE** | `VerifiedSigner`'s key-mode comment no longer claims `Scheme::File` is the only admitted backend; it names the property that actually holds (every implemented scheme reaches `compile_key_reference` as raw PEM) and says what a KMS backend would have to widen. Landed in commit A, so WP-4 never writes into `pipeline.rs`. |

## C-024 verdict: `:910` is fixed

**An OCI 1.1 referrer manifest can carry more than one payload layer.** It is an
ordinary OCI image manifest with a `subject` field; the image-manifest schema bounds
`layers` below at one and sets no upper bound, and OCX's own `ReferrerManifest::build`
writing exactly one layer is a property of *our writer*, not of the bytes a registry
serves. `crate::oci::referrer::ReferrerManifest::layers` is a `Vec<Descriptor>` with no
arity constraint anywhere on the read path.

So C-021's rationale applies verbatim, the "one payload layer by construction" escape in
the contract does not open, and leaving `:910` alone would have left two readers of one
shape — both funnelling into `read_unverified_layer` — answering differently. Both now
share `read_every_layer`.

Test `a_referrer_with_two_payload_layers_lists_both_documents` asserts both documents by
**content**, because a reader returning the first document twice satisfies a count.

## Budget semantics (open question 1, as resolved)

One `budget.examined()` slot per manifest, unchanged, at both callers; every layer's
bytes charged by `read_unverified_layer`. Pinned by
`a_multi_layer_referrer_still_costs_one_candidate_slot`, which measures a two-layer
referrer **against a single-layer control** rather than against a transcribed number —
the pass opens other doors that also spend slots, so an absolute assertion would drift.

## Red/green proofs

Every mutation was applied by a script that asserts the original text is gone and the
replacement present before the test runs, so a no-op edit cannot be mistaken for a
landed one. Green baseline for all eight tests: `cargo test -p ocx_lib --lib` → **exit 0,
8 passed**.

| # | Mutation (the guard removed) | Test | Exit |
|---|---|---|---|
| 1 | `RekorKeyMemo` keyed on `""` instead of `log_id_hex` — **the C-022 proof** | `the_rekor_memo_answers_each_log_id_with_its_own_key` | **101** — log `bb` was served log `aa`'s key (`AQEB` where `AgIC` was required) |
| 2 | memo lookup disabled (`if false && let Some(...)`) | `one_log_id_is_fetched_once_however_many_candidates_ask` | **101** — 4 fetches against the required 1 |
| 3 | failed fetch cached as an empty PEM | `a_failed_rekor_resolution_is_not_memoized` | **101** — second candidate got `""` |
| 4 | `read_every_layer` back to `layers.iter().take(1)` | `a_multi_layer_sbom_sidecar_tag_lists_every_document` **and** `a_referrer_with_two_payload_layers_lists_both_documents` | **101** — both failed, so both callers demonstrably depend on the shared walk |
| 5 | `budget.examined()` added inside `read_every_layer`'s loop | `a_multi_layer_referrer_still_costs_one_candidate_slot` | **101** — 4 slots against the control's 3 |
| 6 | the `.att` door's `cache_sidecar_trust_material` call deleted | `a_keyless_sidecar_verify_writes_the_offline_trust_cache` | **101** — no cache entry written |
| 7 | `single_key` returns the first of any number | `a_sidecar_verify_caches_the_one_rekor_key_it_resolved` | **101** — two logs produced a guessed cache entry |

Mutation 1 is the one that matters. It reproduces exactly the demotion the contract
names: with a trust root pinning two Rekor logs, an unkeyed memo answers the second log
with the first log's key, and the test reds on the PEM bytes rather than on a count.

Note recorded while writing it: the *fetch*-flavoured half of C-022's rationale (a
hostile layer declaring an unpinned log id, forcing a TOFU fetch, whose key an unkeyed
memo then serves to a pinned log) is **not reachable** as stated, because
`TrustRoot::rekor_public_key_pem_for` falls back to the first pinned key for an unknown
log id — so a trust root holding any key never fetches at all. The demotion is real
through the *rotation* door instead: two pinned logs, two different keys. The
implementation and the mandated test are unchanged by this; only the worked example is.

## Gate

`task rust:verify --force` from inside the worktree, redirected to a log, `$?` read on the
next line: **`GATE_EXIT=0`**, `Summary [109.891s] 6431 tests run: 6431 passed, 8 skipped`.
All eight new tests were confirmed present in that run's log by name. An earlier run
failed `clippy::too_many_arguments` on `scan_simplesigning` (8 args) and was fixed with an
`#[expect(...)]` carrying a reason, matching `verify_one_referrer`'s existing treatment in
the same file.

## Commits

- [`63317181`](../../..) `fix(verify): resolve each Rekor log key once per run, and cache trust material after a sidecar verify` — C-022, C-023, C-026. Closes #374, #319.
- [`4a686702`](../../..) `fix(verify): list every SBOM a cosign .sbom tag or referrer manifest carries, not just the first` — C-020, C-021, C-024, C-025. Closes #386.

Nothing pushed. `CHANGELOG.md` untouched.

## Scope note — one line outside the declared file scope

`crates/ocx_lib/src/oci/verify/attestation_sidecar.rs` gains **one line**: the
`rekor_keys` field on the `SidecarVerification` literal in its test-only `gate()` helper.

`SidecarVerification` is where the run's memo has to live for the simplesigning path —
it already carries `trust_root`, `rekor_url` and `offline`, the exact other inputs of the
resolution — and adding a field to it breaks every literal. The field is an owned,
`Clone`-able, `Default`-able handle precisely so this stayed one line rather than a
signature change rippling through every `gate(...)` call site there.

No work package in this wave owns `attestation_sidecar.rs` (the collisions table claims
`pipeline.rs` for WP-3/WP-4 and `simplesigning_read.rs` for WP-3/WP-10), so this collides
with no concurrent writer. Flagged rather than assumed.

## Deferrals

1. **S-008 end-to-end at the acceptance level.** The unit wiring test drives the `.att`
   sidecar door, not the `.sig` one, because no committed fixture makes a `.sig`-door
   keyless verify *with* a transparency entry constructible: cosign v3.1.1 writes no
   `dev.sigstore.cosign/bundle` annotation on a simplesigning layer
   (`simplesigning_read.rs`'s own tests record this), and the golden bundle's tlog entry
   is a `dsse:0.0.1` entry, so `bind_logged_body` refuses it over a `hashedrekord`
   payload. Building one means minting a Rekor SET with the committed
   `test/sigstore/keys/rekor.key.pem` and hand-writing a `hashedrekord` body — a second
   copy of the log's canonicalisation living in a test. The two exits are the identical
   two-line construct and the helper itself is covered in four states, but the `.sig`
   exit's positive path is proven only by the acceptance suite against a real cosign.
2. **`RekorKeyMemo` is `pub`.** `SidecarVerification` is `pub` and `verify::pipeline` is a
   `pub mod`, so a private memo type failed `private_interfaces`. Its `resolve` and
   `single_key` stay `pub(super)`, so outside `oci::verify` the type can only be
   default-constructed. `ocx_lib` is not a published library, so this is visibility
   bookkeeping rather than surface.
3. **Two logs in one run skip the trust-cache write.** Deliberate and documented on
   `RekorKeyMemo::single_key`: the cache holds one key per Rekor authority. The bundle
   path in the same situation caches whichever candidate verified; that divergence is
   recorded rather than reconciled, since both are best-effort.
