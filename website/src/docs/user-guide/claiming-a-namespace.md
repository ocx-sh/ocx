---
outline: deep
---

# Claiming a namespace {#claiming}

You pushed your first package to a registry, wired the publish job, and ran
[`ocx package announce`][cmd-package-announce] to put it in front of people. It exits 79
and says the namespace is unclaimed.

That is the index refusing to let a pipeline invent a name. A public [index][in-depth-indices]
maps `ocx.sh/acme/widget` to a registry repository, and once that mapping exists every
later announce from `acme/*` is a routine update nobody reviews. So the mapping itself is
where a human looks: someone has to judge that whoever is claiming `acme` plausibly *is*
ACME. Announce cannot make that call, and by design it never creates an entry — it only
updates one that exists.

[`ocx package claim`][cmd-package-claim] opens that first request for you. It renders the
entry, resolves the accounts it records as owners, and opens a pull request (GitHub) or a
merge request (GitLab) against the index repository. You do it once per namespace. After it
merges, `announce` works and nobody looks at your releases again.

::: warning A GitLab-sourced claim is a documented prerequisite, not yet a recommendation
The public catalog renders an entry's owner links from GitHub account ids. An entry claimed
from GitLab records GitLab ids, and the catalog currently renders those as github.com
links — pointing at whichever unrelated GitHub account happens to hold that number. The
GitLab recipes below are written and correct on the ocx side; run them against **your own
index repository**, and treat claiming the public `ocx.sh` index from GitLab as blocked
until that catalog issue closes.
:::

## What the entry records {#claiming-entry}

A claim writes one JSON entry into the index repository, at `p/<namespace>/<package>.json`.
Four things go in it, and only two come from you directly:

| Field | Where it comes from | Why it matters |
|---|---|---|
| The logical name | `<namespace>/<package>`, prefixed with the default registry | What a consumer types: `ocx add acme/widget`, `ocx package install acme/widget` |
| The physical repository | `--repository oci://HOST/PATH` | The registry repository every later announce resolves tags against |
| The owners | `--owner`, else the CI environment, else the credential's identity | The governance key — see [below](#claiming-owners) |
| The upstream block | `--upstream-org` and its two optional siblings | Only for a namespace that repackages someone else's software |

The `--repository` pointer names your **registry**, not the index. They are different
systems: the registry holds the bytes, the index holds the mapping. A claim that points at
the index repository would resolve to nothing.

## The command {#claiming-command}

<<< @/_scripts/user-guide/claiming-a-namespace.sh{sh}

<Terminal src="/casts/user-guide/claiming-a-namespace.cast" title="The claim grammar" collapsed />

Flags come before the positional. Every refusal decidable from the command line alone —
mutually exclusive write flags, a malformed `--repository`, an `--upstream-*` flag with no
anchor — is decided before a credential is resolved and before anything is dialled, so a
bad invocation costs you no round trip through a token you did not need.

## Choosing a posture {#claiming-postures}

Four ways to run it, differing in what credential you have and therefore in how the request
gets written.

