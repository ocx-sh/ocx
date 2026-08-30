# Codex adversarial-review gate — plan_issue_sweep_2026-08-30

Target: `.claude/artifacts/plan_issue_sweep_2026-08-30.md` — reviewed against the live
working tree at HEAD `ef1c76fb` (branch `goat`). Note: this run's launch header originally
named `52eecc15`; that was stale bookkeeping only — Codex reads files with its own tools
against whatever is on disk at run time, not a pinned commit, so the actual review target
was current text, including the corrected C-022 rationale (key rotation, not forced fetch),
the corrected C-034 premise, and the declined C-073.

Reviewer: Codex CLI. `--model gpt-5.3-codex` **rejected** (attempt 1, exit 1) — ran on
**account default** (attempt 2, exit 0). Verdict: **needs-attention**.

Status: **complete**.

Format: `SEVERITY | contract-or-WP-id | one-sentence claim | file:line evidence`
Codex's claim is reported verbatim — a cross-model finding is a lead, not a verdict.

---

## Findings

High | C-098 | Destination sidecar tag check-then-write has no atomic create-if-absent/CAS — two concurrent `ocx package copy` runs can both observe "absent" and the later raw PUT silently overwrites the earlier manifest, recreating the accumulation-loss failure C-098 exists to prevent | plan:472-480 (C-098 text); `crates/ocx_lib/src/oci/client/transport.rs:645-652` (Codex cited `transport.rs:645-652`, actual path has a `client/` segment it dropped) — comment there states verbatim "There is **no conditional manifest PUT anywhere in the OCI distribution spec**... this is optimistic rather than atomic: read, append, write, read back, retry"
&nbsp;&nbsp;Codex recommendation (verbatim, prefixed ACTIONABLE): "Add an atomic destination-side conflict mechanism, or serialize/coordinate sidecar writes and verify the tag after writing so a concurrent overwrite is detected and reported rather than accepted."

Med | C-091 | `ensure_target_serves_referrers` treats any successful `list_referrers` response — including an empty 200 — as proof the destination `Supported` the Referrers API; the actual referrer write is a separate plain manifest PUT that can still fail, be orphaned, or not be listed, so the gate cannot guarantee copied referrers stay discoverable | `crates/ocx_lib/src/oci/referrer/capability.rs:88-104` (`probe()`: `Ok(_) => ReferrersSupport::Supported`); `crates/ocx_lib/src/oci/client/native_transport.rs:635-660` (`push_referrer_manifest`, a separate digest-addressed PUT)
&nbsp;&nbsp;Codex recommendation (verbatim, prefixed ACTIONABLE): "Treat the probe as advisory and add post-push persistence/discoverability verification, or explicitly fail the copy when the target cannot prove the referrer was stored and listed."

## Also checked, no material finding (Codex's words: "No issues found in C-022, C-036, C-060, or the present call ordering")

- C-022 (Rekor memo keyed on attacker-supplied `log_id_hex`) — checked against the
  *current*, already-corrected rationale (key-rotation demotion, not a forced-fetch attack).
- C-036/C-036a/C-036b (env-var key scrub completeness across subprocess-spawn sites).
- C-060 (fork dead-code deletion — zero non-test call sites claim).
- The `copy.rs:241` gate / `copy_referrers` (`:438`) ordering claim underlying C-091/C-094
  reachability of S-017 (distinct from the capability-probe soundness finding above, which
  Codex flagged separately).
- The "Execution deviations" rebase-at-merge relaxations (WP-6/`test/**` mechanical-edit
  claim; the four `tasks/sign.rs` sites being non-overlapping).

No line evidence given by Codex for these — treat absence of a finding as "did not
disprove it under the verification asked for," not as an independent proof.

---

## Status-check answers (for team-lead, 2026-08-30 ~18:46)

1. Process: the launcher pid (798237) is dead, and no `codex-companion` node process is
   running — but this is **completion, not the same failure shape**. The script's own log
   shows `=== SCRIPT DONE ===` with attempt 2 exiting 0.
2. Log last entry: `=== SCRIPT DONE ===`, file mtime 2026-08-30 18:45:44 (local), ~2 minutes
   after launch at 18:43:31 — full run including both attempts.
3. `--model gpt-5.3-codex` was rejected again (attempt 1, exit code 1), confirmed by the
   script's own model-rejection grep before it fell back and ran clean on the account
   default (attempt 2, exit 0, which produced the findings above).
