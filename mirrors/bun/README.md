---
title: Bun
description: Incredibly fast JavaScript runtime, bundler, test runner, and package manager
keywords: bun,javascript,typescript,runtime,bundler,package-manager,test-runner,node
---

# Bun

Bun is an incredibly fast JavaScript runtime, bundler, test runner, and package manager — all in one. Built from scratch using Zig and JavaScriptCore, it is designed as a drop-in replacement for Node.js with dramatically better performance and a batteries-included developer experience.

## What's included

This package provides the Bun command-line tool:

- **bun** — JavaScript/TypeScript runtime, package manager, bundler, and test runner

## Usage with OCX

```sh
# Install a specific version
ocx install bun:1.3.10

# Run directly
ocx exec bun:1.3.10 -- bun --version

# Set as current
ocx install --select bun:1.3.10
```

## Links

- [Bun Documentation](https://bun.sh/docs)
- [Bun on GitHub](https://github.com/oven-sh/bun)
