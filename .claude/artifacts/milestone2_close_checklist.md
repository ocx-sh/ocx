# Milestone-2 close checklist — GitHub actions for PR time

> DRAFT ONLY. No GitHub mutations made. Orchestrator applies these (issue
> comments, checkbox edits, new issues) when opening the
> `feat/signing-and-trust → main` PR. Companion to
> `.claude/artifacts/deferrals_107_197.md` (dispositions for #107/#197) and
> `.claude/artifacts/plan_milestone2_signing_trust_master.md` ("Deferred to
> follow-up" section).

## 1. Tracker #24 — sub-issue checkoff

All sub-issues are resolved (done or explicitly deferred); none are blocked or
outstanding:

- [x] PR #87 — OCI referrers sign + verify (slice 1) — merged (pre-merged base)
- [x] #195 — Test infra: referrers-capable registry — done
- [x] #194 — Sign/verify pipeline via sigstore-rs (slice 2) — done (see deviation
      log `plan_issue194_sigstore_pipeline.md`; TUF-criterion caveat below)
- [x] #106 — Referrers capability cache wiring — done
- [x] #98 — Identity-pinned verify (`[[trust.policy]]`) — done
- [x] #196 — Offline/air-gapped verify + trust-root cache — done
- [x] #99 — Auto-verify on install/pull — done
- [ ] #107 — Rekor v2 delta — **deferred** (stays open; disposition in
      `deferrals_107_197.md`)
- [ ] #197 — cosign v3 interop suite — **deferred** (stays open, infeasible in
      v1 regardless of spike outcome; disposition in `deferrals_107_197.md`)

## 2. #24 "Done when" edit

Current text:

```
## Done when

- All sub-issues closed; PR #87 merged.
- Auto-verify is default-on for policy-scoped packages.
- Website sign/verify cast recorded.
- `task verify` green.
```

Strike the cast line, annotate the sub-issues line (two stay open by design):

```
## Done when

- All sub-issues closed or explicitly deferred (#107, #197 — see below); PR #87
  merged.
- Auto-verify is default-on for policy-scoped packages.
- `task verify` green.
```

Post as an issue comment (do not edit the original body in place — preserve
history) or edit the checklist directly, per repo convention at PR time.

## 3. #194 edits

