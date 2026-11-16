# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Packages, tags and version output the docs may show, as read from ocx.sh on SNAPSHOT.

A tag maps to the exact output a run printed, or to ``None``: it exists, but no page may
show its output. Nothing here reads the network; the lint and the doc-script fixtures
only read this file. Refresh by hand with network: `.claude/rules/subsystem-tests.md`,
lint tier admission.
"""

from __future__ import annotations

SNAPSHOT = "2026-11-16"

# Namespaces the docs use for "your own package": an identifier under one of
# these is never checked against the table.
PLACEHOLDER_NAMESPACES = frozenset(
    {
        "acme",
        "acme-bot",
        "acme-corp",
        "acme-labs",
        "corp",
        "example",
        "internal",
        "me",
        "my",
        "myorg",
        "owner",
        "upstream",
    }
)


def _tags(listed: str, known: dict[str, str] | None = None) -> dict[str, str | None]:
    """Every tag of ``listed`` (whitespace separated) maps to ``None`` unless ``known`` names its output."""
    tags: dict[str, str | None] = dict.fromkeys(listed.split())
    for tag, output in (known or {}).items():
        if tag not in tags:
            raise KeyError(f"recorded output for unlisted tag {tag!r}")
        tags[tag] = output
    return tags


REAL_PACKAGES: dict[str, dict] = {
    "nodejs/node": {
        "bin": 'node', "args": '--version',
        "repository": 'ghcr.io/ocx-contrib/nodejs/node',
        "tags": _tags(
            """
            20 20.10 20.10.0 20.11 20.11.0 20.11.1 20.12 20.12.0 20.12.1 20.12.2 20.13 20.13.0 20.13.1 20.14
            20.14.0 20.15 20.15.0 20.15.1 20.16 20.16.0 20.17 20.17.0 20.18 20.18.0 20.18.1 20.18.2 20.18.3
            20.19 20.19.0 20.19.1 20.19.2 20.19.3 20.19.4 20.19.5 20.19.6 20.20 20.20.0 20.20.1 20.20.2 20.9
            20.9.0 22 22.11 22.11.0 22.12 22.12.0 22.13 22.13.0 22.13.1 22.14 22.14.0 22.15 22.15.0 22.15.1
            22.16 22.16.0 22.17 22.17.0 22.17.1 22.18 22.18.0 22.19 22.19.0 22.20 22.20.0 22.21 22.21.0
            22.21.1 22.22 22.22.0 22.22.1 22.22.2 22.22.3 22.23 22.23.0 22.23.1 22.23.2 22.23.3 22.23.4
            22.23.5 24 24.11 24.11.0 24.11.1 24.12 24.12.0 24.13 24.13.0 24.13.1 24.14 24.14.0 24.14.1 24.15
            24.15.0 24.16 24.16.0 24.17 24.17.0 24.18 24.18.0 24.18.1 24.19 24.19.0 24.20 24.20.0 24.21
            24.21.0 24.22 24.22.1 24.22.2 24.22.3 26 26.13 26.13.0 26.13.1 latest
            """,
            {
                '22': 'v22.23.5',
                '22.23': 'v22.23.5',
                '24': 'v24.22.3',
                '24.21': 'v24.21.0',
                '24.21.0': 'v24.21.0',
            },
        ),
    },
    "astral-sh/python-build-standalone": {
        "bin": 'python3', "args": '--version',
        "repository": 'ghcr.io/ocx-contrib/astral-sh/python-build-standalone',
        "tags": _tags(
            """
            3 3.12 3.12.13 3.12.14 3.12.15 3.12.16 3.13 3.13.14 3.13.15 3.13.16 3.13.17 3.13.18 3.14 3.14.10
            3.14.6 3.14.7 3.14.8 3.14.9 latest slim slim-3 slim-3.12 slim-3.12.13 slim-3.12.14 slim-3.12.15
            slim-3.12.16 slim-3.13 slim-3.13.14 slim-3.13.15 slim-3.13.16 slim-3.13.17 slim-3.13.18
            slim-3.14 slim-3.14.10 slim-3.14.6 slim-3.14.7 slim-3.14.8 slim-3.14.9 3.14.7_20260814
            """,
            {
                '3.14.7': 'Python 3.14.7',
                '3.13.14': 'Python 3.13.14',
                '3.14': 'Python 3.14.10',
                '3.13': 'Python 3.13.18',
            },
        ),
    },
    "astral-sh/uv": {
        "bin": 'uv', "args": '--version',
        "repository": 'ghcr.io/ocx-contrib/astral-sh/uv',
        "tags": _tags(
            """
            0 0.10 0.10.0 0.10.1 0.10.10 0.10.11 0.10.12 0.10.2 0.10.3 0.10.4 0.10.5 0.10.6 0.10.7 0.10.8
            0.10.9 0.11 0.11.0 0.11.1 0.11.10 0.11.11 0.11.12 0.11.13 0.11.14 0.11.15 0.11.16 0.11.17
            0.11.18 0.11.19 0.11.2 0.11.20 0.11.21 0.11.22 0.11.23 0.11.24 0.11.25 0.11.26 0.11.27 0.11.28
            0.11.29 0.11.3 0.11.30 0.11.31 0.11.32 0.11.33 0.11.4 0.11.5 0.11.6 0.11.7 0.11.8 0.11.9 0.12
            0.12.0 0.12.1 0.12.10 0.12.11 0.12.12 0.12.13 0.12.14 0.12.15 0.12.16 0.12.17 0.12.18 0.12.19
            0.12.2 0.12.20 0.12.21 0.12.22 0.12.23 0.12.24 0.12.3 0.12.4 0.12.5 0.12.6 0.12.7 0.12.8 0.12.9
            0.13 0.13.0 0.13.1 0.13.2 0.13.3 0.13.4 0.13.5 0.13.6 0.13.8 latest
            """,
            {
                '0.12.5': 'uv 0.12.5 (x86_64-unknown-linux-gnu)',
            },
        ),
    },
    "pypa/pipx": {
        "bin": 'pipx', "args": '--version',
        "repository": None,
        "tags": _tags(
            """
            1 1.16 1.16.0 1.16.1 1.16.2 1.16.3 1.16.4 1.16.5 1.16.6 1.16.7 1.17 1.17.1 1.17.10 1.17.11
            1.17.12 1.17.13 1.17.14 1.17.2 1.17.3 1.17.4 1.17.5 1.17.6 1.17.7 1.17.8 1.17.9 1.18 1.18.0
            1.18.1 1.18.10 1.18.11 1.18.13 1.18.14 1.18.15 1.18.16 1.18.2 1.18.3 1.18.4 1.18.5 1.18.6 1.18.7
            1.18.8 1.18.9 latest
            """,
        ),
    },
    "oven-sh/bun": {
        "bin": 'bun', "args": '--version',
        "repository": 'ghcr.io/ocx-contrib/oven-sh/bun',
        "tags": _tags(
            """
            1 1.3 1.3.10 1.3.11 1.3.12 1.3.13 1.3.14 1.4 1.4.0 1.4.1 1.4.2 1.4.3 1.4.4 latest
            """,
            {
                '1.4': '1.4.4',
                '1.3.10': '1.3.10',
            },
        ),
    },
    "amazon/corretto": {
        "bin": 'java', "args": '-version',
        "repository": 'ghcr.io/ocx-contrib/amazon/corretto',
        "tags": _tags(
            """
            11 11.0 11.0.26 11.0.27 11.0.28 11.0.29 11.0.30 11.0.31 11.0.32 11.0.33 17 17.0 17.0.13 17.0.14
            17.0.15 17.0.16 17.0.17 17.0.18 17.0.19 17.0.20 17.0.21 21 21.0 21.0.10 21.0.11 21.0.12 21.0.13
            21.0.5 21.0.6 21.0.7 21.0.8 21.0.9 25 25.0 25.0.0 25.0.1 25.0.2 25.0.3 25.0.4 25.0.5 26 26.0
            26.0.0 26.0.1 26.0.2 27 27.0 27.0.0 27.0.1 8 8.0 8.0.452 8.0.462 8.0.472 8.0.482 8.0.492 8.0.502
            8.0.504 8.0.512 latest
            """,
            {
                '21': 'openjdk version "21.0.13" 2026-10-20 LTS\nOpenJDK Runtime Environment Corretto-21.0.13.0.11 (build 21.0.13+11-LTS)\nOpenJDK 64-Bit Server VM Corretto-21.0.13.0.11 (build 21.0.13+11-LTS, mixed mode, sharing)',
                '25.0.5': 'openjdk version "25.0.5" 2026-10-20 LTS\nOpenJDK Runtime Environment Corretto-25.0.5.0.9 (build 25.0.5+9-LTS)\nOpenJDK 64-Bit Server VM Corretto-25.0.5.0.9 (build 25.0.5+9-LTS, mixed mode, sharing)',
            },
        ),
    },
    "adoptium/temurin": {
        "bin": 'java', "args": '-version',
        "repository": 'ghcr.io/ocx-contrib/adoptium/temurin',
        "tags": _tags(
            """
            11 11.0 11.0.30 11.0.31 11.0.32 11.0.33 17 17.0 17.0.18 17.0.19 17.0.20 17.0.21 21 21.0 21.0.10
            21.0.11 21.0.12 21.0.13 25 25.0 25.0.2 25.0.3 25.0.4 25.0.5 26 26.0 26.0.0 26.0.1 26.0.2 8 8.0
            8.0.482 8.0.492 8.0.502 8.0.504 8.0.512 jre jre-11 jre-11.0 jre-11.0.30 jre-11.0.31 jre-11.0.32
            jre-11.0.33 jre-17 jre-17.0 jre-17.0.18 jre-17.0.19 jre-17.0.20 jre-17.0.21 jre-21 jre-21.0
            jre-21.0.10 jre-21.0.11 jre-21.0.12 jre-21.0.13 jre-25 jre-25.0 jre-25.0.2 jre-25.0.3 jre-25.0.4
            jre-25.0.5 jre-26 jre-26.0 jre-26.0.0 jre-26.0.1 jre-26.0.2 jre-8 jre-8.0 jre-8.0.482
            jre-8.0.492 jre-8.0.502 jre-8.0.504 jre-8.0.512 latest
            """,
        ),
    },
    "ocx/cli": {
        "bin": 'ocx', "args": '--version',
        "repository": 'ghcr.io/ocx-sh/ocx/cli',
        "tags": _tags(
            """
            0 0.5 0.5.0 0.5.1 0.5.2 0.5.3 0.5.4 0.5.5 0.5.6 0.5.7 0.5.8 0.6 0.6.0 0.6.1 0.6.2 0.6.3 0.6.4
            0.6.5 0.6.6 0.6.7 0.6.8 latest
            """,
        ),
    },
    "github/cli": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            2 2.100 2.100.0 2.101 2.101.0 2.102 2.102.0 2.102.1 2.103 2.103.0 2.103.1 2.103.2 2.103.3 2.104
            2.104.0 2.76 2.76.0 2.76.1 2.76.2 2.77 2.77.0 2.78 2.78.0 2.79 2.79.0 2.80 2.80.0 2.81 2.81.0
            2.82 2.82.0 2.82.1 2.83 2.83.0 2.83.1 2.83.2 2.85 2.85.0 2.86 2.86.0 2.87 2.87.0 2.87.1 2.87.2
            2.87.3 2.88 2.88.0 2.88.1 2.89 2.89.0 2.90 2.90.0 2.91 2.91.0 2.92 2.92.0 2.93 2.93.0 2.94
            2.94.0 2.95 2.95.0 2.96 2.96.0 2.97 2.97.0 2.98 2.98.0 2.99 2.99.0 latest
            """,
        ),
    },
    "gitlab/cli": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            1 1.103 1.103.0 1.104 1.104.0 1.105 1.105.0 1.106 1.106.0 1.107 1.107.0 1.108 1.108.0 1.109
            1.109.0 1.110 1.110.0 1.111 1.111.0 1.112 1.112.0 1.113 1.113.0 1.114 1.114.0 1.115 1.115.0
            1.116 1.116.0 1.117 1.117.0 1.118 1.118.0 1.119 1.119.0 1.120 1.120.0 1.121 1.121.0 1.122
            1.122.0 1.123 1.123.0 1.124 1.124.0 1.125 1.125.0 1.126 1.126.0 1.128 1.128.0 1.129 1.129.0
            1.130 1.130.0 latest
            """,
        ),
    },
    "goreleaser/goreleaser": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            2 2.16 2.16.0 2.17 2.17.0 2.17.1 2.18 2.18.0 2.18.1 2.18.2 2.18.3 2.19 2.19.0 2.20 2.20.0 2.20.1
            2.20.2 latest
            """,
        ),
    },
    "jqlang/jq": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            1 1.8 1.8.0 1.8.1 1.8.2 latest
            """,
        ),
    },
    "ripgrep/ripgrep": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            15 15.0 15.0.0 15.1 15.1.0 15.2 15.2.0 latest
            """,
        ),
    },
    "shellcheck/shellcheck": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            0 0.11 0.11.0 latest
            """,
        ),
    },
    "shfmt/shfmt": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            3 3.10 3.10.0 3.11 3.11.0 3.12 3.12.0 3.13 3.13.0 3.13.1 3.14 3.14.0 3.14.1 3.5 3.5.0 3.5.1 3.6
            3.6.0 3.7 3.7.0 3.8 3.8.0 3.9 3.9.0 latest
            """,
        ),
    },
    "asciinema/agg": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            1 1.0 1.0.0 1.1 1.1.0 1.2 1.2.0 1.3 1.3.0 1.4 1.4.0 1.4.1 1.4.2 1.4.3 1.5 1.5.0 1.6 1.6.0 1.7
            1.7.0 1.8 1.8.0 1.8.1 1.9 1.9.0 latest
            """,
        ),
    },
    "ocx/mirror": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            0 0.4 0.4.0 0.5 0.5.0 0.5.1 0.5.2 0.5.4 0.5.5 0.5.6 0.5.7 0.6 0.6.0 0.6.1 0.6.2 0.6.3 0.6.4
            0.6.5 latest
            """,
        ),
    },
    "go-task/task": {
        "bin": None, "args": None,
        "repository": None,
        "tags": _tags(
            """
            3 3.49 3.49.1 3.50 3.50.0 3.51 3.51.1 3.52 3.52.0 3.53 3.53.1 3.54 3.54.0 latest
            """,
        ),
    },
}


def stub_outputs(name: str, tag: str) -> dict[str, dict[str, str]]:
    """`make_package(outputs=...)` for a real tag; KeyError when the output was not recorded."""
    package = REAL_PACKAGES[name]
    output = package["tags"][tag]
    if output is None:
        raise KeyError(f"{name}:{tag} has no recorded output in real_packages.py")
    return {package["bin"]: {package["args"]: output}}
