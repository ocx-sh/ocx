---
title: CPython
description: Pre-built, PGO+LTO-optimized CPython binaries from python-build-standalone
keywords: python,cpython,runtime,interpreter,pip
---

# CPython

Pre-built CPython binaries from the [python-build-standalone](https://github.com/astral-sh/python-build-standalone) project by Astral. These are PGO+LTO-optimized, self-contained builds that work without any system dependencies.

## What's included

- **python3** — CPython interpreter
- **pip3** — Python package installer
- **python3-config** — build configuration helper

## Available versions

- **CPython 3.14** — latest development series
- **CPython 3.13** — current stable
- **CPython 3.12** — previous stable (LTS)

## Version scheme

OCX versions match Python patch versions directly (e.g., `3.13.9`). Pre-release versions (alpha, beta, rc) are excluded.

Use `cpython:3.13` for the latest 3.13.x release, or `cpython:latest` for the newest release of the highest minor series.

## Links

- [python-build-standalone](https://github.com/astral-sh/python-build-standalone)
- [Python Documentation](https://docs.python.org/3/)
- [Python Downloads](https://www.python.org/downloads/)
