---
title: "Run ocx unattended in a pipeline"
description: "The variables, flags and checks that make an ocx job non-interactive, reproducible and offline-safe."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to run ocx in a pipeline with no human present. It names the settings a job should make, what each one prevents, and the checks that fail a job instead of letting it drift.

ocx never stops to ask a question in a pipeline. The few things that can differ between your laptop and a runner are background checks, shell-activation consent and version discovery. The sections below take them in turn.

## Settings for a pipeline job {#unattended-env}

Most CI systems already set `CI`, which turns off the background checks. The other variables are yours to set, in the job or the image.

| Variable | Set it to | What it settles |
|---|---|---|
| [`CI`][env-ci] | any truthy value | Turns off the self-update notice, the toolchain drift check and the managed-config refresh tick. Most CI systems set it. |
| [`OCX_NO_UPDATE_CHECK`][env-no-update-check] | `1` | The same kill switch, for a runner that does not set `CI`. |
| [`OCX_NO_CONSENT`][env-no-consent] | `1` | Writes no shell-activation consent stamp. See [Consent](#unattended-consent). |
| [`OCX_FROZEN`][env-frozen] | `1` | An unknown tag errors instead of resolving fresh. See [Freeze](#unattended-frozen). |
| [`OCX_OFFLINE`][env-offline] | `1` | No network at all. Use it for the run after a pull. |
| [`OCX_JOBS`][env-ocx-jobs] | a number | Caps parallel pulls on a small runner. |
| [`OCX_NO_CONFIG`][env-no-config] | `1` | Skips the user, `$OCX_HOME` and managed config tiers. A system-scope policy in `/etc/ocx/config.toml` still loads, cut to its locked sections. |

`OCX_NO_CONFIG` is env-only and has no flag. The hermetic pattern is `OCX_NO_CONFIG=1 ocx --config /ci/ocx.toml ...`, which loads the one explicit file plus any system-scope policy.

When ocx cannot install into your profile, as on a pipeline image you build once, pass `--no-modify-path` to `ocx self setup`. The installer then writes no profile block and no session `PATH` entry, and you add the directories yourself. Its [options][install-options] list the rest.

## Consent without a prompt {#unattended-consent}

A consent stamp is what lets a shell hook apply a project's tools at the next prompt. [`add`, `remove`, `lock`, `update`, `upgrade`, `pull`, `exec` and `init`][env-no-consent] write one as a side effect. ocx asks nothing: the stamp is a record, not a prompt.

A pipeline has no prompt hook, so a stamp does nothing for it. It would only record consent for a checkout the pipeline's operator never chose.

Set `OCX_NO_CONSENT=1` in the job or the image, never in an interactive profile. Exported globally, it leaves a project inert once its lock grows past the stamped sources. The flag form is `--no-consent`, and either flag outranks the variable. The variable is forwarded to child ocx processes, so a script run by `ocx exec` that calls `ocx pull` stays consistent.

`ocx shell allow` ignores `OCX_NO_CONSENT`. It is the explicit grant, so use it where you want one. [Shell integration][explanation-shell-integration] explains stamps and grants.

## Freeze what a job resolves {#unattended-frozen}

A pipeline should fail when its inputs move. ocx does most of that by default and gives you two switches for the rest.

**The lock is checked for you.** `ocx pull` and `ocx exec` refuse a stale lock with exit 65. [`ocx lock --check`][cmd-lock] runs the same test with no network and no write. It exits 65 when `ocx.lock` does not match `ocx.toml` and 78 when the lock is missing. Run it first in a job to fail fast.

**`--frozen` closes version discovery.** [`ocx --frozen pull`][arg-frozen] resolves a tag only from the local index or the lock. An unknown tag exits 81. Digest-pinned content still downloads, so frozen is not a network ban.

**`--offline` closes the network.** [`--offline`][arg-offline] resolves from the local index and the package store only. With both flags, offline wins.

A job that pulls once and then runs offline looks like this:

```sh
ocx lock --check          # exit 65 if ocx.toml and ocx.lock disagree
ocx --frozen pull         # download the locked digests, discover nothing new
ocx --offline exec -- cmake --version
```

To make the next run cheap, cache the three directories under `$OCX_HOME` that make a copy offline-capable: `blobs`, `layers` and `index`. Key the cache on the hash of `ocx.lock`, as the [`setup-ocx`][setup-ocx] action does. [Work offline][guide-work-offline] covers what has to travel for a copied home to work with no network.

## Authenticate without a terminal {#unattended-auth}

A runner has no keychain and no TTY. Set `OCX_AUTH_<REGISTRY>_*` variables from the CI system's secret store, or pipe a token to `ocx login --password-stdin`. [Registry authentication recipes][p-registry-auth-recipes] give both for GHCR and ECR.

Signing and attesting also look for a browser by default. Pass `--no-tty` to `ocx package sign` and `ocx package attest` so a missing identity token fails the step instead of waiting for a login. [Publisher CI][p-publisher-ci] shows the OIDC wiring.

## Tell failures apart {#unattended-exit}

A retry wrapper cares about four exit codes. Read the exit code first, then the error document if you ran with `--format json`. [Scripts and JSON][p-scripts-and-json] shows how.

| Exit | Name | Meaning for a pipeline |
|---|---|---|
| 75 | TempFail | A rerun may succeed, for example a registry blip. |
| 69 | Unavailable | A rerun will not help until the service returns. |
| 80 | AuthError | The credential is missing or refused. Fix the secret. |
| 81 | PolicyBlocked | A local rule refused the run, such as `--frozen`, `--offline` or a trust policy. |

The [exit code reference][reference-exit-codes] lists the rest, with the error slug each one carries.

## Runner variables ocx reads {#ci-variables}

Beyond the persistence channels of [GitHub Actions][p-github-actions] and [GitLab CI][p-gitlab-ci], ocx reads a small set of variables the CI system provides. They are the runner's own, carry no `OCX_` prefix and are not ocx configuration.

None is forwarded to child `ocx` processes. The forwarding set carries only `OCX_*` variables that affect resolution. The runner's sink files are runner-managed paths and must not propagate through forked environments.

| Variable | Provider | Read for |
|----------|----------|----------|
| `GITHUB_ACTIONS` | [GitHub Actions][github-actions-docs] | Provider auto-detection for bare `--ci` and `--ci-annotations` |
| `GITHUB_PATH`, `GITHUB_ENV` | GitHub Actions | The sinks `ocx env --ci=github` writes |
| `GITHUB_SERVER_URL`, `GITHUB_REPOSITORY` | GitHub Actions | `org.opencontainers.image.source` under `--ci-annotations=github`, joined with the trailing slash trimmed. If either is absent or blank, no `.source` annotation is written, never a truncated one |
| `GITHUB_SHA` | GitHub Actions | `org.opencontainers.image.revision`, stamped verbatim |
| `GITHUB_RUN_ID`, `GITHUB_ACTOR`, `GITHUB_ACTOR_ID` | GitHub Actions | `ocx package announce` joins the run id with the server URL and repository into the run's URL. `ocx package claim` takes the actor and id as the owner's login and numeric id when no `--owner` is given. The forge confirms neither |
| `GITLAB_CI` | [GitLab CI/CD][gitlab-ci-docs] | Provider auto-detection |
| `CI_PROJECT_URL` | GitLab CI/CD | `org.opencontainers.image.source` under `--ci-annotations=gitlab`, verbatim. Absent or blank writes none |
| `CI_COMMIT_SHA` | GitLab CI/CD | `org.opencontainers.image.revision`, verbatim |
| `CI_PIPELINE_CREATED_AT` | GitLab CI/CD | `org.opencontainers.image.created`, unless `SOURCE_DATE_EPOCH` is set |
| `CI_JOB_TOKEN`, `CI_JOB_URL`, `CI_PROJECT_PATH` | GitLab CI/CD | The credential of `ocx package announce --transport git` inside a job (a secret), the URL of the run making the announce, and the `group/project` path of the job's project |
| `GITLAB_USER_LOGIN`, `GITLAB_USER_ID` | GitLab CI/CD | The claiming owner for `ocx package claim` when no `--owner` is given. The forge confirms neither |
| `SOURCE_DATE_EPOCH` | [Reproducible Builds][reproducible-builds-sde] | `created` under `--ci-annotations` on either provider, outranking `CI_PIPELINE_CREATED_AT`, so a reproducible pipeline's push and its attestations agree on one instant |
| `ACTIONS_ID_TOKEN_REQUEST_URL`, `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, `SIGSTORE_ID_TOKEN`, `CIRCLE_OIDC_TOKEN_V2` | GitHub Actions, GitLab CI/CD, [CircleCI][circleci-oidc] | The ambient OIDC token for keyless `ocx package sign` and `ocx package attest`; see [Signing from CI][signing-ci] |

`CI_PIPELINE_CREATED_AT` is parsed as [RFC 3339][rfc-3339] and re-emitted in UTC seconds with a literal `Z`. That is the spelling every other `created` value uses, so a sub-second or non-UTC input is normalized, not passed through. A value that fails to parse is not stamped: ocx warns on stderr and falls back to the wall clock.

GitHub Actions exposes no equivalent, so `--ci-annotations=github` uses the wall clock unless `SOURCE_DATE_EPOCH` is set. The annotation flow is described under [Stamping from CI][publishing-ci-annotations].

`ACTIONS_ID_TOKEN_REQUEST_TOKEN` and the other tokens are secrets. ocx treats them as secrets. A tool run under `ocx exec` still inherits the OIDC pair, so that the tool can sign keylessly itself.

<!-- external -->
[setup-ocx]: https://github.com/ocx-sh/setup-ocx
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[gitlab-ci-docs]: https://docs.gitlab.com/ee/ci/
[reproducible-builds-sde]: https://reproducible-builds.org/docs/source-date-epoch/
[rfc-3339]: https://www.rfc-editor.org/rfc/rfc3339
[circleci-oidc]: https://circleci.com/docs/openid-connect-tokens/

<!-- commands -->
[cmd-lock]: ../reference/command-line/commands.md#lock

<!-- arguments -->
[arg-frozen]: ../reference/command-line/index.md#arg-frozen
[arg-offline]: ../reference/command-line/index.md#arg-offline

<!-- environment -->
[env-ci]: ../reference/environment.md#external-ci
[env-no-update-check]: ../reference/environment.md#ocx-no-update-check
[env-no-consent]: ../reference/environment.md#ocx-no-consent
[env-frozen]: ../reference/environment.md#ocx-frozen
[env-offline]: ../reference/environment.md#ocx-offline
[env-ocx-jobs]: ../reference/environment.md#ocx-jobs
[env-no-config]: ../reference/environment.md#ocx-no-config

<!-- reference -->
[reference-exit-codes]: ../reference/exit-codes.md

<!-- guides -->
[guide-work-offline]: ../guides/work-offline.mdx
[install-options]: ../getting-started/install-ocx.mdx#script-options
[signing-ci]: ../publishing/sign-a-release.md#ci
[publishing-ci-annotations]: ../publishing/build-and-push.mdx#source-annotation-ci
[explanation-shell-integration]: ../explanation/shell-integration.mdx

<!-- pages -->
[p-registry-auth-recipes]: ./registry-auth-recipes.md
[p-publisher-ci]: ./publisher-ci.md
[p-scripts-and-json]: ./scripts-and-json.md
[p-github-actions]: ./github-actions.md
[p-gitlab-ci]: ./gitlab-ci.md
