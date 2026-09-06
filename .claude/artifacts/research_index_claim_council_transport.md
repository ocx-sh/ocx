# Research: index claim command — council on transport layering

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04 (re-verify: GitLab job-token scope and push-option availability move per minor release)
- Author: hex-discuss council lane — three researcher seats (sonnet), blind to each other and to the user's leaning; synthesis by the orchestrator
- Discussion: `.agents/discussions/index-claim-command.md`
- Question put to every seat: where should the api-vs-git transport choice live? (A) a second `Forge` implementation backed by git; (B) a mode inside the announce/claim pipeline mapping each step onto API or git; (C) a separate git-only writer path sharing only root rendering; (D) something better-founded.

## Synthesis

**Agreement (3/3):** option B is rejected — it re-opens the per-call-site branching `adr_announce_gitlab_forge.md` D1 already refused for GitHub-vs-GitLab, and git transport is stateful where REST is stateless. **Agreement (3/3):** no fallback between transports, ever; the credential must travel in git's idiom (env/config injection or a host-scoped helper), never in a remote URL, argv, or `.git/config`. **Agreement (2/3, premortem + operability):** reads stay on REST and only the write half (commit + push + push options) moves to git — a "pure git forge" cannot exist because MR metadata, fork state and mergeability are server objects with no git-wire answer. Premortem frames that as (A) with a load-bearing fidelity caveat; operability frames it as (D), a `--transport git|api` seam confined to `GitLabForge`'s write half. Those are one design in two packagings.

