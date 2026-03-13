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
  - title: Instant Setup
    icon: 🚀
    details: Single binary download, zero dependencies.

  - title: Cross Platform
    icon: 🖥️
    details:
      Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor
      incididunt ut labore et dolore magna aliqua.

  - title: Smart Caching
    icon: 🎯
    details:
      Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor
      incididunt ut labore et dolore magna aliqua.

  - title: Blazing Fast
    icon: ⚡
    details:
      Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor
      incididunt ut labore et dolore magna aliqua.
---

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
