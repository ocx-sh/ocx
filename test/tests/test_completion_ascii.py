# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests: completion / activation output must be ASCII-only.

Two CLI paths emit shell-completion text, and both can be captured by Windows
PowerShell 5.1, which decodes the stream under the active console codepage (not
UTF-8) -- a single non-ASCII byte in a help string corrupts the parsed script:

  * ``ocx shell completion --shell=X`` -- the standalone generator.
  * ``ocx self activate --shell=X --completion`` -- the PRODUCTION path: the
    ``$OCX_HOME/env.sh`` / ``env.ps1`` shim runs this on every shell start and
    evals the stream inline. This is the path the original WinPS parse-error
    cascade actually shipped through, so it is the more important of the two.

Both drive the SHIPPED binary across every shell value, asserting ASCII bytes
(raw bytes -- never a lossy decode that could hide a non-ASCII byte). They
complement the two in-crate unit guards that cover the generator function
directly: ``app::tests::cli_help_text_is_ascii`` (whole clap help tree) and
``command::self_group::activate::tests::completion_output_is_ascii_for_all_shells``
(the inline activation generator).

Runs on every platform (including Windows in verify-deep.yml): completion bytes
are platform-independent, and Windows is the platform the hazard targets.
"""

from __future__ import annotations

import subprocess

import pytest

from src.runner import OcxRunner

# Shells that clap_complete has a backend for -- the only values
# `ocx shell completion` emits a script for (others exit 64).
_BACKEND_SHELLS = ("bash", "zsh", "fish", "elvish", "powershell")

# `ocx self activate` accepts the full `--shell` value enum and always emits a
# non-empty stream: completion + PATH + global-env eval for backend shells,
# PATH + global-env eval (no completion block) for the rest. Every byte of that
# stream is eval'd by the shim, so all of it -- not just the completion block --
# must be ASCII.
_ALL_SHELLS = (
    *_BACKEND_SHELLS,
    "pwsh",
    "nushell",
    "ash",
    "dash",
    "ksh",
    "sh",
    "batch",
)


def _assert_ascii(label: str, stdout: bytes) -> None:
    offenders = [offset for offset, byte in enumerate(stdout) if byte > 0x7F]
    assert not offenders, (
        f"{label} emitted {len(offenders)} non-ASCII byte(s) (first at offsets "
        f"{offenders[:10]}); Windows PowerShell 5.1 misreads these under the "
        "console codepage. Find the offending CLI help text and replace the "
        "non-ASCII char (`->` for arrow, `-` for em-dash, `...` for ellipsis)."
    )


def _capture(ocx: OcxRunner, *args: str) -> subprocess.CompletedProcess[bytes]:
    """Run the shipped binary, capturing raw bytes (text=False) for byte-exact
    ASCII inspection."""
    return subprocess.run(
        [str(ocx.binary), *args],
        capture_output=True,
        env=ocx.env,
    )


# ---------------------------------------------------------------------------
# ocx shell completion
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", _BACKEND_SHELLS)
def test_shell_completion_is_ascii(ocx: OcxRunner, shell: str) -> None:
    """`ocx shell completion --shell=<shell>` emits a non-empty, ASCII-only script."""
    result = _capture(ocx, "shell", "completion", f"--shell={shell}")
    assert result.returncode == 0, (
        f"`shell completion --shell={shell}` must exit 0; got rc={result.returncode}\n"
        f"stderr:\n{result.stderr.decode(errors='replace')}"
    )
    assert result.stdout, f"`shell completion --shell={shell}` produced no completion script"
    _assert_ascii(f"`shell completion --shell={shell}`", result.stdout)


# ---------------------------------------------------------------------------
# ocx self activate --completion  (the production shim path)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", _ALL_SHELLS)
def test_self_activate_completion_is_ascii(ocx: OcxRunner, shell: str) -> None:
    """`ocx self activate --shell=<shell> --completion` emits an ASCII-only stream.

    `--completion` forces the completion block on even though a subprocess pipe
    is never a TTY, so the completion-bearing path is exercised for every shell
    that has a backend.
    """
    result = _capture(ocx, "self", "activate", f"--shell={shell}", "--completion")
    assert result.returncode == 0, (
        f"`self activate --shell={shell} --completion` must exit 0; got rc={result.returncode}\n"
        f"stderr:\n{result.stderr.decode(errors='replace')}"
    )
    assert result.stdout, f"`self activate --shell={shell} --completion` produced no output"
    _assert_ascii(f"`self activate --shell={shell} --completion`", result.stdout)
