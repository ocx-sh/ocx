# Review: adr_index_claim_command — spec, round 2 (re-validation)

**Date:** 2026-09-05
**Author:** reviewer:spec r2 (Opus 5)
**Inputs:**
- `.claude/artifacts/adr_index_claim_command.md` (1920 lines, revised)
- `.claude/artifacts/system_design_index_claim_command.md` (721 lines, revised)
- `.claude/artifacts/review_adr_index_claim_spec.md` (round-1 list: 3 Block, 11 High, 7 Warn, 1 Suggest)
- `.claude/artifacts/review_adr_index_claim_quality.md` (Block: `mktree` depth), `review_adr_index_claim_security.md` (Block: `spawn_and_wait` stdio)
- `.agents/discussions/index-claim-command.md` (read as data — the nine ratified `## Decisions`, Requirements, Verification)
- Code read in this worktree at `487570fb`: `forge/api.rs`, `forge/gitlab.rs`, `forge/error.rs`, `announce.rs`, `announce/error.rs`, `announce/pipeline.rs`, `utility/child_process.rs`, `env.rs`, `cli/exit_code.rs`, `oci/index/ocx_index.rs`, `oci/ssrf.rs`, `ocx_cli/src/command/index.rs`, `design_spec_announce_initiative.md`

Round-1 IDs are assigned here in the order the round-1 file lists them (it numbered nothing):
B1–B3 = its three Blocks, H1–H9 = its nine actionable Highs, H10–H11 = its two deferred
Highs, W1–W6 = its six actionable Warns, W7 = its deferred Warn, S1 = its Suggest. B4 = the
security lane's Block, B5 = the quality lane's Block.

**Matrix arithmetic re-computed from the weights (5/5/4/4/3/3/5/3 = 32).** All sixteen totals
are correct as printed, and in all four parts the highest total is the option the prose marks
*(chosen)*: C-A 150, **O-B 144 over O-A 141**, T-D 138, W-B 149. The "ten of the thirty-two
weight-points" sentence is also arithmetically right (D1 + D2 = 10).

---

## Disposition of round-1 findings

