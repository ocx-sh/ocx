# Review: adr_index_claim_command — SOTA gap check

Summary: gaps found

## Gaps

1. **Server-side push-option parsing is a live 2026 RCE class, and the ADR's threat model doesn't
   cross-reference it.** CVE-2026-3854 (GitHub Enterprise Server, disclosed 2026-03-04, patched
   same day, published 2026-04-28): "user-supplied git push options were not properly sanitized
   and were embedded into internal service metadata headers using a delimiter character,"
   letting a pusher "inject additional metadata fields," "override the environment the push was
   processed in," "bypass sandboxing," and reach "arbitrary command execution" on the server —
   [GitHub Security blog](https://github.blog/security/securing-the-git-push-pipeline-responding-to-a-critical-remote-code-execution-vulnerability/),
   [SecurityAffairs summary](https://securityaffairs.com/191434/security/cve-2026-3854-github-flaw-enables-remote-code-execution.html)
   (fetched 2026-09-05). The vulnerable component is GHES's own push pipeline, not GitLab, and
   GitHub has no `-o merge_request.*`-equivalent so ocx never sends push options there — but the
   *shape* of the bug (an operator-influenced string value — here, MR title/description built
   from `--owner`/namespace/package input — carried as a push-option value into a server-side
   metadata parser) is exactly the mechanism `D-T4`'s `git push -o merge_request.title=<title> -o
   merge_request.description=<body>` uses against GitLab. The ADR's Security Architecture §3
   ("Push-option and request-body content") frames this risk only as "ocx must not inject
   attacker content into the request" (outbound direction); it does not address the orthogonal
   risk that GitLab's own push-option value parser is the kind of code that just had a critical,
   same-shape defect in a sibling product, six months before this ADR's date. Recommend: check
   whether GitLab has ever patched a similar push-option delimiter-injection class in
   `gitlab-shell`/Gitaly, and treat the character set going into `-o merge_request.title=`/
   `.description=` as passing into an external, security-sensitive parser — worth one explicit
   sentence in the ADR rather than silence. **Severity: High. Actionable** — cheap to add a
   sentence and a `\n`/NUL/control-character rejection on the rendered title/description before
   they reach the push-option string (belt-and-suspenders; GitLab's parser is not ocx's to fix,
   but ocx controls what it sends).

2. **GitLab's HTTPS partial-clone (`uploadpack.allowFilter`) support has a bumpy, feature-flag-gated
   rollout history that the ADR's git recipe doesn't account for.** `--filter=blob:none` is
   load-bearing for `D-T5`'s "compare_branch is computed, not approximated" invariant. GitLab's
   own Gitaly issue tracker shows the capability was feature-flagged (`gitaly_upload_pack_filter`)
   and, even once nominally enabled, had follow-on defects — "Enabling the
   `gitaly_upload_pack_filter` feature flag isn't enough for Partial clone"
   ([gitaly#2510](https://gitlab.com/gitlab-org/gitaly/-/issues/2510)) — and a still-open ask to
   make blob filtering the *default* rather than opt-in
   ([gitaly#2553](https://gitlab.com/gitlab-org/gitaly/-/issues/2553)), referenced as recently as
   [Git Rev News #127, 2025-09-30](https://git.github.io/rev_news/2025/09/30/edition-127/). No
   source found pins down whether every currently-supported self-managed GitLab version enables
   filtering over HTTPS (vs. SSH-only, which an early Gitaly MR text explicitly scoped:
   "this change only affects SSH, not HTTP" —
   [gitaly#1553](https://gitlab.com/gitlab-org/gitaly/-/issues/1553), pre-dates current GA state
   and must be re-verified, not relied on as still true). This directly bears on the ADR's own
   admission that clone size and behavior against `ocx-sh/index` are "unmeasured" (NFR table) —
   the unmeasured gap is larger than "how big," it may include "does the filter even apply on our
   self-managed target," and a server that silently ignores an unsupported filter (rather than
   erroring) would make `compare_branch` correct but silently defeat the cost rationale for
   choosing `git` over full clone. **Severity: Warn. Actionable** — fold into the existing
   pre-0.6.1 validation gate: explicitly test partial-clone fetch against a self-managed GitLab
   instance (not just gitlab.com), not only measure the resulting size.

3. **The design's D-T4 seam does not generalize to a third forge the way "confined to
   `GitLabForge`'s write half" implies, and Gitea/Forgejo are two different problems, not one.**
   Gitea supports push-option-driven PR creation via AGit: `-o topic=<topic>`, `-o force-push`,
   pushed to the *ordinary* branch ref — mechanically close to GitLab's model (branch push +
   `-o` options) — [Gitea AGit docs](https://docs.gitea.com/usage/issues-prs/agit/) (current).
   Forgejo's AGit-Flow is a **different wire convention**: the client pushes to a *special ref
   namespace*, `refs/for/<target-branch>/<topic>`, not the ordinary branch — push options there
   supply metadata (title, etc.) but the ref-target itself carries the "this is a PR, not a
   branch update" signal — [Forgejo AGit-Flow docs](https://forgejo.org/docs/latest/user/agit-support/)
   (current). Bitbucket Server/Data Center has no push-driven PR-creation mechanism at all
   (confirms the existing tooling research's negative finding — REST-only, like GitHub). This
   means a hypothetical `GiteaForge` could reuse `git_workspace.rs`'s "push to a branch with
   options" shape almost as-is, but a hypothetical `ForgejoForge` could not — it needs the
   refspec itself to change (`HEAD:refs/for/<base>/<topic>` instead of
   `<branch>:refs/heads/<branch>`), which `git_workspace.rs` as specified (one push call, target
   ref = the branch) does not parametrize. The ADR's Consequences section correctly scopes
   `--transport git` to GitLab only and states no shared cross-forge push abstraction exists
   (confirmed, see Confirmations) — but neither the ADR nor the system design says the *seam
   itself* would need a refspec-shape parameter, not just a new `ForgeKind` arm, before a third
   forge could reuse it. Worth one sentence for whoever inherits this code next. **Severity:
   Warn. Deferred** — no action needed before 0.6.1 (GitHub/GitLab-only is explicitly in scope),
   but the system design's "Why the transport lives inside the forge container" rationale should
   not be read as "adding forge N+1 is just another enum arm" without this caveat.

4. **OIDC-verified CI identity is a documented, already-GA mechanism the ADR treats as a future
   trend rather than something usable today for the exact "weak form" gap it names.** GitHub's
   Actions OIDC token has carried a documented `actor_id` claim — "the ID of the personal account
   that initiated the workflow run" — since the January 2023 custom-claims rollout
   ([GitHub OIDC reference](https://docs.github.com/en/actions/reference/security/oidc); claim
   list confirmed live at `https://token.actions.githubusercontent.com/.well-known/openid-configuration`).
   This is a *cryptographically verifiable*, GitHub-signed assertion of the triggering account —
   materially stronger than the ADR's D-C4 arm 2 ("CI environment... a name-shape heuristic; the
   weak form"), which trusts the bare `GITHUB_ACTOR`/`GITHUB_ACTOR_ID` environment strings with no
   verification at all. The ADR's Industry Context correctly notes GitHub's 2026-04-23 immutable
   *subject*-claim change as "adjacent, not directly applicable" to job authorship — but
   undersells it: `actor_id` (not the newer immutable-subject work) has been sitting there,
   requestable with a plain `id-token: write` permission, since 2023, and directly answers the
   exact question the security research flagged as a gap ("no CI-native human identity on
   GitHub"). It does not solve the manual/shared-PAT impersonation case (out of scope by design,
   per the ADR's own admission), and consuming it requires a verifier (JWKS fetch + signature
   check) that lives on indexbot's side, not ocx's, since ocx has no forge-write role in
   verification — this is squarely the kind of thing that belongs next to the ADR's already-
   deferred "GitHub owner-adds-owners / invoker-identity" indexbot-governance item, not something
   this ADR should implement. **Severity: Warn. Deferred** — recommend the deferred-items list
   name `actor_id` explicitly (not just "immutable subject claims") as the concrete verifiable
   claim indexbot could check, so the next person picking up that governance question does not
   re-derive this from scratch.

5. **OIDC trusted publishing's actual pattern (PyPI/npm/crates.io) is "exchange the ID token for
   a scoped credential," not "the write transport implies identity" — a materially different, and
   more general, answer to the ADR's core authorship problem than either GitLab's job-token push
   or GitHub's `GITHUB_TOKEN` gives.** All three registries follow the same shape: "the workflow
   requests a short-lived OIDC identity token from GitHub [or GitLab/CircleCI], presents it to the
   registry, and the registry issues a temporary upload credential scoped to that specific package
   and workflow run" —
   [OpenSSF wg-securing-software-repos overview](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html),
   [npm trusted publishing GA changelog, 2025-07-31](https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/),
   corroborated for crates.io ([Alpha-Omega writeup](https://alpha-omega.dev/blog/trusted-publishing-secure-rust-package-deployment-without-secrets/)).
   Applied to this ADR's problem: rather than deriving "who is claiming this" from *which write
   transport the pipeline happened to use* (job-token git push = the invoking human; REST +
   `OCX_ANNOUNCE_TOKEN` = whoever holds the PAT), an index could accept an OIDC ID token directly
   as the claim's identity proof — decoupling "who authored this" from "how the bytes got
   written" entirely, the same way PyPI decouples "who is allowed to publish this project" from
   "what credential uploaded the file." The existing prior-art research (`research_index_claim_
   prior_art.md`) covers OIDC trusted publishing only as a general ecosystem trend line ("Rising")
   and does not connect it back to *this* ADR's specific authorship-vs-write-credential split,
   which is the one place in the whole design where the connection is most direct. This is
   architecture-level, cross-repo (indexbot, not ocx, would be the OIDC verifier — ocx has no
   forge-write role there and G-04 already requires a human merge regardless), so it changes
   nothing in this ADR's decision, but it is the one trending pattern most worth a forward-pointer
   next to D-T*'s "git transport is a stopgap" framing, since it is the actual SOTA answer to the
   framing question the reviewer was asked to probe. **Severity: High (as a missed framing, not a
   missed implementation). Deferred** — belongs in indexbot's ADR lineage as a named alternative
   to evaluate before the git transport's job-token special-case ossifies into permanent
   plumbing.

## Confirmations

- **GitLab version gates hold through 19.4; nothing in 19.2–19.4 changes the ADR's 17.2/18.4/19.1
  floors.** Directly fetched GitLab 19.0, 19.2, 19.3, and 19.4 release notes
  ([19.0](https://docs.gitlab.com/releases/19/gitlab-19-0-released/),
  [19.2](https://docs.gitlab.com/releases/19/gitlab-19-2-released/),
  [19.3](https://docs.gitlab.com/releases/19/gitlab-19-3-released/),
  [19.4](https://docs.gitlab.com/releases/19/gitlab-19-4-released/)) and the current job-token
  doc ([docs.gitlab.com/ci/jobs/ci_job_token](https://docs.gitlab.com/ci/jobs/ci_job_token/),
  fetched 2026-09-05): job-token git push to the same project GA'd in 18.4 (flag
  `allow_push_repository_for_job_token` removed); cross-project push was introduced in 19.0
  behind `allow_push_to_allowlisted_projects` (disabled by default) and reached GA in 19.1 (flag
  removed) — refines, does not contradict, the existing research's "19.0→19.1" framing. 19.3
  added `CI_JOB_TOKEN`-readable repository archives (Composer-related, irrelevant to this ADR's
  recipe). No push-option grammar change, no `detailed_merge_status` change, and no allowlist
  API-shape change found in any of the three intervening releases. The setting name is still
  exactly `ci_push_repository_for_job_token_allowed` and the allowlist limit is still confirmed
  at 200 groups + 200 projects, counted separately, on the live doc.
- **No cross-forge push-primitive exists, confirmed against a fourth forge.** Adding Bitbucket
  Server/Data Center to the survey finds the same negative the ADR already states for GitHub:
  no push-driven PR-creation mechanism, REST-only. This corroborates D-T2/D-T6's "GitHub keeps
  REST only" and the wider claim that GitLab's `-o merge_request.*` has no cross-forge analog —
  strengthens rather than weakens the ADR's existing position.
- **`--force-with-lease=<branch>:<expected-sha>` (the explicit two-part form the ADR specifies
  for the `RefUpdate::Reset` case) is the correct choice over the bare `--force-with-lease`
  form.** The bare form trusts the local remote-tracking ref, which can be stale if the workspace
  never fetched recently, defeating the lease; the explicit `<expected-sha>` form "requires its
  current value to be the same as the specified value," independent of any tracking-ref state —
  [Atlassian force-with-lease writeup](https://www.atlassian.com/blog/it-teams/force-with-lease),
  [git-push docs](https://git-scm.com/docs/git-push). The ADR already specifies the explicit
  form; this is a confirmation, not a change.
- **`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` fail closed on malformed input**
  ("any missing key or value is treated as an error"; an empty/unset count is equivalent to
  zero pairs, not an error) — no silent-drop failure mode found for the credential-injection
  mechanism the ADR relies on. Confirms D-T7's mechanism has no quiet-failure edge case at the
  git-config layer itself.
- **GitHub's numeric owner id is stated as immutable by a primary GitHub source, beyond the
  existing research's citation.** GitHub's own OIDC immutable-subject documentation states "the
  owner ID and repository ID are assigned once and never reused, so renaming, transferring, or
  recreating the repository doesn't change them" —
  [GitHub OIDC reference](https://docs.github.com/en/actions/reference/security/oidc),
  corroborated by
  [Microsoft Entra migration guidance](https://learn.microsoft.com/en-us/entra/workload-id/workload-identities-github-immutable-subjects).
  This is additional primary-source backing for the security research's `login:id` persistence
  rationale (D-W1/owner-entry retention), not a new requirement.

## negative:

- **A GitHub App per-publisher installation token was investigated as a possible "named,
  stable automation identity" alternative to a shared PAT.** It does give each App its own bot
  user (`<app-slug>[bot]`, its own numeric id), which is a real, if minor, improvement over a
  generic `github-actions[bot]`/shared-PAT story — but it is still a **bot** identity under the
  ADR's own bot-refusal rule (server-asserted `type: "Bot"`), so it does not change who can be
  listed in `owners[]` and does not address the "author = invoker" gap the ADR already scopes to
  GitLab only. No further action warranted; not a gap in the decision.
- **No exit-code precedent was found in `oras`, `cosign`, `gh`, `glab`, or `cargo` that cleanly
  distinguishes "server reachable but lacks a capability, admin must act" from "unavailable,
  retry."** This corroborates — does not contradict — the operability research's own negative
  finding that `ReferrersUnsupported`/the new `ForgeCapabilityUnavailable = 86` has no external
  precedent to borrow from; ocx's internal `ReferrersUnsupported` anchor remains the strongest
  available justification. One data point found in passing (an `oras` GitHub issue where a
  missing-binary case surfaced as an unrelated network-timeout exit code) is a cautionary
  counter-example, not a precedent to follow.
- **All-contributors' `login`+`id`+`avatar_url`+`provider`-absent shape** was re-searched
  directly for a JSON-schema-level precedent beyond what the existing prior-art research already
  cites; nothing further was found. Treat the existing citation as complete — no new identity-
  schema precedent exists beyond what `research_index_claim_prior_art.md` already found.
- **GitLab's HTTP-vs-SSH partial-clone-filter scoping** ("this change only affects SSH, not
  HTTP," from an early Gitaly issue) could not be confirmed as still true or since superseded
  for the current GA state — flagged in Gap 2 as unresolved rather than asserted either way.

## Sources

- [GitHub Security: Securing the git push pipeline (CVE-2026-3854)](https://github.blog/security/securing-the-git-push-pipeline-responding-to-a-critical-remote-code-execution-vulnerability/) — fetched 2026-09-05
- [SecurityAffairs: CVE-2026-3854 summary](https://securityaffairs.com/191434/security/cve-2026-3854-github-flaw-enables-remote-code-execution.html) — fetched 2026-09-05
- [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/) — fetched 2026-09-05
- [GitLab 19.0 release notes](https://docs.gitlab.com/releases/19/gitlab-19-0-released/) — fetched 2026-09-05
- [GitLab 19.2 release notes](https://docs.gitlab.com/releases/19/gitlab-19-2-released/) — fetched 2026-09-05
- [GitLab 19.3 release notes](https://docs.gitlab.com/releases/19/gitlab-19-3-released/) — fetched 2026-09-05
- [GitLab 19.4 release notes](https://docs.gitlab.com/releases/19/gitlab-19-4-released/) — fetched 2026-09-05
- [GitLab Gitaly issue #1553 — SSH-only partial-clone scoping (dated, unverified as current)](https://gitlab.com/gitlab-org/gitaly/-/issues/1553)
- [GitLab Gitaly issue #2510 — feature flag insufficient for partial clone](https://gitlab.com/gitlab-org/gitaly/-/issues/2510)
- [GitLab Gitaly issue #2553 — enable blob filter by default, open](https://gitlab.com/gitlab-org/gitaly/-/issues/2553)
- [Git Rev News Edition 127, 2025-09-30](https://git.github.io/rev_news/2025/09/30/edition-127/)
- [Gitea AGit docs (push options, topic/force-push)](https://docs.gitea.com/usage/issues-prs/agit/)
- [Forgejo AGit-Flow docs (refs/for/ convention)](https://forgejo.org/docs/latest/user/agit-support/)
- [Bitbucket Data Center pull request docs (no push-driven creation)](https://confluence.atlassian.com/bitbucketserver/pull-requests-776639997.html)
- [Atlassian: force-with-lease explained](https://www.atlassian.com/blog/it-teams/force-with-lease)
- [git-push documentation](https://git-scm.com/docs/git-push)
- [git-config documentation (GIT_CONFIG_COUNT semantics)](https://git-scm.com/docs/git-config)
- [GitHub OpenID Connect reference (actor_id claim, immutable owner/repo id)](https://docs.github.com/en/actions/reference/security/oidc)
- [Microsoft Entra: migrate GitHub Actions federated credentials to immutable subjects](https://learn.microsoft.com/en-us/entra/workload-id/workload-identities-github-immutable-subjects)
- [GitHub changelog: immutable subject claims, 2026-04-23](https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens/) (already cited by the ADR; re-verified)
- [OpenSSF: Trusted Publishers for All Package Repositories](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html)
- [GitHub changelog: npm trusted publishing GA, 2025-07-31](https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/)
- [Alpha-Omega: Trusted Publishing on crates.io](https://alpha-omega.dev/blog/trusted-publishing-secure-rust-package-deployment-without-secrets/)
- [GitHub Actions: create-github-app-token action / installation-token bot identity](https://github.com/actions/create-github-app-token)
