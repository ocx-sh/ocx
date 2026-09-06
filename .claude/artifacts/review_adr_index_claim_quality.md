# Review: adr_index_claim_command — quality (adversarial)

Summary: needs work
Focus: quality

Scope: `.claude/artifacts/adr_index_claim_command.md` (ADR) and
`.claude/artifacts/system_design_index_claim_command.md` (design). Read-only on code.
Counts: **1 Block, 10 High** (9 actionable + 1 deferred), 11 Warn (9 actionable + 2 deferred), 7 Suggest.

The recommendation (C-A + O-B + T-D + W-B) survives every steelman below — but three of the
four matrices contain scores or cons that the cited code contradicts, one rejected option is
argued against a variant it is not, and the git plumbing recipe cannot produce the file the
command exists to write.

---

## Actionable

- [Block] **ADR:The git recipe (step 4) — a single `mktree` cannot write `p/<ns>/<pkg>.json`.**
  The recipe is `hash-object -w` → `mktree` "fed from `ls-tree o/<base>` plus the changes" →
  `commit-tree`. `git mktree` builds **one** tree object from one flat listing. The claim's
  path is three components deep — verified on the live index at
  `/home/mherwig/dev/index/p/rust-lang/mdbook.json` and `/home/mherwig/dev/index/p/actionlint/actionlint.json` —
  so producing it needs a tree for `<ns>`, a tree for `p`, and a root tree: three `mktree`
  invocations each fed by its own `ls-tree`, none of which the recipe shows. Announce writes
  the identical path shape over REST, so this is not hypothetical.
  **Remediation:** replace the `mktree` line with the depth-independent chain —
  `GIT_INDEX_FILE=<tmp> git read-tree o/<base>`, `git update-index --add --cacheinfo 100644,<blob>,p/<ns>/<pkg>.json`,
  `git write-tree` — then `commit-tree`. It is fewer commands, not more, and is the recipe
  scripted git-commit-without-worktree normally uses. Update the design's `git_workspace.rs`
  responsibility line ("the plumbing chain") to match.

- [High] **ADR:Part 1 (C-A pro 1, C-B con 1) — the invariant that rejects `ocx index claim` is false.**
  C-A's first pro asserts every `ocx index` subcommand "is a local-cache operation that never
  touches a forge"; C-B's first con asserts "`ocx index *` is offline local-cache work" and
  that a write there would make the group "sometimes network, sometimes not". The group is
  already sometimes-network: `ocx index update` "Fetches the requested packages' tags from the
  registry" (`crates/ocx_cli/src/command/index.rs:17-18`), and `ocx index sync` reads "from the
  source live" and is rejected by `--offline` "for contacting the source"
  (`crates/ocx_cli/src/command/index.rs:48-49,71-73`). Only `regenerate` "consults no source"
  (`:88-89`). The error is inherited verbatim from `research_index_claim_recon.md:46`.
  The *narrow* claim — no `ocx index` verb writes to a **forge** — is true and is enough.
  **Remediation:** restate both cells as "no `ocx index` verb writes to a forge; the group's
  network work is read-only registry traffic", and re-score C-B's D3/D5 without the
  offline-purity premise. C-A still wins on the per-package unit of work and the shared
  options struct with `announce`.

- [High] **ADR:Part 1 (Option C-C) — the option is rejected on cons that belong to a different variant of it.**
  C-C's Description names two things: "`--claim`, or auto-claim on absent root". All three cons
  attack auto-claim only — "Removing the refusal removes the signal", "Auto-claim was foreclosed
  by policy". An explicit `announce --claim` removes nothing: `UnclaimedNamespace` still fires
  for the flagless case, so the exit-79 signal a release wrapper branches on is untouched
  (mapping verified: `AnnounceError::UnclaimedNamespace => ExitCode::NotFound` in
  `crates/ocx_lib/src/announce/error.rs:234`, `NotFound = 79` in
  `crates/ocx_lib/src/cli/exit_code.rs:55`). The strongest case against `announce --claim` is
  never made, and it exists: D-C3 requires claim to write `tags: {}` while announce's whole job
  is writing `tags`, so one run would push bot-regenerated content through the G-04 human lane.
  **Remediation:** split C-C into C-C1 (`--claim`, explicit) and C-C2 (auto-claim), give C-C1 its
  own row, and reject it on the `tags: {}` / governance-lane incompatibility plus the
  options-surface argument — not on a signal it does not remove.

- [High] **ADR:Decision Drivers — the criteria set omits the axis that actually decides two of the four matrices.**
  Part 1: C-D scores 108 and is rejected because "it does not solve the reported problem", an
  axis the ADR says "a weighted sum cannot express". Part 1b: O-A scores **116**, the highest in
  the whole document, and is rejected because "a criterion set that cannot see 'the manual path
  is unusable' is measuring the wrong thing". In both cases the ADR diagnoses its own criteria
  set as incomplete and then keeps it. A matrix that picks the wrong winner twice out of four is
  not evidence for the fifth reader; it is decoration around a prose decision.
  **Remediation:** add D7 "solves the reported problem for the named user (CI pipeline *and* the
  hand-invoking publisher)", weight 5, and re-score all sixteen rows — or delete the Part 1b
  matrix and decide it in prose, as the text already does. Do one; the current hybrid claims
  rigour it does not have.