| ID | Round-1 claim | Ruling | Anchor | Evidence |
|---|---|---|---|---|
| **B1** | Git contract undefined for `open_or_update_pull_request` with no preceding `commit_files` | **partially closed** | ADR § D-T4, "**The other direction, which announce actually exercises today**"; design § 5 "Branch state machine (claim)" | The ADR states the contract (REST read of open MRs first, no write when one exists, else a refresh commit) and adds the Validation bullet. Verified the premise: `announce.rs:212-233` calls `open_or_update_pull_request` alone when `root_read.branch_sha.is_some()`. But the design carries no refresh-commit path at all: its state-machine row `Ahead`, content byte-identical reads "commit nothing; ensure the request" with ref update "—", and its git sequence diagram goes straight from `commit_files` to the push. See **N-B1**. |
| **B2** | Exit 86 "instance too old" has no detector; Validation asserts an unreachable outcome | **partially closed** | ADR § "Exit codes — full mapping", "**'The instance is too old' is deliberately absent from that doc comment**"; ADR § "The git recipe", stderr-classification table | The doc comment no longer claims it, D-T9 states the two-signal rule, and Validation now asserts the pair. But the classifier that must raise the second signal has four rows — `(fetch first)`, `(stale info)`, `pre-receive hook declined`, `anything else` — and the last routes to `GitPushFailed`, exit 1. Nothing produces "refused with the capability-gate shape". See **N-B2**. |
| **B3** | S1 amended at its restatement, not its source | **closed** | ADR § Metadata **Amends** first bullet; § Constitution Check §1; docs table row `design_spec_announce_initiative.md` **S1** (`:27`); Implementation Plan step 9; design Phase 5 | Verified `design_spec_announce_initiative.md:27` is exactly the S1 row, text "Transport = **REST API only**. No git subprocess, no local clone of the index repo." It is now the primary Amends entry in all five places; the gitlab-ADR D0 edit is named secondary. |
| **B4** (security) | `spawn_and_wait` inherits stdio and returns only `ExitStatus` | **closed** | ADR § Security Architecture §4, "**A capturing subprocess helper, never `exec` and not `spawn_and_wait` either**"; design § 2 container table, § 7 "How the child's output is captured", § 10 internal dependencies | Verified `child_process.rs:138-151`: `.stdin/.stdout/.stderr(inherit)`, returns `io::Result<ExitStatus>`. Both artifacts now mandate the capturing sibling returning `(ExitStatus, Vec<u8>, Vec<u8>)` with `env_clear()`, signal forwarding and `kill_on_drop` preserved, and the redactor takes a slice including the `base64(user:secret)` form. One stale sentence survives — see **N-W1**. |
| **B5** (quality) | A single `mktree` cannot write a three-deep path | **closed** | ADR § "The git recipe" step 4 and "**Step 4 is an index-file chain, not `mktree`**"; design § 3 `git_workspace.rs` row; design git sequence diagram | Recipe is `hash-object -w` → `GIT_INDEX_FILE` `read-tree` → `update-index --add --cacheinfo 100644,<blob>,p/<ns>/<pkg>.json` → `write-tree` → `commit-tree` → `update-ref`. Design's component row and sequence node both restate the same chain. No `mktree` remains in either file. |
| **H1** | C-B's offline-purity con is false against `index update` / `index sync` | **partially closed** | ADR § Considered Options Part 1, C-B third con (struck through) and C-A first pro | The con is withdrawn in place and C-A's pro now states the forge invariant with the correct facts. Verified `ocx_cli/src/command/index.rs`: `update` "Fetches the requested packages' tags from the registry", `sync` reads "from the source live" and is refused by `--offline`. But D-C1's own mitigation still leans on the withdrawn premise — see **N-H1**. |
| **H2** | `OCX_ANNOUNCE_TOKEN` "Forwarded to children? **No**" is false | **closed** | ADR § "environment variables and precedence", `OCX_ANNOUNCE_TOKEN` row | Row now reads "**Yes — inherited by design**", citing `env.rs:238`. Verified: `CREDENTIAL_KEYS = &[OCX_IDENTITY_TOKEN, OCX_KEY_PASSWORD, OCX_SIGNING_KEY]` — the announce token is not a member. |
| **H3** | Only the job-token half of the `gitlab.rs` comment was scheduled | **closed** | ADR § "Documentation surfaces", row `crates/ocx_lib/src/forge/gitlab.rs:148-157`, "**The whole doc comment, not only its job-token half**" | The row now names both wrong sentences, including "`Authorization: Bearer` accepts only OAuth2 tokens, so it is the narrower choice". Verified the comment at `gitlab.rs:148-160` says exactly that, and D-T8 repeats the schedule. Range is three lines short — see **N-S1**. |
| **H4** | The "#411 flow" one-liner does not work under the default transport | **closed** | ADR § "**The #411 flow, written so it works as pasted**" | Both forms now carry `--transport git`, with a comment stating neither works under `api` "where a job token cannot create a merge request at all". |
| **H5** | Four declared variants missing from the "full mapping" table | **closed** | ADR § "Exit codes — full mapping", last four rows + "**The last four are unclassified on purpose**" | `ForgeError::GitCommandFailed` and `ClaimError::{ForgeRequired, MissingBaseRef, MissingHeadRoot}` are all present at exit 1 with the announce precedent cited. Verified `announce/error.rs:164` and `:173` declare `MissingBaseRef` / `MissingHeadRoot`, and neither appears in the `ClassifyExitCode` match ending `_ => None` at `:208-260`. |
| **H6** | Owner ladder arm 3 has no failure path for `UsersApiUnavailable` | **closed** | design § 5 "**Errors from the confirming call itself**"; step-2 table rows "Users API unreachable, …" | The design states arm 3's `UsersApiUnavailable` is not propagated — it means "unreachable", step 1 falls to arm 4 (`NoActingIdentity`, 64), and it becomes a real error only where step 2 says exit 64. The ADR's exit table carries both rows. |
| **H7** | The #399 carried-tags assertion was dropped for announce | **closed** | ADR § Validation, "**Announce over `--transport git`**, separately" | The bullet is split per command and restores "a spent branch is rebuilt on the index base with **its tag delta carried forward**", with the claim omission justified by D-C7. Verified the guard it protects: `announce.rs:342-344`, "Skipping this would drop on the retry exactly what the first attempt carried." |
| **H8** | Cross-project push ≥ 19.1 has no check and is not flagged as dropped | **closed** | ADR § D-T9, "**Where 19.0 lands**" | Folded into the version-unreadable statement: the allowlist endpoint exists on 19.0, so `job-token-allowlist` can report `passed` while cross-project push still fails; that run reaches the push and lands on the two-signal rule. Inherits B2's defect but the disclosure asked for is present. |
| **H9** | Two sibling credentials get opposite forwarding policies with no rationale | **closed** | ADR § "**Why the two sibling credentials get opposite policies, and what that buys**" | States the standing ocx-mirror exemption, admits the containment is "nominal in the default posture" because push precedence step 2 falls back to the API credential, and declares the fall-through intended even when an outer ocx scrubbed step 1's variable, with the stderr warning as the visibility control. |
| **H10** (deferred) | Amending S1 before the clone-cost measurement its rationale rests on | **carried** | ADR § "Deferred to handoff", second bullet | Intact and addressed to the owner; NFR Latency and the Validation gate both label the size "unmeasured". |
| **H11** (deferred) | The G-04 reviewer control lives in `ocx-sh/index`, unread here | **carried** | ADR § "Deferred to handoff", third bullet | Intact, naming `governance-contracts.md` as the place it must be added if absent. |
| **W1** | Four push options where the dossier specified three | **closed** | ADR § "Deviations from the ratified dossier", row 4 | Listed as "Additive — the reviewer needs the rendered `login:id` in the request body". Dossier Requirements confirms three (`.create`, `.target`, `.title`). |
| **W2** | The stale "ten operations" count also lives in `adr_announce_gitlab_forge.md:59` | **open** | ADR § Metadata **Amends** (D1); § "Documentation surfaces"; Implementation Plan step 9 | Amends names D1, but the only scheduled edit is `crates/ocx_lib/src/forge/api.rs` module doc. Step 9 schedules the D0 restatement in that ADR and not D1's count sentence. Verified `forge/api.rs:7` says "ten" and the trait declares eleven `async fn`s. |
| **W3** | "79–85 used → 79–86 used" understates the shipped range | **closed** | ADR § "Quantified Impact", `Exit codes` row | Now "OCX-specific range 79–85 → 79–86" with a note that the sysexits range 64–78 is unchanged and these commands already use six of it. |
| **W4** | Two off-by-one citation counts | **closed** | ADR § Considered Options T-C pro; § Context | `announce/pipeline.rs` now cited as 2040 lines — `wc -l` returns 2040. The `CLAIM.md` line count is gone rather than corrected, which is equally fine. |
| **W5** | `author: null` reads as reachable without `--owner` | **closed** | ADR § "the `--format json` report", `author` row | Now "**reachable only with an explicit `--owner`**, since the same state yields no detected owner either". |
| **W6** | "Phases 1–3 can run in parallel" is not derivable | **closed** | design § 12, "**These phases are sequential, not parallel.**" | Withdrawn explicitly, with the reason (phase 3 and phase 4 both land in `announce.rs` and the shared report type) and the two genuinely independent pieces named. |
| **W7** (deferred) | The `announce --package` → positional rename needs a named release pair | **carried** | ADR § Constitution Check §5; § "Deferred to handoff" | The record states it deliberately does not name the pair and routes the vehicle to the owner. Verified `package_announce.rs:43` still makes `--package` a required flag. |
| **S1** | No mapping between the ADR's nine steps and the design's five phases | **closed** | design § 12, "**P1 = step 1, P2 = steps 2–3, P3 = steps 4–5, P4 = steps 6–7, P5 = steps 8–9**" | Mapping is explicit. One phase-3 item has no corresponding step — see **N-H3**. |