**Strike the cast items** (Tests & docs #9, Acceptance criteria bullet):

- Tests & docs #9 — `Record the website sign/verify cast (deferred from slice 1
  per followups_oci_sign_verify_slice1.md).` → delete or strike with a note
  pointing at the new follow-up issue (see §5 below).
- Acceptance criteria — `Website cast recorded; task website:build green.` →
  delete or strike the same way.

**TUF-criterion deferral annotation.** The acceptance criterion:

> Verification consults the TUF-distributed trust root; overridable via
> `--trust-root`/env — no hardcoded Fulcio cert / Rekor key.

is met only in the "overridable" half. `TrustRoot::load_embedded` (the actual
TUF-distributed fetch) is stubbed and returns `TrustRootUnavailable` (exit 78);
what shipped is the override seam (`--trust-root`/`--tuf-root`/env) plus the
trust-root cache — never a hardcoded cert, but also never a live TUF fetch.
Comment to post on #194:

> Amending this AC for what shipped: verification does **not** yet consult a
> live TUF-distributed trust root — `TrustRoot::load_embedded` is stubbed
> (exit 78 `TrustRootUnavailable`). What's true: no hardcoded Fulcio
> cert/Rekor key, and the override seam (`--trust-root`/`--tuf-root`/env, plus
> the trust-root cache) is fully wired and required for verify to run. Real TUF
> fetch + refresh is tracked in the production-hardening follow-up (see below),
> not this issue.

## 4. Threat-table caveat text

#24's "Threat coverage (post-milestone)" table, "Compromised registry" row,
currently:

```
| Compromised registry | OCI Referrers API + Sigstore verify | #194 |
```

Add a fidelity caveat (do not claim unqualified production-grade defense):

```
| Compromised registry | OCI Referrers API + Sigstore verify (fidelity caveat: pipeline verified end-to-end against the in-repo fake Sigstore stack only — production Sigstore fidelity, i.e. real Fulcio chain validation, standard Rekor SET, TUF trust root, is deferred; see [signing.md § Deferred to Future Work](https://github.com/ocx-sh/ocx/blob/main/website/src/docs/in-depth/signing.md#deferred-future-work)) | #194 |
```

## 5. Follow-up issue draft — website sign/verify cast

**Title:** Record sign/verify website cast once recordings pipeline has a
referrers-capable registry

**Body:**

> Deferred from milestone 2 (Signing & Trust v1, tracked in #24 / #194). #24's
> "Done when" and #194's acceptance criteria both named a recorded website
> sign/verify cast; de-scoped from milestone-2 close because it cannot be
> produced today.
>
> **Why:** `task recordings:build` runs the doc-script pipeline (`adr_tested_doc_command_mechanism.md`)
> against `registry:2`, which does not implement the OCI 1.1 Referrers API
> (confirmed in #195) — `ocx package sign`/`verify` exit 84 (`ReferrersUnsupported`)
> against it. Recording a working cast additionally needs the in-repo fake
> Sigstore stack (`fake_sigstore.py`) wired into the recordings harness, which
> it is not today (the harness only spawns the registry container, not the
> fake Fulcio/Rekor/OIDC stack).
>
> **Done when:**
> - Recordings pipeline runs against a referrers-capable registry (zot or
>   registry:3), or gains a per-script registry override.
> - `fake_sigstore.py` (or equivalent) is reachable from a recording doc-script.
> - A `# cast: true` doc-script for `ocx package sign` + `ocx package verify`
>   is recorded and embedded in `website/src/docs/in-depth/signing.md` and/or
>   the user guide's Supply-Chain Integrity section.
> - `task website:build` green with the new cast.

## 6. Follow-up issue draft — production Sigstore fidelity hardening

**Title:** Production Sigstore fidelity hardening

**Body:**

> Deferred from milestone 2 (Signing & Trust v1, tracked in #24). Covers the
> three documented-but-untracked gaps in
> [signing.md § Current Limitations](https://github.com/ocx-sh/ocx/blob/main/website/src/docs/in-depth/signing.md#current-limitations),
> which milestone 2 shipped hand-rolled against the in-repo fake Fulcio/Rekor
> stack rather than public-good Sigstore:
>
> 1. **Real Fulcio chain validation** — walk intermediate certificates (today:
>    single-hop, leaf verified directly against the trust-root CA) and check
>    certificate temporal validity (`notBefore`/`notAfter` against the Rekor
>    integrated time; today: not checked).
> 2. **Standard Rekor SET + inclusion proof** — verify the public Rekor
>    canonical wire format (today: Ed25519-verified over OCX's own
>    `ocx-rekor-set-v1` deterministic payload) and the Merkle inclusion/
>    consistency proof (today: only the SET/inclusion-promise is checked).
> 3. **TUF-distributed trust root** — real network fetch + refresh + expiry
>    check of Sigstore's TUF root (today: `TrustRoot::load_embedded` is
>    stubbed; a `--tuf-root`/`--trust-root` JSON is read from disk as-is, never
>    fetched or refreshed).
>
> **Relationship to #107 / #197:** both of those issues are gated on this work
> landing first — #107 (Rekor v2) needs a real wire-format client from
> sigstore-rs, and #197 (cosign v3 interop) is blocked until OCX emits
> real-Sigstore-format bundles against real Fulcio/Rekor (see
> `deferrals_107_197.md`).
>
> **Scope note:** this is production-hardening, not new user-facing surface —
> the sign/verify CLI contract (flags, exit codes) is already stable and
> should not change; only the trust-material fidelity underneath it does.

## 7. PR body — `feat/signing-and-trust → main`

```
Closes #194 #195 #196 #98 #99 #106

Deferred (stay open, tracked separately):
- #107 — Rekor v2 delta (gated on sigstore-rs adding a Rekor v2 client)
- #197 — cosign v3 interop (blocked on production Sigstore fidelity)
- (new) Record sign/verify website cast (gated on recordings pipeline gaining
  a referrers-capable registry) — see §5 above
- (new) Production Sigstore fidelity hardening (real Fulcio chain, standard
  Rekor SET + inclusion proof, TUF trust root) — see §6 above

Known gaps (documented, not tracking issues beyond the above):
- Verification pipeline is proven only against the in-repo fake Sigstore
  stack; production-grade fidelity is the two follow-ups above.
- Auto-verify project-tier `ocx.toml` policies not read on OCI-tier surfaces
  (operator `config.toml` only) — deferred from #99, no dedicated issue yet.
```