- [High] **ADR:D-T4 + The git recipe — the widened `NonFastForward` retry has no defined local-workspace state.**
  Under `git`, `commit_files` "builds objects locally, moves a local ref and returns the sha
  without contacting the server" and the push in `open_or_update_pull_request` raises
  `NonFastForward`. The orchestration then "re-read[s] the winning head and regenerate[s] once".
  Nothing says what happens to the temp clone: the local `refs/heads/<branch>` still points at
  the losing commit and `o/<base>` still holds the pre-move base. Re-running `commit_files`
  would build on the stale parent, the local CAS `update-ref <new> <old>` would *succeed*
  (the local ref does hold `<old>`), and the second push would be rejected identically — the
  retry is guaranteed to fail. The contract this weakens is explicit that getting it wrong
  "silently loses the concurrent announce" (`crates/ocx_lib/src/forge/api.rs:264-268`), and this
  is the exact surface #228/#399 lived on.
  **Remediation:** state in D-T4 that under `git` the retry re-fetches `<base>` and `<branch>`
  into the workspace and rebuilds `o/<base>`/`o/<branch>` before the second `commit_files`, and
  add the re-fetch as an explicit step in the recipe. Add the two-push-rejection case to the
  fixture's moved-target scenario so the retry is proved to converge, not just to run.

- [High] **ADR:D-T8 + Migration and Rollout — the `PRIVATE-TOKEN` → `Bearer` switch is unrequested scope on a shipped path, and the rollout section contradicts itself about it.**
  No requirement in the dossier, the issues, or this ADR asks for OAuth-token support. Today's
  header already accepts personal, project and group tokens (`crates/ocx_lib/src/forge/gitlab.rs:150-151,166`).
  The switch buys one credential kind nobody has asked for and costs a named High-impact risk
  (design §11 row 1: a reverse proxy that forwards `PRIVATE-TOKEN` and strips `Authorization`).
  The `JOB-TOKEN` decision that D-T8 exists for is orthogonal — it can be added while the
  non-job case keeps `PRIVATE-TOKEN`. Separately, Migration and Rollout opens with "**Existing
  announce users: nothing changes by default.**" in bold and then names this change two
  sentences later; the bolded claim is false as written.
  **Remediation:** keep `PRIVATE-TOKEN` for the non-job-token case, add `JOB-TOKEN` for the
  job-token case, and delete the risk row — or, if the switch is kept deliberately, rewrite the
  Migration lead as "one behavioural change on the API path" and mark the commit subject
  accordingly.

- [High] **ADR:D-C4 + design §Owner-resolution ladder — the advertised zero-config path writes a governance field from a source that cannot be verified there.**
  The headline CI story is "nothing at all under `--transport git` inside a job" (`ADR:#411 flow`).
  On that path the credential is a bare `CI_JOB_TOKEN`, so `authenticated_identity` and
  `resolve_user` both raise `UsersApiUnavailable` by the ADR's own trait doc. Arm 2 therefore
  yields and its bot rule degrades to what the design itself labels "a name-shape heuristic; the
  weak form". So the single path the ADR promotes hardest is the one whose `owners[]` entry is
  never checked against the forge — and the ADR never says so. On GitHub the same ordering means
  arm 2 wins whenever `GITHUB_ACTOR` is set, so the server-asserted `type == "Bot"` check the
  design calls "the strong form" never runs on the common path either, despite `resolve_user`
  being reachable there.
  **Remediation:** (a) when arm 2 yields and `resolve_user` is reachable, resolve the login
  through it and take the server's `id` and `bot` field — reuses an operation the design already
  adds, no new code; (b) when it is not reachable, state that in the ADR and qualify the stderr
  owner line as `unverified (CI environment)` so the operator and the G-04 reviewer both see the
  provenance. Add both to Validation.

- [High] **ADR:API Contract — environment variables and precedence — the configuration every #411 reader already has silently defeats the fix.**
  API precedence is `OCX_ANNOUNCE_TOKEN` first, `CI_JOB_TOKEN` second. Push precedence is
  `OCX_ANNOUNCE_GIT_TOKEN` first, "the resolved API credential" second. A pipeline that already
  sets `OCX_ANNOUNCE_TOKEN` to its project access token — which is the only way `announce` works
  today, so it is what the #411 reporter's eight releases use — and then adds `--transport git`
  gets the project token on **both** halves. The push authors as the bot, G-19 does not match,
  and the run looks successful. That is precisely the failure #411 reports, reproduced by the
  feature meant to fix it. It is discoverable only in `push_credential_kind`, which the plain
  report does not render (the five plain columns are `Package`, `Status`, `Transport`, `Branch`,
  `Pull Request`).
  **Remediation:** when `--transport git` is selected inside `GITLAB_CI` with a non-job
  `OCX_ANNOUNCE_TOKEN` and no `OCX_ANNOUNCE_GIT_TOKEN`, emit one stderr line naming the push
  credential kind and the identity it will author as, before the write. Add the case to
  Validation's credential-selection list and to the use-case page's posture table.

