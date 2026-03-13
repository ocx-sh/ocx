---
# https://vitepress.dev/reference/default-theme-home-page
layout: home

hero:
  name: "ocx"
  text: "The Simple Package Manager"
  tagline: A fast cross-platform package manager designed to manage tools with ease and confidence.
  image: /logo.svg
  actions:
    - theme: brand
      text: Install
      link: /docs/installation
    - theme: alt
      text: Get Started
      link: /docs/getting-started
    - theme: alt
      text: Guide
      link: /docs/user-guide

features:
  - title: Zero Infrastructure
    icon: 🗄️
    details: Uses your existing OCI registry — Docker Hub, GHCR, ECR, or any private instance. No servers to host, no taps to maintain, no Artifactory license required.

  - title: Cross-Platform
    icon: 🖥️
    details: One identifier, automatic OS/arch detection. Multi-platform OCI manifests resolve the right binary for each machine — linux/amd64, darwin/arm64, Windows, and more.

  - title: Reproducible
    icon: 🔒
    details: Content-addressed storage with local index snapshots. The same index always resolves to the same binaries — online or offline. Pin to a digest for absolute reproducibility.

  - title: Built for Automation
    icon: ⚙️
    details: JSON output, clean isolated environments, composable commands. Designed to be called by CI pipelines, GitHub Actions, Bazel rules, and scripts — not just humans.
---

::: warning Early Development
OCX is in early development (v0.0.0). The CLI is functional but APIs may change. Packages shown in examples may not yet be available on the public registry — see the [getting started guide](/docs/getting-started) for what's available today.
:::

## Quick Start {#quick-start}

Install ocx with a single command:

::: code-group
```sh [Shell]
curl -fsSL https://ocx.sh/install.sh | sh
```

```ps1 [PowerShell]
irm https://ocx.sh/install.ps1 | iex
```
:::

Run any package instantly — no setup, no config:

```sh
ocx exec cmake:3.28 -- cmake --version
```

Install, pin, and switch between versions:

```sh
ocx install --select cmake:3.28       # install and activate
ocx exec cmake:3.28 -- cmake --build  # run with clean environment
ocx install --select cmake:3.29       # upgrade in one command
```

Compose multiple tools with isolated environments:

```sh
ocx exec java:21 plantuml:1 -- plantuml -version
```

That's it. No formulas, no plugins, no runtime dependencies. [Get started &rarr;](/docs/getting-started)
