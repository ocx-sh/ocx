# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Debian/glibc "shell zoo" for the all-shell activation matrix
# (test/tests/test_shell_activation.py) and the per-prompt reconciler matrix
# (test/tests/test_shell_reconcile.py). It carries every login shell the ocx
# managed block targets — bash, zsh, dash, fish (apt), plus pinned upstream
# nushell, elvish, and PowerShell — the three third-party prompt frameworks the
# coexistence rows need (starship, oh-my-zsh, powerlevel10k), and python3 +
# pytest to run the modules in-container. The ocx binary under test is mounted
# at run time (OCX_ACTIVATION_BINARY); nothing is baked in, so the image is
# reusable across builds.
#
# Pin the base by digest via the build (`--pull`), and every out-of-distro
# component by exact version + SHA-256 (release tarballs) or by exact commit
# SHA-1 (git checkouts) so the image is reproducible.
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
ARG STARSHIP_VERSION=1.26.0
ARG STARSHIP_SHA256=b7c232b0e8249d8e55a40beb79c5c43a7d370f3f9408bd215deb0170daeaadf3
# oh-my-zsh publishes no releases and powerlevel10k's tags are served only as
# GitHub auto-generated archives, whose bytes are NOT stable across
# regeneration — a SHA-256 pin on those tarballs breaks for a non-reason. The
# commit SHA-1 IS the checksum for a git checkout, so both are pinned that way
# and the checkout is verified against the pin before the tree is used.
ARG OHMYZSH_COMMIT=146461f7c6d95f4ba1220559d66eb113418b40a8
# powerlevel10k v1.20.0 (annotated tag ff0311157d6b24fea21aa70699783f362b0f554f).
ARG POWERLEVEL10K_COMMIT=35833ea15f14b71dbcebc7e54c104d8d56ca5268

ENV DEBIAN_FRONTEND=noninteractive

# In-distro shells + runtime deps for the out-of-distro shells + pytest.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash zsh fish dash busybox \
        ca-certificates curl tar gzip git \
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

# ---------------------------------------------------------------------------
# Third-party prompt frameworks — the coexistence rows' subjects
# (test_shell_reconcile.py::test_prompt_hook_coexists_with_a_third_party_prompt_framework).
# Installed system-wide under /usr/local/bin and /usr/share so they resolve
# regardless of $HOME: the matrix runs every shell with a per-test arena HOME,
# so anything under /root would be invisible to it.
# ---------------------------------------------------------------------------

# Starship — pinned upstream release tarball (static musl build, so it is
# libc-agnostic like the ocx binary the zoo mounts), SHA-256 verified against a
# pin rather than against the publisher's own detached sum.
RUN curl -fsSL -o /tmp/starship.tar.gz \
        "https://github.com/starship/starship/releases/download/v${STARSHIP_VERSION}/starship-x86_64-unknown-linux-musl.tar.gz" \
    && echo "${STARSHIP_SHA256}  /tmp/starship.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/starship.tar.gz -C /tmp \
    && install -m 0755 /tmp/starship /usr/local/bin/starship \
    && rm -f /tmp/starship.tar.gz /tmp/starship \
    && starship --version

# oh-my-zsh and powerlevel10k — shallow single-commit fetches pinned by SHA-1.
# `git fetch --depth 1 origin <sha>` works because GitHub serves
# uploadpack.allowReachableSHA1InWant; `rev-parse HEAD` is then compared to the
# pin, so a server that returned a different object fails the build instead of
# baking an unpinned tree into the image. `.git` is dropped afterwards — the
# frameworks are sourced, never updated in place.
RUN set -eu; \
    fetch_pinned() { \
        dir="$1"; url="$2"; sha="$3"; \
        git init --quiet "$dir"; \
        git -C "$dir" remote add origin "$url"; \
        git -C "$dir" fetch --quiet --depth 1 origin "$sha"; \
        git -C "$dir" checkout --quiet --detach FETCH_HEAD; \
        got="$(git -C "$dir" rev-parse HEAD)"; \
        [ "$got" = "$sha" ] || { echo "pin mismatch for $url: want $sha, got $got" >&2; exit 1; }; \
        rm -rf "$dir/.git"; \
    }; \
    fetch_pinned /usr/share/oh-my-zsh https://github.com/ohmyzsh/ohmyzsh.git "${OHMYZSH_COMMIT}"; \
    fetch_pinned /usr/share/powerlevel10k https://github.com/romkatv/powerlevel10k.git "${POWERLEVEL10K_COMMIT}"; \
    test -f /usr/share/oh-my-zsh/oh-my-zsh.sh; \
    test -f /usr/share/powerlevel10k/powerlevel10k.zsh-theme

# oh-my-zsh writes its completion dump and cache under $ZSH when ZSH_CACHE_DIR
# is unset. The matrix sources it from an arena HOME, so the shared tree must be
# world-writable or the very first prompt errors out.
RUN mkdir -p /usr/share/oh-my-zsh/cache /usr/share/oh-my-zsh/log \
    && chmod -R a+w /usr/share/oh-my-zsh/cache /usr/share/oh-my-zsh/log

# powerlevel10k runs its interactive `p10k configure` wizard on the first
# interactive prompt when no configuration is in scope, and the wizard swallows
# the pty session the coexistence row drives — the row then fails with an
# unrelated "Choice [ynrq]" transcript rather than a reconciler verdict.
# `/etc/zsh/zshenv` is the one zsh startup file still read under `--no-rcs`
# (which is how the matrix launches zsh), so it is the only place the flag can
# be set for a test that deliberately reads no rc file and runs with an arena
# HOME. Setting a single POWERLEVEL9K_* variable is inert in every shell that
# does not load the theme.
RUN printf '%s\n' \
        '' \
        '# ocx shell zoo: keep powerlevel10k non-interactive (see shells.Dockerfile).' \
        'export POWERLEVEL9K_DISABLE_CONFIGURATION_WIZARD=true' \
        >> /etc/zsh/zshenv

WORKDIR /work