- [High] **ADR:Security Architecture §4 — a mandatory hygiene control is a no-op on a supported platform.**
  "**Tempdir mode `0700` set explicitly** after creation, not left to the umask (CWE-732)" is
  stated without qualification, and the child-environment allowlist in the same section names
  `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH` and `SYSTEMROOT`, so Windows is in scope (and
  `product-context.md` lists windows/amd64 and windows/arm64 as supported). Unix mode bits do
  not exist on Windows; a `set_permissions(0o700)` there is inert. An acceptance assertion
  written against this control passes on Windows whether or not anything happened — the
  "green that cannot be told from never-ran" shape `quality-core.md` § Unchecked Green names.
  **Remediation:** state the Windows behaviour explicitly (per-user temp directory ACL; no mode
  bits) and make the Validation item Unix-only, or assert the ACL. Same treatment for `LC_ALL=C`,
  whose effect on git-for-Windows message localisation is unstated.

- [High] **ADR:Part 3 (Option W-D con 1) — the con cites as "no reader" the exact reader this ADR breaks.**
  W-D is rejected with "No reader exists. G-19 matches numeric `id`; reviewer requests use
  `login`; the catalog renders `login` as a link." The catalog's link is hard-coded to GitHub:
  `/home/mherwig/dev/ocx-catalog/src/theme/components/detail/MetaRail.vue:228` returns
  `safeHref(\`https://github.com/${login}\`)`, with a self-documented `KNOWN GAP` at `:222`. So a
  claim filed from GitLab — the whole point of the git transport — renders every owner as a link
  to a **github.com** profile that belongs to a different person or to nobody. The ADR does carry
  the catalog href to "Deferred to handoff", but the matrix cell reads as though the field has no
  consumer, when the consumer is the one this work makes reachable.
  **Remediation:** rewrite the W-D con to "the one reader that would consume it, the catalog's
  owner href, is broken for GitLab-sourced indexes today (`MetaRail.vue:222-228`); W-D is
  rejected because the fix belongs in the catalog or in the index's own forge declaration, not
  per-owner" — and raise the catalog issue from "deferred" to a named prerequisite for
  advertising GitLab claims. (W-B itself is safe: `ownerLogin` reads `login ?? github`,
  `/home/mherwig/dev/ocx-catalog/src/theme/composables/usePackageRoot.ts:24-25`.)

- [Warn] **ADR:Part 2 matrix — the D2 column differentiates the winner on a dimension the option descriptions do not touch.**
  T-D scores 5 on credential containment; T-A, T-B and T-C all score 4. All four options perform
  the same git push with the same credential mechanism — the layering choice does not change
  where the secret lives, and no con in any of the four tables mentions credentials. That is a
  free 5 weighted points applied to the winner alone. Correcting T-C to 5 gives it 85 against
  T-D's 107, so the verdict survives; the column does not.
  **Remediation:** set D2 to the same value for all four rows and say in one line that credential
  containment is layering-independent, or state the mechanism by which T-C's containment is worse.

- [Warn] **ADR:Part 1 matrix — two of C-C's six scores are unsupported by its own con column.**
  C-C scores D1 (concurrency correctness) = 2, but none of its three cons is about concurrency;
  folding claim into `announce` would *reuse* the concurrency machinery verbatim. C-C scores D5
  (implementation and test cost) = 4, the same as C-A, although C-A adds five new library files,
  a CLI leaf and a report type while C-C adds a flag. Corrected to D1=5, D5=5, C-C reaches 83
  against C-A's 113 — the verdict survives, the scoring does not.
  **Remediation:** re-score both cells or add cons that justify them.

