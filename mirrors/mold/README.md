---
title: mold
description: A modern, high-performance linker for Linux and Windows
keywords: mold,linker,ld,build,cpp,c,toolchain
---

# mold

mold is a modern linker that is several times faster than the default GNU ld and LLVM lld linkers. It is designed as a drop-in replacement — simply by switching the linker, build times for large C/C++ projects can be dramatically reduced without any source code changes.

## What's included

- **mold** — the mold linker binary
- **ld.mold** — symlink for use as a drop-in replacement via `-fuse-ld=mold`
- **lib/mold/mold-wrapper.so** — LD_PRELOAD wrapper for transparent linker substitution (Linux only)

## Requirements

On Linux, mold requires `libatomic` at runtime. Install it with your system package manager if not already present:

- **Debian/Ubuntu**: `apt install libatomic1`
- **Fedora/RHEL**: `dnf install libatomic`
- **Alpine**: `apk add libatomic`

## Links

- [mold on GitHub](https://github.com/rui314/mold)
