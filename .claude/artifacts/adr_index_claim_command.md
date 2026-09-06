# ADR: Index claim command, forge write transports, and forge-neutral owners

## Metadata

**Status:** Accepted (2026-09-05, owner mandate: full autonomous implementation)
**Date:** 2026-09-05
**Decision title:** Index claim command, forge write transports, and forge-neutral owners
**Deciders:** Michael Herwig (owner) — positions ratified in `.agents/discussions/index-claim-command.md`, 2026-09-04
**Tier:** high
**Blast radius:** external contract (CLI grammar, env vars, index owners wire) + cross-area
**Reversibility:** one-way (high)
**Beads Issue:** N/A
**Related issues:** [ocx#410](https://github.com/ocx-sh/ocx/issues/410) (claim command), [ocx#411](https://github.com/ocx-sh/ocx/issues/411) (git transport), [ocx#399](https://github.com/ocx-sh/ocx/issues/399) (diverged announce branch, fixed in `6feb9c02`)
**Tech Strategy Alignment:**
- [x] Follows the Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, Tokio, the existing `reqwest` client. **Zero new crate dependencies**: the git half shells out to the system `git` through the existing `utility/child_process.rs` boundary rather than vendoring `git2` or `gix`.
- [x] Deviation justified: a `git` subprocess amends `adr_announce_gitlab_forge.md` D0/S1 ("REST only, no git subprocess"). See Constitution Check §1.

**Domain Tags:** integration, security, api
**Amends:**
- `design_spec_announce_initiative.md` **S1**, at its source (`:27`, "Transport = **REST API only**. No git subprocess, **no local clone of the index repo**"). This is the canonical text; `adr_announce_gitlab_forge.md` D0 only restates it, so amending the restatement alone would leave the ratified register still refusing both halves of what this design does.
- `adr_announce_gitlab_forge.md` D0 (the S1 restatement — the secondary edit), D1 (trait operation set), and its `Forge` contract for `commit_files` / `open_or_update_pull_request` / `ensure_push_access`
**Supersedes:** N/A
**Superseded By:** N/A
**Release vehicle:** ocx 0.6.1 — the claim command and the git transport ship together (dossier decision 8)

---

## Context

`ocx package announce` refuses to write a package's first index entry. `require_root`
(`crates/ocx_lib/src/announce/pipeline.rs:123-134`) raises
`AnnounceError::UnclaimedNamespace` whenever the base ref carries no committed root, by
design: a first claim is a human-lane action under the index's G-04 governance contract,
and no automation may bypass the reviewer who judges whether a claimed namespace
plausibly belongs to the entity it names.

The consequence is that every new publisher's first release fails, and the documented
remedy is to hand-write `p/<namespace>/<package>.json` and open the pull request by hand.
`test/manual/announce-e2e/CLAIM.md` in this repository is that remedy written out for a
single package: paste-ready JSON, a `gh pr create` invocation, and field-by-field provenance. It is a good artifact and it is exactly the wrong shape for a tool whose target
users are "automation tools — GitHub Actions, Bazel rules, devcontainer features, CI
scripts" (`product-context.md`).

Three problems are entangled and must be decided together, because each one's solution
constrains the others:

1. **No claim command.** A publisher cannot open the first-claim pull request from the CLI,
   on any forge. Publishers live on github.com, GitHub Enterprise Server, gitlab.com and
   self-managed GitLab.
2. **No write path for a GitLab CI job.** [ocx#411](https://github.com/ocx-sh/ocx/issues/411)'s
   reporter runs eight releases through a shell workaround, because a project access token
   authors the merge request as a bot and the index's G-19 owner-gated auto-merge matches on
   the numeric id of a *human* owner. GitLab's own answer — a job-token `git push` carrying
   `-o merge_request.create` — is unreachable from a REST-only client. GitLab's job token
   cannot create a merge request over REST at all.
3. **The owners rename is half-done.** indexbot 0.5.0 renamed `github`/`github_id` to
   `login`/`id` and dual-emits both. ocx has never written `owners[]` at all — a claim
   command would be the first ocx writer of that field, and 0.6.1 is therefore the release
   that decides which spelling every new root carries.

`ocx` itself never parses `owners`: `IndexRoot` (`crates/ocx_lib/src/oci/index/wire.rs`) has
no such field, and `announce/pipeline.rs` carries the array through as an opaque
`serde_json::Value`. So the owners decision is invisible to every ocx client and visible
only to indexbot, the index's JSON schema, and `ocx-catalog`'s rendering.

---

## Decision Drivers

Weighted criteria, applied to every option matrix below. Weights are on a 1–5 scale and are
the same in all four matrices, so scores are comparable across parts.

| # | Criterion | Weight | Meaning |
|---|---|---|---|
| D1 | **Concurrency correctness preserved** | 5 | The C4 compare-and-swap, the C6 unchanged short-circuit, the C8 stale-fork guard, the C15 atomic commit and the [#228](https://github.com/ocx-sh/ocx/issues/228)/[#399](https://github.com/ocx-sh/ocx/issues/399) branch-state rules keep holding, without being re-derived per transport. |
| D2 | **Credential containment** | 5 | No secret in argv, a URL, `.git/config`, a log line, a child env that does not need it, or a forge error body. |
| D3 | **Blast radius and reversibility** | 4 | How much of the CLI grammar, the wire format and third-party CI is committed by the choice, and what it costs to undo. |
| D4 | **Operator legibility** | 4 | A failure names the setting, the version or the flag that produced it; a pipeline can branch on the exit code without parsing stderr. |
| D5 | **Implementation and test cost** | 3 | Lines written, fixtures built, coverage that must be re-earned rather than inherited. |
| D6 | **Wire and contract stability** | 3 | Whether published artifacts and third-party readers keep working. |
| D7 | **Solves the reported problem for the named users** | 5 | Does the option close [#410](https://github.com/ocx-sh/ocx/issues/410) and [#411](https://github.com/ocx-sh/ocx/issues/411) for *both* named users — the CI pipeline and the publisher invoking by hand? Added after the first draft scored two matrices' winners below their rejected alternatives: a criterion set that cannot see "this does not solve the problem" or "the manual path is unusable" is measuring the wrong thing, and diagnosing that in prose while keeping the set was decoration around a decision already made. |
| D8 | **Cost of ownership** | 3 | What must be maintained forever, in particular any surface that tracks an external tool's **non-contractual** output. The git stderr classifier matches phrases git does not treat as a contract, and a classifier miss degrades every push failure to exit 1 — D5 prices writing it, D8 prices living with it. |

**Known limitation, stated rather than hidden.** D1 and D2 are constants (5) across every row of Parts 1b and 3: neither owner input nor the owners wire changes concurrency or credential handling. Ten of the thirty-two weight-points are therefore uninformative exactly where those two decisions are closest. The criteria set is kept uniform anyway so scores compare across parts; read the differentiating columns (D3, D4, D7) in those two matrices.

---

## Industry Context & Research

**Research artifacts** (all `Expires: 2027-03-04`, all dated 2026-09-04 unless noted):

- [`research_index_claim_recon.md`](./research_index_claim_recon.md) — codebase recon across ocx, indexbot, index, catalog
- [`research_index_claim_prior_art.md`](./research_index_claim_prior_art.md) — claim UX, cross-forge PR automation, CI identity, identity schemas, deprecation practice; **§ addendum** carries the live GitLab job-token fact check
- [`research_index_claim_archaeology.md`](./research_index_claim_archaeology.md) — announce placement, `Forge` trait origin, #399, token history, ocx-mirror shell-out
- [`research_index_claim_council_transport.md`](./research_index_claim_council_transport.md) — three-seat council on transport layering, plus the eight invariants every seat demanded
- [`research_index_claim_security.md`](./research_index_claim_security.md) — credential injection, TLS trust divergence, push-option injection, job-token scope, owner identity, temp-clone hygiene, supply chain
- [`research_index_claim_git_tooling.md`](./research_index_claim_git_tooling.md) — `gix`/`git2`/system-`git` evaluation, plumbing recipe, push-option grammar, fixture shape
- [`research_index_claim_operability.md`](./research_index_claim_operability.md) — failure modes, exit-code precedent, CI recipes, live-forge testing cost, report fields, preflight
- [`discover_index_claim_map.md`](./discover_index_claim_map.md) — 7/7 dossier claims verified against HEAD `487570fb`, full architecture map, prior-decision inventory

**Trending approaches.**

- **Two incompatible claim models, not a spectrum.** Registries whose artifacts are
  automatically validatable treat first publish as the claim (crates.io, npm,
  Terraform/OpenTofu — no PR at all). Registries needing human judgement gate the claim on a
  merged pull request, with a bot doing only fork/branch/push/PR mechanics: winget
  ([winget-create](https://github.com/microsoft/winget-create),
  [Moderation.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Moderation.md)),
  Homebrew (`brew bump-formula-pr`,
  [docs](https://docs.brew.sh/How-To-Open-a-Homebrew-Pull-Request)), conda-forge
  ([staged-recipes](https://github.com/conda-forge/staged-recipes/blob/main/README.md)),
  vcpkg, Scoop. The ocx index is squarely in the second family (G-04), so the CLI's job is
  the mechanics and never the judgement.
- **CI-native identity is displacing long-lived tokens.** GitLab's job-token `git push`
  reached GA in 18.4; GitHub shipped immutable OIDC subject claims GA for new repositories
  from 2026-07-15 ([changelog](https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens/)).
  GitLab shipped fine-grained job-token permissions to GA in 18.3 specifically because the
  ambient job-token scope was over-privileged
  ([announcement](https://about.gitlab.com/blog/fine-grained-job-tokens-ga/)). Making the
  job-token pickup opt-in behind an explicit transport flag tracks that direction rather
  than fighting it.
- **No cross-forge push-option primitive exists.** GitLab has
  `git push -o merge_request.create` (since 11.10, 2019,
  [push options](https://docs.gitlab.com/topics/git/commit/#push-options-for-merge-requests));
  GitHub has nothing equivalent and always needs a second REST call
  ([PR REST](https://docs.github.com/en/rest/pulls/pulls)); Gerrit uses a magic ref; Forgejo
  has AGit, a third convention. There is no shared abstraction worth building, which is why
  the git transport is GitLab-only by construction and not by scope-cutting.
- **Additive-only versioning is the package-index norm.** crates.io's index gates additions
  on `"v"` and never removes fields; PyPI's Simple API took `format_version` 1.0 → 1.4 purely
  additively ([PEP 691](https://peps.python.org/pep-0691/),
  [PEP 700](https://peps.python.org/pep-0700/)). No primary source was found for any package
  index retiring a field by dual-emit-then-drop — that pattern is an ocx original and must be
  framed as one.
- **Nobody has cracked cheap live-forge verification.** GitLab's own CLI team has an open,
  unresolved issue asking for a real-instance integration job
  ([gitlab-org/cli#1161](https://gitlab.com/gitlab-org/cli/-/issues/1161)); `python-gitlab`
  runs a pinned `gitlab/gitlab-ce` on a schedule at real cost. The fixture is the PR gate;
  a human's live run is the only real-server signal.

**Key insight.** A `CI_JOB_TOKEN` *can* read branches, commits, raw files, tags and merge
requests — read-only — and *cannot* compare, write, fork, or create a merge request
([job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/), fetched 2026-09-04). That
single fact is what makes "REST reads, git writes" a coherent design instead of a
compromise: under a bare job token every read the pipeline makes stays on REST, and the one
read with no job-token endpoint — `compare_branch` — is answered *exactly* by a local clone
that has both refs, not approximated. It also corrects two statements in this repository
(`crates/ocx_lib/src/forge/gitlab.rs:148-160` and
`website/src/docs/reference/environment.md:128`), which say a job token reaches none of
files, commits, branches or merge requests. That is true for writes and false for reads.

> The council-transport artifact cites Homebrew discussion #3383 as fixture-testing
> precedent. The tooling lane fetched it: it is a fork-permission troubleshooting thread and
> supports no such claim. It is **retracted and not cited here**, in any section.

---

## Considered Options

### Part 1 — Where a claim lives

#### Option C-A: `ocx package claim <namespace>/<package>` (a sibling command)

**Description:** A new leaf under the `ocx package` group, next to `announce`, sharing its
`--index-repo` / `--forge` / `--fork` / `--out` grammar and gaining `--transport`.

| Pros | Cons |
|------|------|
| **No `ocx index` verb writes to a forge.** The group's network work is read-only registry traffic (`index update` fetches tags; `index sync` reads each source live and is refused by `--offline`; only `regenerate` consults no source), so a forge-writing verb there would be a new *kind* of operation, not more of the same. | "Claim" reads as an index operation, so `ocx index claim` is what a newcomer types first. |
| `ocx package <verb>` is per-package, identifier-driven and never reads `ocx.toml` — exactly the claim's shape (`subsystem-cli-commands.md`). | Two write commands under `package` now share a large options surface that must not drift. |
| The two write commands share one options struct and one credential model, so a transport or credential change lands once. | |

#### Option C-B: `ocx index claim`

**Description:** A new subcommand under the `ocx index` group.

| Pros | Cons |
|------|------|
| Matches the noun a newcomer reaches for, and a publisher's mental model is index-shaped. | The claim writes a *package* entry; the index group's unit of work is a source or registry, not a package. **This is the load-bearing objection**, and it survives whatever the index group later learns to do. |
| | Splits the two write commands across two groups, so `--transport`, `--forge`, `--index-repo` and the credential model must be declared twice. |
| | ~~Breaks an offline-purity invariant~~ — **withdrawn**. The group is already sometimes-network (`index update`, `index sync`), so that objection was false; it is recorded struck through rather than deleted, because the first draft leaned on it and a reader comparing versions should see why it went. |

#### Option C-C1: An explicit `announce --claim` flag

**Description:** `announce` gains a flag that, and only that, permits it to create an absent
root. The flagless invocation still fails `UnclaimedNamespace`.

| Pros | Cons |
|------|------|
| Removes **no** signal: `UnclaimedNamespace` (exit 79, pinned by test) still fires for every flagless run, so a release wrapper's branch is untouched. This is the honest steelman and the first draft attacked it wrongly. | **A claim must write `tags: {}` (D-C3) while writing `tags` is announce's entire job.** One command would push bot-regenerated content through the G-04 human lane, or grow a flag that suppresses its own core behaviour. |
| Teaches the publisher one command instead of two. | **The "one run" it buys is impossible anyway.** `require_root` reads the *committed* root at the base ref, so even inside one process an unmerged claim does not satisfy the announce that follows it. The saved run does not exist. |
| | Announce's option surface is already dominated by four mutually-exclusive tag-selection modes; adding a claim's six flags to it makes a grammar nobody can hold in their head. |

#### Option C-C2: Auto-claim on an absent root

**Description:** `announce` creates the root whenever `require_root` reads `None`, with no flag.

| Pros | Cons |
|------|------|
| Zero new surface for a pipeline. | Removes the `UnclaimedNamespace` signal a release wrapper branches on today. |
| | Auto-claim was foreclosed by policy, never built and never reverted (archaeology §2). Reversing it is a governance decision the index owns, not a CLI one. |
| | Inherits C-C1's `tags: {}` incompatibility in full. |

#### Option C-D: No command — ship a renderer only (`--out`)

**Description:** Render the claim root to a directory; the human still opens the PR.

| Pros | Cons |
|------|------|
| Smallest possible surface; no forge write path at all. | Leaves the actual gap open: the hand-crafted `gh pr create` invocation is the part that does not scale, not the JSON. |
| Zero new credential handling. | Every publisher still writes bespoke PR-opening glue, which is precisely what #410 reports. |

**Weighted matrix.**

| Option | D1 (5) | D2 (5) | D3 (4) | D4 (4) | D5 (3) | D6 (3) | D7 (5) | D8 (3) | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| **C-A sibling under `package`** | 5 | 5 | 4 | 5 | 4 | 5 | 5 | 4 | **150** |
| C-B under `index` | 5 | 5 | 3 | 4 | 3 | 4 | 5 | 4 | **136** |
| C-C1 explicit `announce --claim` | 5 | 5 | 2 | 3 | 5 | 3 | 4 | 3 | **123** |
| C-C2 auto-claim | 5 | 5 | 1 | 2 | 4 | 2 | 2 | 3 | **99** |
| C-D renderer only | 5 | 5 | 5 | 2 | 5 | 5 | 1 | 5 | **128** |

With D7 in the set the arithmetic now agrees with the prose: C-D is cheap and does not solve
[#410](https://github.com/ocx-sh/ocx/issues/410), and that is a scored fact rather than an
override. C-C1's D1 and D5 are scored honestly — folding into announce would reuse the
concurrency machinery verbatim (D1 = 5) and cost one flag rather than five new files
(D5 = 5) — and it still loses, on the governance-lane incompatibility that no score can
soften.

---

### Part 1b — Owner input

#### Option O-A: `--owner` always required

| Pros | Cons |
|------|------|
| No identity lookup, no bot heuristic, no users-API dependency. Nothing can be inferred wrongly. | The common case — one person claiming their own namespace by hand — types their own login and numeric id. Hostile to the manual path the owner explicitly named. |
| Works identically under every credential, including a job token. | Every CI recipe carries a literal id, which is exactly the value nobody can remember. |

#### Option O-B: detect when omitted, an explicit list replaces, bot identities refused *(chosen)*

| Pros | Cons |
|------|------|
| The manual case is `ocx package claim acme/widget --repository oci://…` and nothing else. | Needs a users-API call, or CI env vars, or both; under a bare job token neither is guaranteed. |
| "If you write owners, what you wrote is the list" is the only rule with no surprising middle state. | The bot refusal is not a complete impersonation guard — a human-minted PAT on a shared "release" account reads as a person (security lane §5). |
| Multi-owner claims are `--owner alice --owner bob`, with no ceremony. | |

#### Option O-C: detect when omitted; an explicit list *appends* to the detected user

| Pros | Cons |
|------|------|
| The invoker is never accidentally left off their own claim. | There is no way to claim *on behalf of* someone else — a platform team filing for a product team always ends up in `owners[]`. |
| | The written list differs from what the operator typed, which is the one property a governance field must not have. |

#### Option O-D: detect always; `--owner` adds only

| Pros | Cons |
|------|------|
| Simplest mental model for CI. | Same defect as O-C, and worse: a bot identity can never be excluded, so posture (c) — ownership managed by repository access — becomes unexpressible. |

**Weighted matrix.**

| Option | D1 (5) | D2 (5) | D3 (4) | D4 (4) | D5 (3) | D6 (3) | D7 (5) | D8 (3) | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| O-A always explicit | 5 | 5 | 5 | 4 | 5 | 5 | 2 | 5 | **141** |
| **O-B detect, replace, refuse bots** | 5 | 5 | 4 | 5 | 3 | 5 | 5 | 3 | **144** |
| O-C detect, append | 5 | 5 | 2 | 2 | 3 | 3 | 3 | 3 | **108** |
| O-D detect always, add only | 5 | 5 | 1 | 2 | 3 | 2 | 2 | 3 | **96** |

**O-A's steelman is stronger than the first draft credited, and it is not the ergonomic one.**
O-A is the only option whose guarantee does not degrade on the path this ADR advertises
hardest: under a bare `CI_JOB_TOKEN` the users API is unreachable, so O-B's identity is
verified by a name-shape heuristic and nothing else (see D-C4's two-rule table and the
`owner_identity_source` report field, both added because of this). O-A never infers anything
and therefore never has a weak form. It loses on D7: requiring a person to type their own
numeric id to claim their own namespace makes the hand-invocation case, which the owner named
explicitly, unusable. O-B is chosen with the residual named rather than argued away.

---

### Part 2 — Transport layering

The council put four framings to three blind seats
([`research_index_claim_council_transport.md`](./research_index_claim_council_transport.md)).

#### Option T-A: a second `Forge` implementation backed by git

| Pros | Cons |
|------|------|
| The orchestration stays forge-blind and transport-blind by construction. | A "pure git forge" cannot exist. Merge-request metadata, fork state and mergeability are server objects with no git-wire answer, so five of eleven operations would return `None`/`Unknown` unconditionally. `BranchState::Stale` becomes unreachable, a diverged branch under an open request reads as spent and is rewritten, and [#228](https://github.com/ocx-sh/ocx/issues/228)/[#399](https://github.com/ocx-sh/ocx/issues/399) return invisibly. |
| | Contradicts `forge/api.rs:117` verbatim: an implementation that cannot hold the contract "must return an error rather than approximate it". |

#### Option T-B: a transport mode threaded through the announce/claim pipeline

| Pros | Cons |
|------|------|
| No new type at all. | Exactly the shape `adr_announce_gitlab_forge.md` D1 already rejected for GitHub-vs-GitLab: an `if transport == Git` at one call site and missed at a sibling, with every C-cell decision re-decided per transport at the point of use. All three council seats rejected it. |
| | C15 atomicity splits silently: a local commit is atomic, and a push failing mid-flight is a different half-committed state. |

#### Option T-C: a separate git-only writer sharing only root rendering

| Pros | Cons |
|------|------|
| Smallest diff. `announce/pipeline.rs` (2040 lines) is already forge-free and reuses verbatim; only the forge-touching slice of `announce.rs` needs a twin. | The slice it would duplicate is precisely where every past incident lived: C4 fast-forward CAS, C6 no-op short-circuit, C8 stale-fork guard, C15 atomic commit, D12 read window, D13 fail-closed compare, #228, #399. A hand-copied variant of logic that has changed shape twice already is the drift generator. |
| **Zero risk to the shipped REST path, and no flag added to a shipped command** — the whole `--transport`-on-announce surface disappears. This is the option's strongest argument and it is real. | Inherits none of the fake-forge coverage the REST path earned, and carries the stderr classifier *plus* a second orchestration to keep correct — the worst cost-of-ownership score in the matrix. |

#### Option T-D: a `--transport` seam confined to `GitLabForge`'s write half *(chosen)*

**Description:** One `GitLabForge`, holding a `WriteTransport` enum field. Every read stays
REST under both transports **except `compare_branch`**. `compare_branch` is computed from the
local clone under `git`
because the clone already has both refs. `commit_files` and `open_or_update_pull_request`
become a local commit plus one push carrying `-o merge_request.*`. Fork operations return a
named error. The orchestration learns nothing.

| Pros | Cons |
|------|------|
| Every operation has an honest place to succeed or fail; nothing returns a fabricated `Unknown`. | One type now holds two write paths, so its own tests must cover both. |
| One constructor (`ForgeKind::client`) stays the single place a concrete forge is named. | The commit/push split forces one genuine `Forge` contract change (see D-T4) — a named fidelity cost, not a hidden one. |
| The REST path's fake-forge and mutation coverage applies unchanged to every read but one — `compare_branch`, which under `git` is answered from the clone and needs its own coverage (D-T5). | |

**Weighted matrix.**

| Option | D1 (5) | D2 (5) | D3 (4) | D4 (4) | D5 (3) | D6 (3) | D7 (5) | D8 (3) | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| T-A second `Forge` impl | 1 | 5 | 3 | 2 | 3 | 3 | 3 | 2 | **89** |
| T-B mode in the pipeline | 1 | 5 | 2 | 2 | 4 | 3 | 3 | 1 | **85** |
| T-C separate git writer | 2 | 5 | 4 | 3 | 5 | 4 | 4 | 1 | **113** |
| **T-D seam in `GitLabForge`** | 5 | 5 | 4 | 5 | 3 | 4 | 5 | 2 | **138** |

**D2 is uniform at 5 and does not differentiate.** All four options perform the same push with
the same `GIT_CONFIG_VALUE_n` mechanism; the layering choice does not move where the secret
lives. The first draft gave the winner a free point here, which is corrected — the verdict
never depended on it.

**D8 is where T-C and T-D genuinely differ, and it is the axis the first draft could not
express.** Both must track git's non-contractual stderr forever (so neither scores well);
T-C additionally carries a second orchestration whose divergence from the first is invisible
until it drops a concurrent announce. That is the drift argument, now scored instead of
asserted.

**Risks and reversibility per option.** T-A and T-B are effectively irreversible once
shipped — both put transport knowledge somewhere it has to be surgically removed. T-C is
reversible by deletion but leaves a drifted copy behind while it lives. T-D is reversible by
removing one enum arm and one flag; the flag is the whole external surface.

---

### Part 3 — Owners wire

#### Option W-A: ocx emits both spellings (`login`+`id` and `github`+`github_id`)

| Pros | Cons |
|------|------|
| Byte-identical to what indexbot writes today, so a claim root and a bot-rewritten root never differ. | Perpetuates a duplication the accepted indexbot ADR exists to end, and makes ocx a second producer of a derived field it does not own. |
| A third-party reader that copied the documented `{github, github_id}` shape keeps working against new roots. | The two spellings can disagree in a hand-edited root, and ocx would have no rule for which wins — indexbot's parser owns that rule. |

#### Option W-B: emit `login`/`id` only, no `format_version` bump *(chosen)*

| Pros | Cons |
|------|------|
| ocx clients never parse `owners`, so the change is invisible on the wire ocx consumes. The live `schema/root.schema.json` on `ocx-sh/index` main already accepts a `login`/`id`-only owner (`anyOf`, checked 2026-09-04 via the GitHub API). | A third-party reader that copied `{github, github_id}` breaks silently on new roots. |
| Every new root is single-spelling from 0.6.1, so indexbot's dual-emit survives only on roots it rewrites. | `format_version` gives such a reader no signal that anything changed. |
| The read-both rule stays in indexbot's parser, which is where it belongs. | |

#### Option W-C: emit `login`/`id` only and bump `format_version` to 2

| Pros | Cons |
|------|------|
| Gives a third-party reader an explicit signal. | The index's own wire contract says a client MUST treat an unrecognised higher `format_version` as a hard error requiring an upgrade. A bump would therefore **break every deployed ocx** for a governance-only field ocx does not read. |
| | `format_version` has never been bumped; every change so far landed additive. Spending the first bump on `owners` sets the precedent that governance fields gate client compatibility. |
| | The indexbot 0.5.0 ADR's "needs a `format_version` gate" clause is written for client-visible URL/semantic shape; `owners` is not that. |

#### Option W-D: emit `login`/`id` plus a per-owner `forge` or `host` field

| Pros | Cons |
|------|------|
| Makes an owner self-describing across forges. | **There is exactly one reader that would consume it, and this work is what breaks it.** `ocx-catalog` hard-codes every owner link to `https://github.com/<login>` (`MetaRail.vue:222-228`, with a self-documented `KNOWN GAP`), so a claim filed from GitLab renders each owner as a github.com profile belonging to somebody else or to nobody. W-D would fix that at the wrong layer. |
| | An index lives on one forge, and the compared author id comes from that forge — the field would be a constant on every row. The fact belongs to the index or to the catalog, not repeated per owner. |
| | The only id+login precedent found in the wild (all-contributors) has no provider field, for exactly this reason. Adding one contradicts the sole precedent. |

> **The catalog defect is real and is promoted out of the deferred list.** W-B itself is safe
> for the catalog — `ownerLogin` already reads `login ?? github` (`usePackageRoot.ts:24-25`) —
> but the GitHub-hardcoded href is a **named prerequisite for advertising GitLab-sourced
> claims**, not a nice-to-have. Filing a GitLab claim before it is fixed publishes owner links
> that point at the wrong people.

**Weighted matrix.**

| Option | D1 (5) | D2 (5) | D3 (4) | D4 (4) | D5 (3) | D6 (3) | D7 (5) | D8 (3) | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| W-A dual-emit from ocx | 5 | 5 | 3 | 4 | 3 | 5 | 3 | 3 | **126** |
| **W-B `login`/`id` only, no bump** | 5 | 5 | 4 | 4 | 5 | 4 | 5 | 5 | **149** |
| W-C `login`/`id` + bump | 5 | 5 | 1 | 4 | 4 | 1 | 4 | 3 | **114** |
| W-D add a per-owner forge field | 5 | 5 | 2 | 3 | 3 | 3 | 2 | 2 | **104** |

---

## Decision Outcome

**Chosen:** C-A + O-B + T-D + W-B.

**Rationale.** The three parts are one decision because the claim command is the first ocx
writer of `owners[]` (so it decides the wire), and because a claim from a GitLab CI job is
the reported use case that the API transport cannot serve (so it decides the transport). A
claim command without the git transport ships a command the #411 reporter still cannot use;
a git transport without the claim command ships a transport whose first-run case still
fails. Shipping both in 0.6.1 is the owner's decision 8 and the matrices agree.

The decisions below are numbered by part: `D-C*` for the command, `D-T*` for the transport,
`D-W*` for the wire.

### What is genuinely one-way

The metadata says "one-way (high)" for the record as a whole. Precisely, five things cannot be
taken back, and the rest can:

| Genuinely one-way | Why |
|---|---|
| Bytes in an **already-merged** root | CLAUDE.md's one hard exception: published artifacts must keep resolving. |
| The published names `OCX_ANNOUNCE_GIT_TOKEN` / `OCX_ANNOUNCE_GIT_USERNAME` | This ADR defers `OCX_FORGE_*` *because* renaming breaks every publisher's CI. The same reasoning applies to these the moment they ship. |
| `ExitCode::ForgeCapabilityUnavailable = 86`, **and the `error.kind` value `forge_capability_unavailable` that comes with it** | A number scripts `case $?` on, and a string the JSON error envelope's consumers pattern-match on. The category is not optional bookkeeping: the wildcard-free `from_exit_code` match makes it a compile error to add the code without it. |
| The new JSON keys on **announce's already-shipped report** | Additive today, unremovable tomorrow. |
| The `CapabilityName` / `CheckStatus` wire spellings | Same: they enter a shipped JSON document. |

| Genuinely two-way | Why |
|---|---|
| Every `Forge` trait signature and the `GitLabForge` transport enum | Internal; CLAUDE.md grants crate-internal shape zero stability. |
| The `--transport` flag | Removable; its default preserves today's behaviour, so removal is a no-op for every current invocation. |
| `ocx package claim`'s own grammar | Pre-1.0, and no published artifact records it. |

### Deviations from the ratified dossier

Each is deliberate, each is argued at its point of use, and none touches one of the nine
`## Decisions`.

| Deviation | Where | Kind |
|---|---|---|
| Exit **86**, not the dossier's recommended 69 | Exit-code table | Correction — 85 is taken and 69 already means "unreachable" here |
| `NonFastForward` moves to `open_or_update_pull_request` under `git`; the retry widens | D-T4 | Consequence of the transport, unavoidable |
| `--out` **does** read the forge | CLI grammar | Correction — announce's `--out` already does |
| **Four** push options, not three (`.description` added) | Git recipe | Additive — the reviewer needs the rendered `login:id` in the request body |
| `--upstream-org` / `--upstream-repository-url` / `--upstream-disclaimer` rather than the dossier's `--upstream-org` / `--upstream-url` / `--disclaimer` | CLI grammar | **Partial deviation, now schema-verified.** The spelling deviates: a flag is named for the field it fills, and the live `root.schema.json` (sha `153d55d8`) calls the field `repository_url`. The *independence* the dossier asked for is honoured — only `org` is required inside `upstream`, so the other two are each optional; they merely require the anchor, because there is no object to attach them to without it. An earlier draft required org and URL together; that was wrong and is withdrawn. |
| `PRIVATE-TOKEN` is **kept** for non-job tokens | D-T8 | Correction of dossier *prose* in **two places, not one**: the Requirements paragraph *and* its Verification bullet "a non-job token → `Authorization: Bearer`". Neither is a `## Decision`, but a tester working from the dossier's own checklist would write the wrong assertion, so the Verification bullet is named explicitly — see D-T8 and the Validation item that asserts the shipped header did not move. |
| The merge-request URL comes from **REST**, not the push's `remote:` lines | Git recipe, step 6 | Reversal of dossier prose. The dossier reads the URL from the remote's response lines and then confirms it via `GET /merge_requests`; this record demotes the `remote:` line to a log nicety and makes the REST read the only source of identity. GitLab has changed that line's shape before and its own tracker carries requests to alter or disable it, so parsing it first buys nothing and inherits a courtesy contract. |
| `--format` is the root flag, not a subcommand flag | CLI grammar | Correction of dossier prose |
| One extra REST read under `git` — the branch-existence check before the fetch | Git recipe, step 0 | Addition the dossier's transport sketch omits. Not optional: `git fetch` fails the whole invocation on a missing refspec source, so without it every first claim — the command's headline case — dies in step 2. |
| Merge-request confirmation is a **bounded poll**, not the dossier's single read | Git recipe, step 6 | Correction. GitLab creates push-option merge requests in an asynchronous post-receive worker, so one immediate read reports "absent" for a request that is about to exist. |
| A new `ErrorCategory` variant rides the new exit code | Exit-code section | Compelled, not chosen — `from_exit_code` is exhaustive with no wildcard, so the code does not compile without it. It adds one `error.kind` value to a frozen wire vocabulary. |

### Command

- **D-C1 — Placement.** `ocx package claim`, a sibling of `ocx package announce`, sharing
  `--index-repo` / `--forge` / `--fork` / `--out` and the new `--transport`. Mitigation for
  the discoverability cost: `ocx index claim` gets a clap did-you-mean hint, and the
  `ocx index` group's help states that **no index subcommand writes to a forge**.
  The earlier wording — "index subcommands are local-cache operations" — is withdrawn along
  with the C-B con it rested on. It is false against the shipped group: `index update` fetches
  tags from the registry, `index sync` reads each source live and is refused by `--offline`,
  and `index catalog` lists repositories in the registry. Only `index regenerate` consults no
  source. Help text is contract text under `quality-cli-help.md`, so shipping the old sentence
  would ship a documented lie a user could act on. The forge invariant is the one that is
  actually true and is also the one that does the discoverability work.
- **D-C2 — Claim is its own command, never a mode of announce.** Claim writes the
  human-governed fields with `tags: {}`; announce keeps writing only bot-regenerated fields
  and keeps failing `UnclaimedNamespace` on an absent root. The two-run cost is accepted
  because a human merge sits between them regardless (G-04).
- **D-C3 — Claim never writes a tag.** `tags` is emitted as `{}`. There is no flag that can
  add one. The first announce populates it.
- **D-C4 — Owner input, and the provenance of what gets written.** With no `--owner`, the
  acting identity is detected: CI environment first
  (`GITLAB_USER_LOGIN`/`GITLAB_USER_ID`, `GITHUB_ACTOR`/`GITHUB_ACTOR_ID`), else the token
  holder via the forge's authenticated-identity endpoint. `--owner` is repeatable and, when
  given, is the whole list — the detected user is never added implicitly. A bot identity is
  refused (exit 64) naming `--owner`.

  **`owners[]` is a governance field, so where each value came from is part of the contract.**
  Three rules, applied in this order:

  1. **Whenever the users API is reachable, the server is the authority** — even when the CI
     environment already supplied a login and an id, and even for an `--owner LOGIN:ID` pair.
     ocx resolves the login and takes the server's `id`, its `bot` flag, and its **canonical
     spelling of the login** (matching `ForkIdentity`'s existing rule that every field is read
     from a response body, never composed — GitHub logins are case-insensitive and
     homoglyph-confusable, and this pair is what a human reviewer checks). A supplied id that
     disagrees with the resolved one is refused at exit 64: nothing else binds a login to an
     id, and `alice:<someone-else's-id>` would show the reviewer a name they recognise while
     granting G-19 auto-merge to a different account.
  2. **When the users API is not reachable** — a bare `CI_JOB_TOKEN`, which is exactly the
     zero-config path this ADR promotes hardest — the value is carried unverified. It is
     labelled `asserted` or `ci-environment` in `owner_identity_source`, in the stderr owner
     line, and in the request body, so the operator and the G-04 reviewer both see that the
     forge never confirmed it.
  3. **The bot check is only as strong as the source allows** (see the two-rule table in the
     system design). On the unreachable path it degrades to a name-shape heuristic, which will
     not catch a GitLab service account whose login the operator chose freely. That residual
     is why rule 2 exists: the reviewer, not the heuristic, is the control.
- **D-C5 — An existing committed root is refused**, with a named error that points at
  `ocx package announce`, at exit 65 (see the exit-code table for why not 64).
- **D-C6 — Re-run updates the same open request.** The claim branch is
  `indexbot-claim-<namespace>-<package>`, distinct from
  `indexbot-announce-<namespace>-<package>`, so indexbot's G-04 `new-package` classification
  is never confused with a refresh, and a claim and a later announce never share a branch.
- **D-C7 — Claim's branch handling is the simple half of #399.** Content is fully derived
  from flags, so there is nothing to carry forward.
  **`Absent` first, because it is the state every first claim takes.** The branch does not
  exist on the remote: nothing is compared, the commit is parented on the index base, and the
  ref update is a **create**. Under `git` that is a create-shaped `update-ref` with an
  all-zeros expected value; over REST it is the API's create-branch-and-commit path. Both are
  `RefUpdate::FastForward` — `RefUpdate` has only `FastForward` and `Reset`, and a create is
  the degenerate fast-forward, not a rewrite. No lease applies, because there is no prior tip
  to hold. An earlier draft of this decision enumerated only the four states below, which read
  as complete and silently omitted the headline one.
  A `Diverged` or `Behind` claim branch is
  rebuilt on the current index base and repointed with `RefUpdate::Reset`, which makes the
  request mergeable by construction. `Identical` means the branch **is** the base commit, so
  committing on top of it is an ordinary fast-forward and uses `RefUpdate::FastForward` —
  weakening a real compare-and-swap into a lease for a case that never needs one would be
  free risk. `Ahead` with byte-identical content reports `unchanged` and ensures the request;
  `Ahead` with different content commits with `FastForward` and answers `NonFastForward` by
  re-reading and regenerating once. Claim therefore never needs `pull_request_mergeability`.

### Transport

- **D-T1 — One `GitLabForge`, REST reads, git writes, orchestration transport-blind.** The
  git half is an enum field on `GitLabForge`, not a second `Forge` implementation, so a
  method with no git-native answer cannot be forgotten in a second impl and
  `ForgeKind::client` stays the single constructor. That constructor takes the
  **`GitBinary { path, version }`** the argv-boundary gate produced, required whenever the
  transport is `git`: the transport cannot work without it, and passing it makes the dependency
  a signature rather than a `PATH` lookup repeated per invocation. It is also what lets
  `ensure_push_access` emit the `git-version` capability row from a value it holds.
- **D-T2 — `--transport api|git`, default `api`, shared by `claim` and `announce`.** `git` is
  accepted for the GitLab forge only. On GitHub it is a usage error (exit 64) — GitHub has no
  push-option equivalent and `GITHUB_TOKEN` already works over REST.
- **D-T3 — No fallback, ever.** A failure under one transport never retries under the other.
  A git failure never produces a REST write, and vice versa.
- **D-T4 — The git transport defers the write from `commit_files` to
  `open_or_update_pull_request`, and the `Forge` contract says so.** Under `git`,
  `commit_files` builds the objects locally (`hash-object` → scratch index → `write-tree` →
  `commit-tree`), moves a local ref and returns the sha without contacting the server;
  `open_or_update_pull_request` performs the single push that carries both the ref update
  and `-o merge_request.create/.target/.title/.description`, then **confirms the request with
  a bounded poll**, because the server creates it asynchronously (see the recipe's step 6).
  **Consequence:** `ForgeError::NonFastForward` is raised from
  `open_or_update_pull_request` under `git` and from `commit_files` under `api`, so the
  orchestration's re-read-and-regenerate retry must wrap the pair rather than the first call.
  The alternative — pushing twice, once for the ref and once for the options — was rejected
  because no source verifies that push options are delivered on an already-up-to-date push,
  and designing on an unverified mechanism is the failure this research set exists to
  prevent.
  **What is and is not atomic — an earlier draft of this ADR overclaimed here.** The *local*
  commit is atomic: the tree, the commit object and the ref move are one all-or-nothing local
  step, and that is a real property C15 can rest on. The *server* side is **two steps, not
  one**. GitLab applies the ref update synchronously and then hands the `merge_request.*`
  push options to its asynchronous post-receive worker, so "the push succeeded" and "the merge
  request exists" are separate facts separated by an unbounded interval. Claiming one
  all-or-nothing server operation would have shipped a design with no defined outcome for the
  push-succeeded / request-absent window, which is exactly the window that occurs on a loaded
  instance. The recipe's step 6 therefore polls with a bound and maps exhaustion to exit 75,
  and the rerun path is safe by the refresh-commit direction below. Atomicity under `git` is
  not stronger than under REST; it is *differently shaped*, and the shape is written down.

  **The other direction, which announce actually exercises today.**
  `open_or_update_pull_request` is called with **no preceding `commit_files`** on announce's
  unchanged path (`announce.rs:211-233`): a live branch carries unmerged commits, the run
  changes nothing, and the request is merely ensured. Under `git` there is then no pending
  local commit, and pushing a ref that already matches the remote is the already-up-to-date
  case this decision refuses to design against. The contract is therefore:

  > Under `git`, `open_or_update_pull_request` with no pending local commit first reads
  > `GET /projects/:id/merge_requests?source_branch=<branch>&state=opened` — job-token
  > readable. If an open request exists it is returned and **no write happens at all**. If
  > none exists, the workspace creates a **refresh commit**: the same tree as the branch head
  > with a new committer timestamp, parented on the branch head, so the ref genuinely advances
  > and the server processes the push options. The diff against the base is unchanged by
  > construction, so the request's content is identical to what a REST ensure would have
  > opened.

  A ref update is what makes a server process push options, so the refresh commit is a
  verified mechanism rather than a hopeful one. It costs one empty-diff commit on a branch
  that exists only to carry a request, which is a cheaper price than a named refusal on
  announce's most common repeat-run path.

  **Workspace state across the widened retry.** When the push is rejected `NonFastForward`,
  the temp clone still holds the losing commit at `refs/heads/<branch>` and a stale
  `o/<base>`. Re-running `commit_files` against that state would build on the stale parent,
  the local `update-ref <new> <old>` would *succeed* (the local ref does hold `<old>`), and
  the second push would be rejected identically — a retry guaranteed to fail. So: **the retry
  re-fetches `<base>` and `<branch>` into the workspace and rebuilds `o/<base>` / `o/<branch>`
  before the second `commit_files`.** This is an explicit step in the recipe, and the fixture
  proves the retry *converges*, not merely that it runs.
- **D-T5 — `compare_branch` under `git` is computed, not approximated.** The clone fetches
  both refs with `--filter=blob:none` and never `--depth`, so
  `git rev-list --left-right --count` yields the same four-way classification the server
  would. `--depth` is forbidden on this path: a shallow clone lies about ahead/behind and
  reintroduces D13's "unclassifiable compare read as `Identical`".
- **D-T6 — Fork operations are refused under `git`.** `find_fork` / `ensure_fork` return
  `ForgeError::TransportOperationUnsupported`; `sync_fork` (which returns no `Result` and is
  best-effort by contract) is a logged no-op. `--fork` together with `--transport git` is
  refused at parse (exit 64), so the trait-level refusals are the honest fallback rather than
  a reachable path.
- **D-T7 — Credential model.** `OCX_ANNOUNCE_TOKEN` is the REST token on both transports.
  `OCX_ANNOUNCE_GIT_TOKEN` and `OCX_ANNOUNCE_GIT_USERNAME` (default `gitlab-ci-token`)
  override the push credential independently, falling back to the API token, then to git's
  own credential helpers. Under `--transport git` inside a GitLab job with no ocx variable
  set, the job's own `CI_JOB_TOKEN` is picked up for both halves. `api` never infers a token.
  Full precedence in Technical Details.
  **`OCX_ANNOUNCE_GIT_USERNAME`'s justifying posture is a GitLab deploy token**, named here
  because the variable is otherwise unmotivated: GitLab ignores the Basic-auth username for a
  personal, project or group token, so `gitlab-ci-token` suffices for every *other* posture.
  A deploy token can push but cannot call the REST API, and it carries its own username — so
  it is exactly the case where the API credential and the push credential must be different
  values with different usernames. That is the owner's stated requirement in the discussion,
  and it is why the variable exists rather than being deferred to a future need.
- **D-T8 — REST header selection is by value, not by a user-facing setting, and today's
  `PRIVATE-TOKEN` is kept.** GitLab gets `JOB-TOKEN` when the credential value equals the
  environment's own `CI_JOB_TOKEN`, and **`PRIVATE-TOKEN` otherwise** — unchanged from today.
  Header selection is **transport-independent**; only the token *inference* is gated on
  `--transport git`.
  **This corrects the dossier's Requirements prose** (not one of its nine `## Decisions`),
  which said `PRIVATE-TOKEN` "is replaced by `Bearer`, which is a superset". `Bearer` is
  indeed a superset, but nothing in [#410](https://github.com/ocx-sh/ocx/issues/410),
  [#411](https://github.com/ocx-sh/ocx/issues/411) or this ADR asks for OAuth-token support,
  and today's header already accepts personal, project and group tokens. Switching it would
  buy one credential kind nobody requested at the cost of a real failure mode on a shipped
  path (a reverse proxy that forwards `PRIVATE-TOKEN` and strips `Authorization`). The
  `JOB-TOKEN` arm — the whole reason this decision exists — is orthogonal and lands anyway.
  Consequence: the GitLab REST path gains exactly one new behaviour and loses none, and
  "existing announce users: nothing changes by default" becomes literally true.
  The `gitlab.rs:148-160` comment is still scheduled for correction in full, because its
  *other* sentence ("`Authorization: Bearer` accepts only OAuth2 tokens") is factually wrong
  independently of which header ships.
- **D-T9 — Preflight over failure-parsing, and a preflight never becomes a new blocker.**
  `ensure_push_access` reads `GET /projects/:id` and reports both push permission and
  `ci_push_repository_for_job_token_allowed`; the job-token allowlist
  (`GET /projects/:id/job_token_scope/allowlist`) is read only when the push credential is a
  job token and the publishing project differs from the index project. A check whose field
  cannot be read reports `unknown` and the run proceeds — an instance that hides the field
  must not block a push that would have succeeded.
  **The instance version is not readable under a job token**, so there is no version check and
  "the instance is too old" is not a detectable condition on its own. It is reachable only as
  a **two-signal rule**: the preflight reported `unknown` for `job-token-push` *and* the push
  was then refused with the capability-gate shape. That pair maps to exit 86 with the message
  "push-permission field unreadable (GitLab < 18.4 or hidden); push refused" — naming both
  possibilities rather than asserting the one it cannot distinguish.
  **Where 19.0 lands:** the allowlist endpoint exists there, so `job-token-allowlist` can
  report `passed` while cross-project push (GA 19.1) still does not work. That run reaches the
  push, is refused, and lands on the same two-signal rule at exit 86. The preflight is
  therefore necessary and not sufficient, which the `capability_checks` array makes visible
  rather than hiding.
- **D-T10 — System `git`, minimum 2.31, resolved via `PATH`, checked at the argv boundary.**
  No `git2`, no `gix`. The version is checked once **at the CLI's argv-fault block beside
  `ForgeKind::validate_transport`, before the forge is constructed** — not inside the lazily
  built workspace, which would put it after the first REST reads and make "before any network
  call" false. `--transport git` is known at parse time, so nothing is lost by checking early.
  Absent or too old fails closed with a named error at exit 69. `PATH` resolution matches the
  ecosystem norm (`cargo`, `gh`);
  requiring an absolute path would invent a control no CI operator has a standard way to
  configure, against a threat model where the attacker already owns the job's environment.

**Forward-pointer: the git transport is how ocx carries identity today, not the general
answer.** These decisions derive "who is claiming this" from *which write transport the
pipeline happened to use* — a job-token push means the invoking human; REST with a personal
token means whoever holds it. The state of the art decouples the two. PyPI, npm and crates.io
all implement one shape: the workflow requests a short-lived OIDC identity token from its CI
provider, presents it to the registry, and the registry issues a temporary credential scoped
to that package and that run
([OpenSSF overview](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html),
[npm GA 2025-07-31](https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/)).
An index could accept an ID token directly as a claim's identity proof, making authorship
independent of how the bytes were written. That belongs to indexbot, which would be the
verifier, and G-04 requires a human merge either way — so it changes nothing decided here. It
is recorded beside the decision it would eventually replace, and carried to the handoff, so
the job-token special case gets evaluated against it before it ossifies into plumbing.

### Wire

- **D-W1 — ocx writes `login` and `id` only.** No `github`/`github_id`, no `display`, no
  per-owner `forge`/`host`.
- **D-W2 — No `format_version` bump.** `owners` is governance-only and no ocx client parses
  it; a bump would hard-fail every deployed ocx by the index's own wire contract.
- **D-W3 — Read-both stays in indexbot.** ocx never reads `owners` and gains no parser. The
  emit-drop decision and its release date belong to indexbot's own ADR, referenced here and
  **not authored here**.

### Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| Runs to first published index entry | 2 hand steps + 1 CLI run | 1 CLI run + 1 human merge + 1 CLI run | The human merge is G-04 and is not removable. |
| New crate dependencies | — | **0** | System `git` via `child_process.rs`; no `git2`, no `gix`. |
| `Forge` trait operations | 11 | 13 | +`authenticated_identity`, +`resolve_user`; `ensure_push_access` changes return type. |
| REST calls added per `--transport git` run | — | 1–2 | Preflight folded into the existing `ensure_push_access` probe; +1 allowlist read only for a cross-project job-token push. |
| Exit codes | OCX-specific range 79–85 | OCX-specific range 79–86 | One new variant, `ForgeCapabilityUnavailable = 86`, plus the `error.kind` value `forge_capability_unavailable` its wildcard-free category match compels. The sysexits range 64–78 is unchanged and these two commands already use six of it. |
| Clone size per `--transport git` run | — | **unmeasured** | Bounded by the index's commit/tree graph, not blob bytes (`--filter=blob:none`, never `--depth`). Must be measured against `ocx-sh/index` before 0.6.1 ships — see Validation. |

### Consequences

**Positive:**

- A publisher on any of the four supported forge deployments opens their first claim from
  the CLI, and a GitLab pipeline authors it as the invoking human without a shared bot token.
- The `test/manual/announce-e2e/CLAIM.md` procedure becomes one command, and the E2E pilot
  stops being blocked on a hand-opened pull request.
- Authorship and ownership become two independently reported facts, so the four credential
  postures fall out with no extra configuration and no new switch.
- Every new root is single-spelling, which shortens indexbot's own emit-drop path.
- The job-token misinformation in two shipped surfaces gets corrected.

**Negative:**

- ocx gains an undeclared runtime dependency on the host's `git` for one opt-in flag. A
  minimal CI image without `git` gets a named error where previously the flag did not exist.
- The `Forge` contract for `commit_files` weakens (D-T4): "the write may be deferred". That
  is a real fidelity cost and the orchestration's retry scope widens because of it.
- Two write commands under `ocx package` now share a large options surface that will drift
  unless it lives in one struct.
- A third-party reader of published roots that copied the `{github, github_id}` shape breaks
  silently on roots claimed from 0.6.1 onward.

**Steelman recorded, decision unchanged — splitting 0.6.1.** Half the coupling argument does
not hold. The [#411](https://github.com/ocx-sh/ocx/issues/411) reporter has run eight releases
through a shell workaround, so their namespaces are **already claimed**: they need the
transport and not the claim command at all. Shipping the transport alone in 0.6.1 would
unblock them immediately and halve the release's new surface, with the claim command following
in 0.6.2 de-risked by a release of real git-transport traffic. The counter-argument is real
too — one release means one documentation sweep, one credential model and one changelog — but
it is a **schedule call, not a technical one**, and this record should not have presented it as
technical. Dossier decision 8 stands as ratified; the owner holds the call.

**Risks:**

- *`ci_push_repository_for_job_token_allowed` is absent on older instances or hidden from the
  credential.* Mitigation: D-T9's `unknown`-and-proceed rule plus the two-signal push-time
  mapping as the second net.
- *The exact wire text of a job-token push rejection is undocumented* (operability lane,
  negative finding). Mitigation: the preflight is the primary detector; any push-time string
  match is best-effort and must never be the only path to the named error.
- *Self-managed GitLab behind a corporate CA is unsupported on the REST half of both
  transports.* This predates the work and is not fixed by it. See Security Architecture §2.

---

## Technical Details

### Architecture

```
ocx package claim <ns>/<pkg>                  ocx package announce --package <ns>/<pkg>
        |                                                  |
        v                                                  v
  ocx_cli/command/package_claim.rs             ocx_cli/command/package_announce.rs
        |  (thin: argv -> request, credential resolution, report)
        +--------------------------+-----------------------+
                                   v
                    ocx_lib::claim::claim(...)   ocx_lib::announce::announce(...)
                                   |   transport-blind, forge-blind
                                   v
                            &dyn forge::Forge
                                   |
              +--------------------+--------------------+
              v                                         v
       GitHubForge (REST only)                    GitLabForge
                                              { transport: Api | Git }
                                                     |
                              reads: REST always ----+---- writes:
                                                     |      Api -> REST commits + MR create
                                                     |      Git -> local plumbing + one push
                                                     |             carrying -o merge_request.*
                                                     v
                                              GitWorkspace (temp clone,
                                              created on first git write,
                                              dropped with the forge)
```

### API Contract — CLI grammar

```
ocx [--format json] package claim [OPTIONS] <NAMESPACE>/<PACKAGE>
```

| Flag | Value | Default | Contract |
|---|---|---|---|
| `<NAMESPACE>/<PACKAGE>` | positional, exactly one | — | The logical package being claimed. Required. |
| `--repository` | `oci://HOST/PATH` | — | **Required.** The physical registry repository the package publishes to. Announce derives this from the committed root; claim has no root yet. Parsed by the **existing** `oci::index::parse_physical_repository` — named, not re-implemented: it requires an exact round-trip through the Identifier grammar and therefore rejects whitespace, control characters, and smuggled tags or digests. A hand-rolled `strip_prefix` + `split_once` would carry none of that, and this value reaches both a push-option value and the request body. Malformed → usage error. |
| `--owner` | `LOGIN` or `LOGIN:ID` | detect | Repeatable. When given at least once, the list is exactly what was given. `LOGIN` alone is resolved to its numeric id through the forge's users API. `LOGIN:ID` is accepted, but **is not taken on trust when the users API is reachable**: the login is resolved and a mismatched id is refused at exit 64. Where the users API is unreachable the pair is carried as *asserted* rather than *resolved* and is labelled as such in the report and the request body — see the owner-identity provenance rule below. |
| `--upstream-org` | text | — | The anchor. Emits `upstream.org`, the only key the index schema requires inside the `upstream` object. Requires nothing else. |
| `--upstream-repository-url` | URL | — | Requires `--upstream-org`. Emits `upstream.repository_url` (`format: uri`), which the schema marks optional. |
| `--upstream-disclaimer` | text | — | Requires `--upstream-org`. Emits `upstream.disclaimer`. Never interpolated into a pull/merge request title or body (see Security Architecture §3). |
| `--index-repo` | `[HOST/]NAMESPACE/PROJECT` | `ocx-sh/index` | Same parser and same host rules as announce. |
| `--forge` | `github` \| `gitlab` | inferred | Inferred for `github.com`, `gitlab.com` and no host; **required** for any other host, never probed (`adr_announce_gitlab_forge.md` D3, unchanged). |
| `--transport` | `api` \| `git` | `api` | Also added to `ocx package announce`, with identical semantics. |
| `--fork` | `[HOST/]NAMESPACE/PROJECT` | — | Conflicts with `--out` and with `--transport git`. |
| `--out` | directory | — | Renders the root under the directory and opens no request. Conflicts with `--fork` and with `--transport git`. |

`--format` is **not** a subcommand flag. It is the root `ocx --format plain|json`, resolved
once in `ContextOptions` (`subsystem-cli-api.md`). The dossier's "`--format json` reports …"
prose is corrected here.

Ordering: flags precede the positional. Divergence from `announce --package` is named in the
Constitution Check.

**Mutual exclusions and their exit codes.**

| Combination | Result |
|---|---|
| `--out` + `--fork` | clap conflict, exit 64 |
| `--transport git` + `--fork` | exit 64, message names both flags |
| `--transport git` + `--out` | exit 64 — `--out` performs no push, so a write transport there is a silently ignored flag |
| `--transport git` + a resolved GitHub forge | exit 64, message names the forge and the flag |
| `--upstream-repository-url` without `--upstream-org` | clap `requires`, exit 64 |
| `--upstream-disclaimer` without `--upstream-org` | clap `requires`, exit 64 |
| self-hosted host with no `--forge` | `ForgeError::ForgeKindUnknown`, exit 64 |
| `--fork` host ≠ `--index-repo` host | `ForgeError::ForkHostMismatch`, exit 64 |

**`--out` still reads the forge.** Announce's `--out` reads the committed root over the
contents API; claim's `--out` does the same, for two reasons: the existing-root refusal must
hold in every mode, and an `--owner LOGIN` without an id must be resolved. `--out` therefore
needs network access to the index repository, and a credential only when that repository is
private or an owner id must be resolved. This corrects the dossier's "renders the root
without touching a forge".

### API Contract — environment variables and precedence

| Variable | Role | Inherited by ocx-spawned children? |
|---|---|---|
| `OCX_ANNOUNCE_TOKEN` | REST credential, both transports, both commands | **Yes — inherited by design.** `CREDENTIAL_KEYS` holds only `OCX_IDENTITY_TOKEN`, `OCX_KEY_PASSWORD` and `OCX_SIGNING_KEY` (`env.rs:238`); the announce token is deliberately outside it so `ocx-mirror` inherits it from a plugin process. Constitution Check §4. |
| `OCX_ANNOUNCE_GIT_TOKEN` | push credential (git transport only) | No — **must be added to `ocx_lib::env::keys::CREDENTIAL_KEYS`** |
| `OCX_ANNOUNCE_GIT_USERNAME` | push username, default `gitlab-ci-token` | Yes — documented, not a credential, not in `CREDENTIAL_KEYS`, not in `OcxConfigView` |
| `CI_JOB_TOKEN`, `GITLAB_CI` | read for the job-token pickup and the header decision | Never forwarded to the `git` child |
| `GITLAB_USER_LOGIN`, `GITLAB_USER_ID`, `GITHUB_ACTOR`, `GITHUB_ACTOR_ID` | acting-identity detection | Not applicable |

**Why the two sibling credentials get opposite policies, and what that buys.** The announce
token's exemption is a standing, owner-held decision (`ocx-mirror` announces from a plugin
process and inherits it deliberately); the new push token has no such consumer, so it starts
on the safe side of the line. **The containment this buys is nominal in the default posture**,
because push precedence step 2 falls back to the API credential — the identical secret value
is then scrubbed under one name and inherited under the other. It is real only in the posture
the variable exists for, where the two values differ. Stated here rather than left for a
reader to notice.
**The fall-through is intended even when an outer ocx scrubbed step 1's variable.** A wrapper
that removes `OCX_ANNOUNCE_GIT_TOKEN` from a child's environment gets the API credential on
the push, silently authoring as a different identity. That is the documented behaviour of a
precedence ladder, and the warning line below is what makes it visible.

**API credential precedence** (first match wins):

1. `OCX_ANNOUNCE_TOKEN`, when set and non-empty.
2. `CI_JOB_TOKEN`, **only** when `--transport git` is selected, `GITLAB_CI` is set and
   `CI_JOB_TOKEN` is non-empty.
3. None. Every mode that writes fails with `AuthError` (80) naming `OCX_ANNOUNCE_TOKEN`;
   `--out` proceeds unauthenticated.

**Push credential precedence** (git transport only, first match wins):

1. `OCX_ANNOUNCE_GIT_TOKEN` with username `OCX_ANNOUNCE_GIT_USERNAME` (default
   `gitlab-ci-token`).
2. The resolved API credential, with the same username default.
3. Nothing injected — git's own credential helpers apply.

**Step 3 is the one posture where the helper reset does not apply, and that is deliberate.**
The hygiene rule below reads "`-c credential.helper=` on every invocation"; its precise scope
is **every invocation that injects an ocx credential**. Under steps 1 and 2 ocx supplies the
secret via `http.<prefix>.extraHeader`, and leaving a helper active would re-admit the
credential-helper-protocol CVE class the header mechanism exists to avoid — there the reset is
mandatory. Under step 3 ocx injects nothing at all; the operator has chosen to let git
authenticate itself, and resetting the helper list would make that posture simply fail. So
step 3 keeps helpers in charge while every other hygiene control still applies:
`http.followRedirects=false`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`, the
environment allowlist, and the redacting capture. Residual risk, stated plainly: a helper can
leak its own credential through its own channels, and that exposure belongs to the operator's
helper rather than to an ocx-injected secret. The report's `push_credential_kind:
"git-helper"` is what makes the posture visible to a pipeline.

**GitLab REST header selection** (transport-independent):

| Condition | Header |
|---|---|
| credential value is empty | none sent (the tokenless `--out` path) |
| `CI_JOB_TOKEN` is set and equals the credential value | `JOB-TOKEN: <value>` |
| otherwise | `PRIVATE-TOKEN: <value>` — unchanged from today (D-T8) |

The comparison is between two values ocx already holds in its own environment, so it is not
an oracle, and neither value is ever logged. It is **derived inside a `ForgeCredentials`
constructor that takes the environment snapshot**, never set by a caller: stored as a plain
`bool` that any constructor could set independently, D-T8 would read as a value-equality rule
while actually being a caller-supplied flag. Both mis-derivations fail closed at GitLab, so
this is containment rather than correctness — which is exactly why it is cheap to get right.
GitHub's client is unchanged by this ADR.

**The #411 flow, written so it works as pasted:**

```sh
# Either of these. Both need --transport git; neither works under the default api transport,
# where a job token cannot create a merge request at all.
ocx package announce --transport git --index-repo gitlab.com/acme/index …   # no variable at all
OCX_ANNOUNCE_TOKEN=$CI_JOB_TOKEN ocx package announce --transport git …     # explicit
```

**One stderr line when a pre-existing token silently defeats the pickup.** A pipeline that
already sets `OCX_ANNOUNCE_TOKEN` to a project access token — which is the only way announce
works today, so it is what the #411 reporter's eight releases use — and then adds
`--transport git` gets that project token on **both** halves. The push authors as the bot,
G-19 does not match, and the run looks successful: the exact failure #411 reports, reproduced
by the feature meant to fix it, discoverable only in a JSON field the plain report does not
render. So: when `--transport git` runs inside `GITLAB_CI` with a non-job `OCX_ANNOUNCE_TOKEN`
and no `OCX_ANNOUNCE_GIT_TOKEN`, ocx emits one stderr line **before the write**, naming the
push credential kind and the identity it will author as. Precedence is unchanged (dossier
decision 9); only the silence is.

### API Contract — the `Forge` trait

Two operations are added and one return type changes. The trait goes from 11 operations to
13. Its doc comment currently says "ten operations" — stale since
`pull_request_mergeability` landed in `6feb9c02`. The correction is to **stop naming a
count**, not to bump it; a count in prose is a fact that goes stale and already did. (If a
one-line fix lands ahead of this ADR, the correct interim value is *eleven*.)

**The same stale count lives in a second file, and both are scheduled.**
`adr_announce_gitlab_forge.md:62` (D1) also says "ten operations" — the sentence, not the
heading at `:60` and not the blank line at `:59`. Fixing only the source
comment would leave the ratified record asserting a number that was already wrong before this
work and is wrong by three after it. Both sites are in the documentation-surfaces table and in
Implementation Plan step 10; neither gets a count to maintain.

```rust
/// The identity behind a forge credential or login.
pub struct ForgeIdentity {
    pub login: String,
    pub id: u64,
    /// The forge's own `bot` (GitLab) / `type == "Bot"` (GitHub) assertion.
    pub bot: bool,
}

/// The identity the credential authenticates as, or `None` when the credential
/// has no user (a GitHub App installation token).
///
/// # Errors
/// `ForgeError::UsersApiUnavailable` when the credential may not call the
/// identity endpoint at all (a GitLab job token); any other `ForgeError` on
/// transport, status or decode failure.
async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError>;

/// The identity behind `login`, or `None` when the forge has no such account.
///
/// # Errors
/// `ForgeError::UsersApiUnavailable` when the credential may not call the
/// users API; any other `ForgeError` on transport, status or decode failure.
async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError>;

/// Verify the credential may push a branch to `repo`, and report what was
/// checked.
///
/// Returns the capability checks actually performed, so a caller can prove the
/// preflight ran rather than trusting a bare success. A check whose field could
/// not be read reports `CheckStatus::Unknown` and does NOT fail the call.
///
/// # Errors
/// `ForgeError::PushAccessDenied` when the repository is invisible to the
/// credential or reports no push permission;
/// `ForgeError::WriteCapabilityUnavailable` when a capability the selected
/// transport requires is disabled or unavailable on the instance.
async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError>;

pub struct PushAccess { pub checks: Vec<CapabilityCheck> }

pub struct CapabilityCheck {
    pub name: CapabilityName,
    pub status: CheckStatus,
    /// Human-readable amplification; never carries a credential.
    pub detail: Option<String>,
}

/// Wire spellings via `Display`: `git-version`, `push-access`,
/// `job-token-push`, `job-token-allowlist`.
pub enum CapabilityName { GitVersion, PushAccess, JobTokenPush, JobTokenAllowlist }

/// Wire spellings via `Display`: `passed`, `unknown`, `skipped`.
pub enum CheckStatus { Passed, Unknown, Skipped }
```

**There is deliberately no `Failed`.** An earlier draft carried one, and nothing could ever
emit it: every preflight row either passes, reports `unknown`, reports `skipped`, or raises a
`ForgeError` that ends the run — and the report is only rendered on success, so a "failed"
check and a report never coexist. The one-way table below lists these wire spellings as
unremovable once shipped, which makes an unreachable variant worse than untidy: it would be a
permanent value in a published vocabulary that no run can produce and no consumer can
meaningfully branch on. Dropping it is the smaller and more honest change; the day a check can
fail without failing the run, adding the variant back is additive.

**Changed contracts on existing operations (git transport only).**

| Operation | Under `api` | Under `git` |
|---|---|---|
| `get_file_contents`, `get_ref_sha`, `find_open_pull_request`, `pull_request_mergeability` | REST | REST, unchanged |
| `compare_branch` | REST compare | computed from the local clone with both refs fetched; exact, not approximate. **The one read that is not REST under `git`** — a job token has no compare endpoint, which is the whole reason the transport exists. Not called at all when the branch is `Absent`: there is no second ref to compare. |
| `commit_files` | commits and updates the ref over REST; raises `NonFastForward` here | builds objects and moves a local ref; **performs no network write** |
| `open_or_update_pull_request` | opens or reuses the request over REST | performs the single push carrying the ref update and the merge-request push options, then confirms via a **bounded REST poll** (the server creates the request asynchronously); **raises `NonFastForward` here**, and `MergeRequestUnconfirmed` when the poll's bound is exhausted. **Except with no pending local commit**, where it first reads the open merge requests over REST and **writes nothing if one exists**; if none exists it creates a refresh commit and then pushes as above (D-T4). This row is what an implementer copies into the trait doc, so the exception belongs in it rather than only in the decision. |
| `find_fork`, `ensure_fork` | REST | `ForgeError::TransportOperationUnsupported` |
| `sync_fork` | REST or documented no-op | logged no-op |
| `ensure_push_access` | REST probe | REST probe plus the job-token capability checks, **and the `git-version` row rendered from the `GitBinary` the constructor was given** — the check ran at the argv boundary, but its row is emitted here, so `capability_checks` has exactly one assembler |

**Invariant:** under `git`, a `commit_files` not followed by
`open_or_update_pull_request` performs no network write at all. The orchestration must not
treat `commit_files` alone as having published anything, and the temp clone's lifetime spans
both calls — it is owned by the `GitLabForge` instance, created lazily on the first git-half
operation and removed when the forge is dropped.

### API Contract — the constructor

```rust
pub enum WriteTransport { Api, Git }   // clap ValueEnum: "api", "git"; Default = Api

pub struct ForgeCredentials {
    /// The REST credential. Empty means unauthenticated (`--out`).
    pub api: ForgeToken,
    /// The push credential, when the git transport is selected and one was
    /// resolved. `None` leaves git's own credential helpers in charge.
    pub push: Option<GitPushCredential>,
    /// True when `api` is the environment's own `CI_JOB_TOKEN` — the single
    /// input to the `JOB-TOKEN` header decision.
    pub api_is_job_token: bool,
}

pub struct GitPushCredential { pub username: String, pub secret: ForgeToken }

impl ForgeKind {
    /// Refuse a transport this forge cannot serve. Pure; no network.
    ///
    /// # Errors
    /// `ForgeError::TransportUnsupported` for `GitHub` + `WriteTransport::Git`.
    pub fn validate_transport(self, transport: WriteTransport) -> Result<(), ForgeError>;

    /// # Errors
    /// `ForgeError::TransportUnsupported` (via `validate_transport`) or
    /// `ForgeError::ClientBuild`.
    pub fn client(
        self,
        transport: WriteTransport,
        credentials: ForgeCredentials,
        coordinate: &RepoCoordinate,
    ) -> Result<Box<dyn Forge>, ForgeError>;
}
```

`validate_transport` is called twice on purpose and implemented once: from the CLI's
argv-fault block, so a bad combination is diagnosed before the credential check (matching
announce's existing ordering), and from `client`, so the refusal cannot be bypassed by a
future caller.

### Data Model — the claim root

Written to `p/<namespace>/<package>.json`, serialized byte-exactly through the existing
`oci::index::serialize_root` (2-space indent, `ensure_ascii`, insertion-order fields, one
trailing newline). Field order matches the bot's own serializer, verified against
`crates/ocx_lib/tests/fixtures/index_wire/root/full-fields.json` and
`test/manual/announce-e2e/CLAIM.md`:

```json
{
  "name": "ocx.sh/acme/widget",
  "repository": "oci://ghcr.io/acme/widget",
  "owners": [
    {
      "login": "alice",
      "id": 1234
    }
  ],
  "status": "active",
  "deprecated_message": null,
  "created": "2026-09-05",
  "desc": null,
  "upstream": {
    "org": "Acme Org",
    "repository_url": "https://github.com/acme/widget",
    "disclaimer": "Community-maintained mirror, not officially endorsed by Acme Org."
  },
  "tags": {}
}
```

| Field | Value | Derivation |
|---|---|---|
| `name` | `<default registry>/<namespace>/<package>` | The index's logical prefix comes from the resolved default registry (`OCX_DEFAULT_REGISTRY`, default `ocx.sh`) — the same source `announce` uses for `Identifier::with_domain`. **No new flag.** A mismatch against the path-derived name is caught by the index's own G-02 check. |
| `repository` | `--repository` verbatim, after an `oci://host/path` parse | Refused at exit 64 when malformed. |
| `owners` | resolved `login`/`id` pairs, in the order given | `login`/`id` only (D-W1). |
| `status` | `"active"` | Constant; no flag. A claim is always active. |
| `deprecated_message` | `null` | Required by the schema even when unset. |
| `created` | today, UTC, `%Y-%m-%d` | **A date, not a timestamp** — the tag `observed` format is `%Y-%m-%dT%H:%M:%SZ` and is a different field. One clock, one seam, but reached through `oci::index::current_date()`: the accessor moves **down** out of `announce::pipeline` so `claim` does not depend on an unrelated command for a helper. Announce delegates to the moved function; the `__OCX_TESTING_ANNOUNCE_CLOCK` variable keeps its spelling so the existing fixtures keep working. |
| `desc` | `null` | The first announce fills it from `__ocx.desc`. |
| `upstream` | present only when `--upstream-org` is given; carries `org` always, and `repository_url` / `disclaimer` only when their flags were given | Omitted entirely (not `null`) for a first-party claim. The schema requires only `org` inside the object and sets `additionalProperties: false`, so ocx emits exactly the keys it was given and never a placeholder `null` for an absent optional. Whether a *third-party* namespace must carry `upstream` at all is a governance rule enforced by the index's `schema-validate` and reviewers, not by ocx. |
| `tags` | `{}` | Always. No flag can add one (D-C3). |
| `superseded_by` | omitted | Not a claim-time field. |

### API Contract — the `--format json` report

`ClaimReport`, in `crates/ocx_cli/src/api/data/claim.rs`. Flat, named, independently
selectable fields — the shape `gh --json` established and the operability lane recommends,
not a nested envelope.

```json
{
  "package": "acme/widget",
  "name": "ocx.sh/acme/widget",
  "status": "updated",
  "forge": "gitlab",
  "transport": "git",
  "credential_kind": "job-token",
  "push_credential_kind": "job-token",
  "author": { "login": "alice", "id": 1234 },
  "owners": [ { "login": "alice", "id": 1234 } ],
  "owner_identity_source": "resolved",
  "branch": "indexbot-claim-acme-widget",
  "pull_request_url": "https://gitlab.com/acme/index/-/merge_requests/7",
  "pull_request_number": 7,
  "fork": null,
  "written_paths": [],
  "capability_checks": [
    { "name": "git-version", "status": "passed", "detail": "2.43.0" },
    { "name": "push-access", "status": "passed", "detail": null },
    { "name": "job-token-push", "status": "passed", "detail": null },
    { "name": "job-token-allowlist", "status": "skipped", "detail": "same project" }
  ]
}
```

| Field | Type | Contract |
|---|---|---|
| `package` | string | `<namespace>/<package>` as given. |
| `name` | string | The logical name written into the root. |
| `status` | `"unchanged"` \| `"updated"` | **Exactly the two words announce uses — but not the same referent, and a consumer reading both reports must know that.** Announce compares the rebuilt root against the **committed** root, so its `--out` can report `unchanged`. Claim compares against the **open claim branch**, because a committed root would already have exited 65, and its `--out` is therefore always `updated`. One vocabulary, two subjects, stated rather than assumed. |
| `forge` | `"github"` \| `"gitlab"` | The resolved kind, after `--forge`. |
| `transport` | `"api"` \| `"git"` | The selected transport. |
| `credential_kind` | `"job-token"` \| `"token"` \| `"none"` | **The API credential.** ocx cannot distinguish a personal from a project, group or OAuth token — the only truthful distinction is "this value is the job's own `CI_JOB_TOKEN`". The operability lane's proposed `pat`/`deploy-token`/`oauth` set is narrowed for that reason: reporting a kind ocx cannot observe would be a lie a pipeline could assert on. |
| `push_credential_kind` | `"job-token"` \| `"token"` \| `"git-helper"` \| `null` | The push credential; `null` under `api`. |
| `author` | object \| null | The identity that authored the request, when known: the token identity, else the CI-environment identity. `null` when neither is available — a bare job token with no CI user variables, **reachable only with an explicit `--owner`**, since the same state yields no detected owner either. Distinct from `owners` by construction: this is the authorship-vs-ownership split made machine-legible. |
| `owners` | array of `{login, id}` | The resolved list written into the root. Never a bare login. |
| `owner_identity_source` | `"resolved"` \| `"asserted"` \| `"ci-environment"` | Which rule produced the list, so a reviewer and a pipeline can both see the provenance. `resolved` — every pair came from the forge's users API or the token identity. `asserted` — at least one `LOGIN:ID` was taken on the operator's word because the users API was unreachable. `ci-environment` — the list came from `GITLAB_USER_*` / `GITHUB_ACTOR*` with no server confirmation. The same word is rendered in the stderr owner line and in the request body. |
| `branch` | string | The claim branch, so a script can act without re-deriving the naming convention. |
| `pull_request_url` / `pull_request_number` | string \| null, integer \| null | **The same two keys announce uses**, on both forges. There is deliberately no `merge_request_*` alias: one vocabulary, and announce already reports a GitLab merge request under these keys. |
| `fork` | string \| null | The verified fork as `namespace/project`; `null` on the direct path and under `git`. |
| `written_paths` | array of string | Relative paths written under `--out`; empty otherwise. |
| `capability_checks` | array | Every check, in execution order, **including the ones that did not apply** — those carry `status: "skipped"` rather than being omitted. Non-empty on every run, so a pipeline can assert the preflight *ran* rather than trusting a bare success. A `--out` run is the case that makes the `skipped` rule load-bearing: it writes nothing, so `push-access` is skipped rather than performed, and an omit-when-absent rule would hand it an empty array. |

**Announce's report gains the same operational fields** — `forge`, `transport`,
`credential_kind`, `push_credential_kind`, `branch`, `capability_checks` — so a consumer
parses one vocabulary across the two commands. It does **not** gain `owners` (announce never
writes them) or `author` (announce performs no identity lookup today, and adding one would
cost a request per run for a field nothing consumes). All additions are additive JSON keys;
the changelog line is the commit subject.

**Two of those are new fields on `AnnounceOutcome`, not just new report keys.**
`crates/ocx_lib/src/announce/request.rs:115-141` declares `package`, `status`, `pull_request`,
`fork`, `written_paths`, `desc_status` and `reserved_tags_dropped` — it carries neither
`branch` nor `capability_checks`, and the CLI cannot invent either (the branch name is derived
inside the orchestration, and the checks are the forge's). **Implementation step 5 adds both
fields to `AnnounceOutcome`**, in the same step that wires the report, so the report keys and
their carrier land together instead of the keys arriving with nothing behind them.

**Plain rendering** (5 columns, within the plain-mode column budget):
`Package`, `Status`, `Transport`, `Branch`, `Pull Request`, with a dash for an absent field.
`owners`, `author` and `capability_checks` are JSON-only, the plain table being at its column
budget — the same rule `desc_status` already follows on announce. The resolved
`login:id` pairs are additionally written to stderr as a status line before the write, so a
human running the command by hand sees exactly who they are claiming for.

### API Contract — the preflight

| Check | When it runs | Source | Failure |
|---|---|---|---|
| `git-version` | `--transport git`, at the **CLI argv-fault boundary beside `validate_transport`, before the forge is constructed** | `git --version`, parsed for the first `\d+\.\d+(\.\d+)?` | absent or `< 2.31` → `ForgeError::GitUnavailable`, exit 69, **before any network call** |
| `push-access` | every mode that writes; **`skipped` under `--out`**, which writes nothing | `GET /projects/:id` → `permissions.project_access.access_level >= 30` (GitLab); the existing GitHub probe | `ForgeError::PushAccessDenied`, exit 80 |
| `job-token-push` | `--transport git` **and** the push credential is the job's own token | `GET /projects/:id` → `ci_push_repository_for_job_token_allowed` | `false` → `ForgeError::WriteCapabilityUnavailable`, exit 86, message naming Settings → CI/CD → Job token permissions and the project path; field unreadable → `unknown`, run proceeds |
| `job-token-allowlist` | as above **and** the publishing project differs from the index project | `GET /projects/:id/job_token_scope/allowlist` | publishing project absent → `WriteCapabilityUnavailable`, exit 86, naming both project paths; unreadable → `unknown`, run proceeds |

Every row reports `skipped` when it does not apply, so the `capability_checks` array is always
a complete account of what was and was not examined — never a short array a consumer has to
interpret.

**Where `git-version` runs, and who emits its row.** The check itself is a local subprocess and
must run before the forge exists, or "before any network call" is not true: the workspace is
built lazily on the first git-half operation, so a gate living there would fire after three
REST reads, and a runner with no `git` would make three network calls before failing. The
transport is known at parse time, so the CLI's argv-fault block — the same place that already
refuses `--transport git` on GitHub — runs it beside `ForgeKind::validate_transport`.

That placement leaves the row without a producer unless the value is carried forward, so it is:

> The gate yields **`GitBinary { path: PathBuf, version: Version }`**, and under `--transport
> git` that value is a **required argument to the forge constructor**. `ensure_push_access`
> emits the `git-version` row from it, `status: passed`, `detail` the parsed version.

The forge needs the binary for every later invocation anyway, so this adds no state that was
not already required — it makes an existing dependency explicit in a signature instead of
leaving it implicit in a `PATH` lookup repeated per call. One producer, one row, and a
constructor that cannot be called without the thing the transport depends on. The alternative
considered — the CLI concatenating its own row onto the array the forge returns — was rejected
because it puts report assembly in two places and gives the library no way to state the
dependency.

### Exit codes — full mapping

`ExitCode` gains one variant. **85 is already taken** by `UnsupportedKeyBackend`
(`crates/ocx_lib/src/cli/exit_code.rs:93`), so the operability lane's proposed 85 is stale;
the first free slot is 86.

```rust
/// A forge is reachable and refuses a write because the instance or the target
/// project lacks the capability the selected transport needs — job-token push
/// disabled on the target project, or the publishing project missing from the
/// target's job-token allowlist. Distinct from `Unavailable` (69), which means
/// the forge or a required local tool could not be reached at all, and from
/// `AuthError` (80): the credential is valid and an administrator, not the
/// caller, must act. Sibling of `ReferrersUnsupported` (84).
ForgeCapabilityUnavailable = 86,
```

**A new `ExitCode` variant is not one edit but two, and the second one is a compile error
until it is made.** `ErrorCategory::from_exit_code`
(`crates/ocx_lib/src/cli/error_category.rs:40-79`) matches every `ExitCode` **exhaustively
with no wildcard**, on purpose: the module's own doc comment records that the former
cross-crate form needed a `_ => Internal` arm under which a new exit code compiled clean,
passed clippy, and silently serialized as `internal`. So adding 86 without a category arm does
not degrade gracefully — it fails to build, which is the guard working.

The variant is **`ErrorCategory::ForgeCapabilityUnavailable`, its own category**, serialized
`forge_capability_unavailable`. Not a fold into `UsageError`: the invocation was well-formed.
Not `PermissionDenied`: the credential is valid and the caller is not the one who can act. The
match's existing comment already makes this argument for its neighbours — 84
(`ReferrersUnsupported`) and 85 (`UnsupportedKeyBackend`) each got their own category on the
grounds that a capability is absent and the invocation that named it was well-formed. 86 is
the same genus, so it takes the same shape.

`error.kind` is a frozen wire contract that consumers pattern-match on, so this addition is a
vocabulary extension a consumer can newly observe. It is additive — no existing kind changes
meaning — and it lands in the same phase as the exit code, together with its row in the frozen
`error_kind` inventory tests, so the two can never disagree.

**"The instance is too old" is deliberately absent from that doc comment.** Nothing reads the
GitLab version — a job token cannot — and the setting the preflight looks for simply does not
exist before 18.4, which D-T9 routes to `unknown`-and-proceed. An old instance is therefore
reachable only through the **two-signal rule**: `job-token-push` reported `unknown` *and* the
push was refused. That pair still exits 86, with a message that names both possibilities it
cannot distinguish, rather than asserting a version it never read.

**Why a new code rather than 69.** This repository's own `ExitCode::Unavailable` doc already
says "rerunning the same command will not change the outcome", so the operability lane's
premise (that 69 reads as "retry me") does not hold *here* — its conclusion still does, for a
different reason. `forge/error.rs` maps transport failures and 5xx to 69, so 69 in these two
commands already means "the forge could not be reached". Overloading it with "the forge was
reached and refused on policy" would erase the distinction a CI wrapper needs. The
`ReferrersUnsupported = 84` precedent is the same shape: reachable, but missing a capability,
no fallback.

**Why not `PolicyBlocked` (81), the other plausible neighbour.** 81 is documented as a
*deliberate local policy* — `--offline`, `--frozen` — refusing an operation the caller asked
for. Its remedy is always in the caller's hands: loosen the flag. 86's remedy is never in the
caller's hands: an administrator must enable a setting on a project the caller may not
administer, or the instance must be upgraded. Caller-side policy and server-side capability
are the two halves a pipeline most needs to tell apart, so they do not share a code.

| Condition | Code | Raised by |
|---|---|---|
| clap parse failure; unknown flag; missing `--repository`; missing positional; `--out` with `--fork` | 64 | clap → `app.rs` |
| `--transport git` with a GitHub forge | 64 | `ForgeError::TransportUnsupported` |
| `--transport git` with `--fork`, or with `--out` | 64 | CLI argv-fault block |
| self-hosted host with no `--forge` | 64 | `ForgeError::ForgeKindUnknown` |
| nested namespace on GitHub | 64 | `ForgeError::NestedNamespaceUnsupported` |
| `--fork` host ≠ `--index-repo` host | 64 | `ForgeError::ForkHostMismatch` |
| malformed `--repository` | 64 | `ClaimError::MalformedRepository` |
| detected identity is a bot and no `--owner` was given | 64 | `ClaimError::BotIdentityRefused` |
| `--owner LOGIN` given where the users API is unreachable | 64 | `ForgeError::UsersApiUnavailable`, message naming the `LOGIN:ID` form |
| no `--owner`, no CI identity, no token identity | 64 | `ClaimError::NoActingIdentity` |
| a fork operation reached under `git` | 64 | `ForgeError::TransportOperationUnsupported` |
| **a root is already committed at `p/<ns>/<pkg>.json`** | **65** | `ClaimError::NamespaceAlreadyClaimed`, message pointing at `ocx package announce` |
| `--owner LOGIN` names an account the forge does not know | 79 | `ClaimError::OwnerUnknown` |
| a mode that writes has no credential | 80 | CLI boundary check |
| forge 401/403; push-access probe denied | 80 | `ForgeError` (existing) |
| forge 5xx; transport failure | 69 | `ForgeError` (existing) |
| `git` absent or `< 2.31` | 69 | `ForgeError::GitUnavailable`, before any network call |
| forge 429 | 75 | `ForgeError` (existing) |
| non-fast-forward after the in-run retry | 75 | `ForgeError::NonFastForward` |
| `--force-with-lease` rejected as stale | 75 | `ForgeError::StaleLease` |
| protected-branch or hook rejection that is not a capability gate | 77 | `ForgeError::PushRefused` |
| job-token push disabled, or an allowlist miss; or the two-signal pair (`unknown` capability **and** a refused push), which is where an instance below 18.4 lands | **86** | `ForgeError::WriteCapabilityUnavailable` |
| the push succeeded but no merge request appeared within the ~30 s confirmation bound | 75 | `ForgeError::MergeRequestUnconfirmed`, message naming the rerun as the remedy |
| `--out` write failure | 74 | `ClaimError::OutputWrite` |
| an unrecognised `git push` failure | 1 | `ForgeError::GitPushFailed`, carrying git's stderr redacted and capped |
| any `git` plumbing step failing for an unrecognised reason | 1 | `ForgeError::GitCommandFailed` |
| a forge was required but none was supplied | 1 | `ClaimError::ForgeRequired` |
| no base ref on the target to commit onto | 1 | `ClaimError::MissingBaseRef` |
| the retry's re-read found no root at the winning head | 1 | `ClaimError::MissingHeadRoot` |

The last four are **unclassified on purpose**, and the table is complete rather than silent
about them: they inherit exactly the fall-through `AnnounceError::MissingBaseRef` and
`MissingHeadRoot` already take today (`announce/error.rs:208-262`, no arm, `_ => None`). Each
is an internal-consistency failure with no useful remedy a caller could branch on, and
inventing a code for it would be worse than exit 1 with a message that names the ref.

**Why 65 for an already-claimed namespace, and why it is not 79.** The nearest precedent is
the exact inverse condition on the sibling command: announce's `UnclaimedNamespace` exits
**79**. The two answer the two halves of one question and must stay distinguishable — **79
means "the root is not there", 65 means "the root is there and disagrees with the operation
you asked for"**. So a pipeline reads 79 from `announce` as "claim first" and 65 from `claim`
as "already claimed, go announce", and neither is ambiguous. 65 is also the reading
`DescDisappeared` and `PullRequestUnmergeable` already carry: nothing malformed, nothing
absent, the two sides genuinely disagree. Reusing 79 here would have made "claimed" and
"unclaimed" report the same number.

**Why 1 for an unrecognised push failure.** Any classification would be a guess, and
`forge/api.rs` says an implementation that cannot hold a contract must return an error rather
than approximate it. `ForgeError::Status`'s existing `_ => None` fall-through is the same
choice. The message carries git's own stderr, redacted through `status_detail`'s rule and
capped, so the operator is not left with a bare number.

**Doc widening required in the same PR:** `ExitCode::PermissionDenied`'s comment says
"filesystem `EPERM`" and must be widened to cover a forge-side branch-protection refusal.

### The git recipe

System `git`, resolved via `PATH`, minimum **2.31** — the floor is set by
`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`
([2.31 release notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.31.0.adoc)),
not by push options (2.10). Debian 12 ships 2.39, Ubuntu 22.04 ships 2.34, and both SaaS
runner fleets ship current git.

**Not `gix`:** authenticated HTTPS push with push options is listed as partial in gitoxide's
own [crate-status.md](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md)
(fetched 2026-09-04), and no production example of it was found in Cargo, Helix, jj or Zed.
**Not `git2`:** push options arrived only in libgit2 1.8.0, and libgit2's HTTPS transport has
no first-class rustls build path — taking it risks a second TLS stack in a workspace whose
`Cargo.toml` treats "one rustls provider" as load-bearing. Both `gh` and `glab` shell out to
system `git` for exactly this job.

Every invocation below carries `-c http.followRedirects=false`; every invocation **that
injects an ocx credential** additionally carries `-c credential.helper=` and the
`GIT_CONFIG_*` credential pair. Under push-credential precedence step 3 ("nothing injected")
neither applies, by design — see that step. These are elided from the listing for readability
and are not optional (see Security Architecture §1 and §4).

```
0.  GET /projects/:id/repository/branches/<branch>   # REST, job-token readable; 404 => Absent
1.  git --version                       # parse; refuse < 2.31. RUNS EARLIER THAN THIS LIST:
                                        # at the CLI argv boundary, before step 0 and before
                                        # the forge exists. Listed here for completeness.
2.  git init <tempdir>                  # bare-equivalent; no worktree, no checkout
    git -C <tempdir> fetch --filter=blob:none <url> \
        <base>:refs/remotes/o/<base> \
        [<branch>:refs/remotes/o/<branch>]   # SECOND REFSPEC ONLY WHEN STEP 0 FOUND THE BRANCH
                                        # NEVER --depth: a shallow clone lies about ahead/behind
3.  # SKIPPED ENTIRELY WHEN THE BRANCH IS ABSENT — there is nothing to compare against
    git -C <tempdir> rev-list --left-right --count o/<base>...o/<branch>
                                        # 0/0 Identical, n/0 Ahead, 0/n Behind, n/m Diverged
    git -C <tempdir> merge-base --is-ancestor <a> <b>     # only as a tie-break; normally unused
4.  git -C <tempdir> hash-object -w <file>                # per changed file -> <blob>
    GIT_INDEX_FILE=<tmp-index> git -C <tempdir> read-tree o/<base>
    GIT_INDEX_FILE=<tmp-index> git -C <tempdir> update-index --add \
        --cacheinfo 100644,<blob>,p/<ns>/<pkg>.json
    GIT_INDEX_FILE=<tmp-index> git -C <tempdir> write-tree      # -> <tree>
    git -C <tempdir> commit-tree <tree> -p <parent> -m <message>
                                        # <parent> = o/<branch> head, or o/<base> when Absent
    git -C <tempdir> update-ref refs/heads/<branch> <new> <old>   # local CAS, no worktree
                                        # <old> = 0{40} when Absent (create-only)
4r. # ON RETRY ONLY, before re-running step 4 (D-T4):
    git -C <tempdir> fetch --filter=blob:none <url> \
        <base>:refs/remotes/o/<base> <branch>:refs/remotes/o/<branch>   # rebuild o/* from the winner
    # a retry always names both refspecs: a rejected push means the branch exists now,
    # whatever step 0 reported
5.  git -C <tempdir> push <url> refs/heads/<branch>:refs/heads/<branch> \
        -o merge_request.create \
        -o merge_request.target=<base> \
        -o merge_request.title=<title> \
        -o merge_request.description=<body>
    # --force-with-lease=<branch>:<expected-sha> ONLY for the RefUpdate::Reset case
6.  GET /projects/:id/merge_requests?source_branch=<branch>&state=opened
    # BOUNDED POLL, not one read: 1s, 2s, 4s, 8s, 15s -> give up at ~30s wall clock.
    # the authoritative merge-request identity; the `remote:` URL line is a log nicety only
```

**Step 0 exists because a first claim has no branch.** `git fetch` fails the whole invocation
when a named refspec source does not exist on the remote, so an unconditional
`<branch>:refs/remotes/o/<branch>` makes the `Absent` state — the *normal* state for the
command's headline use case — unreachable: every first claim would die in step 2 with git's
"couldn't find remote ref" rather than creating the branch. The existence read is REST, is
job-token readable, and is the same Branches API call `get_ref_sha` already makes, so it costs
one request and no new capability. `Absent` is its own branch state and is **not** `Identical`:
there is no `o/<branch>` to compare against, step 3 is skipped, the commit is parented on
`o/<base>`, and the ref update is a create (`<old>` all zeros) rather than a lease.

**Step 4 is an index-file chain, not `mktree`, and the difference is load-bearing.**
`git mktree` builds **one** tree object from one flat listing, so writing
`p/<namespace>/<package>.json` — three components deep — would need a separate `ls-tree` and
`mktree` per level, hand-rolled and depth-dependent. `read-tree` into a scratch index,
`update-index --cacheinfo`, `write-tree` is depth-independent, is the recipe scripted
worktree-free commits normally use, and is fewer commands rather than more.

**The push-option set is closed: exactly these four keys, and no others.** This is a security
boundary, not a style preference. `merge_request.merge_when_pipeline_succeeds` is a supported
GitLab push option, and setting it would auto-merge the claim request the moment its pipeline
went green — defeating G-04's "a first claim is never auto-merged", the single governance
control this whole command is built around. `merge_request.remove_source_branch` is the same
shape at lower impact. Both are **forbidden**, and the fixture's `post-receive` hook asserts
`GIT_PUSH_OPTION_COUNT == 4` and the exact key set rather than merely parsing what arrives.

Step 6 is not optional, and it is a **bounded poll rather than one read**. GitLab's own
tracker records requests to change or disable the `remote: View merge request …` line
([gitlab-ce#21451](https://gitlab.com/gitlab-org/gitlab-ce/issues/21451)) and cases where it
is stale after a non-`create` push
([gitlab#441944](https://gitlab.com/gitlab-org/gitlab/-/issues/441944)) — GitLab treats it as
a courtesy it has changed shape on before, not a contract. `glab mr create` itself is
REST-first for the same reason.

**The push and the merge-request creation are two server steps, not one.** GitLab processes
`merge_request.*` push options in the asynchronous post-receive worker, so the ref can be
updated and the request not yet exist when the first REST read lands. A single immediate GET
therefore reports "no request" for a push that will produce one seconds later — the design
must not read that as failure, and must not silently return a claim with a null request
either. The poll is the resolution: **1s, 2s, 4s, 8s, 15s, giving up at roughly 30 seconds of
wall clock**, cheap because the common case answers on the first attempt.

Unconfirmed after the bound is **exit 75 (`TempFail`)** with a message that names the rerun as
the remedy, because the rerun is genuinely safe: it finds no pending local commit and no open
request, and therefore takes D-T4's refresh-commit direction, which re-sends the same four
push options against the branch the first run already pushed. Nothing is duplicated — GitLab
allows one open request per source branch — and nothing is lost. This is the reason 75 is
right and 69 or 1 is not: the operation is incomplete and retrying is the documented fix.

Re-pushing `merge_request.create` to a branch that already carries an open merge request is a
documented no-op rather than an error (GitLab enforces one open request per branch), and the
title and description options update the existing request — which is what makes D-C6's
"a re-run updates the same request" work under `git` with no extra call.

**git stderr classification** (`LC_ALL=C` is set on the child so these phrases are not
localised):

| Phrase | Mapped to | Exit |
|---|---|---|
| `(fetch first)` / non-fast-forward | `ForgeError::NonFastForward` — re-read the winning head, regenerate, retry once | 75 after the retry |
| `(stale info)` | `ForgeError::StaleLease` — re-read the expected sha, retry once | 75 after the retry |
| `pre-receive hook declined` with a protected-branch phrase | `ForgeError::PushRefused` | 77 |
| HTTP 403 on the push, **or** a `remote:` line containing `not allowed to push` / `You are not allowed to`, **and** the preflight reported `job-token-push: unknown` | `ForgeError::WriteCapabilityUnavailable` — **the second signal** | **86** |
| the same rejection shapes with the preflight reporting `job-token-push: passed` | `ForgeError::PushRefused` — the capability was confirmed, so this is an ordinary refusal | 77 |
| anything else | `ForgeError::GitPushFailed`, stderr redacted and capped | 1 |

**Row 4 is what makes the two-signal rule reachable at all.** Without it a job-token push
refusal falls to the last row and exits 1, and D-T9's rule would name an outcome nothing
produces — the defect an earlier draft shipped. Note what carries the weight: **the promotion
to 86 is driven by the preflight result, not by the phrase.** The phrase set is a best-effort
match on text GitLab does not document (the operability lane's negative finding stands), so it
is deliberately not the discriminator; the same text with the capability confirmed lands on 77
instead, and neither exit is ever reached by a string match alone. `LC_ALL=C` on the child is
what keeps the English phrases matchable on a localised runner, and the HTTP status is the
locale-independent half of the pair.

**Commit identity.** `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/
`GIT_COMMITTER_EMAIL` are injected explicitly (a minimal CI image has no `user.*` config and
the commit would fail outright). A fixed `ocx <noreply@ocx.sh>` is correct and must not track
`--owner`: GitLab records the *pushing user* as the merge-request author, and the commit's
own author field is repository metadata, not authorship of the request.

### Test fixture

Extend `test/tests/fake_forge.py`'s existing in-memory object graph with a real
`git init --bare` repository served by `git http-backend` on the same port, so REST and git
are two surfaces over one graph:

- `receive.advertisePushOptions=true` on the bare repository, or the push-option payload is
  silently dropped before any hook sees it.
- A **`post-receive`** hook (not `pre-receive` — the record should exist only after the ref
  actually moved) reads `GIT_PUSH_OPTION_COUNT`/`GIT_PUSH_OPTION_n`, parses
  `merge_request.create`/`.target`/`.title`/`.description`, and creates or updates the merge
  request record the REST handlers already serve, so `GET /merge_requests?source_branch=`
  sees exactly what the push created. **The hook writes that record after a configurable
  delay**, mirroring GitLab's asynchronous post-receive worker: a delay inside the poll bound
  proves the confirmation converges, and one beyond it proves the bound is real and exits 75.
  A fixture that records synchronously can never distinguish "polled and found it" from "found
  it on the first read", which is the state a missing poll is in.
- The bare repository starts **without the claim branch**, so the ordinary happy path exercises
  the `Absent` state — a branch-existence read that returns 404, a fetch naming one refspec,
  no compare, and a create-shaped ref update. A second fixture seeds the branch to reach the
  `Ahead` and `Diverged` arms.
- A separate rejection path (a `pre-receive` hook keyed on a harness env var) simulates the
  server-side capability gate (an instance that refuses a job-token push — not the local
  `git --version` gate, which never reaches a server), the allowlist miss, the
  protected-branch refusal, the ordinary
  non-fast-forward (`(fetch first)`) and the stale lease (`(stale info)`). For the
  capability-gate case the hook writes the **literal line** `remote: You are not allowed to
  push code to this project.` and exits non-zero; the REST half is varied independently so the
  same line can be paired with a `unknown` preflight (expect 86) and with a `passed` one
  (expect 77). Naming the line here rather than "a refusal message" is deliberate: the
  classifier's phrase set is best-effort, so the fixture must pin the exact text the
  assertions depend on. It also carries **the
  two-push-rejection scenario**, where a moved target is rejected, the workspace re-fetches
  (step 4r), and the second push succeeds. That proves the widened retry *converges*; a
  fixture that only rejects once proves it runs.
- A **recording `git` shim placed earlier on the child's `PATH`**, capturing argv and the
  environment block to a file the test reads before delegating to the real binary. Without it
  no assertion about credential containment is possible at all: the existing fake is an
  in-process `http.server` with no subprocess visibility. The HTTP side then asserts the
  complementary half — that `Authorization: Basic` arrives on the git request and appears in
  no other captured surface.

**The fixture's real cost, stated so it is not a surprise in Phase 4.** This is not a small
extension. `fake_forge.py` is a hand-written `BaseHTTPRequestHandler`, which provides no CGI
bridge and does not decode `Transfer-Encoding: chunked` — which smart-HTTP `git-receive-pack`
uses — and the stdlib's `CGIHTTPRequestHandler` is deprecated on this project's Python floor.
So the work is a hand-rolled CGI bridge plus chunked-body decoding, and it is the largest
single new artifact in the plan. **A cheaper fixture was considered and is genuinely
insufficient:** a bare repository over the *local* transport does deliver push options and can
run the same `post-receive` hook, but it exercises no HTTP request, so it cannot test
`http.extraHeader` placement or any of the "credential absent from `.git/config`, argv and the
remote URL" assertions — which is the actual reason HTTP is required.

---

## Security Architecture

### Threat model (STRIDE)

| Threat | Mitigation |
|---|---|
| **Spoofing** — a bot or shared account claims a namespace as a person | The forge's own documented `bot` / `type: "Bot"` field is checked (not a login regex) whenever the identity comes from the token or the users API; the CI-environment path additionally rejects the documented bot login shapes. **Boundary stated plainly:** a human-minted PAT on a shared "release" account is indistinguishable from a person, and the bot check does not and cannot stop it. The real control is posture (c) plus the G-04 human reviewer reading the rendered `login:id` list. |
| **Tampering** — a concurrent write clobbers another | `RefUpdate::FastForward` is the compare-and-swap under both transports (locally via `update-ref <new> <old>`, on the wire via a plain non-force push); `--force-with-lease` is used **only** for the `RefUpdate::Reset` rebuild and never a naive `--force`. |
| **Repudiation** — who opened this request | `author` and `owners` are reported as two independent facts, and the request body renders every owner as `login:id` so a reviewer can verify identity against the forge's own profile. |
| **Information disclosure** — credential leakage | See §1 and §4 below. |
| **Denial of service** — a hostile response floods a log | `status_detail`'s existing trim/cap/redact applies to forge bodies; git stderr goes through the same cap before entering an error. |
| **Elevation of privilege** — a job token reaching more than it should | The pickup is opt-in behind `--transport git`; the token is injected only for the index host's URL prefix; GitLab's fine-grained job-token permissions (GA 18.3) are recommended in docs as an operator-side hardening. |

### 1. Credential injection, and the residual `/proc` exposure

The push credential travels as
`GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n=http.<index-url-prefix>.extraHeader` /
`GIT_CONFIG_VALUE_n=Authorization: Basic <base64(user:secret)>`, scoped by a URL prefix that
includes the full project path, never just the host. It never enters a remote URL, argv,
`git remote -v`, `.git/config`, the reflog, or shell history.

**Why this and not a credential helper.** Every credential-helper-protocol participant
surveyed has shipped a newline/CR-smuggling CVE in the last 18 months — Git core
(CVE-2024-52006), Git Credential Manager (CVE-2024-50349, CVE-2024-50338), Git LFS
(CVE-2024-53263), GitHub CLI (CVE-2024-53858), GitHub Desktop (CVE-2025-23040) — and the
primary [Clone2Leak writeup](https://flatt.tech/research/posts/clone2leak-your-git-credentials-belong-to-us/)
states explicitly that host-scoping a helper does **not** fully prevent leakage via submodule
URLs or redirects. `http.extraHeader` is not a credential-helper-protocol participant at all:
there is no newline-delimited handshake to smuggle into, because the header is attached by
git's HTTP transport directly. The entire class is inapplicable, and "never register a
credential helper for the injected token" is an invariant, not a preference.

**The user-level `credential.helper` must be disabled explicitly.** `GIT_CONFIG_NOSYSTEM=1`
blocks `/etc/gitconfig`, but the allowlist below passes `HOME` through so `~/.gitconfig`'s
proxy and CA settings apply — and `~/.gitconfig` is the *far* likelier carrier of a helper:
`osxkeychain`, `manager` and `store` are what every git install guide configures. Leaving it
active re-admits the entire credential-helper-protocol CVE class this mechanism was chosen to
avoid, and `GIT_TERMINAL_PROMPT=0` does not close it — a helper with a cached credential never
prompts. So **every clone, fetch and push invocation that injects an ocx credential carries
`-c credential.helper=`** (an empty value resets the list), and the fixture asserts no helper
process is invoked on those runs. The scope qualifier is load-bearing rather than a hedge:
push-credential precedence step 3 injects nothing and exists precisely so an operator's own
helper can authenticate the push, so applying the reset there would break the ratified
fallback instead of protecting anything ocx supplied. The fixture therefore has two cases — a
credential-injecting run with no helper invocation, and a step-3 run where the helper *is*
invoked and no `extraHeader` is present. The
alternative — `GIT_CONFIG_GLOBAL` pointed at a null path with proxy and CA settings re-applied
as explicit `-c` flags — is equivalent and more code; the empty-reset is the lazy form of the
same guarantee.

**Named residual, not assumed away.** `GIT_CONFIG_VALUE_n` is in the child's environment
block and is therefore readable via `/proc/<pid>/environ` by the same UID and by root for the
process's lifetime — **CWE-522, Insufficiently Protected Credentials**. This is the same class
of exposure `OCX_ANNOUNCE_TOKEN` already has as a parent-process variable, so it is not a
regression; it is also not zero, and no mechanism that keeps the value in the child's
environment can eliminate it. On a shared runner or beside a compromised same-UID sibling, it
is real.

**`GIT_TRACE` closure is caller-enforced, not structural — the first draft overstated this.**
`GIT_TRACE` and `GIT_CURL_VERBOSE` both dump request headers to stderr. `spawn_and_wait` does
call `env_clear()`, but that only closes inheritance from the *process*: the child then gets
whatever the `Env` value carries, and `Env::new()` — which is also the `Default` impl — seeds
itself from `std::env::vars_os()`. The ergonomic constructor therefore hands the child the
entire ambient environment, `GIT_TRACE`, `CI_JOB_TOKEN` and `OCX_ANNOUNCE_TOKEN` included,
straight past `env_clear()`. So: **`Env::clean()` is mandatory on this path, and
`Env::new()` / `Env::default()` are forbidden on it.** The invariant is caller-enforced and
asserted by test, not delivered by the structure.

### 2. TLS trust is disjoint between the two halves — and this is an unfixed gap

`crates/ocx_lib/src/forge/http.rs` builds the REST client with a **fixed, vendored** Mozilla
root set and offers **no** override: no `SSL_CERT_FILE`, no config key, no flag. `git`, by
contrast, trusts the host's system store and honours `http.<url>.sslCAInfo` / `GIT_SSL_CAINFO`.

**Stated plainly, because it will otherwise be discovered as a surprise bug: a self-managed
GitLab (or GHES) behind an internal corporate CA is not supported on the REST half of
*either* transport today.** An operator who installs the CA into the OS trust store — the
first move every git and GitLab guide recommends — will find `git` works and every REST read
still fails, with no ocx-level knob to reach.

**And ocx now *configures* that divergence rather than merely inheriting it.** The child
allowlist below deliberately forwards `GIT_SSL_CAINFO`, `GIT_SSL_CAPATH`, `SSL_CERT_FILE` and
`SSL_CERT_DIR` to the git child, while the REST client ignores all four — it seeds a fixed
vendored root set whose own documentation says the extra-roots branch never touches the system
store. So the follow-up issue is scoped as **"make the forge REST client honour the same four
variables the git half already honours"**, not as "document a prerequisite". This predates the
claim work, the git transport does **not** fix it, and it must not be described as if it does.
Filed as a named follow-up (Implementation Plan step 10), not left implicit. The issue names
**those four CA variables and only those** — the proxy variables in the same allowlist are
already honoured by `reqwest`, so scoping the issue around them would close it with the real
gap untouched.

### 3. Push-option and request-body content

Push options are **not** a shell or command-injection surface: they travel as pkt-line
capability strings, and `child_process.rs` passes arguments as a `Vec<String>` to
`std::process::Command`/`tokio::process::Command` and never through a shell — CWE-78 does not
apply on this path. GitLab requires the literal two-character sequence `\n` in
`merge_request.description` and converts it server-side, so no raw newline traverses a single
option value.

**But the server's own push-option parser is a live 2026 vulnerability class, and ocx controls
what it sends.** CVE-2026-3854 (GitHub Enterprise Server, disclosed and patched 2026-03-04,
published 2026-04-28) was exactly this shape: "user-supplied git push options were not
properly sanitized and were embedded into internal service metadata headers using a delimiter
character", letting a pusher inject additional metadata fields, override the environment the
push was processed in, bypass sandboxing, and reach arbitrary command execution on the server
([GitHub Security blog](https://github.blog/security/securing-the-git-push-pipeline-responding-to-a-critical-remote-code-execution-vulnerability/)).
The vulnerable component is GHES's push pipeline, not GitLab's, and ocx never sends push
options to GitHub — but a sibling product shipped a critical delimiter-injection defect in the
same mechanism six months before this record's date, so the value going into
`-o merge_request.title=` and `.description=` must be treated as passing into an external,
security-sensitive parser rather than into a display field. Therefore, **belt and braces:
every rendered push-option value is rejected if it contains a newline, a NUL, or any other
control character, before it reaches the pkt-line.** Under the fixed-template rule below no
such character can occur, which is precisely what makes the check cheap and its failure a
loud signal that something upstream changed.

The other risk is **content injection into an artifact a human merges under G-04**: whatever
lands in a title or description renders as markdown for the reviewer. The invariant:

> The claim and announce request title and body are built from a fixed template carrying only
> structured values — the logical name, the physical repository, the branch, and the resolved
> `login:id` pairs. No operator free text is interpolated. `--upstream-disclaimer` reaches the
> **root file only**, where the serializer escapes it, and never the request title or body.

Owner logins are rendered as plain `login:id` text, deliberately **without** an `@`, so the
body fires no mentions. Any future free-text field added to a request body must be escaped and
length-capped before interpolation, the same way REST body construction already is (D9/D15);
the generic pkt-line ceiling is ~65 KB per value and the body is O(owners).

### 4. Temp-clone hygiene — mandatory, not advisory

- **A capturing subprocess helper, never `exec` and not `spawn_and_wait` either.** `exec` never
  returns, so no RAII guard runs and the clone is left on disk — that reasoning stands. But
  `spawn_and_wait` hardcodes all three streams to `inherit` and returns only an `ExitStatus`,
  and this recipe needs captured **stdout** at four steps (`--version` for the 2.31 gate and
  the `git-version` check `detail`; `rev-list --left-right --count` for the compare;
  `hash-object`, `write-tree` and `commit-tree` for the object shas). Worse, git's stderr
  would reach the operator's terminal raw and uncapped, so the `(fetch first)` / `(stale info)`
  classifier would have nothing to match — every push failure would degrade to exit 1 — and the
  redaction promised below would never execute on bytes already printed (**CWE-532**).
  So `child_process.rs` gains a **capturing sibling**: same `env_clear()`, same SIGINT/SIGTERM
  forwarding, same `kill_on_drop(true)`, but all three streams piped and a return of
  `(ExitStatus, Vec<u8>, Vec<u8>)`. `spawn_and_wait` stays for the cases where inherited stdio
  is what is wanted, and is not used on this path.
- **Redaction covers every secret in play, not one.** `status_detail` takes a single token and
  does a plain substring replace. Under `git` there are up to **three** distinct forms: the
  API credential, a separately-configured `OCX_ANNOUNCE_GIT_TOKEN`, and the
  `base64(user:secret)` blob the credential actually exists as on the wire — which no
  plaintext needle matches. The redactor is widened to take a **slice** of secrets and to
  include the base64 form, and each form is proved red by seeding it into a fixture stderr
  before the green is trusted.
- **Tempdir mode `0700` set explicitly** after creation, not left to the umask (CWE-732) —
  **on Unix.** Windows has no mode bits, so a `set_permissions(0o700)` there is inert and an
  assertion written against it passes whether or not anything happened, which is the
  "green that cannot be told from never-ran" shape. On Windows the protection is the per-user
  temp directory's own ACL, which ocx does not modify; the Validation item is therefore
  **Unix-only** and says so.
- **`-c core.symlinks=false`** on the clone, as **defence-in-depth**. The recipe never checks
  out a worktree, so nothing materialises today and the flag is inert on this path; it is kept
  because it is free and because the next path that does check out would otherwise inherit a
  contributor-planted symlink. Stated as defence-in-depth so the next reader does not delete
  it as dead code.
- **Never `--recurse-submodules`.** This closes the one Clone2Leak vector that host-scoping
  alone would not.
- **`http.followRedirects=false`** injected alongside the credential, for parity with the
  REST client's no-redirect policy. An instance that requires a redirect must be addressed at
  its final URL.
- **`GIT_TERMINAL_PROMPT=0`** so git never blocks on an interactive prompt.
- **`GIT_CONFIG_NOSYSTEM=1`** so `/etc/gitconfig` — which the operator may not control and
  which could carry an unexpected `credential.helper` — never participates.
- **A SIGKILL still leaves the directory.** Accepted: it holds no secret at rest (the
  credential lives only in the environment, never in `.git/config`), so the residual is disk
  hygiene, not disclosure.

**Child environment allowlist.** **The capturing helper** — the same one specified in §4, not
`spawn_and_wait`, which is not used on this path — keeps `env_clear()`, so every variable in
the child is explicit. Ambient proxy and CA settings must still be honoured, since a corporate
runner's proxy is legitimate, while credential and identity settings must not.

| Passed through from the ambient environment | Set by ocx | Never passed |
|---|---|---|
| `PATH` (to resolve `git`); `HOME` / `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH` (so `~/.gitconfig` proxy and CA settings apply); `http_proxy`, `https_proxy`, `no_proxy`, `all_proxy` and their uppercase forms; `GIT_SSL_CAINFO`, `GIT_SSL_CAPATH`, `SSL_CERT_FILE`, `SSL_CERT_DIR`; `TMPDIR`/`TEMP`/`TMP`; `SYSTEMROOT` on Windows | `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`, `GIT_AUTHOR_*`, `GIT_COMMITTER_*`, **`LC_ALL=C`** and `LANGUAGE=` | `GIT_TRACE*`, `GIT_CURL_VERBOSE`, `GIT_ASKPASS`, `SSH_ASKPASS`, every `OCX_*` credential, `CI_JOB_TOKEN` |

`LC_ALL=C` is load-bearing: the stderr phrases the classifier matches (`(fetch first)`,
`(stale info)`) are translated by git's NLS on a localised runner, and a classifier that
silently stops matching degrades every push failure to exit 1. **On git-for-Windows the same
variable is honoured** — that build ships gettext and reads `LC_ALL` like any other — so the
control is cross-platform, unlike the tempdir mode above. The Validation item asserts the
English phrase survives on a runner with a non-English locale set, which is the only way to
tell the guard from a habit.

**SSRF, and why the forge path is deliberately not guarded.** Neither the REST client nor the
git remote goes through `oci/ssrf.rs`. The host validator that does run rejects userinfo, IPv6
literals, paths, queries and fragments — closing the `gitlab.com@evil.example` class — but
accepts `127.0.0.1`, `0x7f000001`, `127.1`, `10.0.0.1` and `169.254.169.254` as ordinary
labels. That is correct here and the reasoning is recorded so a future reader does not have to
re-derive it: the forge coordinate comes from **argv**, `oci/ssrf.rs`'s own module doc scopes
that guard to **remote-controlled registry pointers** (the announce path guards its
`repository` pointer precisely because a root document supplies it), and applying it here
would refuse the self-managed-GitLab-on-a-private-network deployment this ADR exists to serve.
The open question this leaves — whether a forge coordinate can ever reach these commands from
something other than argv, such as a `[managed]` config payload, `ocx.toml`, or an ocx-mirror
spec — is carried to the handoff. If the answer is yes, the guard becomes mandatory.

### 5. Supply chain

`git` is resolved through `PATH` with no absolute-path requirement and no hash pin, matching
`cargo` and `gh`. A PATH-hijacked `git` is architecturally real (**CWE-427, Uncontrolled
Search Path Element**) but no CVE or named incident was found for it in CI, and an attacker
who can write an early `PATH` entry in the job already controls every other tool invocation
in it. Requiring an absolute path would invent a control the ecosystem does not have and CI
operators have no standard way to configure. The right-sized response is the one already
specified: fail closed with a named error at the argv boundary — before the forge is
constructed and therefore before any network call — when the binary is absent or too old.

**A leaked job token** carries the triggering user's full permissions, bounded only by the
allowlist (200 groups + 200 projects, counted separately) and the job's lifetime — GitLab
documents this as an accepted platform trade-off (**CWE-269, Improper Privilege Management**).
The docs surface recommends fine-grained job-token permissions (GA 18.3) as operator-side
hardening. One tracked GitLab issue notes a project can be added to an allowlist by a user
with no role in the allowed project; that is the index operator's configuration to get right,
not something ocx can mitigate, and the named exit-86 error is the correct ocx-side response
when the allowlist rejects a push.

---

## Non-Functional Requirements

| NFR | Target / statement |
|---|---|
| **Security** | The credential never appears in argv, a URL, `.git/config`, a log line, a forwarded child environment, or a redacted forge body. **Asserted by test, and the mechanism is named** — the existing in-process HTTP fake has no subprocess visibility, so the assertion rides a recording `git` shim placed earlier on the child's `PATH` (capturing argv and the environment block to a file) plus the `git http-backend` side asserting `Authorization: Basic` arrives on the git request and nowhere else. Without both halves this NFR would be an unbacked sentence. Residual `/proc/<pid>/environ` exposure is named and accepted (§1). |
| **Operability** | Every capability failure names the setting, the version or the flag that produced it, and is discriminable by exit code without parsing stderr. `capability_checks` is present and non-empty on every run — inapplicable checks appear as `skipped`, never omitted — so a pipeline can assert the preflight ran rather than trusting a bare success. |
| **Cost** | Zero new crate dependencies. One extra REST field read per git-transport run (folded into the existing push-access probe), plus one allowlist read only for a cross-project job-token push. No per-PR live-GitLab job: the fixture is the CI gate, following `python-gitlab`'s pinned-image pattern only if a scheduled real-instance job is ever wanted. |
| **Availability** | No fallback between transports, by decision (D-T3). A git failure is an explicit failure with a named error; it never silently degrades into a REST write. The cost is stated: a transient git-side problem that REST could have served fails the run. |
| **Latency** | One blobless clone per git-transport run, bounded by the index's commit and tree graph rather than its blob bytes. **Not yet measured** against `ocx-sh/index` (~1.8k roots) — a measurement is a release gate, not a claim (see Validation). `--depth` is forbidden regardless of what the measurement shows (D-T5). |
| **Scalability** | Not applicable. One package per invocation, one branch per package, one request per branch. |
| **Observability** | Existing `tracing` spans; no new telemetry. The JSON report is the machine-readable account of a run. |

---

## Migration and Rollout

**Existing announce users: nothing changes by default, and that is now literally true.**
`--transport` defaults to `api`, so every current invocation behaves identically; and D-T8
keeps `PRIVATE-TOKEN` for every non-job token, so the GitLab REST path gains one new arm
(`JOB-TOKEN`, reachable only when the credential *is* the job's own token) and changes none.
The announce report gains keys, which is additive.

**ocx-mirror is out of scope, and named as such.** `AnnounceConfig`
(`src/spec/announce_config.rs` in [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror))
has **no `forge` field**, so a mirror pipeline cannot announce to a GitLab-hosted index today
and gains no `transport` field here either. A mirror-invokable claim is a separate change in
that repository. Filed, not fixed.

**`ocx-catalog`'s owner href is a named prerequisite, not a deferred nicety.** It hard-codes
`https://github.com/<login>` for every owner (`MetaRail.vue:222-228`, self-documented as a
`KNOWN GAP`), so the first GitLab-sourced claim publishes owner links pointing at unrelated
github.com accounts. `login`/`id` itself is safe there (`ownerLogin` reads `login ?? github`),
so this does not block W-B — it blocks **advertising the GitLab path**. Fix it in the catalog
before the use-case page tells anyone to claim from GitLab.

**Documentation surfaces.**

| Repository | Surface | Change |
|---|---|---|
| ocx | `website/src/docs/reference/command-line.md` | New `#### claim {#package-claim}` block in the five-part shape (prose, Usage, Options table, Exit codes table, JSON report); `--transport` added to the `#package-announce` block and its exit-code table extended with 86. |
| ocx | `website/src/docs/reference/environment.md` | `OCX_ANNOUNCE_TOKEN` gains a per-transport statement; new sections for `OCX_ANNOUNCE_GIT_TOKEN` and `OCX_ANNOUNCE_GIT_USERNAME`; **line 128's job-token claim corrected** from "does not work here" to "no write access — read-only for branches, commits, raw files, merge requests and tags". |
| ocx | announce user-guide page | `--transport` and the four credential postures. |
| ocx | **new** use-case page | Who authors the pull/merge request under each posture, what each index policy makes of it, a posture → env vars → author identity → owner-gated auto-merge → minimum forge version table, one copy-paste CI recipe per posture, and a troubleshooting section keyed to the named errors this ADR introduces. **No external precedent is cited for the two-credential split**: the only source offered for one (Homebrew discussion #3383) was fetched during research and turned out to be a fork-permission troubleshooting thread supporting no such claim. It is retracted and must not be resurrected — present the split on its own merits (a deploy token can push but cannot call the API; a job token can read but cannot open a merge request). |
| ocx | `crates/ocx_lib/src/forge/gitlab.rs:148-160` | **The whole doc comment, not only its job-token half.** The range runs to `:160` — the earlier `:148-157` stopped three lines short of the comment it demands be rewritten whole. Both wrong sentences sit inside the shorter range, so nothing was lost, but a diff scoped to the citation would leave the empty-token behaviour at `:158-160` orphaned from a rewritten neighbour. Two sentences are wrong: the job-token access claim (read-only access does exist), and "`Authorization: Bearer` accepts only OAuth2 tokens, so it is the narrower choice" — false against GitLab's own REST authentication docs, and false independently of D-T8 keeping `PRIVATE-TOKEN`. Named explicitly so the second sentence is not read as scope a diff may skip. |
| ocx | `.claude/artifacts/design_spec_announce_initiative.md` **S1** (`:27`) | The canonical "REST API only, no git subprocess, no local clone" text. Amended here, at the source; the `adr_announce_gitlab_forge.md` D0 restatement is the secondary edit. Without this the ratified register still refuses what this design does. |
| ocx | `crates/ocx_lib/src/forge/api.rs` module doc | Stop naming an operation count (currently "ten", actually eleven before this ADR, thirteen after). |
| ocx | `.claude/artifacts/adr_announce_gitlab_forge.md:62` (D1) | **The second home of the same stale count.** The sentence is "`Forge` (`forge/api.rs`) is exactly the ten operations `announce()` drives." at `:62`; `:59` is a blank line and `:60` is the D1 heading. Same fix, same reason: drop the count rather than bump it. Scheduled here because a diff aimed at the source comment alone would leave the ratified record wrong — and anchored on the sentence, not only the number, because a line citation that still resolves to *something* is how a scoped diff reports done having edited nothing. |
| ocx | `crates/ocx_lib/src/cli/exit_code.rs` | New variant 86 with its numeric-value regression test; `PermissionDenied`'s doc widened. |
| ocx | `.claude/rules/subsystem-cli.md`, `subsystem-cli-commands.md`, `.claude/rules.md` | New command row; `OCX_ANNOUNCE_GIT_TOKEN` in the credential-exemption table. |
| index (`ocx-sh/index`) | `how-to/claim-a-namespace.md` | Rewritten around the command. |
| index | `reference/entry-schema.md`, `reference/namespace-policy.md` | Moved to `login`/`id`; the reserved-segment list gains `index` (enforced in code today, absent from the doc). |
| indexbot | its own owners-drop ADR | **Referenced, not authored here** (D-W3). |

**No `CHANGELOG.md` edit, ever.** The changelog line is the commit subject.

---

## Constitution Check

Every boundary this design crosses, named rather than smoothed over.

1. **S1 is amended at its source, not only at its restatement.** The canonical text is
   `design_spec_announce_initiative.md:27` — "Transport = REST API only. No git subprocess, no
   local clone of the index repo" — and `adr_announce_gitlab_forge.md` D0 merely says it
   "stands unchanged". Amending only the ADR would leave the ratified register still refusing
   both halves of this design, with no cross-reference: an unflagged live contradiction, which
   is the failure this check exists to catch. **Both** documents are edited, the design spec
   first. It is the reason this record is tier high and one-way.
2. **`ocx_lib` gains a runtime dependency on a tool it does not ship, and a new subprocess
   shape.** `child_process.rs` has existing callers (`ocx exec`, `ocx package exec`, the
   launcher) but every one of them spawns *the user's* program on the user's behalf, with
   inherited stdio. This is the first time the library spawns a tool ocx itself depends on, as
   an implementation detail — a new dependency class, not a new use of an old seam — **and the
   first time it needs the child's stdout and stderr back**, which is why the module gains a
   capturing sibling rather than this path bending `spawn_and_wait`. Mitigated by the opt-in
   flag, the named fail-closed error and a documented prerequisite; not eliminated. (Shelling
   out is nonetheless the *compliant* choice under `quality-core.md`'s "Don't Own Non-Domain
   Code": vendoring libgit2 or gix would be owning solved non-domain code.)
3. **The `Forge` contract for `commit_files` weakens** (D-T4). The trait's own text says an
   implementation that cannot hold the contract must return an error rather than approximate
   it; here the contract itself is rewritten so the git half can hold it honestly. That is the
   correct direction, and it is still a weakening of a guarantee the orchestration relied on
   at a specific call site. The orchestration's retry scope widens as a direct consequence.
4. **Env-var naming: `OCX_ANNOUNCE_GIT_*` says "announce" and serves `claim` too.** The
   variable is named for the first command that used it, not for what it authenticates. The
   honest name is `OCX_FORGE_*`, and renaming is deliberately **not** done here: it would
   break every publisher's CI and would break `ocx-mirror`'s deliberate ambient inheritance of
   `OCX_ANNOUNCE_TOKEN`. Recorded as a deferred, batched decision.
5. **`ocx package claim <ns>/<pkg>` takes a positional where `announce --package` takes a
   flag.** Two sibling commands in one group with two spellings for the same input. The
   positional matches every other `ocx package` verb and the dossier's own draft; announce is
   the outlier, kept because its flag surface is already dominated by tag selection. Aligning
   announce is a separate, batched rename, not part of this ADR — and it is a CLI-grammar
   break needing the hidden-alias, warn-once, named-removal-release treatment. **This record
   deliberately does not name that release pair**; the vehicle is the owner's call, carried to
   the handoff.
6. **`ExitCode::PermissionDenied`'s documented meaning widens** from "filesystem `EPERM`" to
   include a forge-side branch-protection refusal. Additive to the mapping, but the doc
   comment is a contract statement and must change in the same PR.
7. **`ExitCode` gains variant 86 — and, unavoidably, the frozen `error.kind` vocabulary gains
   a value with it.** The exit-code table is a shipped interface; the enum is
   `#[non_exhaustive]` and existing scripts are unaffected, but the addition needs its own
   changelog-bearing commit subject and its numeric-value regression test. The second half is
   the one a reviewer would otherwise miss: `ErrorCategory::from_exit_code` matches every
   `ExitCode` with no wildcard by design, so 86 cannot land without a category, and the
   category's snake_case form is a wire value JSON consumers pattern-match on. `error.kind` is
   documented as frozen; this is an **additive** extension — no existing value changes meaning
   — but it is a wire change and is named here rather than smoothed over.
8. **A new credential variable must complete the three-edit checklist** —
   `subsystem-cli.md`'s exemption table, `ocx_lib::env::keys::CREDENTIAL_KEYS`, and
   `environment.md` — in the same pull request. `OCX_ANNOUNCE_GIT_USERNAME` is *not* a
   credential and must **not** go into `CREDENTIAL_KEYS`; documenting it is still mandatory.

---

## Implementation Plan

Ordered so each step is independently verifiable. Contract-first: stubs and tests precede
implementation, per the project's TDD flow.

**The changelog line is the commit subject, so the subjects are part of the plan.** The
user-facing ones, with `!` marked where it applies:

| Step | Commit subject | `!`? |
|---|---|---|
| 1 | `feat(cli): add exit code 86 for a forge capability a transport needs` | no — additive variant |
| 3, 5 | `feat(announce): add --transport git so a GitLab job token authors the merge request` | no — new optional flag, default preserves today |
| 4, 5 | `feat(package)!: claim a namespace from the CLI with ocx package claim` | **yes** — new command, new exit code in its table, new env vars |
| 5 | `feat(announce): report forge, transport, credential kind and capability checks` | no — additive JSON keys |
| 6 | `fix(announce): widen the non-fast-forward retry to the commit-and-open pair` | no — a fix, and the only step whose omission is silent |

Everything else in the plan is `refactor:`, `test:`, `docs:` or `chore:` and carries no
release-note line.

1. [ ] **Free-standing corrections** (no dependency on anything below): the whole
       `forge/gitlab.rs:148-160` comment, `environment.md:128`, the `Forge` doc-count
       sentence, `ExitCode::PermissionDenied`'s doc,
       `ExitCode::ForgeCapabilityUnavailable = 86` with its numeric test — **and, in the same
       commit because the build requires it, `ErrorCategory::ForgeCapabilityUnavailable`, its
       arm in the wildcard-free `from_exit_code` match, and its row in the frozen
       `error_kind` inventory tests**.
2. [ ] **`Forge` surface**: `ForgeIdentity`, `authenticated_identity`, `resolve_user`,
       `PushAccess`/`CapabilityCheck`/`CapabilityName`/`CheckStatus`, the
       `ensure_push_access` return-type change, the rewritten `commit_files` /
       `open_or_update_pull_request` contract text. GitHub and GitLab REST implementations.
3. [ ] **Credential model, the argv-boundary git gate, and the subprocess helper it needs**:
       `ForgeCredentials`, `WriteTransport`, `ForgeKind::validate_transport`, the new
       `ForgeKind::client` signature taking `GitBinary`, the GitLab header selection
       (`JOB-TOKEN` for a job token, today's `PRIVATE-TOKEN` unchanged for everything else,
       the discriminator derived inside the `ForgeCredentials` constructor), and the CLI-side
       precedence resolution. Three-edit checklist for `OCX_ANNOUNCE_GIT_TOKEN` in this step.
       **Also here, and not later: the `git --version` gate itself** (beside
       `validate_transport`, producing `GitBinary`) **and the capturing subprocess helper in
       `child_process.rs`** — reading a version out of `git --version` needs captured stdout,
       which only the capturing sibling provides, so scheduling the gate here and the helper
       in step 7 would make this step unbuildable as written. The helper is a small
       `utility::child_process` addition with no forge dependency, so it moves up cleanly.
4. [ ] **`ocx_lib::claim`**: `claim.rs` + `claim/{request,error}.rs`, the root renderer, the
       branch-state handling of D-C7, the owner resolution ladder, the bot refusal. Reuses
       `oci::index::serialize_root`, and takes its date from the clock accessor moved **down**
       into `oci::index` in the same step (`current_timestamp` / `current_date`, announce
       delegating to it) so `claim` never calls into `announce` for a helper. The
       `__OCX_TESTING_ANNOUNCE_CLOCK` variable keeps its spelling — the acceptance fixtures
       already set it, and it is `__`-prefixed testing-only, so a rename is free later and
       buys nothing now.
5. [ ] **CLI and the report carriers**: `command/package_claim.rs`, the `Claim` variant on the
       `Package` enum, `api/data/claim.rs`, `--transport` added to `package_announce.rs`, and
       the announce report's new operational fields — **including the two new fields on
       `AnnounceOutcome` itself, `branch` and `capability_checks`**
       (`announce/request.rs:115-141` has neither today, and the CLI can derive neither).
6. [ ] **Widen announce's non-fast-forward retry to the commit-and-open pair.** Its own step,
       because nothing else fails when it is skipped and the design's highest-impact risk row
       depends on it. `announce.rs:292-306` calls `commit_files` and `:307` opens the
       `NonFastForward` arm, while `open_or_update_pull_request` sits at `:404-414` **outside**
       that `match` — so under `git`, where D-T4 moves the rejection to the second call, a
       concurrent announce is silently lost. Step 2 changes only the contract *text* and step 7
       adds the workspace re-fetch; neither touches this `match`. Commit subject:
       `fix(announce): widen the non-fast-forward retry to the commit-and-open pair`.
7. [ ] **Git transport** (the capturing subprocess helper already exists — step 3 built it for
       the version gate): the
       `GitWorkspace` (temp clone, hygiene list, `Env::clean` env allowlist,
       `-c credential.helper=` on credential-injecting invocations), the branch-existence read
       and its conditional refspec, the index-file plumbing chain, the compare computation, the
       retry re-fetch, the single push with the closed four-option set and its
       control-character rejection, the bounded confirmation poll, and the stderr classifier —
       including the **capability-gate row** — with the widened redactor. The `git --version`
       gate is **not** here: it lands in step 3 beside `validate_transport`, at the argv-fault
       boundary, so "before any network call" is literally true.
8. [ ] **Preflight**: the two capability reads, the `unknown`-and-proceed rule, the
       `git-version` row emitted from the constructor's `GitBinary`, the `skipped` rows for
       every check that did not apply (including `push-access` under `--out`), and the
       `capability_checks` wiring into both reports.
9. [ ] **Fixture and acceptance suite**: the bare repository behind `git http-backend` on the
       existing fake-forge port, the `post-receive` push-option hook with its configurable
       delay, the rejection paths and their literal refusal line, and every assertion in
       Validation below.
10. [ ] **Docs sweep** across the surfaces table — including the **S1 amendment in
       `design_spec_announce_initiative.md:27`, the D0 restatement and the D1 operation count
       in `adr_announce_gitlab_forge.md`** — plus **three follow-up issues filed**: make the
       forge REST client honour the four **CA** variables `GIT_SSL_CAINFO` / `GIT_SSL_CAPATH` /
       `SSL_CERT_FILE` / `SSL_CERT_DIR` (not the proxy set — `reqwest` already honours those);
       `ocx-mirror`'s missing `forge`/`transport` fields; and the `ocx-catalog` owner href (a
       prerequisite for advertising the GitLab path, not a nicety).

---

## Validation

- [ ] **Claim over REST, both forge surfaces** (`fake_forge.py`): a new root written with
      `login`/`id` only and the exact field order above; the claim branch; a request opened;
      a re-run updating the same request; an existing root refused at exit 65.
- [ ] **Owner resolution**: CI-environment path; token-identity path; bot refusal at exit 64;
      `--owner` replacing rather than appending; `LOGIN:ID` accepted; the no-users-API error
      naming the `LOGIN:ID` form.
- [ ] **`--out`** renders the root and opens nothing, while still refusing an existing root.
- [ ] **Usage refusals**: `--transport git` on GitHub, with `--fork`, and with `--out`; each
      exit 64 with a message naming both flags.
- [ ] **Claim over `--transport git`** (new fixture): the push carries **exactly** the four
      push options and no fifth; the merge-request record appears via REST; a re-run updates
      it; a moved target triggers re-fetch-rebuild-and-retry, and the **second push succeeds**
      (a fixture that rejects once proves the retry runs, not that it converges); a spent
      branch is rebuilt on base with a lease-checked update.
- [ ] **First claim, branch absent** (the headline case, and the one an unconditional fetch
      breaks): the branch-existence read returns 404, `git fetch` is invoked with **one**
      refspec, no `rev-list` compare runs, the commit is parented on the base, and the ref
      update is a create. Proved red by asserting the two-refspec form fails against a
      repository without the branch, so the conditional is not decoration.
- [ ] **Merge-request confirmation is a bounded poll, both outcomes**: with the fixture's
      record delayed *inside* the bound the claim succeeds and reports the request; with it
      delayed *beyond* the bound the run exits **75** with a message naming the rerun, and the
      rerun then finds no open request, takes the refresh-commit direction, and converges. A
      synchronous fixture is not acceptable evidence here — it cannot tell a poll from a single
      read.
- [ ] **Announce over `--transport git`**, separately: the same push-option and retry
      assertions, plus the #399 guarantee this record must not quietly drop — **a spent branch
      is rebuilt on the index base with its tag delta carried forward**. The carry is correct
      to omit for claim (D-C7: content is derived from flags) and is the regression guard for
      announce.
- [ ] **No fallback: a git failure never produces a REST write** — the fixture records zero
      REST write calls on every git failure path.
- [ ] **The unchanged-path ensure under `git`** (D-T4, second direction): a run with no
      pending local commit and an existing open request performs **no** push at all; the same
      run with no open request creates the refresh commit and the request appears via REST.
- [ ] **Credential selection**: `api` with no token → `AuthError`; `git` with no ocx variable
      inside a fake GitLab job → `JOB-TOKEN` on reads and `gitlab-ci-token` on the push;
      `git` with no variable outside a job → `AuthError`; `OCX_ANNOUNCE_GIT_TOKEN` overriding
      only the push; **a non-job token → `PRIVATE-TOKEN`, unchanged from today** (the assertion
      is that the shipped header did *not* move, which is the regression this arm risks).
- [ ] **Credential-leak assertions**, via the recording `git` shim: the secret absent from
      every child argv, from the temp clone's `.git/config`, from the remote URL, and from
      stderr and logs on **every** failure path. Each of the three secret forms — API token,
      `OCX_ANNOUNCE_GIT_TOKEN`, and the `base64(user:secret)` blob — is **seeded into a fixture
      stderr and proved red before the redactor is trusted green**.
- [ ] **Child-environment assertions** (same shim): `GIT_TRACE`/`GIT_CURL_VERBOSE`/
      `GIT_ASKPASS` and every `OCX_*` credential absent from the child; an ambient `GIT_TRACE`
      set in the parent does **not** reach the child (the `Env::clean` requirement, proved red
      by constructing with `Env::new()`); `LC_ALL=C` present, and the English classifier
      phrases still match on a runner with a non-English locale; an ambient `http_proxy`
      passed through; **no credential helper process is invoked on a credential-injecting
      run**. Its complement is asserted too: under push-credential precedence step 3 the
      helper **is** invoked, no `extraHeader` is configured, and the report says
      `push_credential_kind: "git-helper"` — the ratified fallback would be silently dead if
      the reset were applied unconditionally, and only the paired assertions catch that.
- [ ] **Capability gate**: an allowlist miss → exit 86 with both project paths named; the
      **two-signal** old-instance path → exit 86 with the "field unreadable (GitLab < 18.4 or
      hidden); push refused" message. That case is driven from both ends: the fake forge omits
      `ci_push_repository_for_job_token_allowed` from `GET /projects/:id` so the preflight
      reports `unknown`, and the fixture's `pre-receive` hook writes the literal line
      `remote: You are not allowed to push code to this project.` to stderr and exits non-zero.
      **Both signals are required**: the same hook line with the preflight reporting `passed`
      must land on 77, and the same `unknown` preflight with a push that succeeds must exit 0
      — three cases from one fixture, because either signal alone reaching 86 is the bug.
- [ ] **`unknown` does not block**: a preflight field that cannot be read **with a push that
      then succeeds** → `unknown` and exit 0.
- [ ] **`capability_checks` is complete and has one assembler**: a `--transport git` run
      carries a `git-version` row with the parsed version as `detail`, sourced from the
      `GitBinary` the constructor was given; a `--out` run still carries a non-empty array with
      `push-access` marked `skipped`; and announce's report carries `branch` and
      `capability_checks` from the two new `AnnounceOutcome` fields, not from CLI-side
      reconstruction.
- [ ] **A missing `git` binary** → named error at exit 69 with **zero** network calls
      recorded, which holds because the version gate runs at the argv-fault boundary before
      the forge is constructed (D-T10).
- [ ] **Owner provenance**: a resolvable users API overrides a CI-environment id and takes the
      forge's canonical login; a supplied `LOGIN:ID` whose id disagrees → exit 64; an
      unreachable users API → `owner_identity_source` is `asserted` or `ci-environment` and the
      stderr owner line says so.
- [ ] **The pre-existing-token warning**: `--transport git` inside a fake `GITLAB_CI` with a
      non-job `OCX_ANNOUNCE_TOKEN` and no `OCX_ANNOUNCE_GIT_TOKEN` emits the stderr line naming
      the push credential kind, before any write; the same run without `GITLAB_CI` does not.
- [ ] **Tempdir mode `0700`** — **Unix only**, and skipped with an observed reason on Windows
      rather than asserted vacuously.
- [ ] **Partial clone against a self-managed GitLab, not only gitlab.com.** GitLab's
      `uploadpack.allowFilter` support was feature-flagged and had follow-on defects, and no
      source pins down whether every currently-supported self-managed version enables filtering
      over HTTPS. A server that silently ignores an unsupported filter leaves `compare_branch`
      correct but defeats the cost rationale for choosing `git`, so the pre-0.6.1 gate tests
      that the filter **applies**, not only how big the result is.
- [ ] **Unit (Rust)**: root rendering (field set, order, `tags: {}`, `desc: null`, `created`
      date form); owner serialisation; `ForgeKind::validate_transport` refusals; every new
      exit-code mapping; the git stderr classifier over each phrase.
- [ ] **Every regression test proved red first** — mutate the consumer arm, restore, re-green
      — per `quality-core.md` § Unchecked Green. A green that cannot be told from "never ran"
      is not a check.
- [ ] **Schema**: a generated claim root passes the index repository's `schema-validate`, and
      the negative control (drop `id`) fails.
- [ ] **Latency measurement**: the blobless clone of `ocx-sh/index` timed and sized, recorded
      in the release notes rather than asserted here.
- [ ] **Gates**: `task verify` on ocx; the website build; the index-repo docs sweep reviewed.
- [ ] **Live**: the [#411](https://github.com/ocx-sh/ocx/issues/411) reporter's run against
      their self-managed GitLab 19.3 — the only real-server check, and the authoritative
      signal for the two failure texts this design deliberately does not string-match.

---

## Open Questions

**None.** All three carried questions were answered against primary sources on 2026-09-05, and
the answers are recorded where they belong rather than left as a section a reader has to
reconcile with the decisions.

| Was | Answer | Where it now lives |
|---|---|---|
| Does `root.schema.json` require `upstream.repository_url` / `upstream.disclaimer` when `upstream` is present, and is `upstream` mandatory? | The live `ocx-sh/index` `schema/root.schema.json` (sha `153d55d8`) does **not** list `upstream` in `required`; inside `upstream`, **only `org` is required**, `repository_url` (`format: uri`) and `disclaimer` (`string \| null`) are optional, and `additionalProperties` is `false`. Requiring `upstream` for a third-party namespace is a **governance** rule, not a schema one. | CLI grammar table, the deviation set, and the claim-root derivation table — all corrected below to `--upstream-org` as the anchor with the other two independently optional. |
| Does `../ocx-indexbot/.claude/artifacts/adr_forge_neutral_owners.md` resolve? | Yes — it exists on disk; the path is cross-repository **by design**. | The D-W3 citation, kept qualified as cross-repository. No further action. |
| Does `site/src/docs/how-to/claim-a-namespace.md` exist? | Yes — in `ocx-sh/index`, beside `announce-a-package.md` and `yank-a-version.md`. That repository's site tree is rooted at `site/`. | The index-repo half of the documentation table. No further action. |

**What the schema answer changed.** The earlier draft required `--upstream-org` and
`--upstream-repository-url` *together*, on the assumption that a URL-less upstream block would
fail validation. It would not. The grammar is therefore looser than the draft and closer to
the dossier's own recommendation: **`--upstream-org` is the anchor, and
`--upstream-repository-url` and `--upstream-disclaimer` are each independently optional while
each requiring `--upstream-org`** (clap `requires`, not `requires_all`). The dossier asked for
three independent flags; the honoured half is independence *of each other*, and the retained
constraint is dependence on the anchor — without `org` there is no object to attach a URL or a
disclaimer to, and `additionalProperties: false` means a stray key is refused outright.

**And what it did not change: ocx does not duplicate the index's requiredness.** The index's
own `schema-validate` is the authority. A claim that violates a governance rule fails visibly
in the pull request, which is where a governance rule belongs — an ocx-side copy would be a
second source of truth that goes stale the first time the index tightens or relaxes the rule.

**Owner-field grammar, confirmed against the same schema:** `login` matches
`^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$`, `id` is an integer `>= 1`, and `owners` carries
`minItems: 1`. The `LOGIN:ID` parse and the resolved-owner rendering both conform; a claim can
never emit an empty `owners` array, because the ladder exits 64 rather than proceeding with no
identity.

## Deferred to handoff

The dossier's remaining open questions are **decided** above and need no slot: exit code
(D-T9 + the exit-code table), git-half packaging (D-T1), claim branch name (D-C6), git
fixture shape (Test fixture), minimum git version (D-T10), and the `upstream`/`repository`
flag grammar (CLI grammar table, now schema-verified against `root.schema.json` sha
`153d55d8` and carrying no residual question).

Carried out of scope, for the orchestrator to route — each needs a human, not more design:

- **Whether 0.6.1 ships both or the transport alone.** Steelman recorded under Consequences;
  dossier decision 8 stands until the owner says otherwise. A schedule call.
- **Whether the blobless-clone measurement should gate acceptance of this ADR** rather than
  the 0.6.1 release. S1's own rationale is "clone cost grows", and this record amends S1 while
  recording the size as unmeasured.
- **Whether the index's reviewer checklist already requires verifying `owners[]` against forge
  profiles.** The STRIDE Spoofing row leans on that control, and it lives in `ocx-sh/index`.
  If it is not there today it must be added to `governance-contracts.md` in the same batch.
- **The vehicle and release pair for `announce --package` → positional** (batched-window
  carve-out, hidden alias, warn-once, named removal release).
- **Whether a forge coordinate can reach these commands from anything but argv** — a
  `[managed]` payload, `ocx.toml`, an ocx-mirror spec. If yes, the SSRF guard becomes
  mandatory on the forge path and that section flips from "deliberately unguarded" to a gap.
- **indexbot's dual-emit stop date** (recommended: indexbot 0.7, with its own owners-drop ADR).
- **The GitHub invoker-identity governance question** — and it should name **`actor_id`**
  explicitly, not just "immutable subject claims". GitHub's Actions OIDC token has carried
  `actor_id`, "the ID of the personal account that initiated the workflow run", since the
  January 2023 custom-claims rollout, requestable with a plain `id-token: write` permission.
  That is a cryptographically verifiable assertion of the triggering human — materially
  stronger than the bare `GITHUB_ACTOR` environment string arm 2 trusts — and the verifier
  belongs on indexbot's side, not ocx's. Naming it saves the next person deriving it again.
- **OIDC trusted publishing as the general answer to authorship**, evaluated *before* the
  job-token special case ossifies. PyPI, npm and crates.io all follow one shape: the workflow
  presents a short-lived OIDC identity token and the registry issues a temporary credential
  scoped to that package and run. Applied here, an index could accept an ID token as the
  claim's identity proof — decoupling "who authored this" from "how the bytes got written"
  entirely, which is the one thing this ADR's transport-implies-identity framing does not do.
  Cross-repo (indexbot would be the verifier), changes nothing in this record's decision, and
  is the SOTA answer worth a forward-pointer next to it.
- **`OCX_ANNOUNCE_*` → `OCX_FORGE_*`** (batched; breaks publisher CI and ocx-mirror
  inheritance).
- **`ocx-mirror`'s missing `forge`/`transport` fields**; the corporate-CA REST-client issue;
  and `ocx-catalog`'s owner href (a prerequisite for advertising GitLab claims, promoted out
  of this list in Migration and Rollout).
- **A third forge would need more than an enum arm.** Gitea's AGit is mechanically close to
  GitLab's model (branch push plus `-o` options) and could reuse the workspace nearly as-is,
  but Forgejo's AGit-Flow pushes to `refs/for/<base>/<topic>` — the ref target itself carries
  the "this is a pull request" signal. `git_workspace.rs` as specified takes one push with the
  target ref fixed to the branch, so a Forgejo arm needs a **refspec-shape parameter** on the
  seam, not just a new `ForgeKind` variant. Out of scope at 0.6.1; recorded so the design's
  "the transport lives inside the forge container" rationale is not read as "forge N+1 is just
  another arm".

---

## Links

- [`system_design_index_claim_command.md`](./system_design_index_claim_command.md) — the C4 companion to this ADR
- [`adr_announce_gitlab_forge.md`](./adr_announce_gitlab_forge.md) — D0/S1 (amended here), D1 (trait), D3 (declared kind), D9/D15 (redaction)
- [`adr_announce_publisher_surface.md`](./adr_announce_publisher_surface.md) — D3 (token model), D5 (orchestration in `ocx_lib`, CLI thin)
- [`adr_announce_diverged_branch_rebuild.md`](./adr_announce_diverged_branch_rebuild.md) — `RefUpdate::Reset` with carried tags, the semantics D-C7 simplifies
- [`adr_index_root_variants.md`](./adr_index_root_variants.md) — precedent for staging a published-root field change
- Research: [`research_index_claim_recon.md`](./research_index_claim_recon.md), [`research_index_claim_prior_art.md`](./research_index_claim_prior_art.md), [`research_index_claim_archaeology.md`](./research_index_claim_archaeology.md), [`research_index_claim_council_transport.md`](./research_index_claim_council_transport.md), [`research_index_claim_security.md`](./research_index_claim_security.md), [`research_index_claim_git_tooling.md`](./research_index_claim_git_tooling.md), [`research_index_claim_operability.md`](./research_index_claim_operability.md), [`discover_index_claim_map.md`](./discover_index_claim_map.md)
- Issues: [ocx#410](https://github.com/ocx-sh/ocx/issues/410), [ocx#411](https://github.com/ocx-sh/ocx/issues/411), [ocx#399](https://github.com/ocx-sh/ocx/issues/399), [ocx#228](https://github.com/ocx-sh/ocx/issues/228)
- GitLab: [CI/CD job token](https://docs.gitlab.com/ci/jobs/ci_job_token/), [push options](https://docs.gitlab.com/topics/git/commit/#push-options-for-merge-requests), [job-token scope API](https://docs.gitlab.com/api/project_job_token_scopes/), [Projects API](https://docs.gitlab.com/api/projects/), [Users API](https://docs.gitlab.com/api/users/), [service accounts](https://docs.gitlab.com/user/profile/service_accounts/), [fine-grained job tokens GA](https://about.gitlab.com/blog/fine-grained-job-tokens-ga/)
- GitHub: [PR REST](https://docs.github.com/en/rest/pulls/pulls), [`GITHUB_TOKEN` permissions](https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/controlling-permissions-for-github_token), [username reference](https://docs.github.com/en/enterprise-cloud@latest/account-and-profile/reference/username-reference), [immutable OIDC subject claims](https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens/)
- Git: [2.31 release notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.31.0.adoc), [git-config](https://git-scm.com/docs/git-config), [partial clone](https://git-scm.com/docs/partial-clone), [githooks](https://git-scm.com/docs/githooks/2.27.0), [protocol-common (pkt-line)](https://git-scm.com/docs/protocol-common), [gitcredentials(7)](https://www.man7.org/linux//man-pages/man7/gitcredentials.7.html)
- Security: [Clone2Leak](https://flatt.tech/research/posts/clone2leak-your-git-credentials-belong-to-us/), [CVE-2026-3854 — GHES push-option delimiter injection to RCE](https://github.blog/security/securing-the-git-push-pipeline-responding-to-a-critical-remote-code-execution-vulnerability/), [gitoxide crate-status](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md), [git2-rs README](https://github.com/rust-lang/git2-rs/blob/master/README.md)
- Partial clone on GitLab (unresolved, see Validation): [gitaly#2510](https://gitlab.com/gitlab-org/gitaly/-/issues/2510), [gitaly#2553](https://gitlab.com/gitlab-org/gitaly/-/issues/2553), [gitaly#1553 — SSH-only scoping, dated](https://gitlab.com/gitlab-org/gitaly/-/issues/1553)
- Third-forge conventions (deferred): [Gitea AGit](https://docs.gitea.com/usage/issues-prs/agit/), [Forgejo AGit-Flow](https://forgejo.org/docs/latest/user/agit-support/)
- OIDC identity (deferred): [GitHub Actions OIDC reference — `actor_id`](https://docs.github.com/en/actions/reference/security/oidc), [OpenSSF trusted publishers](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html), [npm trusted publishing GA](https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/)
- Prior art: [winget-create](https://github.com/microsoft/winget-create), [Homebrew PR how-to](https://docs.brew.sh/How-To-Open-a-Homebrew-Pull-Request), [conda-forge staged-recipes](https://github.com/conda-forge/staged-recipes/blob/main/README.md), [Renovate GitLab platform](https://docs.renovatebot.com/modules/platform/gitlab/), [glab mr create](https://docs.gitlab.com/cli/mr/create/), [gh exit codes](https://cli.github.com/manual/gh_help_exit-codes), [gh pr create lacks --json](https://github.com/cli/cli/issues/11558), [glab live-instance testing gap](https://gitlab.com/gitlab-org/cli/-/issues/1161), [PEP 691](https://peps.python.org/pep-0691/), [crates.io registry index](https://doc.rust-lang.org/cargo/reference/registry-index.html)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-09-05 | Architect (Opus 5) | Initial draft. Three coupled decisions from the owner-ratified dossier of 2026-09-04, with four weighted option matrices, the full component contracts, a new exit code at 86 (85 was already taken), the security lane's four gaps folded in, and three open questions. |
| 2026-09-05 | Architect (Opus 5) | Fix round 1, against four reviews (spec, security, quality, SOTA). Five Blocks closed: the git contract for an ensure with no pending commit (D-T4); exit 86's undetectable "too old" branch, replaced by a two-signal rule; S1 amended at its source in the design spec; a capturing subprocess helper, because `spawn_and_wait` can capture nothing; and the `mktree` recipe replaced by an index-file chain that can write a nested path. Criteria set gained D7 (solves the reported problem) and D8 (cost of ownership) and all four matrices were re-scored — the arithmetic now picks the same winners the prose does. `PRIVATE-TOKEN` is kept for non-job tokens, reversing an unrequested header switch. Owner identity gained a provenance rule and an `owner_identity_source` field; `--owner LOGIN:ID` is no longer taken on trust. Added `-c credential.helper=`, `Env::clean`, multi-secret redaction, a closed push-option set, control-character rejection citing CVE-2026-3854, and the recording-`git`-shim capture mechanism. Deferred list gained the OIDC `actor_id` and trusted-publishing pointers and the Forgejo refspec caveat. |
| 2026-09-05 | Architect (Opus 5) | Fix round 2a, against the cross-model adversary pass (2 Blocks, 3 Highs). The git recipe gained step 0, a REST branch-existence read, because an unconditional second refspec made `git fetch` fail on every first claim and left the `Absent` state unreachable. `ExitCode::ForgeCapabilityUnavailable = 86` now carries the `ErrorCategory` arm its wildcard-free match makes compulsory, in the same phase. The `-c credential.helper=` reset is scoped to invocations that inject an ocx credential, so the ratified git-helper fallback survives; the residual risk is stated. Every "REST for every read" claim carves out `compare_branch`. The atomicity claim in D-T4 is withdrawn: GitLab creates push-option merge requests asynchronously, so step 6 became a bounded poll with exit 75 on exhaustion and a rerun that converges by the refresh-commit direction. **Open questions go to zero** — the live `root.schema.json` (sha `153d55d8`) shows only `upstream.org` required, so the `--upstream-*` flags are each independently optional on the anchor, and the two cross-repository paths both exist by design. |
| 2026-09-05 | Architect (Opus 5) | Fix round 2b, spec re-validation. The two partial closures are closed: the stderr classifier gains the **capability-gate row** that makes the two-signal rule reachable at all (promotion driven by the preflight, never by a phrase), and the changed-contracts row now carries D-T4's no-write branch. The `git --version` gate moves to the CLI argv-fault boundary beside `validate_transport`, so "before any network call" is literally true rather than contradicted by the sequence. D-C1's mitigation drops the withdrawn "local-cache operations" help text for the forge invariant. The retry widening becomes **its own step 6** with its own commit subject, renumbering the plan to ten. `CheckStatus::Failed` is dropped — nothing could emit it, and the spellings are one-way. `adr_announce_gitlab_forge.md:59`'s duplicate operation count and the corrected `gitlab.rs:148-160` range are scheduled; the corporate-CA follow-up is pinned to the four CA variables; the deviation set gains the merge-request-URL reversal and the dossier's second Bearer assertion. |
| 2026-09-05 | Architect (Opus 5) | Fix round 3, spec re-validation, closing the two consequences of moving the version gate. The gate now yields **`GitBinary { path, version }`**, which `ForgeKind::client` requires under `git` and from which `ensure_push_access` renders the `git-version` capability row — one producer, one assembler, and the dependency in a signature rather than a repeated `PATH` lookup. Inapplicable checks report `skipped` rather than being omitted, so a `--out` run cannot emit an empty array, and **step 5 now adds `branch` and `capability_checks` to `AnnounceOutcome`**, which carries neither today. The capturing subprocess helper moves from step 7 into step 3, because the gate reads a version from captured stdout and could not otherwise be built where it is scheduled. D-C7 gains the `Absent` row it omitted — the state every first claim takes. Warns: the T-D description carves out `compare_branch`; the stale-count anchor moves to `:62` and is anchored on its sentence; three remaining `gitlab.rs:148-157` citations corrected; two code anchors extended by one line. |

## Handoff decisions (2026-09-05)

Resolved by the orchestrator under the owner's autonomous-implementation mandate
("/hex-plan high … then fully implement the resulting plan"). Each item above maps to one row.

| Deferred item | Decision | Where it lands |
|---|---|---|
| 0.6.1 ships both or transport alone | **Both** — dossier decision 8 stands. | this ADR, the plan |
| Blobless-clone measurement gate | Gates the **0.6.1 release**, not this ADR. The plan carries a measurement step; the number goes into the PR body. | plan step, PR body |
| Index reviewer checklist verifies `owners[]` | Verified during execution against `ocx-sh/index` `governance-contracts.md`; if absent, an issue is opened on `ocx-sh/index` (cross-repo, cannot land here). | execution note, index issue |
| `announce --package` → positional | Ships in this change, joining the open 0.6 → 0.7 batched window: hidden `--package` variant in `deprecated.rs`, warns once on stderr, removal named as 0.7. | plan step, `deprecated.rs` |
| Forge coordinate from non-argv sources | Argv is the only source today (ocx-mirror drives ocx through the CLI). The forge path stays deliberately unguarded; if execution finds a non-argv source, the existing proxy-aware SSRF guard is applied to it — no new guard. | execution note |
| indexbot dual-emit stop date | Recommend indexbot 0.7 with its own owners-drop ADR; opened as an issue on the index repo referencing this ADR. | index issue |
| GitHub invoker identity (`actor_id`) | Out of 0.6.1 scope; index-governance question, filed on `ocx-sh/index` with the `actor_id` note above. | index issue |