- [Warn] **ADR:Metadata + D3 — "Reversibility: one-way (high)" is asserted for the whole ADR and contradicted inside it.**
  Part 2 says "T-D is reversible by removing one enum arm and one flag; the flag is the whole
  external surface", and the `Forge` contract change is internal, where CLAUDE.md grants zero
  stability. What is genuinely one-way is never enumerated: bytes in **already-merged** roots
  (CLAUDE.md's one hard exception), the published env-var names `OCX_ANNOUNCE_GIT_TOKEN` /
  `OCX_ANNOUNCE_GIT_USERNAME` (the ADR itself defers `OCX_FORGE_*` *because* renaming breaks CI),
  `ExitCode::ForgeCapabilityUnavailable = 86`, and the new JSON report keys added to **announce's
  shipped report** plus the `CapabilityName`/`CheckStatus` wire spellings. All five are additive
  today and unremovable tomorrow.
  **Remediation:** add a short "genuinely one-way / genuinely two-way" list under Decision
  Outcome naming exactly those five as one-way and the trait shape, the enum arm and the
  `--transport` flag as two-way. It costs four lines and makes the tier honest.

- [Warn] **ADR:D-C7 + design §Branch state machine — `Identical` is grouped with the force-push cases.**
  `BranchComparison::Identical` means "the branch is the base commit"
  (`crates/ocx_lib/src/forge/api.rs:38-39`), so committing the claim root on top of it is a plain
  fast-forward. Both artifacts route it to `RefUpdate::Reset`, which under `git` is
  `--force-with-lease`. That replaces a strong CAS with a weaker one for a case that never needs
  it.
  **Remediation:** move `Identical` to the `FastForward` row in both the ADR's D-C7 prose and the
  design's table; leave `Behind` and `Diverged` on `Reset`.

- [Warn] **ADR:Implementation Plan + Migration — the ADR states twice that the commit subject IS the changelog and never writes one.**
  "All additions are additive JSON keys; the changelog line is the commit subject" and "**No
  `CHANGELOG.md` edit, ever.** The changelog line is the commit subject", plus Constitution Check
  item 7 ("needs its own changelog-bearing commit subject"). The nine-step plan is a checklist of
  work, not of subjects, so the release notes for 0.6.1 are unspecified for a release whose
  vehicle this ADR names.
  **Remediation:** name the user-facing subjects on the steps that produce them — at minimum
  `feat(package)!: claim a namespace from the CLI with ocx package claim`,
  `feat(announce): add --transport git so a GitLab job token authors the merge request`,
  `feat(cli): add exit code 86 for a forge capability a transport needs`, and one subject for the
  GitLab REST header change if finding 6 is resolved by keeping it. State which, if any, carry `!`.

- [Warn] **ADR:Test fixture + design Phase 4 — the fixture's cost is understated and its mechanism is unstated.**
  "Extend `test/tests/fake_forge.py` … with a real `git init --bare` repository served by
  `git http-backend` on the same port" reads as an extension. The existing fake is
  `_Handler(http.server.BaseHTTPRequestHandler)` with hand-written `do_GET`/`do_POST`/`do_PATCH`
  (`test/tests/fake_forge.py:71,112,173,218`) on `FakeForge(GitLabRoutes, http.server.ThreadingHTTPServer)`
  (`:231`). `BaseHTTPRequestHandler` provides no CGI bridge and does not decode
  `Transfer-Encoding: chunked`, which smart-HTTP `git-receive-pack` uses; the stdlib's
  `CGIHTTPRequestHandler` is deprecated on this project's Python floor (3.13+, per
  `product-tech-strategy.md`). So the fixture is a hand-rolled CGI bridge plus chunked-body
  decoding — the largest single new artifact in the plan, priced at D5=3 alongside "smallest diff".
  **Remediation:** name the CGI bridge and the chunked decoding in Phase 4 / step 8, and state why
  the cheaper fixture is insufficient — a bare repo over the *local* transport delivers push
  options and can run the same `post-receive` hook, but cannot exercise `http.extraHeader`
  placement or the "credential absent from `.git/config`, argv and the remote URL" assertions,
  which is the actual reason HTTP is required. Saying that once makes the cost defensible instead
  of surprising.

- [Warn] **ADR:Decision Drivers — no cost-of-ownership criterion, and the design adds a permanent maintenance liability.**
  D5 prices "lines written, fixtures built". It does not price what must be maintained forever.
  The git stderr classifier matches English phrases (`(fetch first)`, `(stale info)`,
  `pre-receive hook declined`) that git does not treat as a contract; the design's own risk table
  rates a classifier miss "High (every push failure degrades to exit 1)". T-C versus T-D is
  substantially an ongoing-drift argument, so the criterion that would decide it is missing from
  the matrix that decides it.
  **Remediation:** add a cost-of-ownership criterion (weight 3) covering surfaces that must track
  an external tool's non-contractual output, and score all four Part 2 options on it. T-D still
  wins; the reader can now see why the classifier is worth it.

- [Warn] **ADR:D-C5 exit-code rationale — the closest precedent is the sibling command, and it is not addressed.**
  65 is justified against `DescDisappeared` and `PullRequestUnmergeable` (both
  `ExitCode::DataError`, `crates/ocx_lib/src/announce/error.rs:224,252`). The nearest precedent is
  the exact inverse condition on the sibling command: `UnclaimedNamespace` → `NotFound` = 79
  (`error.rs:234`). A pipeline handling "is this namespace claimed or not" now branches on 79 from
  `announce` and 65 from `claim` for the two halves of one question.
  **Remediation:** address the asymmetry in one sentence — 79 answers "the root is not there", 65
  answers "the root is there and disagrees with the operation" — or reuse 79. Either is fine;
  silence about the sibling is not.

- [Warn] **ADR:report `status` field — one word, two definitions, under an explicit one-vocabulary claim.**
  The design says `ClaimStatus` "reuses announce's two words on purpose — one vocabulary across
  the two commands". Announce's `status` is defined against the **committed root**: "`unchanged`
  when the rebuilt root was byte-identical to the committed one"
  (`crates/ocx_cli/src/api/data/announce.rs:36-42`), and announce's `--out` can therefore report
  `unchanged` (`crates/ocx_lib/src/announce.rs:166-172`, root read at `announce.rs:715-721`). Claim's is defined against the **open
  claim branch**, and its `--out` is "always `updated`". The words match; the referent does not.
  **Remediation:** say so in the report contract — claim compares against the branch because a
  committed root would already have exited 65 — so a consumer reading both reports is not misled.

