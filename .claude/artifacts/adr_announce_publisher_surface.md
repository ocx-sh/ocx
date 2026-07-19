# ADR: Publisher-Side Announce Integration Surface

<!--
Architecture Decision Record — analysis phase (design only, no implementation).
Filename: artifacts/adr_announce_publisher_surface.md
Owner: Architect (/architect)
Handoff to: Builder (/builder), Security Auditor (/security-auditor)
Scope: the client/publisher half of the announce lane — the Rust `ocx package
announce` command (ocx#216), the CI units publishers drop into their pipelines,
the token model, and the ocx-internal layering. The index/server half is owned
by index-repo ADR-6 (`adr_fork_pr_announce.md`); this ADR ports its contract,
it does not redefine it.
-->

## Metadata

**Status:** Accepted (D4 CI-unit placement deferred — see Open Items)
**Date:** 2026-07-18 (accepted 2026-07-19)
**Deciders:** Michael Herwig (owner) + Claude design swarm
**Beads Issue:** N/A
**Related PRD:** [ocx#216](https://github.com/ocx-sh/ocx/issues/216) — Rust `ocx package announce` tracking issue
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust/Tokio primary; reuses the workspace `reqwest` already in-tree via the oci-client fork; GitHub Actions + OIDC-era CI posture)
- [x] "Choose Boring Technology" / "zero production services" upheld — GitHub-native and GitLab-native primitives only, no hosted App or relay
**Domain Tags:** integration | security | ci-cd | api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

The OCX public index (`ocx-sh/index`, served static at `https://index.ocx.sh`) has
moved its submission model from a `repository_dispatch` + per-publisher fine-grained
PAT "doorbell" to a **fork-PR announce lane**, recorded in index-repo **ADR-6**
([`adr_fork_pr_announce.md`](../../index/.claude/artifacts/adr_fork_pr_announce.md)).
Under ADR-6 a publisher opens an ordinary pull request from a fork of the index, under
their own GitHub identity: identity is the GitHub account, authorization is
`owners[].github_id` already committed in the package root, and verification is CI
re-derivation from registry truth (nothing claimed is trusted). No publisher ever holds
a credential *on the index* (FP-1, FP-7). The Python `indexbot announce` is the
**reference publisher tool** and the executable definition of the canonical byte-exact
serializer (FP-4, FP-9); `bot/CONTRACTS.md` §12/§14 is the client-facing byte spec.

This ADR decides the **publisher-side integration surface** that ports that contract into
the OCX toolchain and puts a BCR-grade "drop a CI unit in your repo and forget it"
experience in front of it. It covers five design questions (D1–D5) plus the NFR posture.
The index/server behaviour (verify-claims, governance lanes, G-19/G-20, reconcile) is
**out of scope** — owned by ADR-6, ported here verbatim, never redefined.

The following are **already decided with the owner** and enter this ADR as constraints,
not open questions:

- **What's-new detection = owner-curated explicit tags in v1.** Registry-scan/livecheck
  is a later convenience layer feeding the same pipeline (Homebrew `livecheck` pattern).
  This puts OCX in the "explicit-manifest-in-PR" camp (winget/Homebrew per-bump args),
  not the "scan-and-diff" camp (r-ryantm/Scoop).
- **`ocx package push --announce-file <path>`** appends the pushed primary tag plus its
  cascade tags to a per-package file (comma/newline format, matching `indexbot
  --tags-file`).
- **`ocx package announce --tags-file`** unions file tags with the committed root's tags
  (client-side sugar); explicit `--tags` full-replace curation is retained. This is a
  **deliberate deviation** from the reference `regenerate()` (replace-only, "observed =
  the universe") — flagged below as a one-way door needing owner ratification.
- **Fork auto-create if missing** (proposed: yes — wingetcreate pattern; the Python
  reference requires a pre-existing fork).
- **UX bar = Bazel `publish-to-bcr` grade**, and it MUST work for **GitHub Actions AND
  GitLab CI** publishers.

## Decision Drivers

- **Zero index-side credential, zero hosted moving part.** ADR-6 FP-7 removed the whole
  per-publisher-secret class; the publisher surface must not reintroduce it, nor add a
  hosted App/relay the "zero production services" driver forbids.
- **BCR-grade publisher ergonomics.** A publisher configures a fork + a token once and
  drops a ready-made CI unit in; everything else is automatic — on both GitHub Actions
  and GitLab CI.
- **Byte-exact fidelity is correctness, not polish.** The Rust client must reproduce the
  canonical root and observation serialization (CONTRACTS §14) byte-for-byte or every
  honest PR fails index CI. This is the single load-bearing conformance surface.
- **Keep the one-way doors narrow.** The index is GitHub today; private/enterprise
  indices may be GitLab-hosted tomorrow. What must be forge-neutral from v1 is the *user
  surface* (flags, config keys, contract), not necessarily the *implementation*.
- **Lib hosts substance, CLI is thin** (project doctrine); a forge REST client is a
  utility/vocabulary concern that lands inside `ocx_lib`, following the existing
  `*Transport` trait idiom — not a new crate (YAGNI, arch-principles "Core vs Plugin").

## Industry Context & Research

**Research artifacts:** index-repo
[`research_index_announce_bots.md`](../../index/.claude/artifacts/research_index_announce_bots.md);
fresh deep-dives on `publish-to-bcr`, GitLab CI mechanics, and cross-ecosystem prior art
captured in this session (summarised below).

- **Bazel Central Registry / `publish-to-bcr`** is the closest analogue to the target UX
  and the clearest catalogue of what *not* to copy. It migrated **away** from a GitHub App
  (write access to every publisher repo) to a **reusable workflow + composite action**
  after the App's attack surface was flagged post-xz-backdoor
  ([publish-to-bcr#157](https://github.com/bazel-contrib/publish-to-bcr/issues/157)); the
  App is sunsetting June 2026. It uses a **classic PAT** (`workflow`+`repo`) — fine-grained
  PATs **cannot open PRs against public repos**
  ([github/roadmap#600](https://github.com/github/roadmap/issues/600)) — recommends a
  dedicated **machine account**, and bolted on failure-observability issue-by-issue. Key
  lessons: (1) start at PAT + reusable-workflow, never build the App; (2) hardcoding one
  blessed workflow is the top adoption blocker
  ([#262](https://github.com/bazel-contrib/publish-to-bcr/issues/262)) — do not over-fit;
  (3) build structured failure surfacing from day one.
- **winget-pkgs / `wingetcreate`** and **Homebrew / `bump-formula-pr`**: single command
  forks (reusing an existing fork), commits, opens one-package-per-PR — both **mandate a
  classic PAT and explicitly reject the ambient `GITHUB_TOKEN`** (it cannot fork/PR
  cross-repo). Homebrew calls `check_for_duplicate_pull_requests` before opening;
  **winget has no dedupe** ([#32738](https://github.com/microsoft/winget-pkgs/issues/32738))
  — the anti-pattern to avoid. Both treat deprecate/disable/yank as human-only edits the
  bump tool refuses to touch — mirrors ADR-6's G-05 human-lane keys.
- **GitLab mechanics:** for a **GitHub-hosted** index, a GitLab-CI publisher talks *only*
  the GitHub API — GitLab is purely the execution environment, no GitLab API on the write
  leg. The idiomatic reusable unit is a **CI/CD Component** (GA in GitLab 17.0, catalogued,
  typed `spec:inputs`), not `include:remote`. `CI_JOB_TOKEN` **cannot** drive
  fork+commit+MR (Forks API not in its allowlist; Commits/MR APIs read-only) — a real
  PAT/project/group token is required, exactly as on GitHub.
- **Forge-abstraction prior art (Renovate, go-scm):** one small internal interface
  (`createPr`/`findPr`/`updatePr`, or `commit_files`/`open_or_update_pull_request`) with a
  per-forge impl; **credential shape is the only thing the outer CI layer knows per forge**,
  everything else is internal. The Python reference already expresses this as the
  `GitHubPort` Protocol.

**Key insight:** the fork-PR lane is a *solved, boring* shape everywhere it appears. The
publisher surface's whole job is to be a faithful, byte-exact, well-dedup'd client of it —
on two CI platforms — while holding the forge-neutral line at the *surface* and refusing
every temptation (App, relay, hosted service, index-issued token) that ADR-6 already
rejected on the server side.

## Design Decisions

### D1 — Integration-surface shape per forge

How publishers invoke the announce flow from CI, wrapping the same self-contained
`ocx package announce`.

| Option | Pros | Cons |
|---|---|---|
| **(A) GitHub reusable workflow + composite action; GitLab CI/CD Component — thin wrappers over `ocx package announce`** | Matches the BCR-grade UX bar; GitHub-native + GitLab-native, zero hosted parts; each is ~15 lines (install pinned ocx → run announce); versioned/pinnable | Two artefacts to maintain (one per forge CI platform); publisher still configures fork + token once |
| **(B) GitHub App / hosted relay minting per-publisher tokens** | "Install once" ergonomics; ephemeral tokens | Reintroduces a hosted moving part + an index-provisioned credential — the exact thing ADR-6 FP-7 and the "zero production services" driver forbid; BCR is *retiring* this model post-xz |
| **(C) No packaged CI unit — document raw `ocx package announce` invocation only** | Nothing to maintain | Fails the BCR-grade bar; every publisher hand-rolls fork/token/branch wiring; no dedupe or idempotency guarantees baked in |

**Recommendation: (A).** A GitHub composite action / reusable workflow **and** a GitLab
CI/CD Component, both thin wrappers that install a pinned `ocx` (reuse `ocx-sh/setup-ocx`)
and run `ocx package announce`. **Load-bearing reason:** the integration surface must
carry no long-lived index-side secret and no hosted service — a reusable workflow /
component does; an App does not. The self-contained CLI holds all the logic (D5), so the
CI units stay trivially thin and the two-platform requirement costs only two small YAML
wrappers, not two codepaths.

### D2 — Forge abstraction inside ocx (the one-way-door decision)

Does announce hardcode GitHub, or introduce a forge trait (fork / commit / PR-or-MR) with
github + gitlab impls? Disambiguation that reframes the whole question: **a GitLab-CI
publisher announcing to the GitHub-hosted index needs no forge abstraction at all** — the
write leg is 100% GitHub API (D1's GitLab Component + D3's GitHub token cover it). A forge
*abstraction* is needed only when the **index itself is GitLab-hosted** (private/enterprise
indices) — which is not the case today.

| Option | Pros | Cons |
|---|---|---|
| **(A) Hardcode GitHub throughout, GitHub-shaped flags/config/contract** | Least code now | Bakes GitHub into the *user surface*; a later GitLab-hosted index is a breaking change to flags, config schema, and the announce contract — the expensive door |
| **(B) Full `ForgePort` trait now (mirror the Python `GitHubPort`) with github + gitlab impls in v1** | GitLab-hosted index works immediately | A whole GitLab impl (fork async-poll, MR `target_project_id`, draft-via-title quirk) built ahead of any evidence of demand — YAGNI; an interface with a barely-used second impl |
| **(C) GitHub-only implementation in v1 behind a thin test seam (IndexTransport idiom); keep the *user surface* forge-neutral so GitLab is purely additive; commit via forge REST API (no local git)** | Pays only for what ships; the narrow one-way door (the surface) is held open; GitLab impl is a later additive module, not a rewrite; REST-API commit transport works identically for both forges and avoids a git2/gitoxide dependency | Requires discipline to keep every flag/config key/contract string forge-neutral from day one |

**Recommendation: (C).** Ship a **GitHub-only** implementation in v1. Do **not** build a
multi-forge abstraction (single production impl = premature abstraction; extract the trait
only when the GitLab impl genuinely arrives — quality-core DRY "2+ genuinely different
callers"). A thin **test seam** trait (à la `IndexTransport`/`OciTransport`, for a stub in
unit tests) is fine and idiomatic; that is a *testability* seam, not a *multi-forge* one.
Commit transport is the **forge REST contents/commits API** (mirroring the Python
reference's Git Data tree/commit/ref approach) — no local `git clone`, no `git2`/`gitoxide`
dependency, and the same shape ports to GitLab's Commits API later.

**One-way-door analysis (what must be forge-neutral from v1):**
- **CLI flags:** `--fork <owner/repo>`, `--index-repo`, `--tags`/`--tags-file`,
  `--yank`/`--unyank`/`--yank-reason` — all already forge-neutral in the Python reference.
  Shipping any `--github-*`-prefixed flag is the door slamming.
- **Config schema:** the announce target derives from the index config
  (`[indices."<ns>"]`), *not* a GitHub-specific block. The forge is **inferred from the
  target host** (`github.com` → GitHub REST; a GitLab host → GitLab REST). See D5 for the
  one genuinely new config field (index *source-repo* coordinate), which must carry a
  forge-neutral shape (`github:owner/repo` / `gitlab:group/proj`).
- **Index CONTRACTS:** untouched — the wire format is forge-agnostic already; announce
  claims files, it does not encode forge identity into them.

Deferring GitLab therefore leaks **nothing** into flags, config, or CONTRACTS **provided
the surface stays neutral**. That neutrality is the door; the internal GitHub-only code is
not.

### D3 — Token model per forge

| Concern | GitHub (v1) | GitLab (deferred, index-hosted-on-GitLab only) |
|---|---|---|
| Capability required | Fork the index repo + push to the fork + open a cross-repo PR | Fork into a namespace + multi-file commit + open cross-project MR (`target_project_id`) |
| Token type | **Classic PAT** (`repo`/`public_repo` + `workflow`), or a dedicated machine account. Fine-grained PATs **cannot open PRs against the public `ocx-sh/index`** (github/roadmap#600) — document the `open_pull_request:false` manual-URL fallback | PAT / project / group access token with `api` (or `write_repository` + MR-create); group token for org-namespace forks |
| Why the ambient CI token is insufficient | `GITHUB_TOKEN` in Actions is scoped to the publisher's **own** repo — it cannot fork or open a cross-repo PR (documented by winget-create and Homebrew) | `CI_JOB_TOKEN` — Forks API not in its allowlist; Commits/MR APIs are **read-only** via job token |
| Env var | `OCX_ANNOUNCE_TOKEN` (forge-neutral, forge inferred from target host); fall back to `GITHUB_TOKEN` **only** when it demonstrably carries fork+PR scope (it does not in Actions — warn, do not silently proceed) | same `OCX_ANNOUNCE_TOKEN`, GitLab-shaped value |

**Recommendation.** A single forge-neutral env var `OCX_ANNOUNCE_TOKEN`, resolved from the
publisher's CI/org secrets, forge selected by the target host. **Explicit non-goal
(hard):** the announce token **never** enters ocx's docker-style credential store
(`auth/store.rs`) — that store is OCI-registry auth. The announce credential is a CI/org
secret in the *publisher's own pipeline*, read from ambient env only, exactly as ADR-6's
threat model requires (no long-lived, publisher-held, index-scoped secret anywhere). This
inherits ADR-6 FP-7 unchanged. Fork auto-create (owner-proposed) needs the token to carry
repo-creation capability; if absent, fail closed with an actionable message rather than a
partial push.

### D4 — Repo placement and versioning of the reusable CI units

| Option | Pros | Cons |
|---|---|---|
| **(A) Fold the GitHub reusable workflow into `ocx-sh/setup-ocx`; GitLab Component in a dedicated GitLab project** | Reuses `setup-ocx`'s existing release train + SHA-pin consumer convention; the announce workflow is a thin sibling of "install ocx"; only the GitLab side needs a new home (its catalogue is GitLab-side by necessity) | `setup-ocx` grows a second responsibility (install + announce) |
| **(B) Dedicated repos per forge (`ocx-sh/announce-action` + a GitLab component project)** | Clean single-responsibility; independent versioning | Two more repos to stand up and release; follows the setup-ocx *split* precedent but at higher overhead |
| **(C) Embed the CI YAML in the `ocx` repo (`ocx-sh/ocx/.github/`)** | Co-located with the CLI | Mixes the tool with its distribution; forces an ocx release to ship consumer-CI changes; the index would then be reaching into ocx for CI shape — coupling ADR-6 worked to remove |

**Recommendation: (A), leaning lazy.** Put the GitHub reusable workflow in
`ocx-sh/setup-ocx` (already the canonical "get ocx into CI" surface, already SHA-pinned by
consumers per `subsystem-ci.md`, already versioned) and stand up the GitLab CI/CD Component
in a dedicated GitLab project (a GitLab catalogue entry *must* live GitLab-side). Version
semver, consumers SHA-pin per `subsystem-ci.md`. **Reject (C)** — the index is the
*target*; coupling the publisher tool into either the index repo or the ocx CLI repo
re-creates a cross-repo coupling ADR-6 removed. This placement is **reversible** (not a
one-way door); whether to spend a whole new repo vs. fold into `setup-ocx` is an owner
call, flagged below.

### D5 — Layering in ocx and relation to the index-client types (PR #217 / `feat/index-indirection`)

**Where the orchestration lives.** Per "lib hosts substance, CLI thin," the announce
pipeline lives in `ocx_lib` (a new `announce` module, or under `oci/index/`); the CLI
`command/package_announce.rs` is a thin 4-step command wrapper (transform → lib call →
build report from the return value → `report_announce()`), and `ocx-mirror` drives the
*same* library routine after its push (ocx#216 "Also needed for ocx-mirror").

**What announce actually needs — and, critically, what it does NOT.**

- **Reads the committed root via the forge raw-file/contents API at the default branch**,
  **not** via the sparse-index HTTP client (`index.ocx.sh` / `IndexSource`). Rationale:
  announce needs the exact committed bytes on the index repo's `main` for merge-semantics
  and canonical byte-comparison, and it *edits that repo* — the served static site may lag
  deploy and is read-only. The Python reference confirms this: it reads the root via
  `GitHubPort.get_file_contents(path, ref="main")`, never via `index.ocx.sh`. **Therefore
  announce does not depend on the read-only `IndexImpl`/`IndexSource` at all.**
- **Observes registry tags and builds observation objects** by reusing `Publisher` /
  `oci::Client` (`list_tags`, `fetch_manifest`) — present on `main` today.
- **Needs the wire types + canonical serializer** (`IndexRoot`/`RootTag`/`Observation`/
  `ObservationPlatform` and the byte-exact root/observation serialization). These currently
  live in `oci/index/index_source.rs` **on the unmerged `feat/index-indirection` branch**.
  This is the real dependency edge.

**Dependency-order recommendation.** The canonical serializer + wire types are needed by
both the read path (`IndexSource`) and the write path (announce). Extract them into a
shared, forge-agnostic `oci/index/wire.rs` (single source of truth — ADR-6 FP-4's "one
serializer, one code path" discipline applied on the Rust side) that both the index client
and announce depend on. Sequencing: either (a) `feat/index-indirection` lands first and
announce imports the extracted `wire.rs`, or (b) announce extracts `wire.rs` from the
branch ahead of the merge. Planning against unmerged code is a real risk — **flagged as an
open sequencing item.** Announce does **not** need PR #217's `IndexConfig` read plumbing,
`ChainedIndex`, or `SnapshotStore`.

**New config field (genuinely additive).** `IndexConfig` on the branch carries only `url`
(the served `index.ocx.sh` endpoint). Announce needs the index's **source-repo**
coordinate (owner/repo for the forge), which is distinct from the served URL. Add a
forge-neutral field to `[indices."<ns>"]`, e.g. `repository = "github:ocx-sh/index"`, with
the forge inferred from the prefix. This is the one schema addition; it must ship
forge-neutral (D2) and is compat-bound once a `system_locked` tier carries it.

**Recommendation: D5** — announce orchestration in `ocx_lib`; depends on a new forge REST
client (D2) + `Publisher` (main) + an extracted shared `wire.rs` canonical serializer;
independent of the sparse-index read path; adds one forge-neutral index source-repo config
coordinate. The Rust serializer is **conformance-tested against the index repo's golden
fixtures** (CONTRACTS §14 is the byte authority).

## Decision Outcome

**Chosen shape (Proposed):** a self-contained `ocx package announce` in `ocx_lib` (D5),
GitHub-only in v1 behind a forge-neutral user surface (D2), authenticated by a
publisher-held forge token in ambient CI env and never in ocx's credential store (D3),
wrapped by a thin GitHub reusable workflow + GitLab CI/CD Component (D1) placed to reuse
the `setup-ocx` release train where possible (D4). The command ports — never redefines —
ADR-6's contract and byte-exact serializer (FP-4/FP-9, CONTRACTS §12/§14).

### Consequences

**Positive:**
- BCR-grade UX on both GitHub Actions and GitLab CI, with zero index-side credential and
  zero hosted service — ADR-6 FP-7 inherited intact.
- The expensive one-way door (a GitLab-hosted index) stays open at near-zero cost: the
  surface is neutral, the GitLab impl is a later additive module.
- One canonical serializer shared by read and write paths, conformance-gated against index
  golden fixtures — the single correctness surface is testable, not merely described.

**Negative:**
- Two CI-wrapper artefacts (GitHub + GitLab) to maintain.
- A real dependency on `feat/index-indirection`'s wire types (or an up-front extraction).
- Byte-exact serializer drift = every honest publisher PR fails index CI; the conformance
  test is not optional.

**Risks:**
- **Serializer drift** (ADR-6's own top risk, mirrored client-side). Mitigation: extract a
  single `wire.rs`, conformance-test against the index golden fixtures in CI.
- **Surface leak.** A single `--github-*` flag or github-only config key silently slams the
  GitLab door. Mitigation: review every public string for forge-neutrality before v1 lock.
- **Additive `--tags-file` divergence** from the replace-only reference impl (below).

## Non-Functional Requirements

- **Security:** inherits ADR-6's zero-index-credential model wholesale (D3). Publisher
  holds only their own forge token, ambient CI env only, never in `auth/store.rs`. Fork
  auto-create fails closed if the token lacks capability. Untrusted-input concerns are
  server-side (ADR-6 FP-7) — the client only *produces* a PR.
- **Operability / idempotency:** open-PR **dedupe and update-in-place** — mirror the Python
  `open_or_update_pull_request` (open-or-update, hidden HTML marker) and a stable branch
  convention (`ocx-announce-<ns>-<pkg>`). Copy Homebrew's `check_for_duplicate_pull_requests`;
  avoid winget's missing-dedupe gap (#32738). Re-announce is idempotent: unchanged tags
  keep their `observed` timestamp (ocx#216 item 3), so a second run yields an empty diff.
  Structured failure surfacing from day one (the publish-to-bcr lesson) via the
  `DataInterface` announce report + `UserInterface` diagnostics.
- **Latency:** irrelevant (explicitly out of scope).

## Technical Details

### Architecture

```
publisher CI (GitHub Actions | GitLab CI)
  └─ CI unit (D1): reusable workflow / CI/CD Component
       setup-ocx (pinned) ─▶ ocx package announce --package … --tags[-file] … --fork <owner/repo>
                                    │  reads OCX_ANNOUNCE_TOKEN from ambient env (D3)
                                    ▼
                              ocx_lib::announce  (D5)
                                ├─ Publisher/oci::Client  → observe curated tags, build CAS bytes
                                ├─ wire.rs (shared)        → canonical root+observation serialize (FP-4)
                                └─ forge REST client (D2, GitHub v1; test-seam trait)
                                     get root @ main (contents API, NOT index.ocx.sh)
                                     merge tags → commit_files to fork branch
                                     open_or_update_pull_request (dedupe, head_owner=fork)
                                    ▼
                       ordinary fork PR against ocx-sh/index  →  ADR-6 index CI (out of scope)
```

### CLI / config / contract surface (all forge-neutral — D2)

```
ocx package announce --package <id> (--tags <list> | --tags-file <path>)
                     (--out <dir> | --fork <owner/repo>) [--index-repo <owner/repo>]
                     [--yank <tag> | --unyank <tag>] [--yank-reason <text>]
ocx package push … --announce-file <path>     # appends primary+cascade tags (comma/newline)

[indices."<ns>"]
url        = "https://index.ocx.sh"      # served sparse index (existing, branch)
repository = "github:ocx-sh/index"       # NEW: forge-neutral source-repo coordinate (D5)
```

Wire/serializer contract: index-repo `bot/CONTRACTS.md` §14 (root: `indent=2`,
insertion-order, `ensure_ascii`, single trailing `\n`; observation: minified, sorted keys,
no newline, tuple platform-sort). Rust port conformance-tested against index golden
fixtures.

## Implementation Plan

1. [ ] Extract shared `oci/index/wire.rs` (wire types + canonical serializer) — single
   source of truth; resolve the `feat/index-indirection` sequencing (owner item).
2. [ ] Forge REST client (GitHub) in `ocx_lib` behind a test-seam trait: `get_file_contents`,
   `get_ref_sha`, `commit_files`, `open_or_update_pull_request` (dedupe via marker + branch
   convention) — mirror the `GitHubPort` surface, no local git.
3. [ ] `ocx_lib::announce` orchestration (observe curated tags → merge into committed root →
   build CAS bytes → fork/commit/PR), reused by `ocx-mirror`.
4. [ ] Thin `command/package_announce.rs` + `api/data/announce.rs` report; `--announce-file`
   append on `package_push`.
5. [ ] Forge-neutral `[indices."<ns>"].repository` config field + host-inferred forge.
6. [ ] Conformance tests against index golden fixtures; idempotency + dedupe tests.
7. [ ] CI units (D1): GitHub reusable workflow in `setup-ocx`; GitLab CI/CD Component (D4).
8. [ ] Docs: publisher how-to (GitHub Actions + GitLab CI), token setup, fork prerequisite.

## Validation

- [ ] Rust serializer byte-matches index golden fixtures (root + observation).
- [ ] Idempotent re-announce yields empty diff; open-PR update-in-place, no duplicate PRs.
- [ ] Ambient-token rejection path verified (GITHUB_TOKEN warns, does not silently proceed).
- [ ] Security review of the token boundary (never in `auth/store.rs`).
- [ ] E2E against the ADR-6 sandbox topology (fork PR → real index CI → auto-merge).

## Ratified Owner Decisions (2026-07-18/19)

- **Additive `--tags-file` union** — ratified. File tags union with the committed root;
  deletion only via explicit full-replace `--tags`. Documented deviation from the
  replace-only reference `regenerate()`; conformance tests cover both modes.
- **Unchanged ⇒ no-op** (2026-07-18) — byte-identical root + no new CAS files skips commit
  and PR entirely; exit 0, report `status: "unchanged"`. Reference bot gains the same
  short-circuit (index-side handover item 2).
- **Fork auto-create** — ratified yes. Idempotent ensure-fork via API; classic-PAT scope
  covers it.
- **Sequencing** — land `feat/index-indirection` (PR #217) first; announce branches from a
  main that already contains the wire types/serializer. No pre-extraction of `wire.rs`.
- **GitLab-hosted index** — confirmed real future track. v1 code GitHub-only, but every
  user-facing surface (flags, config keys, `OCX_ANNOUNCE_TOKEN`, CI-unit inputs) stays
  forge-neutral; enforced in review.
- **E2E topology** (2026-07-19) — live against the real index: `michael-herwig/ocx-e2e-publisher`
  (real Rust app, dev-channel ocx) → fork `michael-herwig/index` (created, parent verified) →
  `ocx-sh/index`. Sandbox pair deleted. See index-repo
  `handover_announce_alignment.md`.

## Open Items Needing the Owner

- **CI-unit repo placement (D4):** fold the GitHub workflow into `setup-ocx` vs. a dedicated
  `announce-action` repo (reversible; owner preference; decision needed only when the
  CI-unit phase starts).
- **desc-blob authoring surface** — index-side gap, tracked in the index handover.
- **Multi-root announce PRs** — client refuses vs. supported mode.

## Links

- [ADR-6 — Fork-PR Announce Lane](../../index/.claude/artifacts/adr_fork_pr_announce.md)
  (FP-1..FP-9, G-19/G-20) — the server-side design authority this ADR ports
- [index `bot/CONTRACTS.md`](../../index/bot/CONTRACTS.md) §12 (announce entry), §14
  (client-facing byte-exact root serializer)
- [ocx#216](https://github.com/ocx-sh/ocx/issues/216) — Rust `ocx package announce` tracking
- [`adr_public_index_registry_indirection.md`](./adr_public_index_registry_indirection.md)
  — D4 announce-doorbell (superseded by ADR-6), D5 merge policy
- [`handover_index_indirection.md`](./handover_index_indirection.md) — parked client track
- [publish-to-bcr](https://github.com/bazel-contrib/publish-to-bcr) /
  [#157](https://github.com/bazel-contrib/publish-to-bcr/issues/157) /
  [#262](https://github.com/bazel-contrib/publish-to-bcr/issues/262) /
  [github/roadmap#600](https://github.com/github/roadmap/issues/600) — App→workflow
  migration, fine-grained-PAT / public-repo limits
- [winget-create](https://github.com/microsoft/winget-create) /
  [winget-pkgs#32738](https://github.com/microsoft/winget-pkgs/issues/32738) (dedupe gap),
  [Homebrew bump-formula-pr](https://github.com/Homebrew/brew/blob/master/Library/Homebrew/dev-cmd/bump-formula-pr.rb)
- [GitLab CI/CD Components](https://docs.gitlab.com/ci/components/),
  [CI job token limits](https://docs.gitlab.com/ci/jobs/ci_job_token/),
  [Renovate platform](https://docs.renovatebot.com/modules/platform/) /
  [go-scm](https://github.com/drone/go-scm) — forge-abstraction prior art

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-18 | Michael Herwig + Claude design swarm | Initial Proposed draft: publisher-side announce integration surface — CI-unit shape (D1), GitHub-only-behind-neutral-surface forge posture (D2), token model (D3), CI-unit placement (D4), ocx_lib layering + wire-serializer dependency (D5); NFRs; open items for owner |
| 2026-07-19 | Michael Herwig (owner) | Accepted. Ratified: additive `--tags-file` union, unchanged⇒no-op, fork auto-create, land-#217-first sequencing, GitLab-hosted index as real future track (forge-neutral line enforced), live-real-index E2E topology. D4 placement deferred to CI-unit phase. |
