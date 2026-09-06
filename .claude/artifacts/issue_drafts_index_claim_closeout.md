# Index-claim closeout — issue drafts (WP-19)

**Status: DRAFTS. Nothing here has been posted.** No GitHub write tool was called
producing this file. The owner posts each item; WP-19 records outcomes, it does not file.

Source of record: `plan_index_claim_command.md` § "Cross-repository closeout (WP-19)"
(rows 1–6 plus the two issue-body posts). Each row's **must state** clauses are carried
verbatim, uncompressed.

**A PR footer "Closes #a, #b, #c" closes only `#a`.** Audit each number individually
after the merge.

## Repository-name corrections (plan table vs. reality, verified via the GitHub API)

| Plan table says | Actually | Evidence |
|---|---|---|
| `ocx-sh/ocx-catalog` (row 3) | **`ocx-sh/catalog`** | `ocx-sh/ocx-catalog` does not resolve; `MetaRail.vue` is in `ocx-sh/catalog` |
| `ocx-sh/index` (row 4) | **`ocx-sh/indexbot`** (ruled; one issue, not two) | indexbot's source, ADRs and the decision all live there; the `ocx-sh/index` entry-schema line is a deliverable of that issue |

---

## Row 1 — Corporate-CA REST client · RELEASE-BLOCKING (to file)

- **Target repository:** `ocx-sh/ocx`
- **Labels:** `area/cli`, `enhancement`, `priority/high`
- **Title:** `forge: honour the corporate CA environment variables in the REST client`

> **The gap.** `build_forge_http_client` (`crates/ocx_lib/src/forge/http.rs`) builds the
> client both forge clients share with `reqwest::Client::builder()` plus
> `crate::utility::tls::seed_embedded_roots`. It seeds the embedded Mozilla roots and reads
> **no CA environment variable at all**. Behind a TLS-inspecting corporate proxy, whose
> certificate chains to a private root, every forge REST call fails to verify — so
> `ocx package claim` and `ocx package announce --transport api` are unusable on those
> networks with no supported way to trust the enterprise root.
>
> **Scope: the four CA variables, and only those.**
>
> - `GIT_SSL_CAINFO`
> - `GIT_SSL_CAPATH`
> - `SSL_CERT_FILE`
> - `SSL_CERT_DIR`
>
> These four are already the ones ocx recognises for the *git* half: they sit in
> `git_command.rs`'s child-environment passthrough table, so `--transport git` inherits the
> operator's CA configuration through the `git` child while the REST half does not. The
> asymmetry is the bug.
>
> **Explicitly NOT the proxy set.** `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` and their
> lowercase spellings are already honoured — `reqwest` reads them from the environment by
> default, and this builder never calls `.no_proxy()`. An issue written around the proxy
> variables would be closed as already-working with the real gap still open. Do not widen
> this issue to them.
>
> **Constraints.** The redirect policy (`Policy::none()`) is a security control — the guard
> that stops the announce credential being replayed at another host — and is structurally
> asserted by `the_forge_client_disables_redirects`. Adding CA roots must not touch it, and
> must not turn into "disable verification": there is no `--insecure` here and none is
> wanted. Additional roots are *added to* the embedded set, never a replacement for
> certificate verification.
>
> **Acceptance.** With a private root in `SSL_CERT_FILE`, a forge REST call against a
> server presenting a chain to that root succeeds; with the variable unset the same call
> fails to verify. Both directions asserted — a test that only shows the success case
> cannot tell "the variable was read" from "verification was weakened".
>
> Context: the index-claim ADR's §2 unfixed-gap list. The ADR forbids describing this as
> fixed, which is why it is filed rather than folded into that work.

---

## Row 2 — ocx-mirror transport fields

- **Target repository:** `ocx-sh/ocx-mirror`
- **Labels:** `enhancement`, `discussion-needed`
- **Title:** `announce: no forge/transport fields, an incoming ErrorCategory variant, and a possible Forge implementor`

