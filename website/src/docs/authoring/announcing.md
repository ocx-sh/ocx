---
outline: deep
---

# Announcing a package {#announcing}

[`ocx package push`][cmd-package-push] puts bytes in a registry, and a registry holds
anything — container images, Helm charts, whatever else your org already stores there. It has
no notion of which of its repositories are packages, which of them you meant to publish, or
which of their tags are still fit to install.

Announcing writes that into an [index][in-depth-indices]: the software catalog over the
registry. The entry says `acme/widget` exists, which registry repository holds it, which tags
it carries and which are [yanked or deprecated][in-depth-indices-status] — so
[`ocx package install`][cmd-install] `acme/widget` resolves for someone who was never told the
host, and the catalog is the one thing that has to stay current.

::: info Same shape as winget
[winget-pkgs][winget-pkgs] is the equivalent catalog for Windows: a publisher opens a
review-gated pull request against a central manifest repository, and only after it merges does
the package appear in what `winget` resolves from. OCX takes the same shape, minus the
manifest-authoring step.
:::

Two commands write it, and the split is deliberate. Once the mapping exists, every later
update from `acme/*` is a routine refresh nobody reviews — so the mapping itself is where a
human looks: someone has to judge that whoever is claiming `acme` plausibly *is* ACME.

| Command | When | Reviewed by |
|---|---|---|
| [`ocx package claim`][cmd-package-claim] | Once per package, before its first announce | A person, always |
| [`ocx package announce`][cmd-package-announce] | Every release afterwards | The index's auto-merge rule, once the owners match |

`announce` never creates an entry — a package with no committed entry exits 79 and points you
at `claim`. Neither command ever commits to the index's default branch: both
arrive as a pull request (GitHub) or a merge request (GitLab).

::: warning A GitLab-sourced claim is a documented prerequisite, not yet a recommendation
The public catalog renders an entry's owner links from GitHub account ids. An entry claimed
from GitLab records GitLab ids, and the catalog currently renders those as github.com
links — pointing at whichever unrelated GitHub account happens to hold that number. The
GitLab recipes below are written and correct on the ocx side; run them against **your own
index repository**, and treat claiming the public `ocx.sh` index from GitLab as blocked
until that catalog issue closes.
:::

## What the index entry records {#announcing-entry}

A claim writes one JSON entry into the index repository, at `p/<namespace>/<package>.json`.
Four things go in it, and only two come from you directly:

| Field | Where it comes from | Why it matters |
|---|---|---|
| The logical name | `<namespace>/<package>`, prefixed with the default registry | What a consumer types: `ocx add acme/widget`, `ocx package install acme/widget` |
| The physical repository | `--repository oci://HOST/PATH` | The registry repository every later announce resolves tags against |
| The owners | `--owner`, else the CI environment, else the credential's identity | The governance key — see [below](#announcing-owners) |
| The upstream block | `--upstream-org` and its two optional siblings | Only for a package that repackages someone else's software |

The `--repository` pointer names your **registry**, not the index. They are different
systems: the registry holds the bytes, the index holds the mapping. A claim that points at
the index repository would resolve to nothing.

Every later announce rewrites the entry's tag set in place. The owners and the repository
pointer stay as the claim wrote them — changing either is a governance-sensitive edit that
goes back to a person.

## Claiming the package {#announcing-claim}

```sh
ocx package claim --repository oci://ghcr.io/acme/widget acme/widget
```

That is the whole minimum: a logical name and the registry repository behind it. `--owner`
names the accounts recorded as owners and **replaces** the detected list rather than adding
to it; `--upstream-org`, with its optional `--upstream-repository-url` and
`--upstream-disclaimer`, marks a package that repackages someone else's software. Flags
come before the positional; the full grammar and every flag is in the
[`claim` reference][cmd-package-claim].

Every refusal that is decidable from the command line alone — mutually exclusive write flags,
a malformed `--repository`, an `--upstream-*` flag with no anchor — is decided before a
credential is resolved and before anything is dialled, so a bad invocation costs you no round
trip through a token you did not need.

The claimed unit is the **package**, not the namespace prefix: the entry lives at
`p/<namespace>/<package>.json`, and the refusal that guards a second claim reads exactly that
path. So `acme/gadget` needs its own claim run even after `acme/widget` merged — a shared
namespace prefix grants nothing on the ocx side. What you do once per package is this; after
its request merges, `announce` works and nobody looks at that package's releases again.

## Announcing tags {#announcing-tags}

