# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

import io
import json

from ocx_gen import IndexBuilder


def test_add_version():
    builder = IndexBuilder()
    builder.add_version(
        "1.0.0",
        assets={"tool-linux.tar.gz": "https://example.com/tool-linux.tar.gz"},
    )

    index = builder.build()
    assert "1.0.0" in index.versions
    assert index.versions["1.0.0"].prerelease is False
    assert len(index.versions["1.0.0"].assets) == 1


def test_add_prerelease():
    builder = IndexBuilder()
    builder.add_version(
        "2.0.0-rc1",
        assets={"tool.tar.gz": "https://example.com/tool.tar.gz"},
        prerelease=True,
    )

    index = builder.build()
    assert index.versions["2.0.0-rc1"].prerelease is True


def test_skip_empty_assets():
    builder = IndexBuilder()
    builder.add_version("1.0.0", assets={})

    index = builder.build()
    assert len(index.versions) == 0


def test_emit_json():
    builder = IndexBuilder()
    builder.add_version(
        "1.0.0",
        assets={"tool-linux.tar.gz": "https://example.com/tool-linux.tar.gz"},
    )

    output = io.StringIO()
    builder.emit(file=output)

    data = json.loads(output.getvalue())
    assert "versions" in data
    assert "1.0.0" in data["versions"]
    assert data["versions"]["1.0.0"]["prerelease"] is False
    assert "tool-linux.tar.gz" in data["versions"]["1.0.0"]["assets"]


def test_multiple_versions():
    builder = IndexBuilder()
    builder.add_version(
        "1.0.0",
        assets={"tool-1.tar.gz": "https://example.com/tool-1.tar.gz"},
    )
    builder.add_version(
        "2.0.0",
        assets={"tool-2.tar.gz": "https://example.com/tool-2.tar.gz"},
    )

    index = builder.build()
    assert len(index.versions) == 2
