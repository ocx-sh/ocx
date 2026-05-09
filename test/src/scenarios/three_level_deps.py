"""Three-level dep chain: app → mid → leaf (all public)."""
from __future__ import annotations

from src.scenarios import Scenario


class ThreeLevelDeps(Scenario):
    name = "ThreeLevelDeps"

    def setup(self) -> None:
        self.publish("leaf", "1.0.0")
        self.publish_with_deps("mid", "1.0.0", deps=[("leaf", "public")])
        self.publish_with_deps("app", "1.0.0", deps=[("mid", "public")])