An announce run re-observes a set of registry tags and rebuilds the entry from what it finds.
Which tags make up that set is the one required choice, and the four modes differ in whether
they *replace* the committed set or *add* to it:

| Mode | Effect on the committed set |
|---|---|
| `--tags 1.2.0,latest` | **Replaces** it. A committed tag not named here is dropped |
| `--tags-file <path>` | **Adds** the file's tags (comma- or newline-separated). Never removes one |
| `--tags-from-registry` | **Adds** every tag the registry repository currently holds. Never removes one |
| `--refresh` | Changes nothing about *which* tags are curated; re-observes them all, picking up a digest that moved under a rolling tag like `latest` |

`--tags-file` is the mode a release pipeline wants, because
[`ocx package push --tags-file <path>`][cmd-package-push] writes that file as it pushes: the
push records which tags it produced, and the announce step consumes the record instead of
restating it.

```sh
ocx package push --tags-file tags.txt …
ocx package announce --tags-file tags.txt --fork acme-bot/index acme/widget
```

`--fork` opens the request from your own fork of the index — the same trust model as any
open-source contribution, and no index-side credential is ever required. Omit it and the
announce branch is pushed to the index repository itself, which needs push access there; that
is the only working path when the publishing repository and the index share an owner, since a
repository cannot be forked into the namespace that already owns it.

A run that changes nothing makes no commit and reports `unchanged`; a second announce for the
same package updates the open request in place rather than opening a second one. Announcing
also picks up whatever [`ocx package description push`][cmd-package-describe] last published —
title, summary, keywords, README and logo — so a description refresh needs no separate step.

Yanking runs through the same command. `--yank <tag>` with a `--yank-reason` marks a tag as
one that should no longer be installed, and `--unyank <tag>` clears the marker. It is a
publisher signal, not a delete: resolving a yanked tag is refused by default and
[`OCX_ALLOW_YANKED`][env-ocx-allow-yanked] opts back in, while a resolve already pinned to the
digest never needs that opt-in, because immutable content cannot itself be yanked.

## Choosing a posture {#announcing-postures}

`claim` and `announce` share their whole write path: the same `--index-repo`, `--forge`,
`--transport`, `--fork` and `--out` flags, the same credential ladder, the same capability
checks. So the posture you pick applies to both, and the recipes below run the pair.

Four ways to run them, differing in what credential you have and therefore in how the request
gets written.

