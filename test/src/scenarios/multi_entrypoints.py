"""Single package with four entrypoints (cmake-shaped layout).

Exposes:
- `toolkit` — one package, four entries: `tool-a`, `tool-b`, `tool-c`, `tool-d`.

Useful for entrypoint dedup, launcher generation, and PATH-based dispatch
asserts.
"""
from __future__ import annotations

from src.helpers import make_package_with_entrypoints
from src.scenarios import Scenario


class MultiEntrypoints(Scenario):
    name = "MultiEntrypoints"

    def setup(self) -> None:
        bins = ["tool-a", "tool-b", "tool-c", "tool-d"]
        repo = self.repo("toolkit")
        pkg = make_package_with_entrypoints(
            self.ocx,
            repo,
            self.tmp_path,
            entrypoints=bins,
            bins=bins,
            tag="1.0.0",
            file_prefix="mep",
            env=[
                {
                    "key": "PATH",
                    "type": "path",
                    "required": True,
                    "value": "${installPath}/bin",
                    "visibility": "public",
                },
            ],
        )
        self.packages["toolkit"] = pkg
