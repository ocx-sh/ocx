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

- Federation lead: `../ocx-mirror` — this repo participates as satellite key
  `ocx` for plan(s): `mirror-signing`. Run hex orchestrators from the
  lead, not here.

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
- **A hermetic fixture can red a path the old suite only passed via the public internet
  (proxy-aware SSRF guard, ocx#407/#323).** The stdlib forward-proxy fixture made acceptance A
  fail with exit 75 AFTER the proxied pull had succeeded: `ChainedIndex`'s manifest/blob walks
  fell through from an authoritative miss to the registry-backed `ocx.sh` source, and the twin
  test was green only because that dial reached the public ocx.sh and got a 404. Grep the trace
  for external dials before blaming the fixture; fixing the walk (four sibling walks already
  honoured the authoritative stop) was right, aliasing the fixture would have hidden a contract
  violation. Same run: three reviewers missed that a textual host check must judge the SAME
  normalised form the transport dials (`0x7f000001`, `[::1]` slipped a raw `IpAddr` parse);
  the security perspective caught it, the researcher confirmed the class. Codex's one-shot
  then found the remaining route-blind spot in the resolver hook (destination named like the
  proxy on a direct route) — deferred with a pin-map design, since its fix would reopen #323.
- **The cross-model gate earned its keep a SIXTH time, and this run makes the pattern
  unambiguous.** Eight Claude reviewers on the shell-env branch (spec, test-coverage,
  security, escaping, performance, docs, architect, SOTA) produced 6 Block findings between
  them. Codex then found two more that every one of them had missed, both in the revert
  planner and both requiring the same setup nobody had constructed: *two scopes declaring the
  same key, retiring in the same prompt*. Its stated mechanism was wrong on one of them
  (it blamed restore ordering; the real cause was that the global scope carries no priors at
  all), which is the point — **a cross-model finding is a lead, not a verdict.** Opening
  `reconcile.rs` is what turned a wrong explanation into the right fix. Never treat the
  adversary as the optional last layer, and never relay its diagnosis unopened.
- **File-disjoint decomposition is what made a 26-finding fix round parallelisable, and the
  refusals were the most valuable worker output.** Six workers, one worktree each, disjoint
  file sets. Three of them correctly REFUSED items that crossed their boundary rather than
  reaching into another worker's file: W2 refused R1 with a compile-level argument (every
  carrier shape that could hold a global prior is built by exhaustive struct literal in
  `activate.rs`, so Rust has no partial-literal escape), W3 refused P2 because `Verdict`
  lives in W2's file, and W5 refused three register rows needing Rust tests. Each refusal
  came with the exact patch the next worker should apply. A seventh "cross-file follow-up"
  worker then landed all of them in one commit series. **Brief for the outcome and say a
  reasoned refusal with evidence is an acceptable answer** — otherwise the agent reaches
  across the boundary and the merge conflicts.
- **A worker can die on an API error and report nothing.** W5 (the vacuous-check pass) came
  back as `Agent terminated early due to an API error: 403 Unable to verify organization
  membership` having done zero work. That is a *third* failure mode alongside "idle without
  reporting" and "delivered": **terminated without starting**. The tell is that the result is
  an error string rather than a report or silence. Re-spawn it; do not assume the work
  happened. Its worktree was still at the base commit, which is the cheap check.
- **`task --force` skips `preconditions:` on go-task 3.52** (found by the re-spawned W5 while
  writing a guard against a silent degrade). Any guard that must survive the `--force` this
  repo's own conventions recommend belongs in `cmds:`, never `preconditions:`. A guard that
  evaporates under the flag everyone passes is the same class as an unreachable red state.
- **`task verify` exceeds the 10-minute foreground bash cap on this repo.** Backgrounding it
  from a subagent gets it killed at the turn boundary (the exit-143 lesson above). What works
  from an orchestrator: `nohup … > log 2>&1 &` with `disown`, then a `Monitor` until-loop on
  the PID. Full run here: ~25 min wall (5959 unit + 2664 acceptance), exit 0.
