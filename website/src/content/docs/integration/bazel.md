---
title: "Use ocx with Bazel"
description: "What the rules_ocx Bazel module asks of ocx, and which ocx settings reach a Bazel repository rule."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to provision a [Bazel][bazel] workspace's tools through ocx with the `rules_ocx` module, and which ocx settings apply while Bazel fetches them. It covers the ocx side only. The Bazel side lives in the [`rules_ocx` documentation][rules-ocx-docs].

`rules_ocx` never re-implements ocx in Starlark. It downloads a pinned `ocx` binary inside a repository rule and drives that binary. The contracts it relies on are your `ocx.lock` and the OCI manifests.

## Provision the workspace toolchain {#bazel-project}

Start from a Bazel workspace with bzlmod enabled and an `ocx.lock` that [`ocx lock`][cmd-lock] wrote next to your `ocx.toml`. Add the module, point it at both files, and list the repository it creates:

```starlark
# MODULE.bazel
bazel_dep(name = "rules_ocx", version = "0.5.0")

ocx = use_extension("@rules_ocx//ocx:extensions.bzl", "ocx")

ocx.project(
    name = "tools",
    ocx_toml = "//:ocx.toml",
    ocx_lock = "//:ocx.lock",
)

use_repo(ocx, "tools")
```

Every executable that the toolchain's packages declare as their public surface becomes a runnable target named `@tools//:<name>`. Use it in `tools =`, in `$(location ...)` and in `bazel run`.

A test target consumes one through `data` and finds its path with `$(location ...)`:

```starlark
# BUILD.bazel
load("@rules_shell//shell:sh_test.bzl", "sh_test")

sh_test(
    name = "lint",
    srcs = ["lint_test.sh"],
    data = ["@tools//:shellcheck"],
    env = {"SHELLCHECK_BIN": "$(location @tools//:shellcheck)"},
)
```

Run `bazel run` to start a tool directly, and `bazel query` to list the targets the rule created:

```shell
bazel run @tools//:shellcheck -- --version
bazel query @tools//...
```

```text
ShellCheck - shell script analysis tool
version: <version>
...
@tools//:shellcheck
@tools//:shfmt
```

Each tool in your `ocx.toml` appears in the query output, one label per executable. A declared tool that is missing raises no error, so check the list when a target you expect is absent.

The same toolchain then serves your shell, your CI and Bazel. A fetch uses the shared `OCX_HOME` store, so a package that a developer pulled is not downloaded again for Bazel on that machine. The [project toolchain][explanation-project-toolchain] page explains how `ocx.toml` and `ocx.lock` relate.

A single package that is not in your project takes `ocx.package(name = "jq", package = "ocx.sh/jqlang/jq:latest")`. Pin it by digest when the build must not move.

## What the rules ask of ocx {#bazel-commands}

A repository rule calls four ocx commands, so those are the ones to test when a fetch fails:

| Step | Command |
|---|---|
| Staleness gate | [`ocx lock --check`][cmd-lock] |
| Download | [`ocx pull`][cmd-pull] for a project, `ocx package install` for a single package |
| Environment | `ocx --format json env --pinned` |
| Failure reading | The [exit code and error document][p-scripts-and-json] of whichever step failed |

A stale lock fails the fetch with exit 65, the same as in [any pipeline][p-nattended-ci-unattended-frozen]. Run `ocx lock` and commit the result.

## Settings that reach a fetch {#bazel-env}

Bazel runs a repository rule from its own working directory, with its own environment. `rules_ocx` therefore forwards a fixed set of variables and sets a few others itself. The list that matters to an operator:

- Registry traffic: [`OCX_MIRRORS`][env-mirrors], [`OCX_INSECURE_REGISTRIES`][env-insecure] and the `OCX_AUTH_<REGISTRY>_*` variables from [Registry authentication recipes][p-istry-auth-recipes-auth-method]. Bazel cannot enumerate the auth variables, so run `bazel fetch --force` after you change one.
- Resolution: `OCX_INDEX`, `OCX_OFFLINE`, `OCX_FROZEN`, `OCX_REMOTE`, `OCX_JOBS` and `OCX_DEFAULT_REGISTRY`.
- Configuration: `OCX_CONFIG`, `OCX_NO_CONFIG`, `OCX_MANAGED_CONFIG` and `OCX_PATCHES`.
- Trust: `OCX_SIGSTORE_TRUSTED_ROOT` and `OCX_EXTRA_CA_CERTS`. The second one is how a [corporate proxy CA][p-proxy-and-ca-ca-env] reaches Bazel.

`rules_ocx` does not forward `OCX_NO_VERIFY` or `OCX_ALLOW_YANKED` to a fetch. A build that must skip verification or accept a yanked package says so in `MODULE.bazel` with `ocx.policy(...)`, so an exported variable cannot weaken a fetch silently.

The `rules_ocx` guides cover the Bazel half of this: the [offline mirror guide][rules-ocx-mirror] and the [trust and config concepts][rules-ocx-trust].

## Choose a different surface {#bazel-next}

- Without Bazel, use [GitHub Actions][p-github-actions], [GitLab CI][p-gitlab-ci] or the [Docker][p-docker] image.
- To run Bazel itself from ocx, list `bazel` in your `ocx.toml` and call `ocx exec -- bazel ...`.

<!-- external -->
[bazel]: https://bazel.build/
[rules-ocx-docs]: /integrations/bazel/
[rules-ocx-mirror]: /integrations/bazel/guides/offline-mirror/
[rules-ocx-trust]: /integrations/bazel/concepts/trust-and-config/

<!-- commands -->
[cmd-lock]: ../reference/command-line/commands.md#lock
[cmd-pull]: ../reference/command-line/commands.md#pull

<!-- environment -->
[env-mirrors]: ../reference/environment.md#ocx-mirrors
[env-insecure]: ../reference/environment.md#ocx-insecure-registries

<!-- guides -->
[explanation-project-toolchain]: ../explanation/project-toolchain.md

<!-- pages -->
[p-scripts-and-json]: ./scripts-and-json.md
[p-nattended-ci-unattended-frozen]: ./unattended-ci.md#unattended-frozen
[p-istry-auth-recipes-auth-method]: ./registry-auth-recipes.md#auth-method
[p-proxy-and-ca-ca-env]: ./proxy-and-ca.md#ca-env
[p-github-actions]: ./github-actions.md
[p-gitlab-ci]: ./gitlab-ci.md
[p-docker]: ./docker.md
