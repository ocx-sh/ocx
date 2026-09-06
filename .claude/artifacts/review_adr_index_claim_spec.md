# Review: adr_index_claim_command — spec
Summary: needs work
Focus: spec   Phase: design-record   Coverage: 14/14 contracts checked

Contracts read and checked in `## Technical Details`: CLI grammar; mutual-exclusion table;
env vars + API precedence; push precedence; GitLab header selection; `Forge` trait (2 added
operations, 1 changed return type, 7 changed-under-`git` rows); the constructor
(`WriteTransport`, `ForgeCredentials`, `validate_transport`, `client`); the claim root
(field set, order, derivation); the `--format json` report; the preflight table; the
exit-code table; the git recipe (six steps + stderr classification + commit identity); the
test fixture; the child-environment allowlist. Everything below is anchored by
`path:line` read in this worktree at `487570fb`.

3 Block, 11 High (9 actionable + 2 deferred), 7 Warn (6 actionable + 1 deferred), 1 Suggest.

## Actionable

- [Block] `adr_index_claim_command.md`:§ "API Contract — the `Forge` trait", **Invariant**
  paragraph — the git transport has no defined behaviour for `open_or_update_pull_request`
  called with **no preceding `commit_files`**, which is the announce unchanged path that
  actually runs today. `crates/ocx_lib/src/announce.rs:211-233` calls
  `open_or_update_pull_request` alone when a live branch carries unmerged commits and the
  run is otherwise a no-op (C6 amendment), and `announce.rs:199-210` returns earlier still.
  The stated invariant covers only the opposite direction ("a `commit_files` not followed by
  `open_or_update_pull_request` performs no network write"). Under `--transport git` the
  ensure-request call must then push a ref that already matches the remote — an
  already-up-to-date push, which is exactly the mechanism D-T4 rejects as unverified ("no
  source verifies that push options are delivered on an already-up-to-date push"). So
  `announce --transport git` on its most common repeat-run path lands on the one mechanism
  the ADR refuses to design against, and no tester can write the test.
  **Remediation:** state the git-transport contract for `open_or_update_pull_request` with
  no pending local commit — a REST-only ensure (naming the credential requirement, since a
  job token cannot create a merge request over REST) or an explicit named refusal — and add
  the case to Validation alongside the existing "re-run updates it" bullet.

- [Block] `adr_index_claim_command.md`:§ "Exit codes — full mapping" (the
  `ForgeCapabilityUnavailable = 86` doc comment) vs § "API Contract — the preflight" vs
  § Validation — the "instance too old" branch of exit 86 has **no detector**, and Validation
  asserts an outcome the design's own rules make unreachable. No preflight row reads the
  GitLab instance version; `ci_push_repository_for_job_token_allowed` simply does not exist
  before 18.4, which D-T9 routes to `unknown`-and-proceed; the only remaining path is the
  push-time string match, which the same document says "must never be the only path to the
  named error" (§ "The git recipe", closing paragraph). Validation nonetheless asserts "the
  fixture rejects the push as GitLab < 18.4 ... → exit 86 with the setting named".
  **Remediation:** either add a `gitlab-version` capability check with its source and state
  plainly that a bare job token cannot call it, or drop "the instance too old" from 86's doc
  comment and from the Validation bullet and say where an old instance actually lands.

- [Block] `adr_index_claim_command.md`:§ Metadata **Amends** + Constitution Check §1 — S1 is
  amended at its restatement, not at its source. `adr_announce_gitlab_forge.md:53-55` (D0)
  says S1 "**stands unchanged**" and points at the design register;
  `design_spec_announce_initiative.md:27` is S1's canonical text: "Transport = **REST API
  only**. No git subprocess, **no local clone of the index repo**." The claim ADR amends only
  the gitlab-forge ADR, so the ratified register still refuses both halves of what this
  design does, with no cross-reference. An unflagged live contradiction in the register is
  the failure this ADR's own Constitution Check exists to prevent.
  **Remediation:** add `design_spec_announce_initiative.md` S1 to **Amends**, to the
  documentation-surfaces table, and to Implementation Plan step 9; keep the gitlab-ADR D0
  amendment as the secondary edit.

- [High] `adr_index_claim_command.md`:§ Considered Options, Part 1, Option **C-B** con cell —
  false claim about existing code. "Breaks the group's single invariant — `ocx index *` is
  offline local-cache work" is contradicted by `crates/ocx_cli/src/command/index.rs:15-76`:
  `index update` "Fetches the requested packages' tags from the registry", and `index sync`
  reads each source "live … never from the local copy" and is explicitly refused by
  `--offline`. The group's contract is *already* "sometimes network, sometimes not". The
  C-A pro cell states the true invariant ("never touches a **forge**") and the two cells
  contradict each other.
  **Remediation:** restate the C-B con as the forge invariant and note the C-A verdict is
  unchanged — the surviving grounds (unit of work is a package; one shared options struct)
  carry it without the false premise.

- [High] `adr_index_claim_command.md`:§ "API Contract — environment variables and
  precedence", `OCX_ANNOUNCE_TOKEN` row — "Forwarded to children? **No**" is false against
  the code. `crates/ocx_lib/src/env.rs:238` defines `CREDENTIAL_KEYS = [OCX_IDENTITY_TOKEN,
  OCX_KEY_PASSWORD, OCX_SIGNING_KEY]`; `OCX_ANNOUNCE_TOKEN` is not in it, and
  `Env::apply_ocx_config` (`env.rs:609-614`) scrubs only that set from an otherwise inherited
  child environment (`env.rs:521`, `Env::inherited`). The variable **is** inherited — which
  is precisely the "known-open for `ocx-mirror`" the same cell names, so the cell contradicts
  its own footnote. A tester writing the assertion from this row gets a red test.
  **Remediation:** change the cell to "Yes — inherited by design; deliberately outside
  `CREDENTIAL_KEYS` (ocx-mirror ambient inheritance, Constitution Check §4)".

- [High] `adr_index_claim_command.md`:§ "Documentation surfaces", row
  `crates/ocx_lib/src/forge/gitlab.rs:148-157` — the scheduled correction covers only the
  job-token half of that comment, but D-T8 falsifies its other sentence too.
  `gitlab.rs:150-152` reads: "GitLab reads the credential from `PRIVATE-TOKEN`, which accepts
  personal, project and group access tokens; `Authorization: Bearer` accepts only OAuth2
  tokens, so it is the narrower choice, not the safer one." D-T8 replaces `PRIVATE-TOKEN`
  with `Bearer` on the claim that Bearer is a **superset**. The implementation at
  `gitlab.rs:166` changes; the comment justifying the old choice is not scheduled and would
  survive as a shipped statement contradicting the shipped code.
  **Remediation:** schedule the whole doc comment, naming the Bearer-narrowness sentence
  explicitly so it is not read as a scope the diff can skip.

- [High] `adr_index_claim_command.md`:§ "The #411 flow stays one line" (§ env precedence,
  closing line) — the headline recipe is wrong under the default transport.
  `OCX_ANNOUNCE_TOKEN=$CI_JOB_TOKEN` with no `--transport` resolves to `api`, where D-T8's
  header rule sends `JOB-TOKEN` and the ADR's own Key Insight says a job token "cannot
  compare, write, fork, or create a merge request" — so the run cannot succeed. The line
  reads as two alternatives ("…, or nothing at all under `--transport git`"), which implies
  the first form stands on its own. It is the single most copy-pasted sentence in the record.
  **Remediation:** write it as "`--transport git` with `OCX_ANNOUNCE_TOKEN=$CI_JOB_TOKEN`, or
  `--transport git` with no variable at all".

- [High] `system_design_index_claim_command.md`:§ 5 "Error taxonomy" vs
  `adr_index_claim_command.md`:§ "Exit codes — full mapping" — four declared variants appear
  in no mapping row: `ForgeError::GitCommandFailed`, and `ClaimError::{ForgeRequired,
  MissingBaseRef, MissingHeadRoot}`. A table titled "full mapping" that omits declared
  variants is not testable. (Silence may be deliberate: `AnnounceError::MissingBaseRef` and
  `MissingHeadRoot` are likewise unclassified at `crates/ocx_lib/src/announce/error.rs:208-259`
  and fall through to `Failure`.)
  **Remediation:** add the four rows, or add one sentence stating they inherit announce's
  fall-through to exit 1 by the same rule.

- [High] `system_design_index_claim_command.md`:§ 5 "Owner-resolution ladder", arm 3 — no
  failure path. The arm specifies `None` → `ClaimError::NoActingIdentity` (64), but
  `authenticated_identity`'s own documented `# Errors`
  (`adr_index_claim_command.md`, § `Forge` trait) includes `ForgeError::UsersApiUnavailable`
  "when the credential may not call the identity endpoint at all (a GitLab job token)" — the
  exact posture the report's `author: null` case describes and the default #411 flow. Whether
  that error propagates, is swallowed into arm 4, or maps to 64 is unspecified.
  **Remediation:** add the arm-3 error row with its exit code and message; the ladder's
  "first arm that yields a list wins" rule does not cover an `Err`.

- [High] `adr_index_claim_command.md`:§ Validation, "Claim and announce over `--transport
  git`" bullet — the #399 carried-tags assertion is silently dropped for announce. The
  dossier's Verification line reads "a spent branch is rebuilt on base **with carried tags**";
  this bullet reads "a spent branch is rebuilt on base with a lease-checked update". Dropping
  the carry is correct for claim (D-C7: content is fully derived from flags) and wrong for
  announce, which the same bullet covers and where the carry is the `6feb9c02` regression
  guard (`crates/ocx_lib/src/announce.rs:343-345`, "Skipping this would drop on the retry
  exactly what the first attempt carried").
  **Remediation:** split the bullet per command, restoring "with carried tags" on the
  announce half.

- [High] `adr_index_claim_command.md`:§ "API Contract — the preflight" vs dossier
  § Requirements "Version gating" — the dossier's "cross-project push ≥ 19.1" requirement has
  no check and is not flagged as dropped. The preflight tests allowlist *membership*
  (`GET /projects/:id/job_token_scope/allowlist`), which can pass on 19.0 where the feature
  does not exist. Related to the second Block but separately reachable.
  **Remediation:** name it in the preflight table, or fold it into the explicit statement
  that the instance version is not readable under a job token and say where 19.0 lands.

- [High] `adr_index_claim_command.md`:§ "Migration and Rollout" (Documentation surfaces) +
  § Constitution Check §8 — the two sibling announce credentials are given **opposite**
  child-forwarding policies with no rationale. `OCX_ANNOUNCE_GIT_TOKEN` "**must be added to**
  `CREDENTIAL_KEYS`" while `OCX_ANNOUNCE_TOKEN` is deliberately outside it
  (`crates/ocx_lib/src/env.rs:238`). For the deploy-token posture the dossier's decision 9
  works through, a wrapper that inherits one credential and has the other scrubbed falls
  silently through push-precedence step 2 to the API credential — a different identity
  authoring the push, with no error.
  **Remediation:** one sentence stating why the new variable is scrubbed and the old one is
  not, and whether the silent fall-through to step 2 is intended when step 1's variable was
  scrubbed by an outer ocx.

- [Warn] `adr_index_claim_command.md`:§ "The git recipe" step 5 and § Validation vs dossier
  § Requirements "Transport" — the push carries **four** merge-request options where the
  dossier specified three (`.description` added; the dossier's Verification says "the three
  push options"). Additive and internally consistent throughout the ADR, but not listed as a
  deviation. **Remediation:** one line in the deviation set.

- [Warn] `adr_index_claim_command.md`:§ Metadata **Amends** (D1) — the same stale operation
  count the ADR corrects in code also lives in the ADR being amended.
  `adr_announce_gitlab_forge.md:59` reads "`Forge` (`forge/api.rs`) is exactly the **ten**
  operations `announce()` drives", matching `crates/ocx_lib/src/forge/api.rs:7`. Only the
  code sentence is scheduled. **Remediation:** apply the "stop naming a count" fix to the
  ADR text in the same step.

- [Warn] `adr_index_claim_command.md`:§ "Quantified Impact", `Exit codes` row — "79–85 used →
  79–86 used" understates the shipped interface. `crates/ocx_lib/src/cli/exit_code.rs:26-58`
  also defines 64, 65, 69, 74, 75, 77 and 78, and the claim command's own exit-code table
  uses six of them. **Remediation:** say "OCX-specific range 79–85 → 79–86".

- [Warn] `adr_index_claim_command.md`:§ Context and § "Data Model — the claim root" — two
  off-by-one citation counts. `crates/ocx_lib/src/announce/pipeline.rs` is **2040** lines
  (ADR: 2041); `test/manual/announce-e2e/CLAIM.md` is **172** lines (ADR: 173). Immaterial to
  the decision, and named only because this record's argument style rests on exact citation.
  **Remediation:** correct or drop both numbers.

- [Warn] `adr_index_claim_command.md`:§ "API Contract — the `--format json` report", `author`
  field, vs § "Exit codes" — the report documents `author: null` for "a bare job token with
  no CI user variables", but under the owner ladder that same state cannot produce owners
  either, so a run reaching a `null` author must have carried `--owner`. The two contracts are
  reconcilable but neither says so, and a tester reading the report field alone would build an
  unreachable fixture. **Remediation:** add "(reachable only with an explicit `--owner`)".

- [Warn] `system_design_index_claim_command.md`:§ 12 "Implementation Phases" — "Phases 1–3 are
  file-disjoint from phase 4 onward and can run in parallel" is not derivable from the phase
  contents. Phase 3 creates `api/data/claim.rs` and adds "announce's report fields"; phase 4
  ends at "Preflight and `capability_checks`", which the ADR's step 7 wires "into **both**
  reports" — the same two files. **Remediation:** drop the parallelism claim, or move the
  report wiring wholly into one phase.

- [Suggest] `adr_index_claim_command.md`:§ "Implementation Plan" (9 steps) vs
  `system_design_index_claim_command.md`:§ 12 (5 phases) — the two orderings are stated
  independently with no mapping, and the design's phases do not partition the ADR's steps
  cleanly (ADR step 7 straddles design phases 3 and 4). **Remediation:** one mapping line, or
  make the design's § 12 a pointer rather than a restatement.

## Deferred

- [High] `adr_index_claim_command.md`:§ Constitution Check §1 + § Non-Functional Requirements
  (Latency) — S1's own rationale anchor at `design_spec_announce_initiative.md:27` is "ocx
  ships dep-free; index carries CAS objects (**clone cost grows**)". This ADR amends S1 while
  recording clone size as "**unmeasured**" and deferring the measurement to a release gate.
  **Question for the human:** is amending the owner's 2026-07-22 REST-only decision acceptable
  before the measurement its stated rationale rests on, or should the blobless-clone
  measurement against `ocx-sh/index` gate *acceptance of this ADR* rather than the 0.6.1
  release?

- [High] `adr_index_claim_command.md`:§ "Security Architecture", STRIDE **Spoofing** row, and
  `system_design_index_claim_command.md`:§ 5 "Bot refusal" — both state that the residual
  (a human-minted PAT on a shared release account) is caught by "the G-04 human reviewer
  reading the rendered `login:id` list", and the request body is shaped to make that possible.
  That control lives in the `ocx-sh/index` repository, which this ADR does not read.
  **Question for the human:** does the index's reviewer checklist require verifying `owners[]`
  against forge profiles today, or is this an assumed control that must be added to
  `governance-contracts.md` in the same batch?

- [Warn] `adr_index_claim_command.md`:§ Constitution Check §5 — the `claim <ns>/<pkg>`
  positional versus `announce --package` flag split is accepted and the announce alignment
  deferred to "a separate, batched rename". `crates/ocx_cli/src/command/package_announce.rs:43`
  confirms `--package` is a required flag today. **Question for the human:** confirm that the
  batched-window carve-out in `CLAUDE.md` is the intended vehicle — a `--package` → positional
  change on announce is a CLI-grammar break needing the hidden-alias, warn-once,
  named-removal-release treatment, and the ADR does not name the release pair.

## Verified claims (path:line evidence)

Every claim this record makes about existing code was opened and read. All of the following
are **correct as written**:

- `crates/ocx_lib/src/cli/exit_code.rs:93` — `UnsupportedKeyBackend = 85`. 85 **is** taken; 86
  is the first free slot. The operability lane's 85 is stale, as the ADR says.
- `crates/ocx_lib/src/cli/exit_code.rs:32-35` — `Unavailable = 69`: "Rerunning the same command
  will not change the outcome". The ADR's "the operability lane's premise does not hold *here*"
  is exact.
- `crates/ocx_lib/src/cli/exit_code.rs:47-48` — `PermissionDenied = 77`: "Insufficient
  permissions: filesystem `EPERM`". The doc-widening requirement is real.
- `crates/ocx_lib/src/cli/exit_code.rs:81-84` — `ReferrersUnsupported = 84`, "reachable but
  missing a capability, no fallback". The cited precedent holds. Enum is `#[non_exhaustive]`
  (`exit_code.rs:19`).
- `crates/ocx_lib/src/forge/api.rs:117` — "an implementation that cannot hold it must return an
  error rather than approximate it", quoted verbatim and correctly.
- `crates/ocx_lib/src/forge/api.rs:7` — module doc says "**ten** operations". The trait actually
  declares **eleven** (`api.rs:132,145,155,173,197,211,226,239,254,269,291`). Both the staleness
  claim and the "correct interim value is eleven" parenthetical are right.
- `crates/ocx_lib/src/forge/api.rs:254` — `ensure_push_access` returns `Result<(), ForgeError>`;
  the return-type change is real. `api.rs:239` — `sync_fork` returns no `Result` and is
  best-effort by contract, as D-T6 states.
- `crates/ocx_lib/src/forge/api.rs:256-268` — `commit_files`'s contract does say a moved base
  under `FastForward` **MUST** surface as `NonFastForward`; D-T4's "the contract itself is
  rewritten" is an accurate description of the change.
- `crates/ocx_lib/src/announce.rs:292-306` (`commit_files`), `:308` (`NonFastForward` arm),
  `:405-414` (`open_or_update_pull_request`) — **the D-T4 correction is confirmed**. Today's
  retry wraps only `commit_files`, and the ensure-request call sits outside the `match`. Under
  `git`, `NonFastForward` would surface at `:405` and be missed. The retry must widen to the
  pair, exactly as stated.
- `crates/ocx_lib/src/announce.rs:110-121` — `read_committed_root` runs for **every** target
  including `AnnounceTarget::Out`; `crates/ocx_cli/src/command/package_announce.rs:31-38`
  confirms it in the help text. **The claim `--out` correction is confirmed**: announce's
  `--out` already reads the committed root over the forge, and the dossier's "without touching
  a forge" is wrong.
- `crates/ocx_lib/src/forge/api.rs:82-87` — `RefUpdate::Reset` is "repoint the ref even when the
  new commit is not a descendant". D-C7 reuses it unchanged; nothing in the ADR alters the
  enum's semantics.
- `crates/ocx_lib/src/announce/error.rs:234` — `UnclaimedNamespace` → `NotFound` (**79**), with
  its pinning test at `:297-302`. Option C-C's "exit 79, pinned by test" is exact.
- `crates/ocx_lib/src/announce/error.rs:224,252` — `DescDisappeared` and
  `PullRequestUnmergeable` both → `DataError` (65). The "why 65" argument rests on a real
  precedent. `:259` — `OutputWrite` → `IoError` (74), matching the claim table.
- `crates/ocx_lib/src/announce/error.rs:19-35` — `ClassifyExitCode` is implemented explicitly
  because `#[error(transparent)]` makes the generic walker skip the wrapped error. The system
  design's § 5 rationale for `ClaimError` doing the same is correct and correctly attributed.
- `crates/ocx_lib/src/forge/error.rs:203-245` — the exit mappings the ADR reuses all exist:
  401/403 → 80, 429 → 75, 5xx → 69, `Transport` → 69, `NonFastForward` → 75,
  `PushAccessDenied` → 80, the four usage variants → 64, and the `_ => None` fall-through the
  "why 1 for an unrecognised push failure" argument cites.
- `crates/ocx_lib/src/forge/error.rs:165-201` — `status_detail` trims, replaces the token with
  `[redacted]`, and caps at 300 chars on character boundaries. The "existing trim/cap/redact
  applies" claim is accurate.
- `crates/ocx_lib/src/forge/gitlab.rs:148-160` — the job-token comment is where the ADR says,
  and says what the ADR says it says. `gitlab.rs:166` sends `PRIVATE-TOKEN`;
  `github.rs:128` already sends `Bearer`, so "GitHub's client is unchanged" holds.
- `website/src/docs/reference/environment.md:128` — the job-token line is at that exact line and
  carries the claim the ADR corrects.
- `crates/ocx_lib/src/announce/pipeline.rs:122-135` — `require_root` raises
  `UnclaimedNamespace` on `None`, at the cited range.
- `crates/ocx_lib/src/oci/index/wire.rs:145-159` — `IndexRoot` models `repository`, `tags`,
  `status`, `deprecated_message`, `superseded_by` and **no `owners` field**; `wire.rs:480-487`
  states it outright. `oci/index/wire_writer.rs:53` confirms every human-governed field rides
  through as an opaque `Value`. The "ocx never parses owners" premise of D-W1/D-W2/D-W3 holds.
- `crates/ocx_lib/src/oci/index/wire_writer.rs:47-61` — `serialize_root` is re-exported as
  `oci::index::serialize_root` and guarantees 2-space indent, insertion order, `ensure_ascii`,
  one trailing newline. Exactly as the Data Model section states.
- `crates/ocx_lib/tests/fixtures/index_wire/root/full-fields.json` — field order is `name`,
  `repository`, `owners`, `status`, `deprecated_message`, `created`, `desc`, `upstream`,
  `superseded_by`, `tags`. **The ADR's claim-root example matches**, with `superseded_by`
  correctly omitted.
- `crates/ocx_lib/src/forge/kind.rs:117-123` — `ForgeKind::client` is the **only** construction
  site of `GitHubForge`/`GitLabForge` outside their defining files (grepped across `crates/`).
  The "single constructor" property T-D relies on is real, and the signature change is real.
- `crates/ocx_lib/src/env.rs:238` — `CREDENTIAL_KEYS` membership as described; `env.rs:609-614`
  is the scrub site; `env.rs:521` is `Env::inherited`. (See the High finding above for where
  the ADR's table misreads this.)
- `crates/ocx_lib/src/announce/pipeline.rs:103-109` — `__OCX_TESTING_ANNOUNCE_CLOCK` exists as
  the pinnable clock seam the `created` derivation reuses.
- `crates/ocx_lib/src/utility/child_process.rs:87-98,128-141` — both `exec` and `spawn_and_wait`
  call `env_clear()`; the "`GIT_TRACE` cannot reach the child by inheritance" argument is sound,
  and the `spawn_and_wait`-not-`exec` requirement (RAII must run) matches the module's own doc.
- `crates/ocx_cli/src/command/package_announce.rs:43` — `--package` is a required flag;
  Constitution Check §5's asymmetry is real. `:80-91` — `--out` and `--fork` already conflict.
- `6feb9c02` — `pull_request_mergeability` was introduced there
  (`git log -S`, single hit on `forge/api.rs`), so the "ten" count went stale exactly then.
- `test/tests/fake_forge.py` and `fake_gitlab.py` both exist; the fixture-extension plan targets
  a real file.
- **Retracted citation handled correctly.** Homebrew discussion #3383 appears three times and
  every one is a retraction (`adr_index_claim_command.md:147-149`, `:1220`,
  `research_index_claim_council_transport.md:50`). No surviving load-bearing citation.
- **Every research citation resolves.** All eight artifacts named in § "Industry Context &
  Research" and all four prior ADRs linked in § Links exist on disk in `.claude/artifacts/`.
- **Dossier fidelity, decisions 1–9.** All nine `## Decisions` are honoured. The three deviations
  the architect declared are each correct against the code and each explicitly flagged in the
  ADR: exit 86 over 85 (§ "Exit codes", "**85 is already taken**") and over the dossier's
  recommended 69 (§ "Why a new code rather than 69"); the `commit_files`/`NonFastForward` shift
  with the widened retry (D-T4, "**Consequence:**"); and `--out` reading the committed root
  (§ "**`--out` still reads the forge**", "This corrects the dossier's …"). The two prose
  corrections the dossier needed — `--format` as a root flag, `--out` and the forge — are both
  labelled as corrections at the point of use.
- **Prior-decision consistency.** `adr_announce_gitlab_forge.md` D1 (trait, orchestration never
  learns the forge) is honoured by T-D and is the stated ground for rejecting T-A and T-B; D3
  (declared forge kind, never probed) is carried unchanged into the CLI grammar table; D9/D15
  (`adr_announce_gitlab_forge.md:215,332`) are honoured — the credential is redacted from forge
  bodies via `status_detail` and git stderr goes through the same cap.
  `adr_announce_diverged_branch_rebuild.md`'s `RefUpdate::Reset` semantics are reused unchanged
  under `git` (lease-checked), and the widened retry **strengthens** rather than contradicts
  C4/C15: the local commit is atomic and the single push is one all-or-nothing ref update.
- **Trade-off matrices.** Four matrices, each ≥ 3 options (4, 4, 4, 4) against one shared
  weighted criterion set (D1–D6, weights 5/5/4/4/3/3), with the two places the weighted sum is
  overridden named explicitly (C-D "rejected on the one axis a weighted sum cannot express";
  O-A "a criterion set that cannot see 'the manual path is unusable' is measuring the wrong
  thing"). NFR lines, migration/rollout, and the documentation-surfaces table are all present,
  and the table covers every surface the dossier's § "Docs surfaces" names, including the new
  use-case page. Open questions: three in the ADR and one in the system design, each with a
  `Recommended:` or an inline answer. The two code-comment corrections
  (`forge/gitlab.rs:148-157`, `environment.md:128`) and the `Forge` doc-count fix are all
  scheduled in Implementation Plan step 1 and system-design Phase 1.