> Three separate things land on ocx-mirror out of ocx's index-claim work
> (`ocx package claim` + `announce --transport git`, ocx 0.6.1). Filed as one issue because
> they all arrive on the same submodule bump.
>
> **1. `AnnounceConfig` has no `forge` field and gains no `transport` field.** ocx now
> exposes `--forge github|gitlab` and `--transport api|git` on both write commands;
> ocx-mirror's announce configuration can express neither, so a mirror cannot announce to a
> GitLab-hosted index nor use the git transport.
>
> **Must state — if that wiring is added, the push credential is passed explicitly.**
> `OCX_ANNOUNCE_GIT_TOKEN` is scrubbed from plugin child environments while
> `OCX_ANNOUNCE_TOKEN` is not (ocx contract C-066). A transport wiring that relies on
> inheriting the environment will therefore find the REST token present and the *push*
> token absent, and will fail — or worse, silently fall back — in exactly the CI shape the
> git transport exists for. Pass it explicitly.
>
> **2. The next submodule bump hits `E0004` on any exhaustive `match` over
> `ErrorCategory`.** That enum gains a variant in ocx 0.6.1
> (`ForgeCapabilityUnavailable`, exit 86). **No `#[non_exhaustive]` is being added, and
> that is deliberate**: `#[non_exhaustive]` would force a downstream wildcard arm, and a
> wildcard converts this compile error into a silent swallow — which is precisely the
> defect the wildcard-free match exists to prevent. The compile error is the feature. Add
> the arm.
>
> **3. Does ocx-mirror name `ocx_lib::forge::Forge` at all?** ocx 0.6.1 widens that trait
> with required methods and changes `ensure_push_access`'s return type. Both are
> source-breaking for any out-of-repo implementor of the trait. **This is unverifiable from
> the ocx repository** — hence a question rather than a statement. If ocx-mirror implements
> or names `Forge`, say so on this issue and the breakage can be sequenced against the bump
> instead of discovered by it.

---

## Row 3 — catalog owner href · gates the GitLab recipe in ocx's docs

- **Target repository:** `ocx-sh/catalog`  *(not `ocx-sh/ocx-catalog` — that name does not resolve)*
- **Labels:** `bug`
- **Title:** `MetaRail: owner profile links hard-code github.com, breaking GitLab-sourced owners`

