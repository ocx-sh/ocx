# hex — swarm memory

Maintained by the hex skills. Small by contract: pointers and preferences,
not copies. Team-shared — commit it.

## Pointers

- Verification: `CLAUDE.md` › "Build & Development" — run `task verify`
  (full gate) after implementation; `task` = fast check. Subsystem-scoped
  gates per each `.claude/rules/subsystem-*.md` › "Quality Gate".
- Plan / ADR conventions: `CLAUDE.md` › "Workflow" — planning flow
  ADR → Design Spec → Plan; artifacts in `.claude/artifacts/` (patterns
  `adr_<topic>.md`, `design_spec_<comp>.md`, `plan_<task>.md`), templates
  in `.claude/templates/artifacts/`. Plan Status protocol:
  `.claude/rules/meta-ai-config.md` › "Plan Status Protocol" (active plans
  in `.claude/state/plans/`, pointer in `.claude/state/current_plan.md`).
- Product knowledge: `.claude/rules/product-context.md` — canonical
  identity doc (positioning, users, competitors), indexed from `CLAUDE.md`.
- Key rules: catalog `.claude/rules.md` ("By concern" table); architecture
  boundaries + ADR index `.claude/rules/arch-principles.md`.
  Security-sensitive paths: `.github/workflows/**`, `.github/actions/**`
  (`.claude/rules/quality-security.md`), `crates/ocx_lib/src/oci/**`
  (auth/SSRF/wire formats per the CLAUDE.md model policy).
- Worktrees: default `.agents/worktrees/` (gitignored, `.gitignore:50`).
- Constitution: `.claude/rules/arch-principles.md` (optional gate; plans
  checked against it when present).

## Preferences

```yaml
# hex config, vocabulary v2. Unknown keys warn once and are ignored.
models:
  fast-balanced: sonnet
  deep-reasoning: opus
  overrides:
    reviewer:quality: deep-reasoning
    reviewer:security: deep-reasoning
    reviewer:performance: deep-reasoning
    reviewer:spec: deep-reasoning
adversary: codex:rescue
perspectives:
  always:
    - role: reviewer:security
      when: "{.github/workflows/**,.github/actions/**,crates/ocx_lib/src/oci/**}"
research-axes:
  - registry ecosystems / OCI spec evolution
  - package-manager UX (mise, asdf, volta, proto)
  - shell integration mechanisms
```

- Review is never downgraded to save cost — the reviewer overrides above
  encode the CLAUDE.md "MODEL POLICY — NON-NEGOTIABLE" table.
- Fable/Mythos is the session orchestrator only, never a spawn target
  (CLAUDE.md model policy; matches models.md Rule 4).

## Memory

- **Active plan (hex-plan, 2026-08-09):**
  `.claude/state/plans/plan_interpolation_token_grammar.md` — ocx#303, tier high,
  State `plan-approved`. Design record
  `.claude/artifacts/adr_interpolation_token_grammar.md`; research
  `.claude/artifacts/research_interpolation_token_grammar.md`.
  Next: `/hex-execute .claude/state/plans/plan_interpolation_token_grammar.md`.
  Review converged at the round-3 cap: panel (spec/architect/SOTA) → 29 fixes → cross-model
  Codex gate + re-validation → 13 more → final check, one block-tier defect closed.
  **Then the owner reversed the central decision** (2026-08-09, after review): OCX claims
  every `${…}`; no foreign-token pass-through, no reserved-root set, `$${…}` the only escape.
  Reason: pass-through makes OCX's namespace hostage to other tools' vocabularies. Plus D14 —
  refusal scoped to resolve/publish, read-only paths (incl. `pull`/`install`) stay permissive.
  ADR + plan rewritten; #221 amended on GitHub. **D14 and the reversal are unreviewed** — the
  panel ran against the superseded design.
  **`.claude/state/current_plan.md` deliberately NOT repointed** — it still names
  `plan_servable_index_snapshot.md`, which is mid-execution with a dirty worktree at
  `.agents/worktrees/wp8`. Repointing would hijack that run's `/next`. The owner decides
  which plan owns the pointer.
- **Note for the next `/hex-init`:** a worker went idle without delivering its report and
  had to be pulled with `SendMessage`; treat "idle" as "not reported" and pull, do not
  assume completion.
