---
title: git-cliff
description: A highly customizable changelog generator that follows Conventional Commit specifications
keywords: git-cliff,changelog,conventional-commits,release,versioning,rust
---

# git-cliff

git-cliff is a highly customizable changelog generator that follows Conventional Commit specifications. It parses git history, extracts commits matching configurable patterns, and generates changelogs in a variety of formats. It supports custom templates, tag-based grouping, and integration into CI/CD pipelines.

## What's included

This package provides the following executables:

- **git-cliff** — the changelog generator CLI
- **git-cliff-completions** — generates shell completion scripts
- **git-cliff-mangen** — generates man pages

## Usage with OCX

```sh
# Install a specific version
ocx install git-cliff:2.12.0

# Run directly
ocx exec git-cliff:2.12.0 -- git-cliff --version

# Set as current
ocx install --select git-cliff:2.12.0
```

## Links

- [git-cliff Documentation](https://git-cliff.org)
- [git-cliff on GitHub](https://github.com/orhun/git-cliff)
