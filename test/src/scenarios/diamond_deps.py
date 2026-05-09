"""Diamond dep graph: app → {left, right} → leaf (all public).

Exercises cross-root dedup on the shared transitive dep `leaf` and the
"first-seen wins" ordering rule in `composer::compose`.
"""
from __future__ import annotations

from src.scenarios import Scenario


class DiamondDeps(Scenario):
    name = "DiamondDeps"

    def setup(self) -> None:
        self.publish("leaf", "1.0.0")
        self.publish_with_deps("left", "1.0.0", deps=[("leaf", "public")])
        self.publish_with_deps("right", "1.0.0", deps=[("leaf", "public")])
        self.publish_with_deps(
            "app", "1.0.0",
            deps=[("left", "public"), ("right", "public")],
        )
