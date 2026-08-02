# Draft dispositions — #107 and #197

> DRAFT ONLY. No GitHub mutations made. Orchestrator applies these (as issue comments,
> and/or milestone/label changes) at PR time. Doc-only changes for this deferral live in
> `website/src/docs/in-depth/signing.md` (new "Deferred to Future Work" section,
> `#deferred-future-work`) and a corrected overclaim in
> `website/src/docs/reference/command-line.md` (`package sign` no longer claims cosign
> can verify OCX signatures).

## #107 — Rekor v2 migration delta (gated on #194 spike)

**Disposition:** stays open, deferred from milestone 2 ("Signing & Trust v1" / #24).

**Comment to post:**

> Deferred from milestone 2 (Signing & Trust v1, tracked in #24). The #194 spike
> (`.claude/artifacts/research_sigstore_rs_spike.md`) settles the gating question this
> issue asked: sigstore-rs 0.14 ships **no Rekor v2 (tiles) client** — Go/Python/cosign
> have it, Rust does not. Tracked as future work, gated on sigstore-rs adding Rekor v2
> support (or a dedicated hand-rolled effort once the wire format is worth reimplementing
> without upstream help).
>
> v1 rationale: #194 shipped against Rekor v1 (`hashedrekord`) only. `rekor.rs` already
> carries the "v2 deferred" doc-comment this issue asked for. Documented in
> [signing.md § Deferred to Future
> Work](https://github.com/ocx-sh/ocx/blob/main/website/src/docs/in-depth/signing.md#deferred-future-work).
> Not closed into #194 — per the issue's own acceptance criteria, this stays open until a
> Rekor v2 log entry actually signs and verifies.

## #197 — cosign v3 interop suite

**Disposition:** stays open, deferred from milestone 2 ("Signing & Trust v1" / #24) —
**infeasible in v1 regardless of the Step-1 spike**, not merely unscheduled.

**Comment to post:**

> Deferred from milestone 2 (Signing & Trust v1, tracked in #24); tracked as future work
> gated on production Sigstore fidelity landing first.
>
> v1 rationale: #194's sign/verify pipeline is hand-rolled against the in-repo fake
> Fulcio/Rekor/OIDC stack — a custom `ocx-rekor-set-v1` SET payload format
> (`set_signing_payload` in `rekor.rs`) and a single-hop certificate chain, not the public
> Sigstore wire format. `cosign verify` cannot validate an OCX-produced bundle, and
> `ocx package verify` cannot validate a bundle cosign produced against real Sigstore —
> and that stays true even if the Step-1 spike (`cosign verify --trusted-root
> <fake-root.json>`) succeeds, because the *bundle contents* themselves are
> fake-stack-shaped, not the trust-root override. Bidirectional interop becomes buildable
> once OCX emits real Sigstore-format bundles against real Fulcio and Rekor — the same
> production-hardening work #107 and the "Current Limitations" section of signing.md are
> also gated on.
>
> Documented in [signing.md § Deferred to Future
> Work](https://github.com/ocx-sh/ocx/blob/main/website/src/docs/in-depth/signing.md#deferred-future-work).
> Cosign >= 3.0 floor / dropped pre-3.0 compat decision (already recorded in #24 and
> `configuration.md`) is unaffected by this deferral — it describes the *target* version
> once interop is buildable, not a claim that interop works today.
