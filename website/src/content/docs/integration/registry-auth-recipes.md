---
title: "Authenticate to GHCR and ECR from CI"
description: "Copy-paste recipes that give an ocx job credentials for GitHub Container Registry and Amazon ECR."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to give an ocx job credentials for a private registry. It has a recipe each for GitHub Container Registry (GHCR) and Amazon ECR. The recipes need no terminal and leave nothing on a throwaway runner.

ocx reads credentials per registry. `OCX_AUTH_*` variables win, then the Docker configuration. [Authenticate to a registry][guide-authenticate] explains the layers. Here you pick the one that suits a runner.

## Pick a method {#auth-method}

| Method | Use it when |
|---|---|
| `OCX_AUTH_<REGISTRY>_*` variables | The token is in the CI secret store and the job runs only ocx. This is the default choice. |
| `ocx login --password-stdin` | Another tool in the same job reads `~/.docker/config.json` too. |
| A Docker credential helper | The runner is long-lived and a helper can mint short-lived tokens itself. |

Variables are scoped to one registry. The registry name in the variable replaces every non-alphanumeric character with `_`, so `ghcr.io` becomes `ghcr_io`. The reference lists the [type][env-auth-type], [user][env-auth-user] and [token][env-auth-token] variables.

If `TYPE` is omitted, ocx infers it: `USER` and `TOKEN` together mean `basic`, and `TOKEN` alone means `bearer`.

## GitHub Container Registry {#auth-ghcr}

GHCR takes your GitHub username as the user and a token as the password. In GitHub Actions the built-in `GITHUB_TOKEN` is enough for packages the repository can read. Grant the job `packages: read`, or `packages: write` to push.

Set the variables on the **job**, not on one step. The [`setup-ocx`][setup-ocx] action pulls the toolchain before your own steps run, and it needs the credential to do so.

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: read
    env:
      OCX_AUTH_ghcr_io_TYPE: basic
      OCX_AUTH_ghcr_io_USER: ${{ github.actor }}
      OCX_AUTH_ghcr_io_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - uses: actions/checkout@v4
      - uses: ocx-sh/setup-ocx@v1
      - run: ocx exec -- my-private-tool --version
```

A private package must also allow the repository in its package settings. A token that works for `docker login ghcr.io` works here.

To share the credential with other tools, log in once instead. Map the token into the step with `env: GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}`, because the runner exports `GITHUB_ACTOR` but not `GITHUB_TOKEN`:

```sh
echo "$GITHUB_TOKEN" | ocx login ghcr.io --username "$GITHUB_ACTOR" --password-stdin --allow-insecure-store
```

`--allow-insecure-store` is explained [below](#auth-store).

## Amazon ECR {#auth-ecr}

ECR issues a password with the AWS CLI. The user is always `AWS`, and the password is valid for 12 hours, so fetch it inside the job. The registry host is `<account>.dkr.ecr.<region>.amazonaws.com`.

```sh
export OCX_AUTH_123456789012_dkr_ecr_eu_west_1_amazonaws_com_TYPE=basic
export OCX_AUTH_123456789012_dkr_ecr_eu_west_1_amazonaws_com_USER=AWS
export OCX_AUTH_123456789012_dkr_ecr_eu_west_1_amazonaws_com_TOKEN="$(aws ecr get-login-password --region eu-west-1)"
ocx pull
```

The long names come from the host: `123456789012.dkr.ecr.eu-west-1.amazonaws.com` with every dot and hyphen turned into `_`.

The login form writes the same credential to the Docker configuration:

```sh
aws ecr get-login-password --region eu-west-1 \
  | ocx login 123456789012.dkr.ecr.eu-west-1.amazonaws.com --username AWS --password-stdin --allow-insecure-store
```

On a runner that stays up, the [Amazon ECR credential helper][ecr-helper] mints tokens on demand. Name it for the registry in `~/.docker/config.json`, and ocx delegates to it:

```json
{ "credHelpers": { "123456789012.dkr.ecr.eu-west-1.amazonaws.com": "ecr-login" } }
```

## Storing credentials on a runner {#auth-store}

[`ocx login`][cmd-login] writes to the same `~/.docker/config.json` as `docker login`. It prefers a credential helper or a keychain. A runner has neither, so the command refuses with exit 78 unless you pass `--allow-insecure-store`. That flag stores the credential as plaintext base64 in the config file.

Use it only on a runner that is destroyed after the job. The `OCX_AUTH_*` variables take precedence over a stored credential at read time. [`ocx logout`][cmd-logout] exits 0 when no credential is stored, but a failed revocation exits non-zero. A cleanup step that must never fail needs `|| true`.

Never bake a token into an image with `ENV` or `ARG`. The [Docker page][p-docker-build-auth] shows the BuildKit secret form.

## When a job still gets 401 {#auth-troubleshoot}

A refused credential exits 80 (`AuthError`). [`ocx login`][cmd-login] checks the credential against the registry first unless you pass `--no-verify`. Check these in order:

1. The variable name matches the registry host after the `_` substitution, including the account id and region for ECR.
2. `TYPE` is `basic` for a user and password pair, and `bearer` or `token` for a bare token.
3. The token has the scope for the action. Pushing needs write access, and pulling needs read.
4. A mirror in `[mirrors]` needs its own credential. [Auth for the mirror][guide-mirror-auth] covers it.

<!-- external -->
[setup-ocx]: https://github.com/ocx-sh/setup-ocx
[ecr-helper]: https://github.com/awslabs/amazon-ecr-credential-helper

<!-- commands -->
[cmd-login]: ../reference/command-line/commands.md#login
[cmd-logout]: ../reference/command-line/commands.md#logout

<!-- environment -->
[env-auth-type]: ../reference/environment.md#ocx-auth-registry-type
[env-auth-user]: ../reference/environment.md#ocx-auth-registry-user
[env-auth-token]: ../reference/environment.md#ocx-auth-registry-token

<!-- guides -->
[guide-authenticate]: ../guides/authenticate.mdx
[guide-mirror-auth]: ../guides/corporate-mirror.mdx#mirrors-auth

<!-- pages -->
[p-docker-build-auth]: ./docker.md#build-auth
