# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Typed builder for url_index JSON, backed by schema-generated types."""

import json
import sys

from ocx_gen._schema import RemoteIndex, RemoteVersionEntry


class IndexBuilder:
    """Build a url_index JSON document with type-safe version entries."""

    def __init__(self) -> None:
        self._versions: dict[str, RemoteVersionEntry] = {}

    def add_version(
        self,
        version: str,
        *,
        assets: dict[str, str],
        prerelease: bool = False,
    ) -> None:
        """Add a version with its assets.

        Args:
            version: Semver version string (e.g., "22.15.0").
            assets: Map of asset filename to download URL.
            prerelease: Whether this is a pre-release.
        """
        if not assets:
            return
        self._versions[version] = RemoteVersionEntry(
            assets=assets,
            prerelease=prerelease,
        )

    def build(self) -> RemoteIndex:
        """Return the constructed RemoteIndex."""
        return RemoteIndex(versions=self._versions)

    def emit(self, file=sys.stdout) -> None:
        """Serialize to JSON and write to the given file (default: stdout)."""
        index = self.build()
        data = {
            "versions": {
                ver: {"prerelease": entry.prerelease, "assets": entry.assets}
                for ver, entry in index.versions.items()
            }
        }
        json.dump(data, file, indent=2)
        file.write("\n")
