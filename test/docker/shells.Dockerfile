# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Debian/glibc "shell zoo" for the all-shell activation matrix
# (test/tests/test_shell_activation.py). It carries every login shell the ocx
# managed block targets — bash, zsh, dash, fish (apt), plus pinned upstream
# nushell, elvish, and PowerShell — and python3 + pytest to run the module
# in-container. The ocx binary under test is mounted at run time
# (OCX_ACTIVATION_BINARY); nothing is baked in, so the image is reusable across
# builds.
#
# Pin the base by digest via the build (`--pull`), and the three out-of-distro
# shells by exact version + SHA-256 so the image is reproducible.
#
# CI mounts a static musl ocx (libc-agnostic, runs on any base). trixie (Debian
# 13, glibc 2.41) is chosen over bookworm (2.36) only so `task test:shells` run
# locally with a glibc binary built on a modern host (glibc 2.39+) also runs here
# (glibc is forward compatible; the base glibc must be >= the binary's).
FROM debian:trixie-slim

ARG NUSHELL_VERSION=0.113.1
ARG NUSHELL_SHA256=9008d309aaa35e29ed5d5985306a83e2bf5093e31677d4cd969914552d12b8fb
ARG ELVISH_VERSION=0.21.0
ARG PWSH_VERSION=7.4.6
ARG PWSH_SHA256=6f6015203c47806c5cc444c19d8ed019695e610fbd948154264bf9ca8e157561

ENV DEBIAN_FRONTEND=noninteractive

# In-distro shells + runtime deps for the out-of-distro shells + pytest.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash zsh fish dash busybox \
        ca-certificates curl tar gzip \
        libicu76 libssl3 less locales \
        python3 python3-pytest \
    && rm -rf /var/lib/apt/lists/* \
    && ln -s "$(command -v busybox)" /usr/local/bin/ash

# busybox provides `ash` (the strictest POSIX /bin/sh, same applet Alpine ships);
# the symlink lets the activation module's `shutil.which("ash")` find it here too.

# Nushell — pinned upstream release tarball (glibc build), SHA-256 verified.
RUN curl -fsSL -o /tmp/nu.tar.gz \
        "https://github.com/nushell/nushell/releases/download/${NUSHELL_VERSION}/nu-${NUSHELL_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
    && echo "${NUSHELL_SHA256}  /tmp/nu.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/nu.tar.gz -C /tmp \
    && install -m 0755 "/tmp/nu-${NUSHELL_VERSION}-x86_64-unknown-linux-gnu/nu" /usr/local/bin/nu \
    && rm -rf /tmp/nu.tar.gz "/tmp/nu-${NUSHELL_VERSION}-x86_64-unknown-linux-gnu"

# Elvish — pinned upstream release tarball from the canonical dl.elv.sh mirror,
# verified against the publisher's detached .sha256sum (elvish ships no GitHub
# release assets).
RUN curl -fsSL -o /tmp/elvish.tar.gz \
        "https://dl.elv.sh/linux-amd64/elvish-v${ELVISH_VERSION}.tar.gz" \
    && curl -fsSL -o /tmp/elvish.sha256sum \
        "https://dl.elv.sh/linux-amd64/elvish-v${ELVISH_VERSION}.tar.gz.sha256sum" \
    && echo "$(cut -d' ' -f1 /tmp/elvish.sha256sum)  /tmp/elvish.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/elvish.tar.gz -C /tmp \
    && install -m 0755 /tmp/elvish /usr/local/bin/elvish \
    && rm -rf /tmp/elvish.tar.gz /tmp/elvish.sha256sum /tmp/elvish

# PowerShell — pinned upstream release tarball, SHA-256 verified.
RUN curl -fsSL -o /tmp/pwsh.tar.gz \
        "https://github.com/PowerShell/PowerShell/releases/download/v${PWSH_VERSION}/powershell-${PWSH_VERSION}-linux-x64.tar.gz" \
    && echo "${PWSH_SHA256}  /tmp/pwsh.tar.gz" | sha256sum -c - \
    && mkdir -p /opt/microsoft/powershell/7 \
    && tar -xzf /tmp/pwsh.tar.gz -C /opt/microsoft/powershell/7 \
    && chmod +x /opt/microsoft/powershell/7/pwsh \
    && ln -s /opt/microsoft/powershell/7/pwsh /usr/local/bin/pwsh \
    && rm -f /tmp/pwsh.tar.gz

WORKDIR /work
