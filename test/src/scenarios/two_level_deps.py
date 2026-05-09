"""Two-level dep scenario: app → leaf (interface visibility).

Exposes:
- `leaf` — bare package
- `app`  — depends on `leaf` with `"public"` visibility (so leaf's env vars
           reach `app`'s default `exec` surface).
"""
from __future__ import annotations

from src.scenarios import Scenario


class TwoLevelDeps(Scenario):
    name = "TwoLevelDeps"

    def setup(self) -> None:
        self.publish("leaf", "1.0.0")
        self.publish_with_deps("app", "1.0.0", deps=[("leaf", "public")])