> `src/theme/components/detail/MetaRail.vue` builds every owner profile link as a fixed
> `https://github.com/<login>` prefix:
>
> ```ts
> function ownerHref(owner: Owner): string | null {
>   const login = ownerLogin(owner)
>   return login ? safeHref(`https://github.com/${login}`) : null
> }
> ```
>
> The component already carries a `KNOWN GAP` comment saying exactly this: `owners[]` is
> forge-neutral, so on a GitLab-sourced root these are GitLab usernames and the rendered
> link points at a GitHub profile that need not exist — and may belong to an unrelated
> person who happens to hold that handle on github.com.
>
> **Why it is being filed now.** ocx 0.6.1 ships `ocx package claim`, which can mint a root
> whose `owners[]` are GitLab accounts. **This is a named prerequisite for advertising the
> GitLab path**: ocx's use-case page ships the GitLab recipe only once this is closed, and
> until then marks it a prerequisite. Closing this unblocks that documentation.
>
> As the component's own comment notes, the fix needs the index's forge in the view-model,
> which `MetaRail.vue` cannot currently see — so this is a view-model change, not a
> one-line href edit.

---

## Row 4 — indexbot dual-emit stop date

- **Target repository:** `ocx-sh/indexbot` *(ruled; the plan's table said `ocx-sh/index`)*
- **Labels:** `documentation`, `enhancement`
- **Title:** `owners: name the release that stops dual-emitting github/github_id`
- **One issue, not two.** The decision, the ADR and the code all live in `ocx-sh/indexbot`.
  The entry-schema line in `ocx-sh/index` is downstream of the decision, so it is a
  deliverable *of* this issue rather than a second issue: **stop date decided here, then
  named in `ocx-sh/index`'s entry-schema reference.** Splitting an undecided thing across
  two repositories gives it two half-owners.

> indexbot 0.5.0 made `login`/`id` canonical and kept emitting the pre-0.5.0
> `github`/`github_id` pair derived from it, on every root it rewrites. The legacy drop was
> deferred to its own ADR, which has not been written.
>
> **Recommendation: indexbot 0.7, with its own owners-drop ADR**, referencing
> `adr_forge_neutral_owners.md`.
>
> **Why 0.7 and not sooner.** ocx 0.6.1's `ocx package claim` writes `login`/`id` **only** —
> so from that release on, every newly minted root is single-spelling and the dual-emit
> survives solely on roots indexbot itself rewrites. That shrinks the legacy surface without
> any migration, and a 0.7 drop lands after the shrinkage rather than during it.
>
> **The residual this closes.** A third-party reader of published roots may today depend on
> `github`/`github_id`. The stop date is what makes that dependency a scheduled break
> instead of a silent one, so it needs to be *named* in the entry-schema reference, not only
> decided.

---

## Row 5 — `actor_id` governance

- **Target repository:** `ocx-sh/index`
- **Labels:** `discussion-needed`, `documentation`
- **Title:** `governance: identify the invoking human on GitHub via the OIDC actor_id claim`

> **The question.** An "owner adds owners" auto-merge rule needs to know *which human*
> triggered a pull request. On GitLab the job-token git transport already gives
> `author = invoker`, so the MR author is the person. On GitHub, `GITHUB_TOKEN` authors as a
> bot, and the PR author is `github-actions[bot]` regardless of who pressed the button —
> so G-19's author match cannot see the human at all.
>
> **Candidate signal: `actor_id`.** The OIDC claim carrying the *triggering human's account
> id*, requestable from a workflow with a plain `id-token: write` permission and verifiable
> against GitHub's issuer.
>
> **Why `actor_id` specifically, and not `GITHUB_ACTOR`.** `GITHUB_ACTOR` is a bare login
> **string** in the workflow environment: it is a username rather than an id, so it does not
> survive a rename or a handle recycle, and it is an environment value rather than a signed
> claim. `actor_id` is a numeric account id inside a signed token — the same property that
> makes `owners[].id` and not `owners[].login` the ownership key under G-19. It is
> materially stronger, and it costs one permission line.
>
> **Explicitly a governance question, not an ocx one.** The ocx ADR deliberately leaves it
> out; it is recorded here so the index's governance model can decide it on its own terms.
> Alternative to weigh: a machine-account PAT per publisher, which identifies the publisher
> but not the invoking human.

---

## Row 6 — Reviewer checklist · RELEASE-BLOCKING (to verify) · **VERIFICATION DONE: requirement ABSENT → file this**

- **Target repository:** `ocx-sh/index`
- **Labels:** `documentation`, `human-review-required`
- **Title:** `governance-contracts: require the G-04 reviewer to check owners[] against forge profiles`

### Verification outcome (recorded either way, per the row's contract)

**Read performed:** `site/src/docs/reference/governance-contracts.md` at
`ocx-sh/index@main`, fetched through the GitHub contents API (86 lines).
**Corroborated by:** a repository-wide code search for `owners` + `profile` in
`ocx-sh/index` (2 hits: `site/src/docs/reference/entry-schema.md`, whose only "profile"
is an unrelated *optimisation profile*, and an internal research artifact).

**Verdict: the requirement is ABSENT.** `governance-contracts.md` requires human review
for G-04 new-package PRs and for every G-05 human-review-required key (`owners` among
them), and G-19 matches a fork-PR author's `github_id` against the *committed* `owners[]`
for the machine lane. Nothing anywhere instructs the reviewer to check that each
`owners[]` entry actually corresponds to the forge profile it claims. G-19
**presupposes** that list is correct; nothing validates it at the moment it is first
minted.

Therefore the issue below is to be filed.

### Issue body

> **The gap.** `governance-contracts.md` requires a human on every G-04 new-package PR and
> on every change to a G-05 human-review-required key, but never says what the reviewer
> must check about `owners[]`. G-19 matches a PR author's `github_id` against the
> committed `owners[]` — which presupposes that list is right. **Nothing validates the list at the
> moment it is first minted**, which is exactly what a first claim does.
>
> **Why this is the whole control.** In the STRIDE analysis for `ocx package claim`, the
> Spoofing row's stated control is **the G-04 reviewer reading the rendered `login:id`
> list — and nothing else catches a human-minted token on a shared release account.** A
> token belonging to a shared release account can mint a root naming any `login:id` pair
> it likes; every automated gate downstream then treats that list as ground truth. If the
> reviewer is not told to check it, the control named in the threat model does not exist
> in the contract, and the mitigation is documentation of a step nobody was asked to
> perform.
>
> **Requested change.** Add a row (or extend G-04) requiring, on a new-package PR, that the
> reviewer confirm every `owners[]` entry resolves on the index's forge and that the
> `login` and the `id` belong to the **same** profile — the `id` being the ownership key
> G-19 matches on, and the `login` being what a human reads. A `login`/`id` pair that
> disagrees is the spoofing case and must block.
>
> Relevant because ocx 0.6.1's `ocx package claim` lets a pipeline mint these roots
> directly, so first claims arrive more often and more automatically than when each one
> was hand-written.

---

## Post A — `ocx-sh/ocx` [#410](https://github.com/ocx-sh/ocx/issues/410) issue body · post at PR time

- **Target:** replace the (empty) body of [#410](https://github.com/ocx-sh/ocx/issues/410)
- **Proposed title:** `package claim: open the first-claim pull/merge request from the CLI`
- Carried verbatim from the dossier's "Issue drafts" section
  (`.agents/discussions/index-claim-command.md`).

> **Problem.** Claiming a namespace on an ocx index means hand-writing `p/<ns>/<pkg>.json` and opening the PR yourself; `ocx package announce` refuses an unclaimed root (`UnclaimedNamespace`) by design, since a first claim is human-lane (G-04). Every new publisher hits this on their first release, and pipelines cannot do it without scripting a PR/MR. Publishers live on GitHub, GitHub Enterprise Server, GitLab.com and self-managed GitLab.
>
> **Proposal.** `ocx package claim [--forge github|gitlab] [--transport api|git] --index-repo [HOST/]NS/PROJECT [--fork NS/PROJECT] --repository oci://HOST/REPO [--owner LOGIN[:ID]]… [--upstream-org ORG --upstream-url URL --disclaimer TEXT] [--out DIR] [--format json] <ns>/<pkg>`
> - Writes the human-governed root: `name`, `repository`, `owners[]` (`login`/`id` only — the forge-neutral spelling indexbot 0.5.0 introduced), `status: active`, `deprecated_message: null`, `created`, optional `upstream`; plus `desc: null`, `tags: {}`. Never a tag.
> - Owners: with no `--owner`, the acting user is detected (CI env `GITLAB_USER_*`/`GITHUB_ACTOR*` first, else the token holder via `GET /user`); a bot identity is refused (exit 64). `--owner` is repeatable and, when given, is the whole list; a login resolves to its numeric id via the forge users API; `login:id` is always accepted and required only where no users API is reachable.
> - Forges and coordinates as announce: github.com/gitlab.com recognised, others declared with `--forge`, never probed. Same `OCX_ANNOUNCE_TOKEN`. Fork-PR or same-repo branch.
> - Opens the PR/MR on its own claim branch; a re-run while it is open updates it; an already-committed root is refused and points at `announce`.
> - `--transport git` per #411, shared with announce; claim runs over both transports.
> - `--out` renders without a forge; `--format json` reports forge, transport, branch, PR URL, owners.
>
> **Acceptance.** On each of GitHub, GHES (`--forge github`), GitLab.com and self-managed GitLab (`--forge gitlab`), the command opens a PR/MR that passes the index's `schema-validate` and gets the `new-package` label; re-run updates the same PR; existing root → refusal naming the root; `--owner alice --owner bob` yields exactly those two with numeric ids; omitted `--owner` under a project-token bot → exit 64 naming `--owner`; `login`/`id` only in the written root.
>
> **Out of scope.** GitHub git transport; ocx-mirror plumbing; the catalog's owner-link href; indexbot's dual-emit retirement (own ADR).
>
> Design: `.agents/discussions/index-claim-command.md` → ADR.

