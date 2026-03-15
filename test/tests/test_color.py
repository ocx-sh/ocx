"""Tests for ANSI color output and the ``--color`` flag."""

from __future__ import annotations

import re


ANSI_ESCAPE = re.compile(r"\x1b\[")


def test_color_never_suppresses_ansi(ocx):
    """``--color never`` output must not contain ANSI escape sequences."""
    result = ocx.run("--color", "never", "version", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes with --color never, got: {result.stdout!r}"
    )


def test_color_always_emits_ansi(ocx):
    """``--color always`` output must contain ANSI escape sequences (even when piped)."""
    result = ocx.run("--color", "always", "info", format=None)
    assert ANSI_ESCAPE.search(result.stdout), (
        f"Expected ANSI escapes with --color always, got: {result.stdout!r}"
    )


def test_default_piped_suppresses_ansi(ocx):
    """Default (piped through pytest) should not emit ANSI escape sequences."""
    result = ocx.run("info", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes when piped, got: {result.stdout!r}"
    )


def test_no_color_env_suppresses_ansi(ocx):
    """NO_COLOR=1 must suppress ANSI even without --color flag."""
    ocx.env["NO_COLOR"] = "1"
    result = ocx.run("info", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes with NO_COLOR=1, got: {result.stdout!r}"
    )


def test_clicolor_force_enables_ansi(ocx):
    """CLICOLOR_FORCE=1 must enable ANSI even when piped."""
    ocx.env["CLICOLOR_FORCE"] = "1"
    result = ocx.run("info", format=None)
    assert ANSI_ESCAPE.search(result.stdout), (
        f"Expected ANSI escapes with CLICOLOR_FORCE=1, got: {result.stdout!r}"
    )


def test_clicolor_zero_suppresses_ansi(ocx):
    """CLICOLOR=0 must suppress ANSI."""
    ocx.env["CLICOLOR"] = "0"
    result = ocx.run("info", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes with CLICOLOR=0, got: {result.stdout!r}"
    )


def test_term_dumb_suppresses_ansi(ocx):
    """TERM=dumb must suppress ANSI."""
    ocx.env["TERM"] = "dumb"
    result = ocx.run("info", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes with TERM=dumb, got: {result.stdout!r}"
    )


def test_color_always_overrides_no_color(ocx):
    """--color always must override NO_COLOR=1."""
    ocx.env["NO_COLOR"] = "1"
    result = ocx.run("--color", "always", "info", format=None)
    assert ANSI_ESCAPE.search(result.stdout), (
        f"Expected ANSI escapes with --color always despite NO_COLOR=1, got: {result.stdout!r}"
    )


def test_no_color_overrides_clicolor_force(ocx):
    """NO_COLOR takes precedence over CLICOLOR_FORCE."""
    ocx.env["NO_COLOR"] = "1"
    ocx.env["CLICOLOR_FORCE"] = "1"
    result = ocx.run("info", format=None)
    assert not ANSI_ESCAPE.search(result.stdout), (
        f"Expected no ANSI escapes: NO_COLOR should override CLICOLOR_FORCE, got: {result.stdout!r}"
    )