**Recommended fixes for the two partial closures** (they have no separate new-finding ID, to
avoid double counting):

- **B1** — add the byte-identical/no-open-request case to the design's branch state machine as
  its own row ("`Ahead`, byte-identical, no open request → refresh commit on the branch head,
  `FastForward`"), and add the MR-read-first node to the git sequence diagram.
- **B2** — add a fifth row to the git stderr classification table: when `job-token-push`
  reported `unknown`, an otherwise-unclassified push rejection maps to
  `ForgeError::WriteCapabilityUnavailable` (86) instead of `GitPushFailed` (1), and say that
  the promotion is driven by the preflight result rather than by a phrase.

---

## New findings

### Block

- **N-B1 — the git version gate cannot run "before any network call" as the design sequences it.**
  Anchor: ADR § "API Contract — the preflight", `git-version` row ("**before any network call**")
  and § "The git recipe" step 1; design § 3 "Sequence — the same claim over the git transport".
  The ADR asserts the ordering five times (preflight table, exit-code table, D-T10, Risks,
  Validation's "**zero** network calls recorded"), but the design's own sequence performs
  `get_file_contents` / `get_ref_sha` and `ensure_push_access`'s `GET /projects/:id` before
  `GL->>WS: ensure workspace`, and the workspace is where `git --version` runs — the ADR itself
  says the clone is "created lazily on the first git-half operation".
  *Failure scenario:* a runner with no `git` binary, `--transport git`. The design's order makes
  three REST calls, then fails at 69. The Validation item "a missing `git` binary → named error
  at exit 69 with **zero** network calls recorded" fails against a correct implementation, and a
  fixer cannot tell which of the two artifacts to change.
  *Fix:* run the version gate at the CLI argv-fault boundary, alongside `validate_transport`,
  before the forge is constructed; move the `git-version` node above the first REST arrow in the
  design sequence, and say in the preflight table that this one check is not part of
  `ensure_push_access` even though it is reported in `capability_checks`.

### High

- **N-H1 — D-C1 prescribes shipping help text this ADR proves false.**
  Anchor: ADR § Decision Outcome, D-C1, "the `ocx index` group's help states that index
  subcommands are local-cache operations"; contradicted by § Considered Options Part 1, C-A first
  pro. The fix round withdrew the C-B con as false but left the mitigation that rests on it.
  Verified against `ocx_cli/src/command/index.rs`: `catalog` lists repositories in the registry,
  `update` fetches tags from the registry, `sync` reads sources live and is refused by
  `--offline`. Only `regenerate` consults no source.
  *Failure scenario:* the implementer writes the prescribed group help; a user reads "local-cache
  operations", runs `ocx index sync` offline, and gets exit 81. The record ships a documented lie
  in a `--help` string, which `quality-cli-help.md` treats as contract text.
  *Fix:* restate D-C1's mitigation as the forge invariant — "the `ocx index` group's help states
  that no index subcommand writes to a forge" — and keep the did-you-mean hint unchanged.

- **N-H2 — `ClaimOutcome` cannot carry `owner_identity_source`, so the CLI cannot render it.**
  Anchor: design § 5 "Request and outcome types", the `ClaimOutcome` struct; ADR § "the
  `--format json` report", `owner_identity_source` row, and § D-C4 rule 2. The field was added by
  the fix round and made mandatory in three places (the JSON report, the stderr owner line, the
  merge-request body), but the outcome type the library returns has ten fields and none of them
  is it. Every other report field is either present on `ClaimOutcome` or derivable in the CLI
  (`forge`, `transport`, the two credential kinds); this one is produced by `claim/owners.rs` and
  cannot be recomputed downstream — the CLI does not know whether the users API answered.
  *Failure scenario:* the stub phase compiles `ClaimOutcome` from the design, phase 3 wires the
  report, and the field is silently emitted as a constant or dropped. The G-04 reviewer then sees
  no provenance label on exactly the bare-job-token path D-C4 rule 2 exists for.
  *Fix:* add `pub owner_identity_source: OwnerIdentitySource` to `ClaimOutcome` and name the enum
  (`Resolved | Asserted | CiEnvironment`) beside `ClaimStatus` in the same section.

- **N-H3 — no implementation step owns widening announce's retry scope.**
  Anchor: design § 12 Phase 3, fourth bullet ("the widened retry scope"), against ADR
  § Implementation Plan steps 4 and 5, which Phase 3 claims to be. Step 4 is `ocx_lib::claim`,
  step 5 is the CLI plus `--transport` on announce and the announce report fields; neither names
  the change to `announce.rs`'s `match` around `commit_files`. Step 2 covers the *contract text*
  only, step 6 covers the workspace re-fetch. The design's own risk table then says "The
  retry-scope change is its own reviewable step" — a step that does not exist.
  Verified the change is real and load-bearing: `announce.rs:292-306` is the `commit_files` call,
  `:307` opens the `NonFastForward` arm, and `open_or_update_pull_request` sits at `:404-414`
  outside the `match`. Under `git` a `NonFastForward` raised at `:404` is missed.
  *Failure scenario:* the highest-impact row in the design's own risk table ("silently loses a
  concurrent announce under `git`") ships unimplemented, because no checklist item fails when it
  is skipped.
  *Fix:* add the retry widening as its own numbered step between the current 5 and 6, with the
  commit subject already drafted (`fix(announce): widen the non-fast-forward retry to the
  commit-and-open pair`), and repoint Phase 3's bullet at it.

- **N-H4 — the two artifacts scope the corporate-CA follow-up to different "four variables".**
  Anchor: ADR § Security Architecture §2, "make the forge REST client honour the same four
  variables the git half already honours", and Implementation Plan step 9, both naming
  `GIT_SSL_CAINFO` / `GIT_SSL_CAPATH` / `SSL_CERT_FILE` / `SSL_CERT_DIR`; design § 7 "Unsupported
  deployment", which allowlists "`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` and `SSL_CERT_FILE`
  (plus `HOME`…)" and then scopes the issue to "the same four variables the git child is given".
  Three of the design's four are proxy settings that the REST client's TLS trust has nothing to
  do with, and the design's list also omits `all_proxy` and the two `GIT_SSL_*` names the ADR's
  allowlist table does pass through.
  *Failure scenario:* the follow-up issue is filed from the design's wording and asks for proxy
  support the `reqwest` client already has, leaving the actual gap — a self-managed GitLab behind
  an internal CA, which the ADR calls out as "will otherwise be discovered as a surprise bug" —
  unaddressed and marked done.
  *Fix:* replace the design's parenthetical list with a pointer to the ADR's child-environment
  allowlist table, and name the four CA variables verbatim in the follow-up sentence.

### Warn

- **N-W1 — the child-environment allowlist paragraph still credits `spawn_and_wait`.**
  Anchor: ADR § Security Architecture §4, "**Child environment allowlist.** `spawn_and_wait`
  clears the environment, so every variable is explicit" — three bullets after the same section
  says `spawn_and_wait` "is not used on this path". The clearing is a property of the capturing
  sibling here. Left as is, a reviewer grepping for `spawn_and_wait` on the git path reads the
  allowlist rationale as sanctioning it. *Fix:* name the capturing helper in that sentence.

- **N-W2 — the "Changed contracts" row contradicts D-T4's second direction.**
  Anchor: ADR § "API Contract — the `Forge` trait", changed-contracts table,
  `open_or_update_pull_request` under `git`: "performs the single push carrying the ref update
  and the merge-request push options". D-T4 adds a branch where the operation performs a REST
  read and **no write at all**. The table is the row an implementer copies into the trait doc.
  *Fix:* append "…, except with no pending local commit, where it first reads the open merge
  requests over REST and writes nothing if one exists (D-T4)".

- **N-W3 — `CheckStatus::Failed` has no producer.**
  Anchor: ADR § "API Contract — the `Forge` trait", `pub enum CheckStatus { Passed, Failed,
  Unknown, Skipped }`, against § "API Contract — the preflight", whose four rows all either pass,
  report `unknown`, report `skipped`, or raise a `ForgeError`. The report is emitted on success,
  so nothing ever serialises `failed`. The ADR's own "What is genuinely one-way" table lists the
  `CheckStatus` wire spellings as unremovable once shipped. *Fix:* either drop the variant or add
  the case that emits it (a check that failed on a run that nevertheless succeeded).

- **N-W4 — the Deviations table understates two dossier departures.**
  Anchor: ADR § "Deviations from the ratified dossier", `PRIVATE-TOKEN` row, "(the Requirements
  paragraph, not a `## Decision`)". The dossier asserts the Bearer switch twice — Requirements
  *and* its Verification bullet "a non-job token → `Authorization: Bearer`" — so a tester working
  from the dossier's own checklist still writes the wrong assertion. Separately, the dossier's
  Transport prose says "the MR URL comes from the remote's response lines, then is confirmed via
  `GET /merge_requests`", while the ADR's recipe step 6 demotes the `remote:` line to "a log
  nicety only"; that reversal gets no row. *Fix:* extend the `PRIVATE-TOKEN` row to name the
  Verification bullet, and add a row for the merge-request-URL source.

### Suggest

- **N-S1 — the scheduled `gitlab.rs` range stops short of the comment it schedules.**
  Anchor: ADR § "Documentation surfaces", row `crates/ocx_lib/src/forge/gitlab.rs:148-157`, whose
  own text demands "**The whole doc comment**". The comment runs `:148-160`; `:158-160` (the
  empty-token behaviour) is outside the cited range. Both wrong sentences are inside it, so
  nothing is lost today. *Fix:* cite `:148-160`.

---

`Blocks remaining: 2 · Highs remaining: 1 · New: 1B/4H/4W/1S`
