# Autonomous decisions — index sync performance initiative (#330)

Context: on 2026-08-22 the owner put this session into full autonomous mode for this
initiative — no prompting, every open question settled locally and written down. This file
is that record. Each entry states the question, what was checked, and the decision.

Design record: [`adr_index_sync_performance.md`](./adr_index_sync_performance.md).
Plan: [`../state/plans/plan_index_sync_performance.md`](../state/plans/plan_index_sync_performance.md).
Review panel output: `.claude/state/review/index_sync_perf/` (spec, security, architect, sota).

---

## A — Questions the review panel raised that needed a decision, not a fix

### A1. Does a transient index failure change its exit code? (spec F-06, deferred D-1)

**Question.** ADR scenario S-004 asserted a rate-limited index request surfaces as `TempFail`
(75). `crates/ocx_lib/src/oci/index/error.rs:283` gives `IndexHttpFailed → ExitCode::Unavailable`
(69) today, and ADR §10.4 promises "no exit code" change.

**Checked.** Read `error.rs:155-165` (the variant carries `{ url, source }`, no status — so
D-010a's premise holds) and `:275-290` (the classification arm, verbatim).

**Decision: keep 69.** D-010a's typed status exists for the *retry classifier*, not to reclassify
the exit code. Exit codes are the CLI surface other tools `case $?` on; changing one is a
decision requiring a changelog line, and nothing in #330 needs the distinction. S-004's clause was
dropped and C-024 pins the non-regression.

### A2. Ship a retry ladder at an unchanged 512-wide ceiling, before the concurrency measurement? (spec D-2)

**Question.** The plan adds retries while declining to touch concurrency (A9 → #333). Adding an
amplifier before its governor is designed is a sequencing risk.

**Decision: proceed.** The governor is no longer undesigned — the budget's mechanism, floor,
scope and ownership are now decided (D-010 rule 2) and contracted (C-019). The residual risk is
recorded with ADR §7's reversal trigger.

### A3. Does the `RetryPolicy`/`TransportHardening` seam get its own work package? (architect F-001, spec F-03, D-3)

**Question.** WP2 was a struct with no consumer, no contract and no possible failing test, landed
first on the critical path.

**Decision: no — fold it into WP4, its first and only consumer, one commit.** A gate whose passing
state is indistinguishable from its never having run is the "unchecked green" shape at the process
level. §3 A′ needs the seam to *exist*, not to land first. ADR §10.3's wave 0 row was rewritten to
say so.

### A4. Do WP4 and WP5 run concurrently? (architect F-008, spec F-19)

**Question.** Both edit `ocx_index.rs`, ~500 lines apart. Two opus reviewers independently argued
git merges disjoint hunks fine and the serialization costs a wave on the critical path.

**Checked.** `.claude/rules/workflow-swarm.md:235` requires owned files "disjoint across
concurrently-running WPs"; `:241` lists "two concurrent WPs touch the same file" as a named hazard
(*merge conflicts, lost work, non-deterministic outcome*).

**Decision: keep the serialization, fix the justification.** The reviewers are right that git does
not force it; the project's own swarm rule does, and this session has a recorded incident of two
agents sharing a write surface. The plan now says "file-ownership order", cites the rule rather
than git, and states the one-wave cost explicitly. WP6 → WP7 was re-labelled: that edge is a
genuine *logical* dependency and survives even if the file rule were waived.

### A5. Is the published or the derived index in use at the reporting site? (plan open question 3)

**Question.** Unanswerable from here; it re-orders waves 1–2 without changing scope.

**Decision: build both, order as planned.** Wave 0 + wave 1 now contains every fix for the reported
symptom *and* both dominant published-path fixes, so the ordering question no longer gates the
first shippable increment either way.

### A6. Does `utility::singleflight::Group` need an eviction API before D-004/D-005 can coalesce on it? (settled — D-005a)

**Question.** `try_acquire`'s map-hit arm returned a leader's error to every later caller for the
group's lifetime, with no eviction API — flagged during design as a risk D-004/D-005's coalescing
would inherit.

**Checked.** `singleflight.rs:210-214` (the unconditional `Some(Err(e)) => return Err(e)` arm),
`:155-158` (the doc naming retention, offering scoping rather than eviction as the only mitigation),
and cross-referenced against D-004's "any other status caches nothing" rule and
`subsystem-oci.md`'s fail-closed jurisdiction guarantee, both of which a process-lifetime failure
would silently violate.

**Decision: fix eviction-on-read in the shared primitive, and it shipped.** Settled as ADR D-005a
(`adr_index_sync_performance.md:567`): `try_acquire`'s `Some(Err(_))` arm drops the entry and hands
the asking caller fresh leadership, while a waiter already in `wait_for` still receives the leader's
outcome verbatim off its own receiver clone — so exit-code parity for one in-flight operation
survives, and only a *later* operation stops inheriting a resolved failure. Implemented at
`crates/ocx_lib/src/utility/singleflight.rs:237` (landed in `ff346f58`), covered by six tests
(`failed_leader_propagates_error_to_waiters`, `subsequent_acquire_after_failure_returns_a_fresh_leader`,
`failed_key_is_retried_by_a_later_acquire`, `abandoned_key_is_retried_by_a_later_acquire`,
`a_failed_key_holds_no_capacity_slot`, `complete_between_borrow_and_wait_is_caught`). Full rationale
in [`decision_singleflight_error_eviction.md`](./decision_singleflight_error_eviction.md).

---

## B — Corrections the panel forced, recorded because they change the work

| # | Finding | Resolution |
|---|---|---|
| B1 | The CAS gate was specified "inside `persist_dispatch`" — a `pub` function with **four** production callers, two on the ordinary resolve path (`chained_index.rs:727`, `:1675`), which cannot compute the gate (no digest) and whose three-tuple return the callers consume | Gate moved to `refresh_published`, before the call. `persist_dispatch` unchanged. C-027(a) is the contract that catches a regression |
| B2 | Coalescing was aimed at `get_auth_token`, which has exactly one caller (`client.rs:2731`, the header-attach path) — the sync stampede goes `ensure_auth → authenticate → auth() → _auth()` and never reaches it | Coalesce the token **acquisition**; both miss paths route through it. New C-023 covers concurrent cold `ensure_auth`, which no contract did |
| B3 | The fork's cache is one-tier; containerd's is two (host challenge cache under per-scope tokens), so every package's first touch still pays a redundant `GET /v2/` | D-003a adds the host tier **with containerd's purge-on-401**, which is not optional once the challenge is cached. C-025 |
| B4 | `Retry-After` had no upper clamp — attacker-controlled header, `86400` freezes an unattended CI sync for a day | Clamped to 30 s; above it means *stop retrying*. Past-dated and unparseable resolve to zero |
| B5 | Dropping the total deadline was undetectable by any contract — a peer dribbling one byte per `idle_bound − ε` never terminates | Outer cap pinned at **300 s per attempt**; C-028 asserts it with a demonstrable red |
| B6 | `build_index_http_client`'s third fallback arm is a bare `reqwest::Client::new()` — no timeouts, redirects followed (CWE-918/CWE-319), contradicting the function's own comment | Arm deleted (pre-existing defect, fixed in the WP that owns the function). D-011b |
| B7 | D-001 removes an *accidental* per-call token refresh; expiry is a strict `epoch > expiration` with no margin against a 60 s default TTL | 30 s renewal margin (C-029); the three `.expect("Time went backwards")` sites resolve to "expired" |
| B8 | `RegistryAuth::Bearer` early-returning would strand a rotated token, and C-001(a)'s request-count assertion could not see it | Bearer excluded from the early return; C-001(a) restated as a staleness assertion |
| B9 | A12 inferred manifest shape from CAS presence — but the local tree is a distributable artifact, so a hash-correct wrong-shape object is reachable | Decode the bytes (free — already in hand). C-011(d) |
| B10 | "Never `Path::exists()`" had no enforcement, while the neighbouring `JoinSet` rule did | C-030, with both a negative denylist and a positive assertion |
| B11 | Guards cited for `local_index.rs` live in `ocx_cli` and cannot see it | D-008d rewritten to cite the guard that covers the file and to state its actual (weaker) strength; C-026 closes the silence half |
| B12 | The retry inventory was one ladder short, and the missing one (`forge/gitlab.rs`) is a duplicate | Corrected — the duplication is worse than #324 claimed, which strengthens the seam argument |

---

## B2 — Found by the second adversary pass, after the panel

**Disclosure: this pass was not actually cross-model.** It was dispatched to a Codex-capable agent
but that agent performed the analysis itself, in the same model family as the four panel reviewers,
and said so when asked. Its findings are recorded below because they are real and were each
re-verified against source before acceptance — but the *cross-model* property the tier-high flow
calls for was not obtained here. A genuine `codex exec` run against the same documents was launched
afterwards; its findings, if any, are appended to this section.

| # | Finding | Resolution |
|---|---|---|
| B2-1 | `C-026` demanded **zero** `log::warn!`/`log::info!` in two files that already carry 8 between them (`local_index.rs:196/516/873`, `ocx_index.rs:196/1012/1022/1023/1029`). Red before any of this work exists — and the escape hatch a builder would reach for is deleting `ocx_index.rs:1012`'s **yank warning**, a load-bearing publisher signal | Restated as a per-site guard over the measured inventory (1 info + 2 warn / 0 info + 5 warn), each argument asserted individually. The property is "this change adds none", not "this file has none" |
| B2-2 | A partial commit with an **empty** succeeded set still writes a root: `merge_root`'s tail is `(changed \|\| !usable)`, and for a first-sight package `usable == false`, so it writes unconditionally — adopting `repository` with zero tags. The package then shows in `ocx index catalog` pinning nothing, with no tag refreshed | D-008f: `refresh_published` returns the failure **before** `commit_published_root` when the set is empty — an explicit guard, not a reliance on `merge_root`. C-012(a)'s fixture must be a first-sight package or it is green for the wrong reason |
| B2-3 | `refresh_derived`'s `try_collect → collect` reclassifies a total transport failure from **69** to **79**: the `is_empty` gate at `:329` fires before the failure surfaces, so the operator is told "no indexable tag" and the transport error is discarded | D-008g + new contract C-032, asserting the **exit code**. Every other partial-success contract was written against `refresh_published`; the derived half had none |
| B2-4 | No contract could tell a ladder wrapping the whole of `get` from one wrapping only `send()`. Every retry contract fails in phase one; the body loop carries no status. A `send()`-only ladder passes all of them **and** fails on exactly #330's reported shape — a proxy that returns `200`, then resets mid-body | C-016 gains the discriminating case: `200` + partial body + reset on attempt 1, full body on attempt 2, assert complete bytes and 2 requests |
| B2-5 | `fetch_root_document` has a **third** `Ok(None)` — the `serves_registry` early return, which issues no request — while `resolve_root` memoizes under the repository **with no registry component**. A tail-position insert poisons `ns/pkg` with `None` from a `ghcr.io/ns/pkg` call, and the package silently stops resolving through the index for the rest of the process | D-004a: insert only on the two paths that issued the request. C-006 gains edge case (a2) asserting the **request count**, since the return value looks identical |
| B2-6 | A12 costs an extra HEAD on **every leaf tag**, permanently — a leaf never has an `o/` object, so HEAD + GET replaces one GET. On a leaf-heavy registry the performance fix is a regression, invisible to every contract | C-011(a) asserts the total request count (2 vs 1); recorded as an accepted negative in §13 and as a second descope trigger on D-007a |
| B2-7 | `C-027(a)`'s stated red — revert the gate into `persist_dispatch` — **does not compile**, because the digest is not available there, which is the reason for the placement | Relabelled a non-regression guard alongside C-002/C-005/C-015; the red it claimed is already C-010's |

B2-2, B2-3 and B2-5 are the ones that would have shipped index-correctness bugs. All three are in the
class the owner's paranoia clause names — a wrong pin, a wrong exit code, and a read that silently stops
resolving.

---

## B3 — Found by the genuine cross-model pass (`codex exec`, 0.144.1)

Ran against the already-twice-corrected documents. It confirmed **no net-new defect** in read routing,
D-008's transaction/root persistence, or eviction-on-read synchronization, and explicitly excluded the
findings already in B/B2. Three net-new, all in the fork, all verified against source before acceptance:

| # | Finding | Resolution |
|---|---|---|
| B3-1 | **Block.** D-001a excluded only `RegistryAuth::Bearer` from the cache-first return. `Basic` has the identical defect: `_auth()`'s fallback builds `RegistryTokenType::Basic(user, pass)` **from the caller's argument** (`client.rs:915-925`) and `auth()` caches it — so a cache hit serves the *previous* caller's credentials after a rotation | Generalised: the cache may short-circuit only a credential the **registry** minted (a realm-exchanged bearer token). Anything the caller handed in is re-derived every call. The key carries no credential identity and cannot tell one caller's secret from another's |
| B3-2 | The 30 s renewal margin lived only in `TokenCache::get`, but `auth()` returns the freshly minted token straight after `insert` (`client.rs:865-870`) and never reads back — so a token minted with 10 s of life reaches the leader and every coalesced waiter unchecked | D-001e: the margin binds at **acquisition** too. C-029 gains a second half, on a different code path from the `get` half |
| B3-3 | Purge-on-401 copied containerd's narrow `error=`-parameter trigger. A **revoked** token commonly draws a plain `Bearer` challenge with no `error=`, so the host entry survives and the revoked token is reused | Widened to the **first authenticated 401**, purge + one retry (C-020). A second consecutive 401 is exit 80, which is what stops it looping. Also requires a fork change: `validate_registry_response` (`client.rs:2541-2546`) discards the 401's headers, so the `WWW-Authenticate` the retry needs is not currently preserved |

---

## C — Open, and deliberately so

- **A9/A10** (configurable concurrency ceiling) stay in [#333](https://github.com/ocx-sh/ocx/issues/333)
  behind [#324](https://github.com/ocx-sh/ocx/issues/324), with ADR §7's reversal trigger.
- **Worst-case wall time grows** (D-011): a pathological path can run ~15 minutes per document
  where today it fails at 60 s, and the run carries no wall-clock bound by design. Accepted
  knowingly — #330's complaint is failure, not slowness — and disclosed in ADR §13.

---

## D — Observed during execution, not caused by it

**`ocx_lib`'s unit suite is load-sensitive.** Running three `cargo test` processes concurrently on this
machine produced 2–3 failures in `project::mutate::tests` (`remove_binding_with_explicit_group_targets_that_group`,
`remove_binding_without_group_errors_when_ambiguous`) on **both** a work-package branch and the **clean**
tree, and zero failures on a quiet machine or when the module is run in isolation (24/24). The tests use
`tempdir()` so the interference is not a shared path; it is timing. Pre-existing, not introduced by this
work, and recorded because "the pipeline passes" is part of this initiative's definition of done — a
loaded CI runner can reproduce it.

**`task rust:verify` fails its license gate on every branch, including a clean tree.** `hawkeye` is
installed by `taskfiles/rust.taskfile.yml` with an unpinned `cargo install --locked hawkeye`, and
hawkeye **7.0.0** replaced `.licenserc.toml`'s `inlineHeader`/`includes`/`excludes` keys with
`header`/`files`/`git`/`styles`/`rules`. The 6.x config no longer parses, so the gate is a hard
`cannot load config` error with no code change involved. `main` is red on
[the verify-licenses workflow](https://github.com/ocx-sh/ocx/actions/workflows/verify-licenses.yml)
for this reason.

Already owned by [ocx-sh/ocx#332](https://github.com/ocx-sh/ocx/pull/332) ("chore: migrate HawkEye
to v7"). An independent migration produced here was **byte-identical** to #332's `.licenserc.toml`,
which is good corroboration that the migration is right — it was discarded rather than duplicated.

**One delta #332 does not carry, and it is the part that matters for next time:** #332 fixes the
schema but leaves the install unpinned, so the same trap is armed for v8. `.ensure-cargo-tool`'s
status check is only "some version is installed", so an unpinned `cargo install` silently rides a
major bump onto every machine and every CI runner. Pinning `hawkeye` to an exact version — the
convention `install:cargo-about` already uses in the same file — closes it. Verified locally that
the v7 config passes (`488 files, 0 changes`, exit 0), goes red on a stripped header
(`488 files, 1 change`, exit 201), and still excludes `external/**`.

**Consequence for this initiative:** the index-sync feature branch will fail the license gate in CI
until #332 lands. That failure is pre-existing and unrelated to this work; it is not a regression
introduced by these commits.

---

## E — A12 (WP7) descoped, and the evidence

**Decision: A12 — the derived path's HEAD-then-skip — is dropped.** The descope trigger D-007a
recorded in advance fired, and the evidence is conclusive. Verified directly against the fork
rather than taken from a report:

- `external/rust-oci-client/src/client.rs:1048-1052` — `pub async fn fetch_manifest_digest(...) -> Result<String>`.
  The signature returns the digest and nothing else.
- `client.rs:1090-1105` — when the registry omits `Docker-Content-Digest`, the documented fallback
  issues a **second GET**, reads the whole body (`res.bytes().await?`), validates the digest against
  it, and **discards the body**.

So against a non-conforming registry, A12 as specified costs `HEAD` + fallback `GET` (body
discarded) + `fetch_manifest_raw_bytes` `GET` — **two manifest-body GETs for one tag**. That is the
same fetch-discard-refetch shape as [ocx-sh/ocx#314](https://github.com/ocx-sh/ocx/issues/314) and
[#319](https://github.com/ocx-sh/ocx/issues/319), and re-introducing it inside a fix for it is not
acceptable. Surfacing the fallback body requires changing the fork's return type — a **second** fork
change, which D-007a rules out for this item's payoff.

**The arithmetic below was measured, not derived.** A12 was built in full, all six contracts
written, exercised against a real socket, and then reverted per D-007a — so these are observed
request counts from two stub registries differing in one variable, the presence of the
`Docker-Content-Digest` header:

| stub registry | HEADs | manifest-body GETs |
|---|---|---|
| sends `Docker-Content-Digest` | 1 | **1** |
| omits it | 1 | **2** |

**The payoff never justified it anyway, and the arithmetic is worth recording so this is not
re-litigated:**

| tag shape | today | with A12 |
|---|---|---|
| held multi-platform | 1 GET | 1 HEAD — saves the **body**, not the round trip |
| cold tag (absent from `o/`) | 1 GET | 1 HEAD + 1 GET — **+1** |
| leaf (single-platform) | 1 GET | 1 HEAD + 1 GET — **+1, every run, permanently.** Measured across two consecutive runs: a leaf never writes to `o/`, so its gate is cold forever |
| registry omitting `Docker-Content-Digest` | 1 GET | 2 body GETs |

So on a registry whose tags are predominantly single-platform, A12 makes `ocx index sync`
**slower**, and on a non-conforming registry it is strictly worse. It is a win only for held
multi-platform tags on a conforming registry, and even there it saves bytes rather than a round
trip.

**A second, independent descope trigger.** The leaf row above is net-negative *even on a
spec-conforming registry*, so A12 does not become viable merely by fixing the header case. Both of
D-007a's triggers fired, not one.

**One finding survives A12 and is already contracted (C-011(d)).** If a future implementation ever
infers a dispatch object's *shape* from its presence in `o/` rather than decoding it, a
hash-correct **bare platform manifest** in a copied index tree is recorded as a root version — and
the refresh *succeeds* where it must refuse. `decode_index_manifest`, run on the bytes
`read_dispatch_object` already returns, is the only thing preventing that. Keep the contract even
though the item it was written for is gone.

**A test-harness note for whoever revisits this:** `ScriptedSource::fetch_manifest_digest` currently
routes through `fetch_manifest_raw_bytes`, so anything that needs to count HEADs separately from
body GETs must split that counter first.

**What replaces it: nothing, and nothing is needed.** The dominant published-path cost was A3's
dispatch-object skip (landed) and A4/A8's root stampede (landed). A12 was always the smallest item
in the plan. Reopening it requires a fork change that returns the fallback body — worth doing only
if the fork is being touched for another reason anyway.

---

## F — Round-2 review findings, recorded so the next reader does not re-dig the review files

Full detail: `.claude/state/review/index_sync_perf/FIXQUEUE.md`. Every item there was re-verified
by the orchestrator against source, not accepted from the reviewer's report as filed.

| # | Severity | Site | Finding |
|---|---|---|---|
| P1 | **BLOCK** | `oci/transport_policy.rs:66-85` | The retry ladder's classifier only recognises `is_connect()`/`is_timeout()`/a walkable `io::Error`. `h2::Error` carries no `source()` (verified in the locked `h2` crate), so an HTTP/2 `GOAWAY`/`REFUSED_STREAM`/`RST_STREAM` — exactly what a TLS-inspecting proxy or a CDN edge recycling a connection emits — classifies terminal, not retryable. reqwest negotiates h2 by default against `index.ocx.sh`, so the whole retry ladder is inert on the transport #330 most likely hit. |
| A1 | high | `oci/index/chained_index.rs:76` | `is_source_outage` never learned the `SourceFetchFailed` wrapper `broadcast_failure` puts around a coalesced leader's error, so a warm local index plus a down source now propagates a hard transport failure under `--remote` instead of the offline-capable fallback it gave before D-004/D-005's coalescing. |
| A2 | high | `project/project_lock.rs:106-128` | The symlink pre-check guarding `MutationGuard::commit` sits outside the retry loop it now guards, so the TOCTOU window is the whole `CONTENTION_BUDGET` (up to 21 retries) instead of one check — an attacker holding `flock` on `ocx.toml` can swap the symlink mid-loop (CWE-59/367). |
| A8 | warn | `oci/index/local_index.rs:352`, `:442` | Both partial-commit paths run `commit_*(...).await?` before `Err(error)`; if the commit of the survivors itself fails, `?` returns the commit error and silently drops the original transport failure — the operator gets a bare I/O error and is never told which tag caused the partial sync, the one thing D-008 exists to report. |
| A4 | warn | `chained_index.rs:4936`, `:4954` | Both outage-guard tests drive an `UnreachableSource` stub that cannot emit the `SourceFetchFailed` shape A1 introduced — green whether or not A1's fix is present, which is why A1 shipped unnoticed. Needs a guard against a real `OcxIndex` over a failing transport. |
| A5 | warn | `cli/command/index_catalog.rs` | `CATALOG_TAG_CONCURRENCY` is pinned only by an acceptance test, which `task rust:verify` does not run — deleting the constant and its `acquire_owned()` reds nothing on the Rust-only gate. |
| A9 | warn (quality-core rates the shape itself Block-tier) | `oci/index/ocx_index.rs:4530-4533` | `production_source()`'s non-vacuity assertion splits `include_str!(...)` on the *first* occurrence of `"#[cfg(test)]"` and checks the prefix — true in every state of the file, including the truncation bug it claims to catch. An unreachable red. |

**Did not survive verification.** A reviewer reported the fork's test suite as unrunnable — filed
as a blocker. Re-verified directly: the suite runs and discriminates (15/15 green outside a cargo
workspace; deleting the 401 purge at `client.rs:2862` reds two tests; restore confirmed by empty
diff). The actual cause was cargo workspace nesting in agent worktrees — cargo walks past the
fork's own `exclude` up to the top-level manifest — not a defect in the suite or a CI gap.
