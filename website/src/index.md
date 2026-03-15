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
    details: Your OCI registry is the package server. No taps, no Artifactory.

  - title: Cross-Platform
    icon: 🖥️
    details: One identifier. Auto-detects OS and arch via OCI manifests.

  - title: Reproducible
    icon: 🔒
    details: Content-addressed storage. Same index, same binaries — online or offline.

  - title: Built for Automation
    icon: ⚙️
    details: JSON output, clean environments, composable commands. CI-first.
---

::: warning Early Development
OCX is in early development (v0.0.0). The CLI is functional but APIs may change. Packages shown in examples may not yet be available on the public registry.
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
ocx exec node:22 bun:1 -- bun --version
```

That's it. No formulas, no plugins, no runtime dependencies. [Get started &rarr;](/docs/getting-started)