| Posture | Environment | `--transport` | Request authored by | Minimum version |
|---|---|---|---|---|
| GitHub, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a PAT | `api` (default) | The token's own account | — |
| GitLab, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a personal, project or group token | `api` (default) | The token's own account | — |
| GitLab CI job token | `GITLAB_CI` and `CI_JOB_TOKEN`, set by the runner; no `OCX_ANNOUNCE_TOKEN` | `git` | `GITLAB_USER_LOGIN` / `GITLAB_USER_ID` — the user who triggered the pipeline | git 2.31.0 |
| GitLab, split credential pair | [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] (+ [`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username]) for the push, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] for the reads | `git` | The API token's account | git 2.31.0 |

The version column is the floor ocx enforces itself: under `--transport git` it runs
`git --version` before it constructs the forge and exits 69 below 2.31.0. There is no
GitLab version number ocx checks — instead it asks the instance the two questions that
actually matter (does this project accept job-token pushes, and does its allowlist admit
the publishing project) and exits 86 naming whichever answer is missing.

### Why `--transport git` exists {#claiming-transport}

A [GitLab CI job token][gitlab-job-token] is the credential every job already has, and it is
the one a pipeline should prefer: it is scoped to the job, it expires with it, and nobody has
to store it. It also cannot open a merge request. It reads the API fine — branches, commits,
raw files, merge requests, tags — and it is *read-only* for all of them.

What it can do is push, when the target project allows it. So `--transport git` takes the
other route: clone the index repository into a temporary directory, build the commit there,
and create the merge request from the push itself using [GitLab's merge-request push
options][gitlab-push-options]. One authenticated push, no API write, no stored secret.

That same split is why the credential pair exists. The two halves are asked for different
things and a real deployment often has to answer with different identities:

- A [deploy token][gitlab-deploy-token] can write the repository but has no API surface to
  call, so it can carry the push and nothing else.
- A job token can read the API but cannot open the merge request, so it can carry the reads
  and nothing else.

Set [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] and the push uses it while the
REST reads keep using [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Leave it unset and the
push reuses whatever the API credential resolved to.

### GitHub, from a workflow {#claiming-github}

```yaml
# .github/workflows/claim.yml
name: Claim the namespace
on: workflow_dispatch

jobs:
  claim:
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - env:
          OCX_ANNOUNCE_TOKEN: ${{ secrets.INDEX_PAT }}
        run: |
          ocx package claim \
            --repository oci://ghcr.io/acme/widget \
            --fork acme-bot/index \
            acme/widget
```

`GITHUB_TOKEN` is not a substitute for `INDEX_PAT`: it is scoped to this repository and can
neither fork nor write the index. Omit `--fork` only when the token can push to the index
repository directly; ocx verifies that before it writes anything and exits 80 naming the
repository and the missing permission if it cannot.

### GitLab, with an access token {#claiming-gitlab-token}

```yaml
# .gitlab-ci.yml
claim:
  image: alpine:3
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
  variables:
    OCX_ANNOUNCE_TOKEN: $INDEX_TOKEN     # masked project variable
  script:
    - ocx package claim
        --repository oci://$CI_REGISTRY_IMAGE
        --index-repo gitlab.example.com/acme/index
        acme/widget
```

The plain-API posture, and the one to prefer when you can store a token: no clone, no git
binary, no capability preflight. `--transport` stays at its default.

### GitLab, from a CI job {#claiming-gitlab-job-token}

```yaml
# .gitlab-ci.yml
claim:
  image: alpine:3
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
  script:
    - ocx package claim
        --repository oci://$CI_REGISTRY_IMAGE
        --index-repo gitlab.example.com/acme/index
        --transport git
        --owner "$GITLAB_USER_LOGIN:$GITLAB_USER_ID"
        acme/widget
```

Nothing is stored: `CI_JOB_TOKEN` and `GITLAB_CI` are both set by the runner, and ocx falls
through to the job token when `OCX_ANNOUNCE_TOKEN` is unset or empty. The `--owner` pair is
explicit here on purpose — see [the owners section](#claiming-owners).

This posture needs the index project to have enabled job-token pushes and to have
allowlisted the publishing project. Both are settings on the **index** project, not yours;
if either is missing the run exits 86 naming it, and an administrator there has to act.

### GitLab, with a split credential pair {#claiming-split}

```yaml
# .gitlab-ci.yml
claim:
  image: alpine:3
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
  variables:
    OCX_ANNOUNCE_TOKEN: $INDEX_READ_TOKEN       # masked; API reads
    OCX_ANNOUNCE_GIT_TOKEN: $INDEX_DEPLOY_TOKEN # masked; the push only
    OCX_ANNOUNCE_GIT_USERNAME: acme-index-writer
  script:
    - ocx package claim
        --repository oci://$CI_REGISTRY_IMAGE
        --index-repo gitlab.example.com/acme/index
        --transport git
        acme/widget
```

[`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username] defaults to `gitlab-ci-token`
and only needs setting when the instance expects a specific user for that token. Both
secrets travel as an HTTP Basic header injected through git's own configuration
environment — never in a URL, never in `argv`, never written to `.git/config`.

## Who the entry says owns it {#claiming-owners}

An owner is a `login:id` pair, and the **id** is the load-bearing half. Logins get renamed
and recycled; a numeric account id does not. That is why the entry records both and why
governance keys on the number.

Without `--owner`, ocx works down a short ladder: the CI environment's user variables
(`GITLAB_USER_LOGIN` + `GITLAB_USER_ID`, or `GITHUB_ACTOR` + `GITHUB_ACTOR_ID`, both halves
required), then the identity behind the credential. Passing `--owner` **replaces** that
result rather than adding to it — the invoking identity is not appended, so a run that names
owners names all of them. A bot account is refused at exit 64: a namespace owned by a
service account has nobody to ask when something goes wrong.

The report's `owner_identity_source` says which rule produced the list, and it is **not
uniform across forges**:

| Value | What it means | Where it happens |
|---|---|---|
| `resolved` | The forge's users API answered; its canonical spelling, id and bot flag were taken from it | GitHub and GitLab |
| `ci-environment` | The CI variables named the list, confirmed against the users API | GitHub and GitLab |
| `asserted` | The users API was out of reach and a `LOGIN:ID` pair was taken on your word | **GitLab only**, and only under a job token |

`asserted` is unreachable on GitHub. It needs the users API to be closed while a `LOGIN:ID`
was supplied, and the only credential that closes it is a GitLab job token — which is
exactly why the [job-token recipe](#claiming-gitlab-job-token) passes `--owner` explicitly.
Without it, a bare login with no reachable users API is a usage error naming the `LOGIN:ID`
form, because guessing an id would write a stranger into a governance field.

The report also carries `author`, which is who opened the request rather than who owns the
namespace. **Do not read it as an attestation.** Only its first rung — the forge's answer
about the credential — is the forge speaking; the second is an ordinary read of
`GITLAB_USER_LOGIN` / `GITHUB_ACTOR`, which an earlier pipeline step can set to anything,
and the key does not say which rung answered.

## Who reviews it, and what happens after {#claiming-review}

On the public [`ocx-sh/index`][index-repo] the claim request is labelled `new-package` and
carries a red `governance/review-required` status that does not auto-resolve. A human
approves and merges it. That review is the whole point of the human lane: judging whether
the claimed namespace plausibly belongs to the entity it names is not automatable, and the
claim command deliberately does not try.

What the owner list buys you is everything *after* that merge. The index's auto-merge rule
requires the announcing identity to own every root a request touches, so an
[`ocx package announce`][cmd-package-announce] run whose credential resolves to an account id
in `owners[]` merges without a human; one that does not, waits for a reviewer. Getting the
owner list right at claim time is what makes your release pipeline unattended later. A
self-hosted index sets its own rules — check with whoever runs it.

Re-running a claim before its request merges is safe: the refusal that guards an
already-claimed namespace reads the index's **base** branch, not the claim branch, so a
second run reports `unchanged` rather than failing. Once the request merges, a further claim
exits 65 and points you at `announce`.

## Secrets on a shared runner {#claiming-runner}

Under `--transport git`, ocx hands the push secret to `git` through git's own configuration
environment rather than through the command line — so it is absent from `argv`, from the
remote URL, from `.git/config` and from the reflog. That closes the surfaces a later reader
of the repository or of `ps` could reach.

One surface it does not close: the secret is in the child process's environment while the
push runs, and on Linux any process running as the same user can read
[`/proc/<pid>/environ`][proc-man]. On a dedicated runner that is nobody. On a **shared**
runner — several projects' jobs on one host under one uid — it is every other job that
happens to be running at the same moment. If that is your situation, the fix is runner
isolation (one uid or one container per project), not a different flag: no way of passing a
secret to a child process avoids it.

## Troubleshooting {#claiming-troubleshooting}

Every failure below is an [exit code][exit-codes] you can branch on, not a string to grep.

| Exit | What ocx says | What to do |
|---|---|---|
| 64 | `--out` and `--fork` cannot both be given; or `--transport git` was given with `--fork`, with `--out`, or against a GitHub forge | Drop one. `--out` opens no request, so a fork has nothing to open from; the `git` transport pushes to the index itself and is GitLab-only |
| 64 | `malformed --repository`: expected `oci://host/path` | The pointer names the registry repository, scheme included — `oci://ghcr.io/acme/widget`, not `ghcr.io/acme/widget` and not the index |
| 64 | `--owner` is neither a `LOGIN` nor a `LOGIN:ID` pair | The id is a non-negative whole number, split on the first colon. `alice:7`, not `alice:7:8` and not `:7` |
| 64 | `--upstream-repository-url` must be an http or https URL without embedded credentials | The refusal never echoes the value, because the most likely one is a forwarded `CI_REPOSITORY_URL` whose userinfo is a live job token. Pass the public URL |
| 64 | `no acting identity` | Neither the credential nor the CI environment named an account. Pass `--owner LOGIN:ID` |
| 64 | `unknown owner … expected LOGIN:ID` while the users API is unreachable | You are on a job token, which cannot read the users API. Pass the pair, as the [job-token recipe](#claiming-gitlab-job-token) does |
| 64 | `owner … is a bot account` | Name a human. Governance needs somebody to ask |
| 64 | `--forge` is required for a self-hosted host | A hostname says nothing about which forge runs behind it, and a wrong guess would send your credential to the wrong API. Say `--forge gitlab` or `--forge github` |
| 65 | `namespace already claimed` | It merged. Publish tags with [`ocx package announce`][cmd-package-announce] instead |
| 69 | the forge is unreachable or returned a 5xx; or no `git` was found, or it is older than 2.31.0 | Retry the first; install a newer git for the second — the floor is checked before the forge is even constructed |
| 74 | writing under `--out` failed | Check the directory's permissions and the free space. Parent directories are created for you; a parent that is a regular file is not |
| 75 | rate-limited (429), or a concurrent claim kept winning the branch | Retry with backoff |
| 77 | the push was refused by the forge's own policy | A protected branch or a push rule on the index project. Its administrator has to relax it, or use `--transport api` with a token that may push |
| 79 | `unknown owner …: the forge has no such account` | Check the spelling, or pass `LOGIN:ID` to skip the lookup |
| 80 | no credential, a rejected one, or one that cannot push to `--index-repo` | Set [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Without `--fork` the credential also needs push access, which ocx checks up front and names |
| 86 | a capability the transport needs is absent | Job-token pushes are disabled on the index project, or the allowlist does not admit yours. Only an administrator of the index project can grant either; `--transport api` with a stored token is the way around it |

`--out` is the way to see what a run would commit without opening anything: it needs no
credential, writes the whole entry every time, and reports `updated` on every run because
there is no branch to be unchanged against.

<!-- external -->
[gitlab-job-token]: https://docs.gitlab.com/ci/jobs/ci_job_token/
[gitlab-push-options]: https://docs.gitlab.com/user/project/push_options/
[gitlab-deploy-token]: https://docs.gitlab.com/user/project/deploy_tokens/
[proc-man]: https://man7.org/linux/man-pages/man5/proc.5.html
[index-repo]: https://github.com/ocx-sh/index

<!-- commands -->
[cmd-package-claim]: ../reference/command-line.md#package-claim
[cmd-package-announce]: ../reference/command-line.md#package-announce
[exit-codes]: ../reference/command-line.md#exit-codes

<!-- environment -->
[env-ocx-announce-token]: ../reference/environment.md#ocx-announce-token
[env-ocx-announce-git-token]: ../reference/environment.md#ocx-announce-git-token
[env-ocx-announce-git-username]: ../reference/environment.md#ocx-announce-git-username

<!-- internal -->
[in-depth-indices]: ../in-depth/indices.md
