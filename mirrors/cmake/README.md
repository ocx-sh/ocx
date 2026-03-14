---
title: CMake
description: Cross-platform build system generator for C and C++ projects
keywords: cmake,build,cpp,c,build-system,cross-platform
---

# CMake

CMake is an open-source, cross-platform build system generator. It produces native build files — Makefiles, Ninja scripts, Visual Studio projects, Xcode workspaces — from a single, platform-independent configuration. Originally developed by Kitware, CMake is the de facto standard for building C and C++ projects across Linux, macOS, and Windows.

## What's included

This package provides the CMake command-line tools:

- **cmake** — configure and generate build systems
- **ctest** — run test suites
- **cpack** — create installers and packages

## Usage with OCX

```sh
# Install a specific version
ocx install cmake:3.31

# Run directly
ocx exec cmake:3.31 -- cmake --version

# Set as current
ocx install cmake:3.31 --select
```

## Links

- [CMake Documentation](https://cmake.org/cmake/help/latest/)
- [CMake on GitHub](https://github.com/Kitware/CMake)
- [CMake Download Page](https://cmake.org/download/)
