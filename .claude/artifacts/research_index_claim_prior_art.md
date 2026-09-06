# Research: index claim command — prior art

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04 (re-verify: GitLab job-token scope and push-option availability move per minor release)
- Author: hex-discuss prior-art lane (researcher, sonnet, web); persisted by the orchestrator — the worker had no write tool
- Discussion: `.agents/discussions/index-claim-command.md`
- Freshness: sources flagged inline where older than ~18 months

## Direct answer

Two incompatible claim models exist, not a spectrum: registries with automated artifact validation (npm, crates.io, Terraform/OpenTofu, Go modules) treat **first publish itself as the claim** — no PR, no human review, ownership resolved via API afterward. Registries whose artifacts need human judgment (winget, Homebrew, conda-forge, vcpkg, Scoop) gate the claim on a **PR merged by a human reviewer**, with bots doing only the fork/branch/push/PR mechanics. Cross-forge PR automation is **not unifiable at the git-push layer**: GitHub has no push-option equivalent to GitLab's `git push -o merge_request.create` (GitLab ≥ 11.10) and always needs a second REST call; Gerrit is a third model (push to `refs/for/<branch>`, no PR object). CI identity is forge-native and asymmetric: GitHub exposes `GITHUB_ACTOR`/`GITHUB_ACTOR_ID`, GitLab `GITLAB_USER_LOGIN`/`GITLAB_USER_ID` — neither needs an API call, both pair a stable numeric id with a renameable login, matching crates.io's `gh_id`/`gh_login`. No forge-neutral human-identity schema in the wild carries a provider+id+login triple; SBOM/provenance standards (CycloneDX, SPDX, SLSA, Sigstore) use name+email, opaque builder ids, or workflow-scoped OIDC subjects. The one precedent for id+login+avatar together (all-contributors) has no provider field because it is GitHub-only — the shape ocx already has.

## Trends

- **Rising**: OIDC Trusted Publishing displacing long-lived publish tokens — PyPI (live), npm (GA July 2025), crates.io (RFC 3691, implemented July 2025); GitLab's `CI_JOB_TOKEN` git push reached GA in GitLab 18.4.
- **Established**: fork→branch→PR bot pattern for any claim needing human review (winget, conda-forge, vcpkg, Homebrew, Scoop) — forge-agnostic in shape, forge-specific in implementation; GitLab push options for MR creation (stable since 2019); numeric-id-as-authorization-key + login-as-label (crates.io, GitHub API, all-contributors).
- **Emerging**: ForgeFed/ActivityPub federation (Forgejo) — experimental, cross-instance identity by exact email match, no id mapping; SPIFFE `spiffe://trust-domain/path` for workloads, not people.
- **Declining**: Gerrit's magic-ref model; long-lived PATs for publish auth.

## Key findings

### 1. Claim/registration UX per tool

