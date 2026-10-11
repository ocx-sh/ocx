---
title: "Announce from CI"
description: "Announce every release from a GitHub or GitLab CI job with the right index credential and no secret leaking on a shared runner."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

This page sets up a CI job that announces every release to the index. You need a package that is already claimed (see
[Announce a package][announce]). You also need a CI system on GitHub or GitLab and a credential for the index repository. Pick a posture from
the table, then copy its recipe. The wider release pipeline (signing, SBOM, provenance) is on [Publisher CI][integration-publisher-ci].

## Choosing a posture {#announcing-postures}

`claim` and `announce` share their whole write path. They take the same `--index-repo`, `--forge`, `--transport`, `--fork` and `--output`
flags, the same credential ladder and the same capability checks. So the posture you pick applies to both, and the recipes below
run the pair.

Four ways to run them differ in what credential you have, and therefore in how the request gets written.

| Posture | Environment | `--transport` | Request authored by | Minimum version |
|---|---|---|---|---|
| GitHub, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a PAT | `api` (default) | The token's own account | none |
| GitLab, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] = a personal, project or group token | `api` (default) | The token's own account | none |
| GitLab CI job token | `GITLAB_CI` and `CI_JOB_TOKEN`, set by the runner. No `OCX_ANNOUNCE_TOKEN` | `git` | `GITLAB_USER_LOGIN` / `GITLAB_USER_ID`, the user who triggered the pipeline | git 2.31.0 |
| GitLab, split credential pair | [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] (+ [`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username]) for the push, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] for the reads | `git` | The owner of `OCX_ANNOUNCE_GIT_TOKEN`, since it makes the push | git 2.31.0 |

One variable, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token], serves both forges, and the token it holds follows the forge the run
targets. Writing locally with `--output` needs none.

| Index forge | Token | Required scope |
|---|---|---|
| GitHub, GitHub Enterprise Server | Personal access token | `repo`, or fine-grained: read plus pull-request write on the index and the fork |
| GitLab, self-managed GitLab | Personal, project or group access token | `api` |

What the variable has to carry depends on the transport. Under `--transport api` it authenticates every REST call: reading the
committed entry, creating the branch and the commit, opening the request.

Under `--transport git` it authenticates only the REST reads and the merge-request confirmation. The repository write uses
[`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] when set, otherwise this token.

Inside a GitLab job (`GITLAB_CI` non-empty), an unset or empty `OCX_ANNOUNCE_TOKEN` falls through to `CI_JOB_TOKEN` under
`--transport git` only. Under `--transport api` it does not, because a job token cannot open the request there. That run needs a
real access token in a masked variable.

The version column is the floor ocx enforces itself. Under `--transport git` it runs `git --version` before it constructs the forge
and exits 69 below 2.31.0. ocx checks no GitLab version number. Instead it asks the instance two questions: does this project
accept job-token pushes, and do its allowlists admit the publishing project? It exits 82 or 77 naming whichever answer is missing.
The second question has two answers, because GitLab keeps a project list and a group list.

The index does not have to be on GitHub. Both commands speak **GitHub and GitLab**, each on its public host and on self-hosted
instances. On GitLab the change arrives as a merge request. Name the host in `--index-repo` (`gitlab.com/acme/index`).

For a self-hosted instance, add `--forge github` or `--forge gitlab`. A hostname says nothing about which forge runs behind it, so
ocx asks rather than guessing where to send your credential. Nested GitLab group paths work as written:
`--index-repo gitlab.example.com/acme/platform/tooling/index`.

:::caution[Claim the public index from GitHub, not GitLab]
The public catalog renders an entry's owner links from GitHub account ids. An entry claimed from GitLab records GitLab ids, and the
catalog still renders those as github.com links. They point at whichever unrelated GitHub account holds that number.

The GitLab recipes below are correct on the ocx side. Run them against **your own index repository**. Treat claiming the public
`ocx.sh` index from GitLab as blocked until that catalog issue closes.
:::

### Why `--transport git` exists {#announcing-transport}

A [GitLab CI job token][gitlab-job-token] is the credential every job already has, and a pipeline should prefer it. It is scoped to
the job, it expires with it, and nobody has to store it. It also cannot open a merge request. It reads the API fine (branches,
commits, raw files, merge requests, tags) and is *read-only* for all of them.

What it can do is push, when the target project allows it. So `--transport git` takes the other route. It clones the index
repository into a temporary directory and builds the commit there. Then it creates the merge request from the push itself, using
[GitLab's merge-request push options][gitlab-push-options]. That is one authenticated push, no API write and no stored secret.

That same split is why the credential pair exists. The two halves are asked for different things, and a real deployment often has
to answer with different identities:

- A [deploy token][gitlab-deploy-token] can write the repository but has no API surface to call. It can carry the push and nothing else.
- A job token can read the API but cannot open the merge request. It can carry the reads and nothing else.

Set [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] and the push uses it while the REST reads keep using
[`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Leave it unset and the push reuses whatever the API credential resolved to.

### GitHub, from a workflow {#announcing-github}

```yaml
# .github/workflows/claim.yml (run once, by hand)
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
# .github/workflows/release.yml (the push and announce steps, every release)
      - uses: ocx-sh/setup-ocx@v1
      - run: |
          ocx package push --cascade --tags-file tags.txt \
            -p linux/amd64 -i ghcr.io/acme/widget:1.2.0 widget-1.2.0.tar.xz
      - env:
          OCX_ANNOUNCE_TOKEN: ${{ secrets.INDEX_PAT }}
        run: |
          ocx package announce \
            --tags-file tags.txt \
            --fork acme-bot/index \
            acme/widget
```

The push step writes `tags.txt`, the record that [announcing tags][announcing-tags] describes. It also needs registry credentials, set through the `OCX_AUTH_<REGISTRY>_*` variables.

`GITHUB_TOKEN` is not a substitute for `INDEX_PAT`. It is scoped to this repository and can neither fork nor write the index. Omit
`--fork` only when the token can push to the index repository directly. ocx verifies that before it writes anything. If it cannot, it exits 80
naming the repository and the missing permission.

### GitLab, with an access token {#announcing-gitlab-token}

```yaml
# .gitlab-ci.yml
.install-ocx:
  before_script:
    - apk add --no-cache curl
    - curl -fsSL https://setup.ocx.sh/sh | sh
    - export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"

claim:
  extends: .install-ocx
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
  extends: .install-ocx
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

This is the plain-API posture, and the one to prefer when you can store a token. It needs no clone, no git binary and no capability
preflight. `--transport` stays at its default.

The hidden `.install-ocx` job puts ocx on the bare `alpine:3` image. The recipes that use `--transport git` also install `git`.

### GitLab, from a CI job {#announcing-gitlab-job-token}

```yaml
# .gitlab-ci.yml
.install-ocx:
  before_script:
    - apk add --no-cache curl git
    - curl -fsSL https://setup.ocx.sh/sh | sh
    - export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"

claim:
  extends: .install-ocx
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
  extends: .install-ocx
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

Nothing is stored. The runner sets `CI_JOB_TOKEN` and `GITLAB_CI`, and ocx falls through to the job token when `OCX_ANNOUNCE_TOKEN`
is unset or empty. The `--owner` pair is explicit on the claim on purpose, as the [owners section][announcing-owners] explains.
Announce needs no `--owner`, because the entry already records who owns it.

This posture needs the index project to have enabled job-token pushes and to have allowlisted the publishing project. Either
allowlist is enough: the project by name, or any group it sits under. One group entry covers every publisher in that group, at any
depth.

Both are settings on the **index** project, not yours. If either is missing, the run exits 82 (pushes disabled) or 77 (allowlist
miss), and an administrator there has to act.

A job token may read only [the endpoints GitLab opens to it][gitlab-job-token-access], which do not include the settings above. So
in this posture ocx cannot check them before it pushes. It pushes and lets GitLab's own rejection decide. The exit code is 82
either way, but the message is generic, because GitLab's rejection carries no field saying which setting was missing.

The [split pair](#announcing-split) reads the same two settings up front, because its API half is an ordinary token. It gets the
specific message this posture cannot.

### GitLab, with a split credential pair {#announcing-split}

```yaml
# .gitlab-ci.yml
.install-ocx:
  before_script:
    - apk add --no-cache curl git
    - curl -fsSL https://setup.ocx.sh/sh | sh
    - export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"

.index-write: &index-write
  extends: .install-ocx
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

[`OCX_ANNOUNCE_GIT_USERNAME`][env-ocx-announce-git-username] defaults to `gitlab-ci-token`. Set it only when the instance expects a
specific user for that token. Both secrets travel as an HTTP Basic header injected through git's own configuration environment.
They never appear in a URL or in `argv`, and ocx never writes them to `.git/config`.

## Secrets on a shared runner {#announcing-runner}

Under `--transport git`, ocx hands the push secret to `git` through git's own configuration environment rather than the command
line. So it is absent from `argv`, from the remote URL, from `.git/config` and from the reflog. That closes the surfaces a later
reader of the repository or of `ps` could reach.

One surface stays open. The secret is in the child process's environment while the push runs, and on Linux any process running as
the same user can read [`/proc/<pid>/environ`][proc-man]. On a dedicated runner that is nobody.

On a **shared** runner, several projects' jobs run on one host under one uid. Then it is every other job running at the same
moment. The fix is runner isolation (one uid or one container per project), not a different flag. No way of passing a secret to a
child process avoids it.

The credential variables follow a few fixed rules:

- `OCX_ANNOUNCE_TOKEN` is sent as a request header, never logged and never placed in a URL. The header is `Authorization: Bearer` on GitHub, `PRIVATE-TOKEN` on GitLab, or `JOB-TOKEN` when the resolved credential is this job's own `CI_JOB_TOKEN`.
- Under `--transport git` with no `OCX_ANNOUNCE_GIT_TOKEN` set, the same token is also presented to `git` as the secret half of the Basic pair.
- Redirects are disabled on the forge client, so a cross-host redirect cannot replay the header at another host.
- A forge's error body is echoed in diagnostics with the token redacted first. A reverse proxy that reflects request headers cannot put the credential in a CI log.
- The host in `--index-repo` and `--fork` must be a well-formed hostname. Anything that could shift the URL's authority (userinfo, a query, a path) is refused rather than interpreted.
- Any mode other than `--output` fails immediately with exit `80` when the credential ladder resolves nothing. It never falls back to an unauthenticated attempt.
- Without `--fork`, the credential must also carry write permission on the index repository. The run verifies this before writing anything.
- `OCX_ANNOUNCE_GIT_TOKEN` is the push half only. Set to an empty string, it counts as unset. A wrapper that exports `OCX_ANNOUNCE_GIT_TOKEN=` therefore cannot turn an authenticated write into one that authenticates as nobody.
- `OCX_ANNOUNCE_GIT_USERNAME` is ignored, with the default used instead, when it is empty or contains `:`. HTTP Basic has no escaping, and a server splits the decoded pair on the first colon.
- With `a:b` as the username, the server would read user `a` and secret `b:<secret>`. That is a `401` that looks like a bad token. The username is not a credential, so ocx does not strip it from a plugin's environment.
- OCX strips `OCX_ANNOUNCE_GIT_TOKEN` from the environment of every subprocess, plugins (`ocx-<name>`) included.
- `OCX_ANNOUNCE_TOKEN` is deliberately not stripped. A plugin that announces from its own process inherits the API half and not the write half. A plugin that must write over the git transport has to be handed the git token explicitly.

`OCX_ANNOUNCE_TOKEN` is read from the environment only, never from a stored credential. It never reaches the
[`~/.docker/config.json` that `ocx login` writes][authentication-storing]. Writing an index and authenticating to a registry are
two trust boundaries with two credentials.

For a failing run, see [troubleshooting][announcing-troubleshooting] on the announce page.

<!-- external -->
[gitlab-job-token]: https://docs.gitlab.com/ci/jobs/ci_job_token/
[gitlab-job-token-access]: https://docs.gitlab.com/ci/jobs/ci_job_token/#job-token-access
[gitlab-push-options]: https://docs.gitlab.com/user/project/push_options/
[gitlab-deploy-token]: https://docs.gitlab.com/user/project/deploy_tokens/
[proc-man]: https://man7.org/linux/man-pages/man5/proc.5.html

<!-- environment -->
[env-ocx-announce-token]: ../reference/environment.md#ocx-announce-token
[env-ocx-announce-git-token]: ../reference/environment.md#ocx-announce-git-token
[env-ocx-announce-git-username]: ../reference/environment.md#ocx-announce-git-username

<!-- internal -->
[announce]: ./announce.md
[announcing-owners]: ./announce.md#announcing-owners
[announcing-tags]: ./announce.md#announcing-tags
[announcing-troubleshooting]: ./announce.md#announcing-troubleshooting
[integration-publisher-ci]: ../integration/publisher-ci.md
[authentication-storing]: ../guides/authenticate.mdx#authentication-storing
