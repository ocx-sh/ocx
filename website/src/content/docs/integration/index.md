---
title: "Integrate OCX"
description: "Find the page that connects OCX to your CI system, your container, your editor, your build tool or your own scripts."
---
<!-- doc_type: landing -->
<!-- doc_tier: integration -->

Pick the system you want to connect to OCX, and open the page that shows the shortest working setup.

## Run OCX in a pipeline {#integration-ci}

- [Use OCX in GitHub Actions][p-github-actions] puts a pinned toolchain's `PATH` entries and variables in every step.
- [Use OCX in GitLab CI][p-gitlab-ci] does the same for script jobs and step runner jobs.
- [Run OCX unattended in a pipeline][p-unattended-ci] lists the settings that make a job reproducible and offline-safe.
- [Authenticate to GHCR and ECR][p-registry-auth-recipes] gives a job credentials for a private registry.
- [Publish packages from CI][p-publisher-ci] pushes, signs and promotes a package with an SBOM.

## Connect other tools {#integration-tools}

- [Use OCX in an editor][p-editors] shares the toolchain with VS Code and any other editor.
- [Dev Container Feature][p-integrations-devcontainer] installs OCX and restores the project toolchain with one line in `devcontainer.json`.
- [Docker][p-docker] covers the official image and baking a toolchain into your own.
- [Use OCX with Bazel][p-bazel] lists what `rules_ocx` asks of OCX.
- [Use OCX with Python][p-python] drives OCX from Python and ships a locked Python application.
- [Read OCX output from a script][p-scripts-and-json] parses JSON, exit codes and the error document.

## Work behind a proxy {#integration-network}

- [Proxy and corporate CA][p-proxy-and-ca] sets the proxy variables and the certificate roots.

## Other integrations {#integration-more}

Project documentation lives on separate sites: [rules_ocx (Bazel)][p-integrations-bazel], [ocx-sdk (Python)][p-integrations-python] [CMake][p-integrations-cmake] and the [Dev Container Feature][p-integrations-devcontainer].

<!-- pages -->
[p-github-actions]: ./github-actions.mdx
[p-gitlab-ci]: ./gitlab-ci.md
[p-unattended-ci]: ./unattended-ci.mdx
[p-registry-auth-recipes]: ./registry-auth-recipes.mdx
[p-publisher-ci]: ./publisher-ci.mdx
[p-editors]: ./editors.mdx
[p-docker]: ./docker.mdx
[p-bazel]: ./bazel.mdx
[p-python]: ./python.mdx
[p-scripts-and-json]: ./scripts-and-json.mdx
[p-proxy-and-ca]: ./proxy-and-ca.mdx
[p-integrations-bazel]: /integrations/bazel/
[p-integrations-python]: /integrations/python/
[p-integrations-cmake]: /integrations/cmake/
[p-integrations-devcontainer]: /integrations/devcontainer/