- winget: `wingetcreate` forks `microsoft/winget-pkgs`, branches, opens a PR via Octokit; needs a PAT (or autonomous mode for CI). Azure Pipelines validation plus a human moderator's approval trigger a `Moderator-Approved` label that auto-merges. [winget-create](https://github.com/microsoft/winget-create), [Moderation.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Moderation.md), [auth](https://deepwiki.com/microsoft/winget-create/2.2-authentication-setup)
- Homebrew: `brew bump-formula-pr` does fork+commit+push+PR in one command; same-repo (no-fork) PRs for collaborators. [docs.brew.sh](https://docs.brew.sh/How-To-Open-a-Homebrew-Pull-Request), [bump-formula-pr.rb](https://github.com/Homebrew/brew/blob/master/Library/Homebrew/dev-cmd/bump-formula-pr.rb), [PR #6723](https://github.com/Homebrew/brew/pull/6723/files)
- conda-forge: new package = PR into `staged-recipes`; CI builds/lints; a bot pings the language team (first-timers cannot ping teams); merge triggers an hourly `create_feedstocks` job — the PR merge **is** the namespace claim. [README](https://github.com/conda-forge/staged-recipes/blob/main/README.md), [life cycle](https://conda-forge.org/docs/maintainer/understanding_conda_forge/life_cycle/)
- nixpkgs: `r-ryantm`/`nixpkgs-update` automate version bumps only, not new-namespace claims. [wiki](https://wiki.nixos.org/wiki/Nixpkgs/Automatic_Updates), [update-bot](https://nix-community.org/update-bot/)
- crates.io / npm / Terraform-OpenTofu: no PR — `cargo publish`/`npm publish` is the claim; ownership via `cargo owner --add <login>` (resolved server-side to `gh_id`+`gh_login`) or `npm owner add`. [cargo-owner](https://doc.rust-lang.org/cargo/commands/cargo-owner.html), [crates.io #765](https://github.com/rust-lang/crates.io/issues/765), [npm-owner](https://docs.npmjs.com/cli/owner), [npm scope](https://docs.npmjs.com/using-npm/scope.html)
- Terraform/OpenTofu: no claim step — the GitHub org/user login **is** the namespace, assigned on OAuth login, detected via webhook. [protocol](https://opentofu.org/docs/internals/provider-registry-protocol/), [blog](https://opentofu.org/blog/building-the-opentofu-registry/)
- Squatting: npm's ToS bans reserving names with no genuine use; Wasmer bans squatting without an enforcement definition — inconsistent, mostly unenforced. [npm disputes](https://docs.npmjs.com/policies/disputes/), [Wasmer](https://wasmer.io/policies/disputes)
- Re-run/idempotency: Homebrew's no-fork PRs and winget-create's update commands imply in-place updates; no primary source states "detects and updates an already-open PR" (gap).

### 2. Cross-forge PR/MR automation

- GitHub: always push a branch, then `POST /repos/{owner}/{repo}/pulls`; no create-PR-on-push flag. [PR REST](https://docs.github.com/en/rest/pulls/pulls)
- GitLab: `git push -o merge_request.create` (+ `.target=`, `.title=`, `.description=`, `.merge_when_pipeline_succeeds`, `.remove_source_branch`) — GitLab 11.10 (2019). [push_options.md](https://github.com/diffblue/gitlab/blob/master/doc/user/project/push_options.md), [MR !26752](https://gitlab.com/gitlab-org/gitlab-foss/-/merge_requests/26752)
- Gerrit: push to `refs/for/<branch>`; no PR object; `Change-Id` trailer. [docs](https://gerrit-review.googlesource.com/Documentation/user-upload.html)
- GitHub CI identity: `GITHUB_TOKEN` is a GitHub App installation token scoped to the workflow's repo, ≤24h, does not trigger downstream workflows; a separate App installation token has its own bot identity, 1h lifetime. [guide](https://michaelheap.com/ultimate-guide-github-actions-authentication/), [About GITHUB_TOKEN](https://docs.github.com/en/actions/automating-your-workflow-with-github-actions/authenticating-with-the-github_token)
- GitLab `CI_JOB_TOKEN`: scoped to the originating project by default; cross-project API calls require the target project's allowlist (max 200 groups + 200 projects); git push via job token GA in GitLab 18.4 — older instances need a PAT/deploy token. [CI_JOB_TOKEN](https://docs.gitlab.com/ci/jobs/ci_job_token/), [issue 383084](https://gitlab.com/gitlab-org/gitlab/-/issues/383084)
- GHES vs github.com: base URL `https://HOST/api/v3`, own release cadence and versioned docs; API surface lags. [GHES REST](https://docs.github.com/en/enterprise-server@3.5/rest/guides/getting-started-with-the-rest-api)
- Self-managed GitLab vs GitLab.com: push options identical; self-managed admins get instance-level overrides (e.g. enforcing the job-token allowlist). [CI_JOB_TOKEN](https://docs.gitlab.com/ci/jobs/ci_job_token/)
- Forge-kind detection: no auto-probing precedent. Renovate requires explicit `platform:` + `endpoint:`. [Gitea platform](https://docs.renovatebot.com/modules/platform/gitea/), [GitHub platform](https://docs.renovatebot.com/modules/platform/github/)

### 3. Identity in CI and via API

- GitHub Actions: `GITHUB_ACTOR` (login) and `GITHUB_ACTOR_ID` (numeric) env vars; `GITHUB_ACTOR` may be a non-human default actor on scheduled workflows. [variables](https://docs.github.com/en/actions/reference/workflows-and-actions/variables), [discussion #50250](https://github.com/orgs/community/discussions/50250)
- GitLab CI: `GITLAB_USER_ID`, `GITLAB_USER_LOGIN`, `GITLAB_USER_NAME`, `GITLAB_USER_EMAIL` — `USER_ID`/`USER_EMAIL` since 8.12 (2016), `USER_LOGIN`/`USER_NAME` later. [predefined variables (mirror)](https://sels.tecnico.ulisboa.pt/gitlab/help/ci/variables/predefined_variables.md), [issue #26692](https://gitlab.com/gitlab-org/gitlab-foss/-/issues/26692)
- Login→id: `GET /users/{username}` (GitHub, callable with `GITHUB_TOKEN`); GitLab Users API `username` filter (job token scope-restricted; env vars are the documented route for the acting user). [GitHub users API](https://docs.github.com/en/rest/users/users)
- No primary source found stating "id survives a username rename" for either forge — implied by every id/login pairing (gap).

### 4. Forge-neutral identity schemas

- Sigstore/Fulcio: OIDC issuer + raw `sub` (e.g. `repo:org/repo:ref:refs/heads/main`) in the cert SAN — a workflow invocation, not a person. [OIDC in Fulcio](https://docs.sigstore.dev/certificate_authority/oidc-in-fulcio/), [oid-info](https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md)
- SLSA `builder.id`: opaque string. [v1.2](https://slsa.dev/spec/v1.2/build-provenance)
- Backstage: owner is a Backstage-internal entity ref (`user:default/login`), not a forge triple — evidence against. [relations](https://backstage.io/docs/features/software-catalog/well-known-relations/)
- CycloneDX/SPDX: name+email(+URL) contacts; no provider/host, no numeric id (CycloneDX 1.5 reference; 1.6/1.7 added `metadata.manufacturer` — re-check if load-bearing). [CycloneDX 1.5](https://cyclonedx.org/docs/1.5/xml/), [crosswalk](https://sbomify.com/compliance/schema-crosswalk/)
- all-contributors: `login`+`id`+`avatar_url`+`profile` together, no provider field (GitHub-only); manual entries drop `login`/`id`. [config](https://allcontributors.org/en/bot/configuration/)
- SPIFFE: `spiffe://trust-domain/path` — workload identity. [SPIFFE-ID](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md)
- Forgejo/ForgeFed: cross-instance identity by exact email match, unsolved upstream. [Q&A 2023](https://forgejo.org/2023-01-10-answering-forgejo-federation-questions/) (older than 18 months), [issue #794](https://codeberg.org/forgejo/forgejo/issues/794)

### 5. Deprecation practice

- crates.io index: `"v"` gates additions, never removals — `features2` added alongside `features`; empty fields still emitted for compat. [registry-index](https://doc.rust-lang.org/cargo/reference/registry-index.html)
- PyPI Simple API: `format_version` 1.0→1.4 purely additive; missing marker = 1.0. [PEP 691](https://peps.python.org/pep-0691/), [PEP 700](https://peps.python.org/pep-0700/), [spec](https://packaging.python.org/en/latest/specifications/simple-repository-api/)
- No primary-source example of a package index retiring a field via dual-emit-then-drop; both precedents are additive/version-gated.
- Trusted publishing: [PyPI](https://docs.pypi.org/trusted-publishers/), [npm GA 2025-07](https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/), [RFC 3691](https://rust-lang.github.io/rfcs/3691-trusted-publishing-cratesio.html)

## Orchestrator addendum — job-token read scope (fact check, 2026-09-04)

Fetched https://docs.gitlab.com/ci/jobs/ci_job_token/ directly. A `CI_JOB_TOKEN` can call these **read-only** endpoints: Branches API (`GET /projects/:id/repository/branches`), Commits API (`GET …/commits/:sha`, `…/commits/:sha/merge_requests`, `…/commits/:sha/refs`), Files API (`GET …/repository/files/:file_path/raw`), Merge requests API (`GET /projects/:id/merge_requests`, `GET …/merge_requests/:iid`), Tags API. Not listed: repository compare, file/commit writes, forks, MR create. Git push with a job token: introduced 17.2 behind `allow_push_repository_for_job_token`, GA 18.4; "the job token has the same access permissions as the user who started the job"; no pipeline is triggered by a job-token push; cross-project push introduced 19.0, GA 19.1. Allowlist: a project's allowlist includes only itself by default; caps 200 groups + 200 projects, counted separately. This corrects ocx's `forge/gitlab.rs:148-157` comment and `website/src/docs/reference/environment.md:128`, which state the job token reaches none of files/commits/branches/MRs — true for writes, false for reads.

## negative:

- No single claim API generalises: first-publish-is-claim vs PR-gated-claim are two architectures, correlated with whether the artifact is auto-validatable.
- No forge auto-detects its kind from an arbitrary host (Renovate: explicit `platform` + `endpoint`).
- GitHub has no push-option equivalent to GitLab's `-o merge_request.create` — no uniform "open PR" primitive at the git-push layer.
- `CI_JOB_TOKEN` could not push until GitLab 18.4 GA — symmetric "bot pushes via job token" assumptions break on older self-managed instances.
- Provenance/SBOM schemas avoid a forge+id+login triple; the only matching precedent (all-contributors) has no provider field because it is single-forge — argues against adding host/provider ahead of a real second-forge need.
- No package-index precedent for removing a wire field via dual-emit-then-drop.
- Terraform/OpenTofu skips the claim artifact entirely (namespace = OAuth identity).
- Re-run/idempotency behaviour not confirmable from primary sources.

## leads:

- forge lane — GitHub (push+REST) and GitLab (push -o) are separate code paths; no shared primitive upstream.
- adr lane — the "batched-window carve-out" deprecation pattern has no package-index precedent; frame it as an ocx-original choice.
- identity-schema — adding host/provider to `owners[]` contradicts the only precedent found.
- recon-claim — first-publish-is-claim vs PR-gated-claim is a binary architecture choice.
- ci-identity — job-token git push needs a documented GitLab minimum version or a PAT fallback.
