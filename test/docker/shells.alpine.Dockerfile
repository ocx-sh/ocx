# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Alpine/musl "shell zoo" for the all-shell activation matrix
# (test/tests/test_shell_activation.py). Alpine's `/bin/sh` is busybox `ash` —
# the strictest POSIX shell in the matrix — so this leg guards the POSIX fence
# against the most minimal interpreter. It carries the apk-available login
# shells (ash/bash/zsh/fish/dash) plus python3 + pytest; nushell, elvish, and
# PowerShell are Debian-leg only and the module skips them here via
# `shutil.which`. The ocx binary under test (a musl build) is mounted at run
# time via OCX_ACTIVATION_BINARY.
FROM alpine:3.21

RUN apk add --no-cache \
        bash zsh fish dash \
        python3 py3-pytest

WORKDIR /work