- Plan `.claude/state/plans/plan_package_integrations.md` — package
  `integrations`, [ocx-sh/ocx#221](https://github.com/ocx-sh/ocx/issues/221),
  branch `soraka`. **Complete, awaiting merge.** Two review rounds
  (`/swarm-review` max, then `/hex-review` high) plus two fix rounds; Codex `sol`
  returned `approve, no material findings` on the final state. `task verify`
  exit 0 (2011 acceptance). Deferred, filed as
  [ocx-sh/ocx#306](https://github.com/ocx-sh/ocx/issues/306): the patch overlay
  re-emits a shared dependency's *env* entries, because base roots and companions
  are composed by separate `compose` passes that share no `seen` set. The
  `integrations` carrier was fixed surgically (merge-site dedup keyed on the
  **stripped** identifier); unifying the two passes needs its own ADR.
- **The single highest-value review lesson of this feature:** the cross-model
  (Codex) pass found the one defect the entire 8-worker Claude panel missed — a
  duplicate-row regression the fix round had just introduced. Twice now the
  adversary has produced the run's most valuable finding. Never treat it as the
  optional last layer.
- **Corollary, learned the same run:** a reviewer that "confirms" a finding is
  not evidence. Round 2's architect asserted D16 ratified the shipped wire key;
  it ratified the opposite, and only opening the ADR settled it. Round 1's spec
  reviewer marked the merge-site dedup CLOSED while the architect found it broken
  on the advisory-tag axis. Open the file before relaying a claim either way.
- **Dead gate (repo finding, 2026-08-09, unfixed):**
  `.claude/tests/test_ai_config.py::TestPlanStatusBlock` filters candidates to
  git-tracked files, but `.gitignore:39` ignores all of `.claude/state/` — so
  the set is always empty and its three real assertions skip in every worktree,
  reporting "fresh checkout where no plans exist", a cause never observed.
  29 plan files on disk, 0 tracked. Plan Status blocks are effectively
  hand-verified. Out of scope when found; owner notified.
- **Protocol drift:** `meta-ai-config.md` "Plan Status Protocol" enumerates only
  `/swarm-*` values for the `Step` field; hex plans write `/hex-plan → …`.
  Accurate but outside the enumeration `/next` and `/finalize` read.
- **Perspective gap for the next `/hex-init` — reproduced across two rounds:**
  7 review-shaped workers produced 1 full report (researcher) and 1 partial
  (the Codex adversary, which nonetheless found the single most valuable
  defect of the run). reviewer:spec / reviewer:security / architect returned
  nothing in round 1, and re-spawning security + architect in round 2 with
  explicit "your final message IS the deliverable" briefs reproduced the
  failure exactly. Two SendMessage pulls each changed nothing.
  **The pattern:** workers whose output is a *file* deliver through the file
  (all 5 Discover explorers delivered; the ADR author wrote 1287 lines but
  never returned a summary); workers whose only output is a *report* go idle.
  Practical consequence for an orchestrator: verify load-bearing claims
  yourself and treat review-panel delivery as unreliable — in this run the
  necessity of the constitution deviation, the DoS bound, the traceability
  coverage and a CWE-451 finding in C-005 all had to be established directly.
  **FIX CONFIRMED (2026-08-09, execute phase):** give the reviewer a *file* as
  its deliverable — "write findings to `<path>`, append each the moment you
  confirm it, reply with just the path and a one-line verdict". An 8th
  review-shaped worker, same opus model and same task as one that had just
  idled twice, produced a full 16-contract sweep this way: 1 blocking gap and
  4 notes, including five acceptance scenarios that were Unchecked Green
  because the stub threads `Vec::new()` instead of `unimplemented!()`. The
  incremental-append instruction is load-bearing — it makes a partial run still
  worth something. Make this the default shape for every review spawn.
- **Session-outage recovery (2026-08-09, proven):** a killed session loses every
  subagent *process* but keeps their *file edits* — the working tree is the
  durable artifact. Recover by re-running the phase gate and reading what
  actually compiled, never by re-spawning the original brief blind: the stub
  worker died ~90% done, and a blind re-spawn would have redone finished work.
  Checkpoint the instant a gate goes green (`task checkpoint`); this run went
  through two outages with the stub uncommitted.
- **The command proxy fabricates line numbers and collapses grep output.**
  Observed repeatedly this run: `grep -n` returning `"N matches in 1F"` with
  mangled bodies instead of matching lines, and — worse — an `awk` call
  reporting a match at line 2092 of a file that is 1767 lines long. A
  verification step that trusts that output is worse than no verification,
  because it reads as evidence. When a line number or match count is
  load-bearing, cross-check it (`wc -l`, a second tool, or the Read tool,
  which is not proxied) before acting on it.
- **`git grep` is blind to untracked files.** During a stub phase the newest
  file is untracked by definition, so `git grep -c '<symbol>'` reports zero and
  reads as "the worker did nothing". Cost one false accusation and one wasted
  worker spawn here. Use plain `grep -rn` to verify anything a stub phase
  created, and prefer the compiler's own output over a text search.
- Displaced pointer: `.claude/state/current_plan.md` previously named
  `plan_testing_hardening.md` (branch `testing-hardening`, phase 4 complete,
  awaiting PR #287 which was closed unmerged — its fixes are unlanded, so that
  plan is stale-but-unfinished, not done).
- **A silent subagent is not a failed one — and not a finished one either**
  (execution session, 2026-08-11; sharpens the older "treat idle as not
  reported" note above with a second dataset). `wincheck`, `winfix`,
  `pubreview`, `conflict-core` and `conflict-cli` all signalled idle and never
  delivered, including after explicit pulls naming the exact output contract;
  `conflict-docs` delivered a first-rate report unprompted. Same models, same
  session — so delivery is not a model property and cannot be planned around.
- **So audit the refs, not the report.** `winfix` had produced two sound
  commits; reading its diff directly — rather than waiting on a report it never
  sent — is what surfaced a defect *that diff introduced*. `conflict-core`
  resolved its last hunk correctly in the window between one grep and the next.
  The work is observable without the agent's cooperation: `git log`,
  `git status`, marker counts, and a compile. Check those first, pull second.
- **Corollary — the orchestrator's own checks lie the same way.** Three in one
  session returned a passing result while being structurally incapable of
  failing: `${PIPESTATUS[0]}` (zsh uses `$pipestatus`, expands empty),
  `find -newermt '-75 seconds'` (`find` is `bfs` here and rejects it — error to
  stderr, empty stdout read as "settled"), and `for f in $FILES` (zsh does not
  word-split unquoted parameters, so `stat` got one giant filename and the
  sentinel survived as "settled"). Each was caught only because it contradicted
  something directly observed seconds earlier. Make the failure loud — guard
  every watcher with an explicit failure branch rather than inferring success
  from empty output.
- **PARKED (owner decision, 2026-08-11): `/hex-plan high` for ocx-sh/ocx#183**
  (zip layer media type). Discover + Research complete (7 workers); Design never
  started and no plan artifact or ADR exists. Research rejected the issue's stated
  digest-identity rationale, so #183 is parked pending a vendor-checksum metadata
  field rather than reframed as ordinary archive-format support. Do not resume it
  as a media-type feature — a resume starts from the checksum field.
  - Research persisted: `.claude/artifacts/research_zip_layer_oci_precedent.md`,
    `research_zip_artifact_classes.md`, `research_zip_streaming_constraints.md`.
  - Key constraints for whoever resumes: media-type legality is settled and cheap
    (artifact manifests impose no layer-format rule; Sylabs/WASM precedent).
    Streaming zip is disqualified — buffer→verify→parse via central directory
    only, PLUS local-vs-central cross-validation, because CVE-2025-54368 (uv)
    shows one digest can parse two ways. Port `read_entry_capped` from
    ocx-mirror `crates/ocx_python/src/repack.rs` for bomb caps. Zip cannot carry
    the exec bit reliably (protobuf#10301) so a post-extract mode policy is
    mandatory. `adr_layer_layout_config.md` binds: existing tar publishes must
    stay byte-identical.
  - Related landed work: "package test refuses a layer archive it cannot name a
    media type for" made unrecognized layer extensions a hard error in
    `pull_local::stage_layers`; its regression test pins "zip is refused" and
    must be deliberately flipped if the decision is ever revisited.
  - Note for next `/hex-init`: `worker-researcher` has no Write tool, so
    "persist a research artifact" instructions cannot be followed by that role —
    the orchestrator must persist, or the role needs Write.
- **2026-08-19 `/hex-architect` lesson (tier high, `adr_sbom_attestations.md`):** the
  cross-model adversary caught a wire-format directive error the whole panel had
  propagated (rekor `dsse:0.0.1` payloadHash covers the DECODED payload bytes, not the
  PAE) — keep the adversary ON for wire-format ADRs regardless of overlay defaults.
- **SBOM attestations milestone: done.** `.claude/state/plans/plan_sbom_attestations.md`
  finalized; landed as [ocx-sh/ocx#325](https://github.com/ocx-sh/ocx/pull/325)
  (merged 2026-08-20). No active plan.
- 2026-08-20 execution lesson: running a wave-landing or history-writing git operation
  with an inherited working directory landed it on the fixed `soraka` branch (repaired
  same turn; `soraka` moved back to its prior tip). Every git call in an orchestrator
  turn carries `-C <worktree>` or a leading `cd &&` — no exceptions.
- **2026-08-21 `/hex-review high` on `ocx package copy` (branch `evelynn`, 33 files, +3757).**
  Verdict Request Changes. 8-worker panel + Codex gate; 73 severity-tagged findings.
  - **The adversary produced the run's best finding for the THIRD time.** Codex found a
    Block the entire 8-worker Claude panel missed: `fetch_manifest_raw_bytes_addressed`
    (`oci/client.rs:2060-2062`) verifies bytes against the **registry-supplied**
    `Docker-Content-Digest`, never against the **requested** digest, and
    `oci/copy.rs:125` never compares the two. A registry answering `GET /manifests/A`
    with self-consistent bytes for B gets B pushed while `publisher/copy.rs:240` merges
    **A** into the target index — pointing it at a manifest that was never copied. The
    re-run reports `Unchanged` (`:188` compares source-vs-target, both A) over a
    permanently broken index. Never treat the cross-model pass as the optional layer.
  - **A read-site audit table is worth more than a security narrative.** Asking
    reviewer:security for "every read: file:line, addressing, does it decide a write,
    verdict" produced 15 rows and found **4** Invariant #5 violations where Stage 1 had
    found 1 — proving the obvious one-line fix was insufficient. Ask for the enumeration,
    not the opinion.
  - **`worker-researcher` still has no Write tool** (confirmed again). It returned inline;
    the orchestrator persisted `.claude/artifacts/review_r1_sota_package_copy.md`. Its
    outside-in lens found the mount auth-scope gap all seven Claude reviewers missed.
  - **File-first delivery defeats the idle-worker failure.** Every worker was told to write
    its artifact BEFORE returning a summary. Five of eight then went idle without a
    summary — and lost nothing, because the file was on disk. Make this the default brief.
  - **Two workers disagreed on a fix direction and the file settled it.** spec said "amend
    the ADR to match the code" on the canonical-tag phase; architect said "fix the code".
    Reading `client.rs:642-682` showed the merged index is used for one digest lookup the
    copy path already has — architect right. Open the file; do not pick the confident one.
- **2026-08-21/22 `/hex-execute high` on `ocx package copy` (branch `evelynn`).** Applied every
  finding from the round-1 panel, ran a second 5-worker panel plus the Codex `terra` gate, and
  converged. `task verify` exit 0. Plan at `review`; nothing pushed.
  - **The adversary earned its keep a FOURTH time — but differently.** It found nothing blocking
    in the diff and instead surfaced two *pre-existing* defects in a file the diff merely touched:
    an unbounded description-layer ingestion path (`client.rs:1819`, no size pre-check, no stream
    cap, `fs::read` of the whole blob) and a lost-update RMW on the target index (`:531`, no
    conditional PUT). Both verified byte-identical at the base commit. This is exactly the class
    `quality-core.md` calls invisible to diff-scoped review: the file already existed, so nobody
    reviewing the *change* was prompted to question it. Ask the cross-model pass for that class
    explicitly — it is the one thing the Claude panel structurally cannot produce.
  - **"My verification was wrong" happened twice, both times in my own work.** I verified a retry
    *helper* red/green and reported the Block closed; a round-2 reviewer found the warm-primary
    path still emptied the suite green. And I claimed the addressing inversion closed the cascade
    Block "for every caller"; security showed push's blocker *list* was still mirrored. Both times
    the mistake was verifying the mechanism rather than the path that reaches it.
  - **A `#[error(transparent)]` variant forwards `source()` PAST the wrapped error.** A worker's
    `classify()` returned `None` expecting the chain walker to reach the cause; exits 79/80/84 all
    collapsed to 1. Its own test — "the failure kind is reachable by a chain walk" — passed
    throughout, because reachability and classification are different properties. When a test and
    a behaviour disagree, check whether the test asserts the property you actually care about.
  - **`task test:build` does not exist.** It silently no-op'd, I read the following `ls` as proof
    of a rebuild, and re-ran the acceptance suite against a six-minute-stale binary. The real
    command is `cargo build --release -p ocx --features ocx/__testing --locked` then `cp` to
    `test/bin/ocx`. A task runner's unknown target is not an error here.
  - **Two more silently-empty checks, same session as the three already logged above.** A
    tolerance-band grep died on a regex parse error and printed `0 matches`, which reads as a
    clean result; and `find -newermt '-90 minutes'` was rejected by `bfs` with the error on stderr
    and nothing on stdout, so "no Codex artifact was written" was an unfounded conclusion until I
    re-checked with `ls -t`. Both were caught only by deliberately falsifying the check. Make the
    canary explicit: prove the pattern can match before trusting that it did not.
  - **Never edit the tree while `task verify` runs.** I fixed a CLI message mid-gate; the release
    binary the acceptance suite would have used no longer matched HEAD. Killed the run, committed,
    re-ran clean. A gate result against a tree that moved under it is not evidence.
  - **`--theirs` on a test-file conflict silently drops coverage.** Taking one WP's side removed
    another's two assertions. Graft the dropped assertions back and anchor each with an
    asserted-once check rather than trusting the merge.
  - **Idle-worker failure reproduced again, and the file-first fix worked again.** The Codex gate
    went idle three times delivering nothing; the third pull replaced its deliverable with "write
    findings to `<path>`, append each as you confirm it, reply with just the path" and it
    delivered a full report immediately. Make the file the deliverable in the first brief, not the
    third.