- [Suggest] **ADR:API Contract — the `Forge` trait — the stale count sentence is in the module doc, not the trait doc.**
  "Its doc comment currently says 'ten operations'" — the sentence is at
  `crates/ocx_lib/src/forge/api.rs:7`, the module doc; the trait's own doc
  (`:113`) reads "The forge operations `announce` drives." with no count. Eleven async methods
  are declared. The correction (stop naming a count) is right; the pointer sends the implementer
  to the wrong doc block.

- [Suggest] **ADR:Quantified Impact — "Exit codes | 79–85 used | 79–86 used" misstates the range.**
  The ADR's own exit-code table for these two commands lists 1, 64, 65, 69, 74, 75, 77, 79, 80 and
  86, and `crates/ocx_lib/src/cli/exit_code.rs:27-93` defines 64 through 85. Say "one new variant,
  86" and drop the range, or state the range correctly.

- [Suggest] **ADR:Context — `announce/pipeline.rs` is 2040 lines, not 2041** (`wc -l`). Trivial, but
  it is a number a reader can check, and the ADR asks to be checked.

- [Suggest] **ADR:Security §4 — `core.symlinks=false`'s stated reason is unreachable on this path.**
  "a symlink planted by a past commit must not materialise on disk" — the recipe is `git init` +
  `fetch` with no checkout and no worktree (step 2, "bare-equivalent; no worktree, no checkout"),
  so nothing materialises. Keep the flag (it is free), but state it as defence-in-depth against a
  future path that does check out, or the next reader will delete it as dead.

- [Suggest] **ADR:Data Model — `created` reuses a seam that returns an unvalidated string.**
  `current_timestamp()` returns the pinned env value **verbatim** when
  `__OCX_TESTING_ANNOUNCE_CLOCK` is set (`crates/ocx_lib/src/announce/pipeline.rs:106-113`), under
  `#[cfg(any(test, feature = "__testing"))]`. "Taking its date part" therefore slices the first ten
  characters of a string nothing validates, and a claim test that pins a bare date would silently
  corrupt announce's `observed`. It also lives in the `announce` module, which the forge-blind
  `ocx_lib::claim` would have to reach into.
  **Remediation:** move the seam to a shared location with a `current_date()` accessor that
  formats from the same source, and say so in the design's component table.

- [Suggest] **ADR:Decision Drivers — D1 and D2 are constants in two of the four matrices.**
  Every row of Part 1b and Part 3 scores 5 on both, contributing an inert 50 points each. The
  stated benefit ("scores are comparable across parts") is real but buys comparability at the cost
  of ten of twenty-four weight-points being uninformative exactly where the decisions are close.
  Worth one sentence acknowledging it.

- [Suggest] **ADR:Decision Drivers — documentation burden is not a criterion, and it differs by option.**
  The chosen set adds a new use-case page plus edits across six surfaces in three repositories
  (Documentation surfaces table). C-D (renderer only) would need a fraction of that. Not decisive —
  C-D is rejected on a harder axis — but the matrix cannot see a cost the ADR itself enumerates.

---

## Deferred

