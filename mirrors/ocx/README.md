---
title: OCX
description: A fast, cross-platform binary package manager built on OCI registries
keywords: ocx,package-manager,oci,binary,cross-platform
---

# OCX

OCX is a fast, cross-platform binary package manager that uses OCI registries as storage. It turns any Docker/OCI registry — Docker Hub, GHCR, ECR, or a private instance — into a binary distribution platform. A single standalone binary with no runtime dependencies.

## What's included

This package provides the OCX command-line tool:

- **ocx** — install, manage, and execute pre-built binaries from OCI registries

## Usage with OCX

OCX can manage itself — install a specific version alongside your current one:

```sh
# Install a specific version
ocx install ocx:0.1

# Run directly
ocx exec ocx:0.1 -- ocx version

# Set as current
ocx install --select ocx:0.1
```

## Links

- [OCX Documentation](https://ocx.sh/docs/user-guide.html)
- [OCX on GitHub](https://github.com/ocx-sh/ocx)
- [Getting Started](https://ocx.sh/docs/getting-started.html)
