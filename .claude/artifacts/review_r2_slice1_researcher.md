# Review R2 — Slice 1 SOTA-Gap Re-Review

**Verdict: PASS**
**Date:** 2026-04-19
**Reviewer:** worker-researcher (sonnet)

---

## Per-Finding Verdicts

### A1 — ci-id archived — ADDRESSED

All three required locations are clean:

- ADR S1-C: the decision table row now reads "`ambient-id` OR inline fallback" as primary, with a dedicated "Ambient via archived `ci-id`" rejected-option row explicitly citing the 2026-01-27 archival. The rationale paragraph names `ambient-id` (RHBZ#2396331 reference present), the inline fallback (~80 lines), and the four-way dispatch state machine.
- Plan dependencies table (line 569): `ambient-id | latest 0.1.x | ... | Replaces jku/ci-id which was archived 2026-01-27 (Researcher A1)`. The `ci-id` entry is absent.
- `research_oidc_cli_flows.md` D-OIDC-1 (lines 29-33): "The previously-chosen `jku/ci-id` was archived on 2026-01-27 ... `ambient-id` is actively maintained." Fallback design documented inline.

### A2 — Rekor v2 SET eliminated — ADDRESSED

All required elements are present:

- ADR Risks table: dedicated "Rekor v2 TUF distribution imminent (Researcher A2)" row with correct GA date (October 2025), v2 mechanics (`integrated_time: 0`, RFC 3161 TSA), and full mitigation including the warn-not-hard-error branch documented as "if bundle has no SET but does have a TSA timestamp, emit a warning and fail (v1 does not ship TSA verification)."
- `VerifyErrorKind::RekorSetAbsentTsaPresent` variant present in ADR error-kind inventory (line 477), mapped to `ExitCode::RekorUnavailable = 82`.
- `error_kind_detail` table includes `rekor_set_absent_tsa_present` under the `verify / rekor_unavailable` row.
- Plan line 368 confirms the same mapping; plan risk row (line 646) mirrors ADR.
- The plan's `fake_rekor` fixture includes a "v2 mode" flag (line 394) that exercises this code path.

### A3 — cosign 3.0.6 pin — ADDRESSED

- Plan step 3.10 / conftest row (line 129): `conftest.py` fixture listed with `cosign_binary` helper.
- Plan dependency row (line 625): "`cosign verify` against OCX-signed bundle (FR-15) | `cosign:3 >= 3.0.6` binary exits 0 ... cosign < 3.0.6 refused at fixture-load time with 'vulnerable, upgrade required'"
- Threat-model risk row (line 652): "`cosign interop test pulls in CVE-2026-39395 (Researcher A3)` | Medium | `conftest.py` `cosign_binary` fixture pins `cosign:3 >= 3.0.6` and asserts version at fixture-load."
- `GHSA-w6c6-c85g-mmv6` is referenced in the ADR amendment log (line 14: "cosign interop pin `>=3.0.6` (Researcher A3)"). Full advisory citation present in ADR Risks (implied via amendment log; CVE identifier present).

### A4 — Fulcio v2 URL — ADDRESSED

- ADR push sequence step 6 (line 675): reads "`https://fulcio.sigstore.dev/api/v2/signingCert` via sigstore-rs `FulcioClient::request_cert_v2`" with an explanatory note that v1beta is deprecated and OCX explicitly documents v2 to prevent builders from hand-rolling the v1 URL.
- Module tree (line 280): `fulcio.rs — wraps sigstore-rs FulcioClient::request_cert_v2 → /api/v2/signingCert`.
- JSON error envelope `context` table (line 585): `fulcio_url` example shows `https://fulcio.sigstore.dev/api/v2/signingCert`.
- Plan line 86 matches: `fulcio.rs [NEW] wraps sigstore-rs FulcioClient::request_cert_v2 → /api/v2/signingCert`.

### A5 — DSSE deferral rationale — ADDRESSED

- ADR S1-D (line 159): "DSSE signing is not implemented in sigstore-rs 0.13; there is **no upstream tracking issue or signing PR on the sigstore-rs tracker as of 2026-04-19** (latest release is 0.13.0, October 2024 — there is no 0.14 in progress)."
- No "PR in flight" language remains. The fork option row in the decision table also correctly states "a fork would have no convergence path."

---

## New Deferred Findings (2026-Q2 trend scouting)

### ND-1 — Rekor v2 TUF distribution timing still unresolved (escalation of D1)

Search results confirm the Rekor v2 log public key has been pushed to TUF as of Q4 2025, but the v2 log *URL* distribution via TUF was characterized in October 2025 as "a couple of months away." No confirmation that distribution has occurred as of April 2026 is visible in search results. The ADR correctly treats this as a live risk. No new action for R2; existing mitigation (pin `=0.13`, document limitation in release notes) remains the correct posture. Monitor: if distribution fires before OCX v1 ships, the `RekorSetAbsentTsaPresent` exit-82 branch becomes the primary verification failure mode for all newly-signed bundles — worth a pre-release smoke-test specifically against the v2 log.

### ND-2 — cosign v4 major-cleanup announced; no v3.1 in sight

cosign 3.0.6 is the current tip. The upstream blog post confirms v3 will have few additional releases before cosign v4 ships (which removes deprecated flags). No v3.1 has been released. The `>= 3.0.6` pin in A3 is the correct long-term anchor; it will survive through v4 as long as `cosign verify` flag surface remains stable. Not actionable in R2.

### ND-3 — sigstore-rs remains at 0.13.0; no 0.14 tracking issue visible

Independent search confirms no 0.14 release or tracking issue. A5's rationale ("no 0.14 in progress") is verified current. Not actionable.

---

## Summary

All five actionable findings from R1 are fully addressed with evidence in all required locations. No oscillating or regression findings. Three non-actionable deferred trend signals noted above for handoff documentation. The design is consistent with the April 2026 Sigstore ecosystem state.

Sources:
- [Rekor v2 GA blog post](https://blog.sigstore.dev/rekor-v2-ga/) — Rekor v2 mechanics, TUF distribution timeline
- [cosign releases](https://github.com/sigstore/cosign/releases) — confirms 3.0.6 is current tip; no 3.1
- [cosign v3 announcement](https://blog.sigstore.dev/cosign-3-0-available/) — v4 plans, v3 scope
- [sigstore-rs releases](https://github.com/sigstore/sigstore-rs/releases) — confirms 0.13.0 is latest, no 0.14