- **A red CI leg can be fixed by a change aimed at something else — check before investigating.**
  `test_a_real_pwsh_prompt_hook_applies_on_cd` was red on the PR before this round. The fix
  round never targeted it; W4's unrelated refactor of `power_shell_registration` (extracting
  `function global:__ocxReconcile` so the wrapper and the prompt share one guarded entry
  point) made it pass. Re-run the failing leg against the new tip before opening an
  investigation into it.

- **Design record (hex-architect, 2026-08-24/25):**
  `.claude/artifacts/adr_shell_env_overhaul.md` — tier high, Status `Proposed`,
  supersedes `adr_live_env_reload.md`. Replaces direnv with a native per-prompt
  reconciler; consent model, `[shell.trust]` whitelist, `__OCX_ENV_STATE` carrier,
  one project key + one per-project state root, `--[no-]hook` symmetric with
  completions. Scope brief `.claude/artifacts/brief_env_overhaul.md` (authoritative);
  Discover map `discover_shell_env_map.md`; research
  `research_{project_state_layout,trust_whitelist_grammar,shell_integration_rollout,private_env_state_vars}.md`;
  panel findings `review_adr_env_{spec,security,quality,sota}.md`.
  Converged in 3 rounds: opus panel (spec/quality/security) + SOTA → 9 Block fixed →
  Codex cross-model gate → 3 net-new (2 Block) fixed.
  **Key reversal from the predecessor**: the phase-1/phase-2 dependency on
  `adr_project_toolchain_links.md` ([#189](https://github.com/ocx-sh/ocx/issues/189))
  is NOT real — the reconciler works against digest paths, links are an optimization
  plus the frozen-process class. The two are independent tracks.
  **Owner gates**: 3 open questions (whitelist key shape, default-on blast radius,
  WinPS 5.1 fidelity) and the flip to `Accepted`.
  Research axes worth keeping as `hex.md › Preferences` hints: per-project state
  layout; trust/whitelist config grammar; generated-shell-integration rollout lag.

- **Active plan (hex-plan high, 2026-08-25):**
  `.claude/artifacts/plan_shell_env_overhaul.md` — the shell env overhaul, State
  `plan-approved`, 19 file-disjoint work packages in six waves. Spine
  `.claude/artifacts/design_spec_shell_env_overhaul.md` (C-001..C-052, S-001..S-045).
  Research `research_{shell_hook_cast_recording,prompt_hook_ci_testing,shell_env_sota_gap_check}.md`.
  Next: `/hex-review .claude/artifacts/plan_shell_env_overhaul.md` — waves 0-5 all
  merged except WP-12b (spike-gated nushell leaf, nothing depends on it).
  **Execution lesson, waves 4-5:** every defect this run found came from running the
  suite somewhere the author could not, never from reading the code. The host has no
  nushell or elvish, so those arms skipped silently and shipped broken; a uid-0
  container ignores the `chmod` three tests staged their premise with, so they were
  green without checking anything; and `coexistence::detect` passed CI for a whole
  wave against a `DIRENV_DIR` spelling real direnv never emits. **Run the fixture
  against the real thing before believing a green.**
  **Plan lives in `.claude/artifacts/`, not `.claude/state/plans/`** — the latter is
  gitignored (`.gitignore:39`) and the plan had to be committable.
  **Review lesson, third dataset:** the panel's single highest-value finding was a
  *false* claim in its own input — the architect worker asserted `project/hook.rs` had
  zero call sites and scheduled it for deletion in the sequential commit gating all 18
  other packages; it is live (`direnv_export.rs:11,94,96,102`). Two other Block-tier
  findings were the same shape: a gate homed at a seam that does not reach the surface
  it defends (`emit_lines` never routes through `Env::apply_entries`), and a security
  property asserted against a shipped mechanism that implements the opposite
  (`ScopeSpec`'s deserializer *drops* unknown keys). **A "zero call sites" or
  "X already does Y" claim is not evidence until you grep excluding the defining file.**
  **Cross-model gate caveat:** `codex:rescue` ran but `--model gpt-5.3-codex` (the
  `terra` mapping) was rejected — *"not supported when using Codex with a ChatGPT
  account"* — so the run used Codex's account-default model. It still found two real
  Blocks the opus panel missed (the `resolve_env*` seam is `tasks/resolve.rs:724`, not
  `composer.rs`; WP-14's DAG was missing two edges). Fix the `terra` model string.

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
- **A correction pass overshoots into a new false claim — budget a check for the fix, not
  only for the defect.** The 2026-08-31 consent round found that the record's premise about
  git (`safe.directory` "has no glob support") was simply wrong. The fix pass corrected it
  and then asserted *exact* parity at six sites, including a doc comment that feeds the
  published JSON schema. Measured with controls, git's `/*` **refuses** the named directory
  and allows only what is nested under it; ours grants it. A fix-round reviewer caught it —
  the same failure class the round existed to close, one layer later. Two rules fell out:
  when a claim about an external tool is load-bearing, **run the tool** (a five-line probe
  with a positive and a negative control beats any man-page paraphrase), and always spend
  one reviewer on the fix diff itself, never assume a fix round is self-verifying.
- **A codex-companion job's `status` stays `running` after its worker dies.** The
  2026-08-31 consent review's gate job froze at minute 1; its pid was gone while the state
  JSON still read `"status": "running"`, and the agent polling it reported "long reasoning
  turn" for two hours. Liveness is `ps -p <pid>` on the pid in the job's own JSON plus log
  mtime growth — never the status field, and never the shared broker process, which stays
  alive with CPU burn across all jobs and so returns the same answer in every state. Brief
  every Codex-driving agent with this and give it a time budget; a skip with a reason is a
  valid gate result.
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
- **2026-08-23 `/hex-execute high` on the ocx#272 review set (branch
  `fix/oci-cross-host-upload-auth-272`).** Rebased onto a `main` that had moved 9 commits, then
  applied the `/hex-review` actionable set across three parallel work packages.
  - **`worker-researcher` has no Write tool — THIRD run in a row I briefed it with a file
    deliverable.** It routed around the gap by pasting the whole report into its reply, which
    works but defeats the file-first rule that exists precisely because workers go idle holding
    their output. Stop writing "write your findings to `<path>`" for this role: either brief it
    to reply inline, or spawn `worker-explorer`/`general-purpose` when the deliverable must
    land on disk. The orchestrator persists its output by hand.
  - **The `Cargo.lock` inside `external/rust-oci-client` is gitignored.** A review finding read
    it as a committed second lockfile pinning `reqwest 0.13.2` against the workspace's 0.13.4
    and graded it High. It is a local artefact — but the real consequence survived the regrade:
    every mutation proof ever run *inside* the fork executed against a dependency graph that is
    not what ships. Any fork work package must `cargo update` and assert the resolved version
    before producing evidence. Check whether a lockfile is tracked before grading a lockfile
    finding.
  - **`rtk` rewrites `cargo test` output into its own summary line.** `cargo test | grep
    '^test result'` returns silently empty — a passing-looking gate that never ran. Same class
    as the greps already logged above; the tell was an empty section where a number belonged.
    Run the command unfiltered, or match `rtk`'s own `cargo test: N passed` line.
  - **A mutation that fails to red is a lead, not a weakness — and reading the dependency
    settled it.** A 307-shaped redirect test stayed green when the guard was removed. The cause
    was in `tower-http`'s `follow_redirect`: the body is *moved* into the inner service and
    `reqwest::Body::wrap_stream` is not cloneable, so a 307 is never followed at all. A 303
    zeroes the body before the take and IS followed, so only that shape discriminates. The
    worker root-caused it out of the vendored source rather than concluding the guard was fine.
  - **Ask the research axis "does refusing this break anyone?" before shipping a hard refusal.**
    A behaviour change (any 3xx on an upload `PUT`/`PATCH` is now fatal) looked like a
    compatibility risk worth deferring to review. One sonnet researcher retired it instead, and
    turned up the strongest evidence in the run: `distribution` computes the blob digest
    server-side as bytes stream past, so a registry *structurally cannot* delegate an upload via
    redirect; and oras-go shipped CVE-2026-50151 for this exact function shape. Prior art beat
    a review round.
  - **The cross-model gate earned its keep a FIFTH time, and this was its clearest win yet.** Seven
    Claude reviewers — including a security pass that enumerated all 24 sites where a
    registry-supplied URL reaches a request builder — produced two Blocks between them. Codex
    then found two more that every one of them had missed:
    - **A zero-padded port defeated the system lock.** The round had just fixed the *case* axis
      of the same comparison and nobody asked what else two spellings of one authority could do.
      `OCX_INSECURE_REGISTRIES=registry.corp:05000` misses a lock on `:5000` at every string
      compare, and `url::parse_port` accumulates `port*10+digit`, so the socket lands on 5000
      anyway. Lesson: when you normalise one axis of an identity comparison, enumerate the other
      axes in the same sitting — case, port spelling, default-port elision, trailing dot.
    - **The read-site audit marked the session-opening POST "clean" for the same reason the
      implementer did**: its `Location` is vetted afterwards. Both true, both missing that the
      *request itself* gets relocated before any `Location` exists. Two independent reviewers
      agreeing is not corroboration when they share the premise. Codex proved it pre-existing
      with a `git diff` against the fork base rather than asserting it.
  - **A fix round can staledate its own reviewers.** Two agents fixed the same seam from opposite
    sides in one round: the fork replaced `attempt.stop()` with `attempt.error(...)` while the ocx
    side was documenting `attempt.stop()`. The resulting comment described neither tree. When
    parallel work packages share a seam, re-read the other side before accepting a comment about
    it — and sequence the gitlink bump before, not after, the dependent write-up.
  - **"Not on either branch" does not mean "unlanded".** An auditor reported the hawkeye-7 work
    lost because its commit was on neither branch; `main` had done the same migration
    independently and the effect was live. The decisive test is whether the *effect* is present
    (one `git show` plus one gate line), never commit ancestry alone.
  - **The best worker output of the run refused the task as briefed.** Told to close a finding,
    wp1-fork instead proved the defended branch unreachable and fixed the *comment* that claimed
    otherwise; told to wire `ssrf_guard`, wp2-crates refused with the caller list that would break
    (56 acceptance files on loopback, every air-gapped registry) and the subsystem rule already
    documenting it as design. Brief for the outcome, and say that a reasoned refusal with evidence
    is an acceptable answer — otherwise the agent implements the wrong thing well.

- **Shell-env overhaul, waves 2-3 execution (2026-08-25, branch `feat/shell-env-overhaul`,
  tip `5089ccdf`, `task verify` exit 0 / 2285 acceptance).** WP-10, WP-12a, WP-13, WP-11
  merged. Both plan open questions closed by spike, not reasoning: nushell `hide-env
  --ignore-errors` DOES reach the caller from inside an `env_change.PWD` hook (full
  parity, WP-12b ungated; re-runnable harness `test/manual/nushell-hide-env-spike.sh`),
  and the C-047 dispatcher ceilings are measured per family with an
  `INLINED_LOGIC_FLOOR = 500` assertion so the constants cannot go vacuous by drift.
  - **One Docker registry serves every worktree.** All `.agents/worktrees/*` share compose
    project `test_default` on `localhost:5000`, and `test_patches.py` holds a
    registry-wide, non-UUID-scoped slot — so three concurrent `task verify` runs produced
    three different failures, none of them real. Per-WP gates must be `task rust:verify`;
    exactly one serialized `task verify` on the integration branch, after
    `docker compose down -v` and after pre-building the release binary so the suite run
    cannot be killed mid-module (a kill mid-`test_patches.py` poisons the slot).
  - **`${PIPESTATUS[0]}` expands EMPTY under zsh** (it is `$pipestatus`), so a piped gate
    yields no exit code at all while its output still scrolls past looking green. Never
    pipe `task verify`; redirect to a log and capture `$?` on the next line. Compounding
    trap: a background wrapper that *echoes* the exit code exits 0 itself, so the task
    notification says "completed (exit code 0)" for a red gate — read the logged
    `VERIFY_EXIT=`, never the notification.
  - **Four worker checks were vacuous, and all four were caught only by insisting on a
    red.** A shim-body denylist whose needle was a literal in the file it scanned; a
    `$PWD` grep mis-escaped so it matched identically in both states; a fake-binary
    fixture that emitted no carrier, so the empty-carrier term fired every prompt and the
    named term was never reached; a headline test whose precondition fired before its
    assertion. A worker reporting "red demonstrated" is a claim, not evidence — ask for
    the `grep -c` of the mutated token and the two exit codes.
  - **Rust privacy is module-scoped, so a unit proof-struct proves nothing.** C-028 was
    made compile-time by a `ConsentProof` the consent gate alone can mint — but the first
    attempt used a *unit* struct and the hoist it was meant to forbid still compiled. A
    private `()` field fixed it. The injection caught it; review had not.
  - **Merging a sibling's stale SHA costs a content conflict.** A worker amended after I
    merged; the amended tip then conflicted with my own merge of its predecessor.
    `--theirs` is only defensible because both sides were the same author's drafts, and
    only after proving the result byte-identical to the amended tip
    (`git diff --stat <amended> -- <files>` empty). Re-read a worker's tip immediately
    before merging.
  - **A pty driver named after the stdlib module it imports shadows it.** The nushell
    spike wrote its driver to `${workdir}/pty.py`, so `import pty` found itself,
    `pty.fork` did not exist, no output was produced — and the harness concluded
    "hide-env did not propagate", the opposite of the truth. A harness that can only
    report one outcome is the failure mode; gate on the mutation being present.
  - **The headline use case was broken on 4 of 5 shells and invisible.** The prompt
    guard had no `$PWD` term and the watch set is fixed at shell start, so `cd` alone
    never re-reconciled — nushell was unaffected, which is exactly why nothing caught it.
    `shell/hook.rs` was unowned by any work package. **Assign every file the feature's
    headline scenario traverses**, not only the files the contracts name.

- **`adversarial-review --background` is a no-op in this codex-companion.mjs version** (2026-08-30,
  issue-sweep plan gate). Only `handleTask` checks `options.background`, so the review ran in the
  foreground and was SIGTERM'd by the 2-minute Bash call timeout after ~15 tool calls, before
  emitting a verdict. **Use the Bash tool's own `run_in_background`, never the script's flag.**
  - The tell is a stale job file: `"status":"running"` with a dead pid and a log whose last entry
    is minutes old. That reads as *pending* and is actually *failed* — the same silent-negative
    class as an empty grep. Treat "running + dead pid" as FAIL, and check the pid, not the status
    field.
  - Costs nothing to survive: the adversary's deliverable must be an incrementally-appended FILE.
    A gate killed at 90% leaves a full report on disk instead of nothing. This is the third
    distinct failure mode for review-shaped workers here, alongside "idle without reporting" and
    "terminated on an API error before starting".

- **rustc exempts leading-underscore identifiers from `dead_code`, so an underscore-named probe
  red-proofs nothing** (2026-08-30, WP-7 fork deletion). A worker's first mutation probes were
  named `__wp7_*` and came back green even under `RUSTFLAGS="-D dead_code"` — the canary was
  structurally incapable of firing. Renaming them without the underscore produced the expected
  `function ... is never used` at both lib level and inside `#[cfg(test)] mod test`. Two
  compounding traps in the same check: `cargo build` never compiles `#[cfg(test)]` mods at all,
  and the `rtk` hook replaces compiler output with its own summary, hiding warnings entirely.
  **A zero-warning claim needs `cargo check --all-targets`, run unfiltered, with a probe whose
  name rustc will actually complain about.**
- **A submodule gitlink bump can cross a merge without carrying a foreign change — prove it by
  tree, not by log.** WP-7 branched off `origin/ocx/integration` (`609d3f7`), one merge past the
  pinned `21ded5e`. Commit ancestry says a change rode along; `git rev-parse <sha>^{tree}` on both
  says the trees are identical (`253279661d64c…`), so nothing did. Same lesson shape as
  "not on either branch does not mean unlanded": compare the *effect*, never the graph.

- **Prove the mutation changed the BINARY, not just the file** (2026-08-30, WP-9, issue sweep).
  Gate an acceptance mutation on the release binary's **sha256 changing**, not only on a
  file-level `grep -c`: the grep proves the edit landed on disk, which is *not* the same as
  proving the binary under test contains it.
  - **But do NOT gate on the rebuilt binary hashing back to the baseline** — `vergen-gix` stamps
    build metadata into `ocx`, so a rebuild is not byte-identical and that assertion is luck, not
    a check. It reported three correct proofs as BROKEN when run against a stale baseline. The
    sound gates are: **mutated ≠ baseline, restored ≠ mutated, source byte-identical, green run
    passes.** (WP-9 shipped the wrong form first and corrected it — carry the corrected form.)
  - **Restore on abort.** The same harness aborted mid-mutation and left the mutation on disk when
    a mutation broke the compile under `-D warnings`; only the *next* run's anchor gate
    (`anchor hits=0`) stopped it silently measuring a mutated tree. Write mutations that revert
    behaviour without breaking the build, and restore in a trap.
  Two sibling packages in the same run hit the weaker form: one measured the previous mutation's
  binary after `cargo fmt` silently staledated its needle, another had two mutations come back
  **build-broken rather than red** ("a build break is not a red").
- **Keep a control leg that passes UNDER the mutation.** WP-9's acceptance test carried a
  bare-path leg beside the `file://` leg; the bare leg stayed green while the mutated `file://`
  leg reddened, which is what proves the test discriminates *the spelling* rather than the whole
  rung. A red alone only shows something broke.
- **A guard that cannot red on this host is a platform guard, not a weak test.** WP-9 could not
  red the rooted rows of `anchored_at`: Unix `Path::join` already replaces the base for an
  absolute argument, so the `has_root()` branch is Windows-only. It mutated the branch
  *condition* instead and recorded which rows are load-bearing only on Windows. Correct reading
  of the "a mutation that fails to red means you have not found every guard" corollary.
- **The recurring defect shape of this run: a widening that lands at some doors and not others.**
  Three separate packages found a third or fourth reader of the thing they were fixing —
  `append_to_tags_file` (third unbounded `--tags-file` caller), `read_unverified_referrer`
  (second `.first()` site), `context.rs:1099` + `managed_config/publish.rs:316` (third and fourth
  readers of `OCX_SIGSTORE_TRUSTED_ROOT`). **Every one was found by the implementer, none by
  either review pass.** Brief work packages to enumerate every reader/caller of the seam they
  touch, and grant the scope extension when they do — a contract honoured at two of four doors
  is a worse contract than the one before it.
- **Scenario lists do not get regenerated when a contract is corrected.** S-023 kept specifying
  the probe-only-when-empty optimisation that C-094 had explicitly deleted; a tester working from
  it would have pinned the defect as the spec. Second instance in one plan. The only reader
  forced to reconcile a contract with its scenario is the implementer — so when a review corrects
  a contract, re-read its scenarios in the same edit.
- **Worker idle-notifications race the orchestrator's reply.** Two packages committed "awaiting
  your decision" while the grant was already in their inbox, costing a round trip each. When a
  worker parks on a decision, assume the next message it sends crossed yours and re-state the
  answer in one short paragraph rather than referring back to it.