- [High] **ADR:Release vehicle + Decision Outcome — the coupling argument for shipping both in 0.6.1 is half false, and splitting is viable.**
  The rationale is "A claim command without the git transport ships a command the #411 reporter
  still cannot use; a git transport without the claim command ships a transport whose first-run
  case still fails." The second leg does not hold: the #411 reporter "runs eight releases through
  a shell workaround" (ADR Context §2), so their namespaces are already claimed — they need the
  transport and not the claim at all. Shipping Phases 1, 2 and 4 (the transport) in 0.6.1 unblocks
  #411 immediately; claim follows in 0.6.2 with the fixture and the owner ladder de-risked by a
  release of real git-transport traffic. The counter-argument is real too — one release means one
  docs sweep, one credential model and one changelog — but it is a schedule call, not a technical
  one, and the ADR presents it as technical.
  **Question for the human:** is 0.6.1 a fixed date? If it is, do you want the git transport alone
  in 0.6.1 (unblocks #411 now, halves the release's new surface) with `ocx package claim` in 0.6.2,
  or both together as the dossier's decision 8 says?

- [Warn] **ADR:D-T7 + environment table — `OCX_ANNOUNCE_GIT_USERNAME` is never justified.**
  The push credential is HTTP Basic. GitLab ignores the username for a personal, project or group
  access token, so the default `gitlab-ci-token` suffices for every posture the ADR names. The one
  credential kind whose username is load-bearing is a **deploy token** — which appears nowhere in
  the requirements, and which the ADR explicitly narrows out of `credential_kind` ("the operability
  lane's proposed `pat`/`deploy-token`/`oauth` set is narrowed"). A published env-var name is
  one-way by the ADR's own reasoning for deferring `OCX_FORGE_*`.
  **Question for the human:** are GitLab deploy tokens a posture you want supported at 0.6.1? If
  yes, name them in D-T7 as the justifying case. If no, drop the variable and add it when a real
  deploy-token posture appears — one fewer published name to live with.

- [Warn] **ADR:Open Questions 1 — the `--upstream-*` grammar is decided differently in two places.**
  Open Question 1 recommends "`--upstream-org` and `--upstream-repository-url` required together,
  `--upstream-disclaimer` optional but requiring `--upstream-org`". The dossier's own recommendation
  is "`--upstream-org`, `--upstream-url`, `--disclaimer` optional and **independent**", with
  different flag spellings. The CLI grammar table implements the ADR's version. The divergence from
  the ratified dossier is not flagged, and the answer lives in `ocx-sh/index`'s
  `root.schema.json`, which neither artifact reads.
  **Question for the human:** the ADR silently overrode the dossier here (both the requiredness and
  the flag names `--upstream-repository-url` / `--upstream-disclaimer`). Confirm the ADR's version
  is what you want, and confirm someone will read the index schema before Phase 3 rather than after.

---

## Steelman record (per rejected option and per dossier decision)

### Part 1 — placement

- **C-B `ocx index claim` — [High, see Actionable].** Strongest case not made: the group is
  *already* network-touching (`index update`, `index sync`), so the "offline purity" con is
  fabricated. Real remaining case for C-B: "claim" is a noun about the index, and a publisher's
  mental model is index-shaped. Real remaining case against: the unit of work is a package, and
  splitting the two write commands across two groups duplicates the flag surface. C-A survives on
  the second; the matrix must stop leaning on the first.
- **C-C `announce --claim` — [High, see Actionable].** Steelman: an explicit flag removes no signal
  and teaches the publisher one command instead of two. Defeated by a con the ADR does not state:
  D-C3 requires `tags: {}`, announce's job is `tags`, so one run pushes bot-regenerated content
  through the G-04 human lane. Also defeated by `require_root` reading the **committed** root at
  `base_ref` (`crates/ocx_lib/src/announce/pipeline.rs:123-134`) — even in one process an
  unmerged claim would not satisfy announce, so the "one run" is genuinely impossible, not merely
  ill-advised. That second point is the ADR's best argument and it is only half-stated.
- **C-D renderer only — [Suggest, recorded as considered].** Steelman: it is nearly free, adds zero
  credential surface, and `--out` already exists on announce. Defeated correctly: `#410` reports
  the `gh pr create` glue as the problem, not the JSON. The ADR's handling of this one is honest —
  it says outright that the weighted sum cannot express the rejection.

### Part 1b — owner input

- **O-A always explicit — [High, see Actionable].** It scores 116, the highest number in the ADR,
  and loses. Steelman the ADR did make: nothing can be inferred wrongly, and it works under every
  credential including a bare job token — which, per the High finding above, is exactly the
  posture where O-B's identity is unverifiable. That is a stronger case than the ADR credits: O-A
  is the only option whose guarantee does not degrade on the advertised zero-config path.
  Defeated by owner evidence (manual invocation, multi-owner ergonomics) that the criteria set
  cannot see — which is the defect, not the decision.
- **O-C detect-and-append / O-D detect-always — [Suggest].** Both correctly rejected: the written
  list differing from what the operator typed is disqualifying for a governance field, and O-D
  makes the "ownership by repository access" posture unexpressible. No stronger case exists.

### Part 2 — transport layering

- **T-A second `Forge` impl — [Suggest].** Steelman: it is the shape `adr_announce_gitlab_forge.md`
  D1 establishes for a second forge, so it is the *idiomatic* answer in this codebase. Defeated
  decisively by `crates/ocx_lib/src/forge/api.rs:115-117` ("an implementation that cannot hold it
  must return an error rather than approximate it") plus the concrete `BranchState::Stale`
  unreachability. Well argued.
- **T-B mode in the pipeline — [Suggest].** No steelman survives; all three council seats and the
  prior ADR reject it. Correctly dismissed.
- **T-C separate git writer — [Warn, see Actionable].** Steelman the ADR undersells: it is the only
  option with *zero* risk to the shipped REST path, and it is `--transport`-free — no flag added to
  a shipped command, so finding 6's whole class disappears. Its D5=5 is real. Defeated on drift
  risk, which is the right axis — but the matrix has no cost-of-ownership criterion to express
  drift, and gives T-D a free credential-containment point instead. Correct verdict, wrong
  arithmetic.

### Part 3 — owners wire

- **W-A dual-emit from ocx — [Suggest].** Steelman: byte-identity with indexbot's own output means
  a claim root and a bot-rewritten root never differ, which removes a whole class of "why did the
  bot rewrite my file" confusion. Defeated correctly — ocx would become a second producer of a
  derived field it does not own, with no rule for which spelling wins.
- **W-C bump `format_version` — [Suggest].** Correctly and decisively rejected: the index's own wire
  contract makes an unrecognised higher `format_version` a hard error, so the bump would break every
  deployed ocx for a field ocx does not read. The design also correctly notes there is no
  `format_version` on a package root at all — it lives on the catalog document.
- **W-D per-owner forge field — [High, see Actionable].** The con is wrong about the reader.
  Steelman: the catalog's GitHub-hardcoded href is a live defect that a per-owner `forge` would fix
  at the source. Still correctly rejected — an index lives on one forge, so the right place for the
  fact is the index or the catalog, not every owner row — but for a different reason than stated.

### Dossier decisions 1–9

1. **Placement** — see C-A/C-B above. Sceptic's objection ("`ocx index` will grow a remote write
   side once claims exist; the noun will be wrong then") is answered: claim writes a *package*
   entry, `ocx index` verbs operate on a *source*. The unit of work argument holds regardless of
   what the index group later learns to do. **Agreement is justified, on the second reason only.**
2. **Claim is its own command** — two-run first release is genuinely unavoidable, not merely
   accepted: `require_root` reads the committed root at `base_ref` and G-04 forbids auto-merge.
   For the primary CI user this is one extra pipeline stage on the *first* release of a package,
   ever. **Agreement justified.** But see C-C1 above: the explicit-flag variant deserved its own row.
3. **No `format_version` bump** — the consumer outside indexbot is `ocx-catalog`, and it reads
   `login ?? github` (`usePackageRoot.ts:24`), so W-B is safe for it. The ADR does account for the
   catalog in Context and in the deferred list. **Agreement justified**, with the W-D con
   correction above.
4. **0.6.1 release vehicle for the wire** — dependency verified live (index `root.schema.json`
   `anyOf`). **Agreement justified.**
5. **ADR homes** — D-W3 references indexbot's ADR without authoring it. Correct separation.
   **Agreement justified.**
6. **Transport layering** — see Part 2. **Agreement justified**, arithmetic aside. The D1
   "orchestration never learns the forge" promise is *not* cracked by the widened retry: the
   orchestration handles `NonFastForward` from either call under both transports, so it learns
   nothing transport-specific. What it does gain is a real correctness hole in the retry's
   workspace state — see the High finding — which is a different, worse problem than the one the
   question anticipated.
7. **Owner input: detect, replace, refuse bots** — **Agreement is under-examined.** The sceptic's
   objection the room did not raise: on the path the ADR advertises hardest, the bot rule is the
   weak form and no server check is possible. See the High finding. A second unraised path:
   `GITHUB_ACTOR` in a fork-triggered or `pull_request_target` workflow names an outside
   contributor, who would be written into `owners[]` as the acting identity; the STRIDE spoofing
   row names only the shared-account and bot cases. G-04's human merge bounds the impact, which is
   why this is High and not Block.
8. **0.6.1 ships both** — **Agreement not justified as stated.** See the Deferred item: the
   reporter already has claimed namespaces, so the transport alone unblocks them.
9. **Three env vars + job-token inference** — the inference is *not* the silent-identity hazard
   #411 warned about; it is the fix, and it is correctly gated on an explicit `--transport git`
   plus `GITLAB_CI`. The hazard is the opposite one, and it is real: a pre-existing
   `OCX_ANNOUNCE_TOKEN` suppresses the pickup on both halves and restores bot authorship silently.
   See the High finding. `OCX_ANNOUNCE_GIT_USERNAME` is plausibly YAGNI — see the Deferred item.

### Probes with no finding

- **`--transport` added to a shipped command and the stability tiers.** No violation. CLAUDE.md
  makes flag grammar an interface, but an *added* optional flag with a default that preserves
  today's behaviour is additive, not a break. It needs a changelog-bearing commit subject, which is
  the separate Warn above. No deprecation window, no migration prose — correct for pre-1.0.
- **A bespoke preflight.** Not bespoke: `ensure_push_access` already exists in the trait
  (`crates/ocx_lib/src/forge/api.rs:254`) and the ADR changes its return type rather than adding a
  parallel mechanism. This is "extend, don't duplicate" done right.
- **A `--check` flag.** Not proposed. `--check` appears nowhere in either artifact; there is nothing
  to cut.
- **Blast radius on `ocx-mirror`.** Correctly scoped out and named twice (Migration and Rollout;
  Deferred to handoff), with the concrete reason (`AnnounceConfig` has no `forge` field) and a
  filed follow-up. No finding.
- **Owning non-domain code.** Shelling out to system `git` rather than vendoring `git2`/`gix` is the
  compliant choice under `quality-core.md` § Don't Own Non-Domain Code, and the ADR says so
  explicitly in Constitution Check §2. The `git2` rejection (a second TLS stack against a
  rustls-only workspace) is a real, checkable reason, not a preference. The one place the design
  *does* own non-domain code is the plumbing chain itself — and the Block finding above makes it
  smaller, not larger.
- **Documentation surfaces completeness and migration prose.** Six surfaces across three
  repositories are enumerated, `CHANGELOG.md` is explicitly forbidden, and no migration prose is
  proposed for user docs — the wire change is described as "the entry-schema docs name the drop",
  which is a statement of current shape, not a migration guide. Compliant with the pre-1.0 rule.

---

## Verified claims (path:line evidence)

| ADR/design claim | Verdict | Evidence |
|---|---|---|
| `require_root` raises `UnclaimedNamespace` on an absent committed root | HOLDS | `crates/ocx_lib/src/announce/pipeline.rs:123-134` |
| `UnclaimedNamespace` exits 79, the signal a wrapper branches on | HOLDS | `announce/error.rs:234` → `cli/exit_code.rs:55` (`NotFound = 79`) |
| `DescDisappeared` / `PullRequestUnmergeable` are exit 65 | HOLDS | `announce/error.rs:224,252` (`DataError = 65`, `exit_code.rs:30`) |
| `forge/api.rs:117` says an implementation must error rather than approximate | HOLDS | `crates/ocx_lib/src/forge/api.rs:117` |
| The `Forge` trait has 11 operations today | HOLDS | 11 `async fn` in `crates/ocx_lib/src/forge/api.rs` from `:132` to `:291` |
| The doc says "ten operations" | PARTIAL | It is the **module** doc `crates/ocx_lib/src/forge/api.rs:7`, not the trait doc `:113` |
| `85` is taken by `UnsupportedKeyBackend`, so 86 is the first free slot | HOLDS | `crates/ocx_lib/src/cli/exit_code.rs:93` |
| `ExitCode::PermissionDenied` documents only filesystem `EPERM` | HOLDS | `crates/ocx_lib/src/cli/exit_code.rs:47-49` |
| `ExitCode::Unavailable` already says rerunning will not change the outcome | HOLDS | `crates/ocx_lib/src/cli/exit_code.rs:34-36` |
| `gitlab.rs:148-157` misstates the job token's read scope | HOLDS | `crates/ocx_lib/src/forge/gitlab.rs:152-157` |
| The same comment also claims `Bearer` accepts **only** OAuth2 | HOLDS, and the ADR does not list it for correction | `crates/ocx_lib/src/forge/gitlab.rs:150-151`; header sent at `:166` |
| Every `ocx index` subcommand is a local-cache operation | **FAILS** | `index update` fetches from the registry (`command/index.rs:17-18`); `index sync` reads the source live and is `--offline`-rejected (`:48-49,71-73`); only `regenerate` "consults no source" (`:88-89`) |
| `announce/pipeline.rs` is 2041 lines | FAILS (2040) | `wc -l crates/ocx_lib/src/announce/pipeline.rs` |
| `announce --out` still reads the committed root | HOLDS | `crates/ocx_lib/src/announce.rs:166-172`, root read at `announce.rs:715-721`; the ADR correctly corrects the dossier on this |
| Announce's `status` uses `unchanged`/`updated` against the committed root | HOLDS | `crates/ocx_cli/src/api/data/announce.rs:36-42` |
| The announce report has no `forge`/`transport`/`branch` today, so the additions are additive | HOLDS | `crates/ocx_cli/src/api/data/announce.rs:33-62` |
| `serialize_root` is the byte-exact writer both commands share | HOLDS | `crates/ocx_lib/src/oci/index/wire_writer.rs:59` |
| `__OCX_TESTING_ANNOUNCE_CLOCK` is an existing seam | HOLDS, with caveats | `crates/ocx_lib/src/announce/pipeline.rs:100-113`; returns the pinned value unvalidated, `#[cfg(any(test, feature = "__testing"))]` |
| `CREDENTIAL_KEYS` exists and is the scrub list | HOLDS | `ocx_lib::env::keys::CREDENTIAL_KEYS`, consumed at `crates/ocx_cli/src/app/plugin_dispatch.rs:192,264,267` |
| `spawn_and_wait` is the subprocess boundary | HOLDS | `crates/ocx_lib/src/utility/child_process.rs:138` |
| `commit_files`'s contract requires `NonFastForward` or the concurrent announce is lost | HOLDS | `crates/ocx_lib/src/forge/api.rs:264-268` |
| `BranchComparison::Identical` means the branch **is** the base commit | HOLDS | `crates/ocx_lib/src/forge/api.rs:38-39` |
| `IndexRoot` has no `owners` field; ocx never parses it | HOLDS | `crates/ocx_lib/src/oci/index/wire.rs` (no `owners`), per `research_index_claim_recon.md:33` |
| The catalog reads `owners` and tolerates both spellings | HOLDS | `/home/mherwig/dev/ocx-catalog/src/theme/composables/usePackageRoot.ts:9,24-25` |
| The catalog's owner href is hard-coded to github.com | HOLDS, and W-D's con contradicts it | `/home/mherwig/dev/ocx-catalog/src/theme/components/detail/MetaRail.vue:222,228` |
| The claim path is nested two directories deep | HOLDS — this is what breaks the `mktree` recipe | `/home/mherwig/dev/index/p/rust-lang/mdbook.json`, `/home/mherwig/dev/index/p/actionlint/actionlint.json` |
| `fake_forge.py` is a hand-written `BaseHTTPRequestHandler` with no CGI support | HOLDS | `test/tests/fake_forge.py:71,112,173,218,231` |
| `announce` takes `--package`, so claim's positional diverges | HOLDS, and the ADR names it in Constitution Check §5 | `crates/ocx_cli/src/command/package_announce.rs:292,308` |
