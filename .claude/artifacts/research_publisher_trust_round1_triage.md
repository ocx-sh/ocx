# Review round 1 — consolidated 4-way triage, 2026-10-25

Sources: codex (C), security (S), spec (P), quality (Q). Gap-check (G) to be merged when it lands.
Dedup: one row per defect, all sources listed.

## Actionable (fix pass)

### Block
- A1 Verify seam wrong — every root GET (fetch_root, fetch_root_document inline GET, revalidate_root_document, resolve_root, remote_view) via one fetch_root_verified before memo/route/commit; test per entry; state prune. [C1 S-B1 P1 Q]
- A2 Pre-signed future catalogs — bind version/expires to Rekor integrated time (|version−IT|≤1h, expires≤IT+8d); refuse bundle w/o IT; issuance/revocation of anchor stated. [C4 S-B2 P7 Q]
- A3 Required check "neutral" admits — 75/outage keeps check pending/failing; explicit success only. [C2]
- A4 Renderer/bot compromise uncontained + npm build feeds signed bytes — sign job re-renders independently (no cache, SHA-pinned), byte-identical gate, binds bytes to checked commit; regrade T8; indexbot bumps human-reviewed. [C3 S-H3 S-W6]

### High
- A5 G-21 holes — publishers read from base; classifier allowlist (tags/desc/source); runs under pull_request_target; base-has/head-lacks → human; refuse PR touching publishers+tags; bot released before schema; empty-tempdir run; digest-pinned ocx. [S-H5 S-S4 S-W10]
- A6 publishers optional → unverified roots vouched — mandatory for new roots (owner confirms; default to mandatory, legacy roots via explicit lane). [C8 P-D1]
- A7 Revocation TOCTOU check→merge — require up-to-date branch/merge queue for p/**, or re-verify at render. [S-H6]
- A8 Managed/unpinned protocol downgrade (index="") — ignore protocol-disabling settings from unpinned tiers on anchored namespace; refuse plain OCI at strict. [C6 S-H7]
- A9 Trust-root cache poisoning — public-good anchor uses only TUF-sourced root / separate storage w/ provenance. [C10 S-H1]
- A10 sign.yml signing oracle — takes run_id, API-verifies release run on refs/tags/v*, signs only that run's digests in allowlisted prefix; label NOVEL if kept; consider signing inside release workflow (uv/ruff precedent) — architect picks, cites. [S-H4 Q]
- A11 Trigger ban too narrow — structural `on:` check: render-deploy push-main/schedule/dispatch; sign.yml dispatch only; pin .20 Build Trigger. [S-H2]
- A12 ref optional — required (default branch), require repository_id or Build Signer repo == Source repo; fix family-pin example. [S-W5 P7 Q]
- A13 Cross-product substitution under shared signer — binding package+version+digest (in-toto statement) or narrow the claim. [C7]
- A14 Audit-mode poisoning persists — record verified provenance per root; strict revalidates audit/legacy entries. [C5]
- A15 High-water vs publish race — carry provenance into publication; atomic recheck at commit. [C9]
- A16 High-water in ephemeral state/ — amendment row or different tier; reset semantics; corrupt/unknown v WARN, older binary never overwrites newer v. [P12 Q S-S1]
- A17 Offline claim false — load_embedded is network; correct, list egress dep, restricted-egress lane or embed root. [P4 Q]
- A18 Liveness alarm inside watched workflow — external watchdog (otel.ocx.sh/Grafana or ocx-repo schedule) failing when expires−now<5d; P3 gate; workflow-level concurrency + deploy refuses version≤served. [P5 Q S-W8]
- A19 Rekor monitoring optional — gate S2/S3 on index-identity monitoring. [S-W7]
- A20 Compile errors — IgnoredAny → serde_json::Value flatten; VerifyErrorKind; add compile_signers. [P2 P6]
- A21 Unknown key must narrow, not Err in TrustPolicy::compile — drop entry w/ log; sibling still matches test. [P3]
- A22 .23/.15/.17 never observed; Rekor v2/TSA one-way — P0 real-cert decode gate + Rekor v2/TSA bundle spike before wire freeze; name one-way doors (anchor constants, validator constants, Rekor v1/bundle v0.3). [P11 Q]
- A23 Option analysis — add C1 (minimal TUF: offline root + 1 online key, root chaining, no delegations); re-score honestly (Q re-score ties A/B); publish flip points; state lane disagreement; mark D eliminated; per-mechanism reversibility. [C11 Q]
- A24 NOVEL labels — sign.yml oracle, .23 pin, public publishers placement (and dropped pending/activation), skip/audit/strict ≠ Notation semantics, version/window constants, G-21 = post-hoc check; cite Homebrew for compiled-in identity; fix apt Valid-Until citation. [Q]
- A25 Amendments — S1-H (skip/audit vs no escape hatch), DR2 relabel, state/ contract, adr_announce_publisher_surface row. [Q P17 P12]

### Warn (fold into same pass)
- T6 regrade Partial; permissive max-age floor or cut permissive. [S-W1]
- --no-verify / OCX_NO_VERIFY never affect snapshot checks; test. [S-W2]
- Override surface threat row (user tier/OCX_HOME/OCX_CONFIG = anchor-equivalent); old binaries ignore index_verification. [S-W3]
- Explicit operator signers default strict. [S-W4]
- Backfill subjects from release records, not registry; multi-platform digest timing; dedicated-identity+cutoff row. [S-W9 Q]
- Claim forge split, GH-ID lookup creds/SSRF, GHES unsupported, ClaimReport field; publisher removal path. [P9 P10]
- Clock-scan sentence; file:// under anchor; container diagram new work (index repo lacks ocx.toml/lock/cosign). [P8 P15 P13 Q]
- Tests for every Validation row listed in P14. [P14]
- Exit codes re-derived per IC-22 (rollback/invalid window vs CDN skew retry 75; unsigned private 78/81). [Q P18 P-D3]
- Solo-maintainer cut list (permissive, invalid_window/equivocation, staged successor, 3 claim flags…) — architect cuts or justifies each. [Q]
- Stale numbers: 132 pkgs / 13,059 B. [Q]
- Cold-runner NFR; strict outage blocks self update — workaround. [Q]
- Rename catalog `version` → `snapshot_version`. [P16]
- S2 duplicate-OID reject, read claims from chain-checked leaf, compare full URL; S3 OCX_INDEX grow-on-resolve; S5 clock set-back residual. [S]

## Deferred (owner — handoff, not fix pass)
1. Built-in anchor = compiled-in CI identity (rejected design #1 at infra scope) vs managed-channel / TUF root-chain anchor. [Q-D1 C11]
2. Keyless-only vs offline root (C1). [Q-D2]
3. Owner account SPOF (hardware 2FA + monitoring vs NC3 early). [S-DEF1]
4. Admin bound by required checks / direct push off. [S-DEF2]
5. Index-declared publishers only ever AND, never ANY-of at system level. [S-DEF3]
6. Reviewer on every sign.yml dispatch vs backfill only. [S-DEF4]
7. Revocation for ocx.lock-pinned digests (deny/advisory channel?). [P-D2]
8. Third-party packages before NC3. [Q-D3]
9. High-water storage tier. [Q-D5] (fix pass proposes; owner confirms)

## Stated convention (no action)
- No new exit code; slugs under existing codes; additive cli.json/config; crate edges allowed. [P holding]

## Trivia (dropped)
- none beyond renames folded above.

## Gap-check (G) — merged 2026-10-25 (.claude/artifacts/research_publisher_trust_gapcheck.md)
- G1 High → A18. G2 High → new A26: sign-job pinned verify gates deploy; TUF signing_config can flip to Rekor v2 w/o cosign release.
- G3 Warn → A27: per-source window min/max (apt); built-in anchor stale-if-error + mirror route (A17).
- G4 Warn → A11/A12: .20 pin / forbidden triggers; never pin .24; signer_workflow rename = one-way.
- G5 Warn → A10: reword NC1; signature ≠ provenance.
- G6 Warn → A28: reject zero/empty expected IDs; second-implementation golden; CVE-2026-48815.
- G7/G8/G9 Suggest → A22 / NOVEL label / per-source overrides.