| Posture | Environment | `--transport` | Request authored by | Minimum version |
|---|---|---|---|---|
| GitHub, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a PAT | `api` (default) | The token's own account | — |
| GitLab, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a personal, project or group token | `api` (default) | The token's own account | — |
| GitLab CI job token | `GITLAB_CI` and `CI_JOB_TOKEN`, set by the runner; no `OCX_ANNOUNCE_TOKEN` | `git` | `GITLAB_USER_LOGIN` / `GITLAB_USER_ID` — the user who triggered the pipeline | git 2.31.0 |
| GitLab, split credential pair | [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] (+ [`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username]) for the push, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] for the reads | `git` | The API token's account | git 2.31.0 |

The version column is the floor ocx enforces itself: under `--transport git` it runs
`git --version` before it constructs the forge and exits 69 below 2.31.0. There is no
GitLab version number ocx checks. Instead it asks the instance the two questions that
actually matter — does this project accept job-token pushes, and do its allowlists admit
the publishing project — and exits 86 naming whichever answer is missing. The second
question has two answers, because GitLab keeps a project list and a group list.

The index does not have to be on GitHub. Both commands speak **GitHub and GitLab**, each on
its public host and on self-hosted instances, and on GitLab the change arrives as a merge
request. Name the host in `--index-repo` (`gitlab.com/acme/index`), and for a self-hosted
instance add `--forge github` or `--forge gitlab` — a hostname says nothing about which forge
runs behind it, so ocx asks rather than guessing where to send your credential. Nested GitLab
group paths work as written: `--index-repo gitlab.example.com/acme/platform/tooling/index`.

### Why `--transport git` exists {#announcing-transport}

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

### GitHub, from a workflow {#announcing-github}

```yaml
# .github/workflows/claim.yml — run once, by hand
name: Claim the package
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

```yaml
# .github/workflows/release.yml — the announce step, every release
      - env:
          OCX_ANNOUNCE_TOKEN: ${{ secrets.INDEX_PAT }}
        run: |
          ocx package announce \
            --tags-file tags.txt \
            --fork acme-bot/index \
            acme/widget
```

`GITHUB_TOKEN` is not a substitute for `INDEX_PAT`: it is scoped to this repository and can
neither fork nor write the index. Omit `--fork` only when the token can push to the index
repository directly; ocx verifies that before it writes anything and exits 80 naming the
repository and the missing permission if it cannot.

### GitLab, with an access token {#announcing-gitlab-token}

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

announce:
  image: alpine:3
  rules:
    - if: $CI_COMMIT_TAG
  variables:
    OCX_ANNOUNCE_TOKEN: $INDEX_TOKEN
  script:
    - ocx package announce
        --tags-file tags.txt
        --index-repo gitlab.example.com/acme/index
        acme/widget
```

The plain-API posture, and the one to prefer when you can store a token: no clone, no git
binary, no capability preflight. `--transport` stays at its default.

### GitLab, from a CI job {#announcing-gitlab-job-token}

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

announce:
  image: alpine:3
  rules:
    - if: $CI_COMMIT_TAG
  script:
    - ocx package announce
        --tags-file tags.txt
        --index-repo gitlab.example.com/acme/index
        --transport git
        acme/widget
```

Nothing is stored: `CI_JOB_TOKEN` and `GITLAB_CI` are both set by the runner, and ocx falls
through to the job token when `OCX_ANNOUNCE_TOKEN` is unset or empty. The `--owner` pair is
explicit on the claim on purpose — see [the owners section](#announcing-owners). Announce
needs no `--owner`: the entry already records who owns it.

This posture needs the index project to have enabled job-token pushes and to have
allowlisted the publishing project. Either allowlist is enough: the project by name, or
any group it sits under. One group entry covers every publisher in that group, at any
depth. Both are settings on the **index** project, not yours, and if either is missing the
run exits 86; an administrator there has to act.

A job token may read only [the endpoints GitLab opens to
it](https://docs.gitlab.com/ci/jobs/ci_job_token/#job-token-access), which does not include
the settings above — so in this posture ocx cannot check them before it pushes. It pushes
and lets GitLab's own rejection decide. The exit code is 86 either way, but the message is
not: GitLab's rejection carries no field saying which setting was missing, so the message
is generic rather than naming one. The [split pair](#announcing-split) below reads the same
two settings up front, because its API half is an ordinary token — and gets the specific
message this posture cannot.

### GitLab, with a split credential pair {#announcing-split}

```yaml
# .gitlab-ci.yml
.index-write: &index-write
  image: alpine:3
  variables:
    OCX_ANNOUNCE_TOKEN: $INDEX_READ_TOKEN       # masked; API reads
    OCX_ANNOUNCE_GIT_TOKEN: $INDEX_DEPLOY_TOKEN # masked; the push only
    OCX_ANNOUNCE_GIT_USERNAME: acme-index-writer

claim:
  <<: *index-write
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
  script:
    - ocx package claim
        --repository oci://$CI_REGISTRY_IMAGE
        --index-repo gitlab.example.com/acme/index
        --transport git
        acme/widget

announce:
  <<: *index-write
  rules:
    - if: $CI_COMMIT_TAG
  script:
    - ocx package announce
        --tags-file tags.txt
        --index-repo gitlab.example.com/acme/index
        --transport git
        acme/widget
```

[`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username] defaults to `gitlab-ci-token`
and only needs setting when the instance expects a specific user for that token. Both
secrets travel as an HTTP Basic header injected through git's own configuration
environment — never in a URL, never in `argv`, never written to `.git/config`.

## Who the entry says owns it {#announcing-owners}

An owner is a `login:id` pair, and the **id** is the load-bearing half. Logins get renamed
and recycled; a numeric account id does not. That is why the entry records both and why
governance keys on the number.

Without `--owner`, ocx works down a short ladder: the CI environment's user variables
(`GITLAB_USER_LOGIN` + `GITLAB_USER_ID`, or `GITHUB_ACTOR` + `GITHUB_ACTOR_ID`, both halves
required), then the identity behind the credential. Passing `--owner` **replaces** that
result rather than adding to it — the invoking identity is not appended, so a run that names
owners names all of them. A bot account is refused at exit 64: a package owned by a
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
exactly why the [job-token recipe](#announcing-gitlab-job-token) passes `--owner` explicitly.
Without it, a bare login with no reachable users API is a usage error naming the `LOGIN:ID`
form, because guessing an id would write a stranger into a governance field.

The report also carries `author`, which is who opened the request rather than who owns the
package. **Do not read it as an attestation.** Only its first rung — the forge's answer
about the credential — is the forge speaking; the second is an ordinary read of
`GITLAB_USER_LOGIN` / `GITHUB_ACTOR`, which an earlier pipeline step can set to anything,
and the key does not say which rung answered.

## Who reviews it, and what happens after {#announcing-review}

On the public [`ocx-sh/index`][index-repo] a claim request is labelled `new-package` and
carries a red `governance/review-required` status that does not auto-resolve. A human
approves and merges it. That review is the whole point of the human lane: judging whether
whoever is claiming a name under `acme` plausibly *is* ACME is not automatable, and the claim
command deliberately does not try. ocx verifies no ownership of its own at any point — every
privileged check runs in the index's CI. How much scrutiny a second package under an
already-reviewed prefix gets is that index's policy, not something ocx decides.

What the owner list buys you is everything *after* that merge. The index's auto-merge rule
requires the announcing identity to own every root a request touches, so an
[`ocx package announce`][cmd-package-announce] run whose credential resolves to an account id
in `owners[]` merges without a human; one that does not, waits for a reviewer. A change
touching a governance-sensitive field always goes back to a person, whichever command made
it — the [governance contracts reference][index-governance-contracts] documents which fields
route to which lane. Getting the owner list right at claim time is what makes your release
pipeline unattended later. A self-hosted index sets its own rules — check with whoever runs
it.

Re-running a claim before its request merges is safe: the refusal that guards an
already-claimed package reads the index's **base** branch, not the claim branch, so a
second run reports `unchanged` rather than failing. Once the request merges, a further claim
exits 65 and points you at `announce`.

## What your consumers need {#announcing-consumers}

Nothing. The [compiled-in defaults][config-registries-index] name `https://index.ocx.sh` as
the index for the `ocx.sh` namespace, so [`ocx package install`][cmd-install] `<ns>/<pkg>`
resolves through the [public index][in-depth-indices-public] on a machine with no OCX config
at all. A consumer opts *out*, not in — `index = ""`, or a [`[mirrors]`][config-mirrors]
entry pinning `ocx.sh` at a registry endpoint.

That authority is exclusive, and it cuts both ways. The index is the sole resolver for
`ocx.sh/…` — which is what lets a yank reach everyone who installs the package — so an
`index.ocx.sh` outage fails those installs outright rather than falling through to whatever
registry happens to serve a repository under the same name. An announced package inherits the
index's availability along with its guarantees.

## Secrets on a shared runner {#announcing-runner}

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

`OCX_ANNOUNCE_TOKEN` is read from the environment only, never from a stored credential, and
it never reaches the [`~/.docker/config.json` that `ocx login` writes][authentication-storing].
Writing an index and authenticating to a registry are two trust boundaries with two
credentials.

## Troubleshooting {#announcing-troubleshooting}

Every failure below is an [exit code][exit-codes] you can branch on, not a string to grep.
Codes marked *claim* or *announce* are reachable from that command only; the rest are shared.

| Exit | What ocx says | What to do |
|---|---|---|
| 64 | `--out` and `--fork` cannot both be given; or `--transport git` was given with `--fork`, with `--out`, or against a GitHub forge | Drop one. `--out` opens no request, so a fork has nothing to open from; the `git` transport pushes to the index itself and is GitLab-only |
| 64 | `--forge` is required for a self-hosted host | A hostname says nothing about which forge runs behind it, and a wrong guess would send your credential to the wrong API. Say `--forge gitlab` or `--forge github` |
| 64 | *claim* — `malformed --repository`: expected `oci://host/path` | The pointer names the registry repository, scheme included — `oci://ghcr.io/acme/widget`, not `ghcr.io/acme/widget` and not the index |
| 64 | *claim* — `--owner` is neither a `LOGIN` nor a `LOGIN:ID` pair | The id is a non-negative whole number, split on the first colon. `alice:7`, not `alice:7:8` and not `:7` |
| 64 | *claim* — `--upstream-repository-url` must be an http or https URL without embedded credentials | The refusal never echoes the value, because the most likely one is a forwarded `CI_REPOSITORY_URL` whose userinfo is a live job token. Pass the public URL |
| 64 | *claim* — `no acting identity`, `unknown owner … expected LOGIN:ID`, or `owner … is a bot account` | Pass `--owner LOGIN:ID` naming a human. On a job token the users API is closed, so the pair is required — as the [job-token recipe](#announcing-gitlab-job-token) shows |
| 64 | *announce* — the curated set resolved to nothing but reserved tags | Every tag named was an OCX-internal `__ocx` or legacy keep tag. Name a real version |
| 65 | *claim* — `package already claimed` | It merged. Publish tags with [`ocx package announce`][cmd-package-announce] instead |
| 65 | *announce* — the recorded description no longer exists on the registry, or an unchanged run's open request can no longer merge | Republish the description with [`ocx package description push`][cmd-package-describe]; for the second, close the request or delete the branch and announce again |
| 69 | the forge is unreachable or returned a 5xx; the registry could not be resolved; or no `git` was found, or it is older than 2.31.0 | Retry the first two; install a newer git for the third — the floor is checked before the forge is even constructed |
| 74 | writing under `--out` failed, or `--tags-file` could not be read | Check the path's permissions. Parent directories are created for you; a parent that is a regular file is not |
| 75 | rate-limited (429), or a concurrent run kept winning the branch | Retry with backoff |
| 77 | the push was refused by the forge's own policy | A protected branch or a push rule on the index project. Its administrator has to relax it, or use `--transport api` with a token that may push |
| 78 | *announce* — a curated tag's physical host resolves to a private, loopback, link-local or metadata address | Add it to that namespace's [`trusted_hosts`][config-registries-trusted-hosts] to allow it |
| 79 | *announce* — a curated tag does not resolve on the registry, or the package is unclaimed | Check the tag for a typo; for the second, run [`ocx package claim`][cmd-package-claim] first |
| 79 | *claim* — `unknown owner …: the forge has no such account` | Check the spelling, or pass `LOGIN:ID` to skip the lookup |
| 80 | no credential, a rejected one, or one that cannot push to `--index-repo` | Set [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Without `--fork` the credential also needs push access, which ocx checks up front and names |
| 86 | a capability the transport needs is absent | Job-token pushes are disabled on the index project, or neither of its allowlists admits your project or one of its groups. Only an administrator of the index project can grant either; `--transport api` with a stored token is the way around it |

`--out` is the way to see what a run would commit without opening anything: it needs no
credential and writes the whole entry every time, including on a run that changes nothing.

::: tip Learn more
[Building and pushing packages][authoring-building-pushing] — the push workflow that produces
the `--tags-file` an announce consumes.
[Indices in depth → Writing to an index][in-depth-indices-writing] — the transport and
credential model behind both commands.
[index.ocx.sh][in-depth-indices-public] — how the public index resolves an announced package
back into an install.
:::

<!-- external -->
[gitlab-job-token]: https://docs.gitlab.com/ci/jobs/ci_job_token/
[gitlab-push-options]: https://docs.gitlab.com/user/project/push_options/
[gitlab-deploy-token]: https://docs.gitlab.com/user/project/deploy_tokens/
[proc-man]: https://man7.org/linux/man-pages/man5/proc.5.html
[index-repo]: https://github.com/ocx-sh/index
[index-governance-contracts]: https://index.ocx.sh/docs/reference/governance-contracts
[winget-pkgs]: https://github.com/microsoft/winget-pkgs
[winget-submit]: https://learn.microsoft.com/en-us/windows/package-manager/package/repository

<!-- commands -->
[cmd-package-claim]: ../reference/command-line.md#package-claim
[cmd-package-announce]: ../reference/command-line.md#package-announce
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-describe]: ../reference/command-line.md#package-description-push
[cmd-install]: ../reference/command-line.md#package-install
[exit-codes]: ../reference/command-line.md#exit-codes

<!-- environment -->
[env-ocx-announce-token]: ../reference/environment.md#ocx-announce-token
[env-ocx-announce-git-token]: ../reference/environment.md#ocx-announce-git-token
[env-ocx-announce-git-username]: ../reference/environment.md#ocx-announce-git-username
[env-ocx-allow-yanked]: ../reference/environment.md#ocx-allow-yanked

<!-- configuration -->
[config-registries-index]: ../reference/configuration.md#keys-registries-index
[config-registries-trusted-hosts]: ../reference/configuration.md#keys-registries-trusted-hosts
[config-mirrors]: ../reference/configuration.md#keys-mirrors

<!-- internal -->
[in-depth-indices]: ../in-depth/indices.md
[in-depth-indices-public]: ../in-depth/indices.md#public-index
[in-depth-indices-status]: ../in-depth/indices.md#public-index-status
[in-depth-indices-writing]: ../in-depth/indices.md#writing
[authoring-building-pushing]: ./building-pushing.md#first-push
[authentication-storing]: ../user-guide.md#authentication-storing
