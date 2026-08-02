# Codex Cross-Model Plan Review — Slice 2

**Target:** `.claude/state/plans/plan_slice2_external_discovery.md`
**Reviewer:** Codex (via `codex-companion.mjs task --effort high`)
**Date:** 2026-04-19
**Verdict:** **BLOCK**
**Duration:** 5m 17s
**Codex session:** `019da7af-66d1-7822-ab21-f48100346e94`

Codex ran as a one-shot adversarial gate after the in-family Review-Fix Loop converged PASS. The plan is not ready for `/swarm-execute` because several core contracts are internally inconsistent: the offline-cache path cannot satisfy its own acceptance tests, legacy `.sig`/`.att` discovery is specified three different ways, exit-code `83` is declared but not actually locked by tests, and the SBOM parser section leaves key behavior to implementation-time invention. Gate 1 / Gate 3 currently cannot yield unambiguous pass/fail results.

Summary: 5 Actionable · 0 Deferred · 3 Trivia dropped.

---

## Actionable findings

### C-S2-1: Offline-cache contract is self-contradictory

- `plan_slice2_external_discovery.md:432` — `sbom_context()` should fail only when `remote_client.is_none()` AND the referrer cache is cold.
- `plan_slice2_external_discovery.md:650` — requires `remote_client=None` to error before dispatch.
- `plan_slice2_external_discovery.md:804` — implementation unconditionally errors.
- Conflicts with required acceptance case `test_sbom_offline_with_cache_succeeds` at `plan:678`.
- As written, the command cannot both support offline cache hits and fail before dispatch. **Must be resolved** — which is the real contract?

### C-S2-2: Legacy fallback-tag and `.att` discovery are specified inconsistently

- `plan:274` probes legacy signatures as `sha256-<digest>.sig`.
- `plan:992` says digest-addressed subjects must skip that probe "by construction".
- Research says the probe is exactly `sha256-{digest}.sig` at `research_cosign_sigstore_notation.md:73,80`.
- ADR uses `<tag>.att` at `adr_oci_referrers_discovery_v2.md:451`, while research says `sha256-{digest}.att` at `research_oci_referrers_2026.md:140`.
- Stub enum omits any `AttTag` method at `plan:232`, even though the ADR cache schema includes it at `adr:160`.
- Digest-pinned refs end up with undefined discovery behavior. Must be normalized before implementation.

### C-S2-3: Exit-code `83` and cache-corruption handling are not locked by executable tests

- Exit-code rule reserves `ReferrersUnsupported = 83` at `quality-rust-exit_codes.md:85`.
- ADR activates it for Branch 4b fallback failure at `adr:483`.
- Phase 3 test inventory only covers the success side of Branch 4b at `plan:576`; error-envelope tests stop at 65/74/80 at `plan:655`; acceptance table never exercises `83` at `plan:1005`.
- Cache corruption taxonomy is inconsistent: `plan:470` lists corrupt referrer cache under exit `65`, while `plan:989` and `plan:1059` say it is debug-only and should not affect exit code. Must resolve to one contract.

### C-S2-4: SBOM parser section is not executable as written

- CycloneDX contract says pre-parse `specVersion` and only accept `1.3/1.4/1.5` at `adr:193` and `plan:994`.
- But the implementation step falls back to crate-driven parsing plus "unknown discriminators" at `plan:746` — which defeats the pre-parse guard.
- SPDX: tests pin `spdx_rs::models::SPDX::from_str` at `plan:995`; implementation says API "TBD" and offers multiple alternatives at `plan:750`.
- ADR's `serde-spdx` contingency exists at `adr:183`, but the phase plan never adds a gate, dependency, or test path for actually taking that branch.
- `plan:762` introduces recursive in-toto unwrap with no depth bound; Phase 3 never defines a recursion/DSSE test for it.

### C-S2-5: Mixed-format verify JSON contract is missing from the stub surface

- ADR requires `ocx verify` JSON to emit all successful candidates under `signatures[]` when both v0.3 and legacy signatures are present at `adr:268`.
- Acceptance test expects that array and `signature_count=2` at `plan:692`.
- But the only explicit Phase 1 surface change is a singular `VerifyResult` with scalar `signature_format` and `discovery_method` fields at `plan:293`.
- Gate 1 cannot review the real mixed-format report contract — plan never says where `signatures[]` lives or how it reaches the CLI.

---

## Deferred findings

0.

## Stated-convention / trivia dropped

3 (not enumerated per policy).