**Divergence:** simplicity picks (C) on diff size — `announce/pipeline.rs` is already forge-free, so only the forge-touching slice of `announce.rs` needs a git twin. Premortem and operability answer that the slice in question is exactly where every past incident lived (C4 fast-forward CAS, C6 no-op short-circuit, C8 stale-fork guard, C15 atomic commit, D12 read window, D13 fail-closed compare, #228/#399), and a hand-copied variant of logic that has changed shape twice already is the drift generator. The orchestrator weighs that as decisive: the small diff C buys is paid back as a second orchestration to keep correct.

**Fact that moved the question** (orchestrator, from the live GitLab docs — see `research_index_claim_prior_art.md` addendum): a `CI_JOB_TOKEN` can read branches, commits, raw files and merge requests (read-only), and can push since 18.4 GA (cross-project since 19.1). So under a git transport every read the pipeline makes stays on REST under the job token, except `compare_branch` (no job-token compare endpoint), which a local clone with both refs fetched answers exactly — a computation, not an approximation, so premortem's "never approximate the three branch-state ops" rule holds.

**Recommendation to the discussion:** one `GitLabForge`, forge-blind orchestration unchanged, with a write transport: reads on REST; `compare_branch` from the local clone under git transport; `commit_files` + `open_or_update_pull_request` via git commit + push with `-o merge_request.create/.target/.title`; `find_fork`/`ensure_fork`/`sync_fork` return a named error under git transport (in-project branch only; `--fork` with `--transport git` refused at parse, exit 64). GitHub keeps REST only (no push-option equivalent; `GITHUB_TOKEN` already works). Whether the git half is a second type or an enum inside `GitLabForge` is for the ADR.

## What every seat said the design must contain regardless of option

1. Never approximate `BranchComparison` / `find_open_pull_request` / `pull_request_mergeability` — answer them truthfully via some API or return a named error (premortem).
2. No fallback between transports — a git failure never retries via REST or vice versa; credential-disclosure and double-post risk, same class D3 refused for forge probing (premortem, simplicity, issue #411).
3. Credential hygiene in git's idiom — token never in a remote URL, argv, `git remote -v`, `.git/config`, shell history; `GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0` (git ≥ 2.31) or a host-scoped helper (premortem, simplicity).
4. One shared fake-forge object graph extended, not forked — a git-transport fake must drive the same graph as the REST fakes (premortem, operability). Push options have no git-wire effect; the fake must react server-side (a receive hook reading `GIT_PUSH_OPTION_*`) or the test proves only "options were sent" (operability).
5. A `git` subprocess reopens S1 ("REST only, no git subprocess") — needs an explicit ADR amendment, not a flag-level exception (premortem).
6. Reusable by `claim`, not announce-only (premortem).
7. Version gating with a named error: job-token push needs GitLab ≥ 18.4 (flagged from 17.2, may be disabled by policy on 18.x), cross-project push ≥ 19.1; the failure must name the setting, per the D14 exit-64 doctrine, not read as a generic transport error (operability).
8. Reuse #399's `RefUpdate::Reset`-with-carried-tags semantics for the spent-branch case; never naive force-push (archaeology lead, premortem).

## Seat: premortem

- **(A)** A `GitLabGitForge` under job-token pressure hardcodes `None`/`Unknown` for `find_open_pull_request`, `pull_request_mergeability`, `compare_branch`, `find_fork`/`ensure_fork`; `BranchState::Stale` becomes unreachable, a diverged branch under an open MR reads `Spent` and is rewritten — #228/#399 return invisibly. Early signal: a trait method that unconditionally returns `None` with no path to another value.
- **(B)** The shape D1 rejected; an `if transport == Git` branch bolted on at one call site and missed at a sibling; C15 atomicity splits silently (local commit atomic, push failing mid-flight is a different half-committed state).
- **(C)** `pipeline.rs` holds "everything a forge is not needed for", so branch-state classification, D1 tag carry-forward, and `NonFastForward` retry all live in `announce.rs` — a second writer re-implements them and drifts (forgets the carry on retry, drops a concurrent announce's additions).
- **(D)** as usually framed assumes job-token reads work; where an instance disables cross-project job-token access, reads fail too and a `--depth 1` fallback reintroduces D13's "unclassifiable compare read as Identical".
- **Verdict: (A) with the fidelity caveat** — REST for the five ops with no git equivalent, git only for the write path; keeps orchestration forge-blind, gives each method an honest place to succeed or fail. (D) is a variant of (A) that under-specifies the read-rejection case.
- Sources: `crates/ocx_lib/src/forge/api.rs:1-300`; `crates/ocx_lib/src/announce.rs:592-671`; `adr_announce_gitlab_forge.md` D1 `:60-70`, D4 `:124-155`, D13 `:304-313`, D15 `:332-342`; `announce/pipeline.rs:1-12`; `forge/gitlab.rs:1-27`; `test/tests/fake_forge.py:1-25`; [ocx#411](https://github.com/ocx-sh/ocx/issues/411).

## Seat: operability

- **(A)** cannot be pure git — MR lookup and mergeability are server metadata — so it is a hybrid still holding an API-scoped token for reads, undercutting the "second implementation" story; operator surface: `--forge gitlab --transport git` plus `OCX_ANNOUNCE_TOKEN` now meaning PAT or `CI_JOB_TOKEN` by transport, which `package_announce.rs:19-21` must document.
- **(B)** if/git-or-api at each write step of a 2000-line pipeline; centralise the seam or repeat D1's failure.
- **(C)** worst operationally: forks read→regenerate→commit→open-PR→retry into a second copy that inherits none of the fake-forge, mutation-tested coverage for C4/C6/C8/C15/D12.
- **(D)** `--transport git|api` orthogonal to `--forge`, wired only into `GitLabForge`'s `commit_files` and the MR-open half of `open_or_update_pull_request`; every other method REST unconditionally. One flag, one documented credential-shape note.
- Testability: `fake_forge.py` serves REST over an in-memory object graph and never speaks the git wire protocol; a git transport needs a bare repo + `git http-backend` or a stub that inspects push options and opens a fake MR record — new fixture burden under every option; it proves "options sent", not "GitLab acted", so a live-GitLab smoke test (already flagged absent for REST, `adr_announce_gitlab_forge.md:262-264`) becomes more urgent.
- Version skew: job-token push 17.2 flag-gated, 18.3 enabled on GitLab.com, 18.4 GA; cross-project 19.0 → GA 19.1; per-project toggle + 200-entry allowlist. Self-hosted lags; 17.x lacks it, 18.x may disable it by policy — the error must name the setting.
- **Verdict: (D), narrowly scoped.**
- Sources: `forge/api.rs:113-300`; `adr_announce_gitlab_forge.md:47-113,262-264`; `fake_forge.py:1-25`; `package_announce.rs:19-21,205-215`; [ocx#411](https://github.com/ocx-sh/ocx/issues/411); [GitLab job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/); [push options](https://raw.githubusercontent.com/terrchen/gitlab/master/doc/user/project/push_options.md); [Renovate GitLab platform](https://docs.renovatebot.com/modules/platform/gitlab/) (REST-only, no git-push mode); [Homebrew discussion #3383](https://github.com/orgs/Homebrew/discussions/3383) — **retracted 2026-09-05**: the tooling research lane read it; it is a fork-permission troubleshooting thread and supports no hybrid-transport or fixture claim. Do not cite it.

## Seat: simplicity

- **(A)** `Forge` is 11 async methods (`api.rs:125-300`); `GitHubForge`/`GitLabForge` are 1518/1021 lines. Git has no faithful analog for `find_fork`/`ensure_fork`/`find_open_pull_request`/`pull_request_mergeability`; `CommitBase`/`RefUpdate`/`BranchComparison` exist to fake git's fast-forward refusal (C4) and atomicity (C15) over stateless REST, so mapping a real git backend onto them is manufactured work. An impl approximating ~40% of the contract contradicts `api.rs:116` ("must return an error rather than approximate it"). ~800–1200 lines.
- **(B)** threads `if git {…}` through every step of `announce()`'s ~260-line orchestration; re-opens C4/C6/C8/C15 per branch.
- **(C)** `pipeline.rs` (2041 lines) is forge-free and reuses verbatim; only the forge-touching slice of `announce.rs` needs a git twin, smaller still because fork/mergeability do not apply to the job-token case. No `git2`/`gix` in `Cargo.toml`; shell out to system `git` via `utility/child_process.rs` (`spawn_and_wait`/`Env`), zero new dependencies; credential via `GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0`; fixture = local bare repo. One ~150–300 line module + a flag.
- Is a transport needed at all? Yes — author override on the API commit is moot (job token has no MR write access); no GitLab setting re-attributes an MR; OIDC per-user tokens defeat the ask; `--out` does not open the MR.
- **Verdict: (C).**
- Sources: `forge/api.rs:36-300`; `announce.rs:69-150`; `announce/pipeline.rs:1-16`; `utility/child_process.rs:96,138`; `adr_announce_gitlab_forge.md` D1; [ocx#411](https://github.com/ocx-sh/ocx/issues/411); [GitLab job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/); [gitlab-foss#64320](https://gitlab.com/gitlab-org/gitlab-foss/-/issues/64320).
