---
title: "Integrate ocx"
description: "Find the page that connects ocx to your CI system, your container, your editor, your build tool or your own scripts."
---
<!-- doc_type: landing -->
<!-- doc_tier: integration -->

Pick the system you want to connect to ocx, and open the page that shows the shortest working setup.

## Run ocx in a pipeline {#integration-ci}

- [Use ocx in GitHub Actions][p-github-actions] puts a pinned toolchain on `PATH` for every step.
- [Use ocx in GitLab CI][p-gitlab-ci] does the same for script jobs and step runner jobs.
- [Run ocx unattended][p-unattended-ci] lists the settings that make a job reproducible and offline-safe.
- [Authenticate to GHCR and ECR from CI][p-registry-auth-recipes] gives a job credentials for a private registry.
- [Publish packages from CI][p-publisher-ci] pushes, signs and promotes a package with an SBOM.

## Connect other tools {#integration-tools}

- [Use ocx in dev containers and editors][p-devcontainers-and-editors] shares the toolchain with VS Code and a dev container.
- [Docker][p-docker] covers the official image and baking a toolchain into your own.
- [Use ocx with Bazel][p-bazel] lists what `rules_ocx` asks of ocx.
- [Use ocx with Python][p-python] drives ocx from Python and ships a locked Python application.
- [Read ocx output from a script][p-scripts-and-json] parses JSON, exit codes and the error document.

## Work behind a proxy {#integration-network}

- [Trust a corporate proxy and its CA][p-proxy-and-ca] sets the proxy variables and the certificate roots.

## Other integrations {#integration-more}

Separate projects have their own sites: [Bazel][p-integrations-bazel], [Python][p-integrations-python] and [CMake][p-integrations-cmake].

<!-- pages -->
[p-github-actions]: ./github-actions.md
[p-gitlab-ci]: ./gitlab-ci.md
[p-unattended-ci]: ./unattended-ci.md
[p-registry-auth-recipes]: ./registry-auth-recipes.md
[p-publisher-ci]: ./publisher-ci.md
[p-devcontainers-and-editors]: ./devcontainers-and-editors.md
[p-docker]: ./docker.md
[p-bazel]: ./bazel.md
[p-python]: ./python.md
[p-scripts-and-json]: ./scripts-and-json.md
[p-proxy-and-ca]: ./proxy-and-ca.md
[p-integrations-bazel]: /integrations/bazel/
[p-integrations-python]: /integrations/python/
[p-integrations-cmake]: /integrations/cmake/