---

## Post B — `ocx-sh/ocx` [#411](https://github.com/ocx-sh/ocx/issues/411) comment · post at PR time

- **Target:** a new comment on [#411](https://github.com/ocx-sh/ocx/issues/411) — enrichment, not a rewrite of the body
- Carried verbatim from the dossier's "Issue drafts" section.

> Design settled in discussion (`.agents/discussions/index-claim-command.md`), heading to an ADR:
> - **Layering:** one `GitLabForge`; reads stay REST, writes (`commit_files`, MR open) go through git commit + push with push options; `compare_branch` is computed from the local clone with both refs fetched; fork ops are refused under `--transport git`; the announce/claim orchestration stays transport-blind. This amends ADR S1 ("REST only, no git subprocess") explicitly.
> - **Job-token facts (GitLab docs, 2026-09-04):** a job token *can* read files, branches, commits and MRs (read-only), which is what makes REST-reads/git-writes work under `CI_JOB_TOKEN` alone; it cannot compare, write, fork or create MRs. Push with a job token: 17.2 flagged, 18.4 GA, cross-project 19.1 GA. ocx's `forge/gitlab.rs` comment and `environment.md` currently say "no access at all" and will be corrected.
> - **Gating:** a rejected push maps to a named error citing the missing version/setting (job-token push, cross-project allowlist), never a generic transport failure.
> - **Credential:** `OCX_ANNOUNCE_TOKEN` stays the REST token (reads under `git` too). The push credential is separately configurable — `OCX_ANNOUNCE_GIT_TOKEN` + `OCX_ANNOUNCE_GIT_USERNAME` (default `gitlab-ci-token`) — falling back to the announce token, then to git's own credential helpers. With no ocx variable set, `--transport git` inside a GitLab job picks up `CI_JOB_TOKEN` for reads and push, so your flow needs no token line at all; `api` never infers. Injected via `GIT_CONFIG_KEY_n/VALUE_n` scoped to the index host; never URL/argv/`.git/config`/log. No fallback between transports, as proposed.
> - **REST header:** ocx sends `JOB-TOKEN` when the value is the job's own `CI_JOB_TOKEN` (so the reads under `--transport git` work with no extra token), `Authorization: Bearer` for every other kind.
> - **Spent branch:** reuses #399's rebuild-on-base with carried tags and a lease-checked force update.
> - **Scope:** ships with `ocx package claim` (#410) in 0.6.1; the new command uses the same transport. GitHub git transport stays out.
> - **Testing:** bare-repo + `git http-backend` fixture whose receive hook reads `GIT_PUSH_OPTION_*` and records the MR, plus credential-leak assertions. Your offer to run a branch against GitLab 19.3 is the live check we lack — we will take it.

---

## Row 7 — release gate 4, the live run · staged draft, **not posted**

- **Target:** appended to Post B, the [#411](https://github.com/ocx-sh/ocx/issues/411) comment, or sent to that reporter directly.
- **Why it is a draft and not a message:** gate 4 is outward-facing and owner-gated like every
  other row in this file. Nothing here has been sent.

> Gate 4 is the only real-server signal in the release, and it cannot be run from the ocx
> repository. On your self-managed GitLab, please run `ocx package announce --transport git`
> (and `ocx package claim --transport git`) against a throwaway index project and report:
>
> 1. **The two server-side refusal texts.** The exact `remote:` line GitLab emits when a
>    job-token push is refused for (a) job-token pushes disabled on the index project and
>    (b) the publishing project absent from that project's job-token allowlist. **These two
>    strings are the authoritative source for what C-044 matches on** — today they are
>    proved only against a fixture-authored line, so a real instance wording them
>    differently would make the classifier fall through to a generic transport failure.
> 2. **The partial-clone proof against your instance.** `git rev-list --objects --all
>    --missing=print` must report a non-zero missing-object count on a `--filter=blob:none`
>    fetch and **zero** on a full-fetch negative control on the same host, and a
>    `GIT_TRACE_PACKET=1` capture must show `filter blob:none` negotiated on the filtered
>    fetch and absent on the control. `test/manual/measure-index-clone.sh <your-repo-url>`
>    does all of this and exits non-zero if the discrimination fails. The measurement
>    already taken is against a GitHub-hosted repository and **cannot** answer this: it says
>    nothing about a self-managed GitLab's Gitaly.
> 3. The instance's GitLab version, and whether the push was job-token or PAT.


## Row 8 — REST fakes permit by default · staged draft, **not posted**

Found during the fix round, 2026-09-06. Same defect class as the git HTTP fixture
that `4da29cbf` repaired, one layer up. Deliberately **not** fixed on the
index-claim branch: it would widen the diff past the feature and reds would land
across acceptance files this branch does not otherwise touch.

**Repo:** `ocx-sh/ocx` · **Labels:** `testing`, `tech-debt`

### Issue body

> **Title:** REST fakes accept uncredentialed requests, so C-063's REST ladder is
> proved against a server with no opinion
>
> `test/tests/fake_forge.py` and `test/tests/fake_gitlab.py` never refuse a request
> that carries no credential at all. Any row asserting that ocx resolved and sent a
> credential over REST passes identically with none — the assertion is about the
> client, and the server has no opinion, so the test cannot fail for the reason it
> exists.
>
> - `fake_forge.py:73` — `AUTH_HEADERS` is a **recording** list, captured into
>   `auth_headers` at `:589` for assertions. It is never consulted as an
>   enforcement gate.
> - The only refusals in either file are armed knobs: `token_identity_absent` → 403
>   on `/user` (`fake_forge.py:918`), and `gitlab_job_token_allowlist_unreadable`
>   → 403 (`fake_gitlab.py:375`). Both require a test to opt in.
>
> This is exactly the defect `4da29cbf` fixed on the git side, where the HTTP
> fixture served `git-receive-pack` anonymously and every credential row was
> therefore vacuous. Repairing that one immediately exposed
> `test_bridge_stderr_is_surfaced`, which had been POSTing with no credential and
> passing.
>
> **Fix shape** (the git fixture is the worked precedent): the fake refuses an
> uncredentialed request by default — 401 with a challenge — and each acceptance
> becomes an explicit knob a test must set, as `git_http_credential` /
> `git_http_private` / `git_http_forbid_fetch` now are. Then a row that forgets the
> credential fails loudly instead of passing quietly.
>
> **Expect existing rows to red.** On the git side that was the point: every row
> that reddened had been proving nothing. Fix the rows, never the fixture.
>
> **Acceptance:** a new row asserting an uncredentialed REST request is refused,
> proved by mutation — remove the fixture's refusal and watch that row fail.


## Row 9 — exit-code classification rests on git's prose · staged draft, **not posted**

Raised by the cross-model adversarial pass on the fix round, 2026-09-06, as two
`high` findings. Triaged here: **neither blocks this branch** — the behaviour it
replaces was strictly worse (a rejected credential exited 1 against a documented
80) and no structured alternative exists at the git-subprocess boundary. The
doc-comment half was fixed on the branch; the design half is this issue.

**Repo:** `ocx-sh/ocx` · **Labels:** `reliability`, `tech-debt`

### What was refuted

The adversary argued `LC_ALL=C` "only controls gettext for this particular client
invocation". That is exactly what the two translated needles are — git's own
gettext strings, emitted by the child git this transport spawns with `LC_ALL=C`
set. It also warned a bare status could match unrelated output; the 403 needle
already uses the full literal for that reason, because the bare number appears in
ordinary object counts.

### Issue body

> **Title:** Published exit codes derive from git's English stderr, with a silent
> fallthrough on any wording change
>
> `credential_rejection_status` (`crates/ocx_lib/src/forge/git_stderr.rs`) maps
> exit 80 from three literal substrings of git's stderr. Two are gettext strings
> stabilised by the `LC_ALL=C` this transport sets; the third is libcurl's and is
> untranslated. Measured against git 2.54.0.
>
> Two residual risks, neither fixed:
>
> 1. **Version drift is silent.** git may reword a message between versions. The
>    classifier then falls through to a generic failure and the run exits 1 while
>    the CLI contract promises 80. The failure mode is a *quiet downgrade*, which
>    is the hardest kind to notice — nothing errors, a number just changes.
> 2. **A 403 carries no provenance.** The classifier reads a 403 as
>    authenticated-then-forbidden, but a WAF, proxy, or repository policy can
>    return 403 before the forge ever sees the credential. The consequence is a
>    wrong *reason*, never a wrong success: such a fetch reports the credential
>    refused (80), such a push reports permission or capability (77/86). The
>    assumption is now stated at the `RejectionScope` definition rather than
>    asserted as fact.
>
> **Why not fixed now:** git's subprocess boundary exposes no structured HTTP
> status. Getting one means parsing `GIT_TRACE_CURL`, or not shelling out to git
> for the transport at all — both larger than the defect they would fix, and the
> current behaviour is a strict improvement on exiting 1.
>
> **Acceptance:** a supported-git compatibility matrix with a row per version
> whose wording is asserted, so a drift reds a test instead of downgrading an exit
> code in production; and either positive evidence of authentication before
> assigning the authenticated-then-forbidden verdict, or a documented statement
> that 403 provenance is not established.


## Row 10 — `push_to_a_moved_branch_classifies_non_fast_forward` is flaky on macOS · staged draft, **not posted**

Observed 2026-09-06 across two deep runs. **Not fixed, and not reproducible
locally** — it needs a macOS runner.

**Repo:** `ocx-sh/ocx` · **Labels:** `flaky-test`, `testing`

### The evidence that it is flaky rather than fixed

| Run | SHA | macOS result |
|---|---|---|
| 34040908190 | `a7f3419a` | **FAILURE** — `GitPushFailed { stderr: Redacted("") }` |
| 34042282820 | `b4b071e6` | **success** |

`git diff a7f3419a b4b071e6` is **two lines, both inside an assertion's format
string**. The code under test is byte-identical, so the difference between red
and green was the run, not the change.

### Issue body

> **Title:** `push_to_a_moved_branch_classifies_non_fast_forward` intermittently
> fails on macOS with an empty git stderr
>
> On `macos-26-arm64` the row fails intermittently with
> `Err(GitPushFailed { stderr: Redacted("") })`: `git push` exited **non-zero
> having written nothing to stderr**, so `classify_push_failure` had no text to
> match and fell through to the unclassified variant. Linux has not reproduced it.
>
> Ruled out by inspection, so the search can start past them:
> - the recording shim `exec`s the real git, so it cannot swallow the stream;
> - `run_git` uses `command.output()`, which pipes both streams;
> - `redact` only substitutes and skips empty secrets, so the empty string is
>   genuine rather than a masking artefact;
> - the remote is a `file://` path under the repository's own `.tmp/`, so macOS's
>   `/private/tmp` symlink is not involved;
> - the sibling `retry_converges_after_a_moved_branch`, which drives the same
>   moved-branch path, passed on the same runner in the same run.
>
> The assertion now reports the recorded argv
> (`fixture.invocations()`), so the next red is diagnostic rather than merely red.
> That output is what this issue is waiting on.
>
> **Why it matters beyond the flake:** the exit code this row defends is derived
> from git's stderr *prose*. An empty stderr is the degenerate case of the
> limitation already stated on `RejectionScope` — when git says nothing, there is
> nothing to classify, and the run reports a generic failure in place of the
> documented one. Whatever makes git silent here is worth knowing for the
> production path, not only for the test.
>
> **Acceptance:** the cause of the empty stderr identified, and either the race
> removed or the classifier given a non-prose signal for this case. A `#[cfg]`
> that skips the row on macOS is **not** acceptable — that converts a flaky check
> into one that cannot fail.
