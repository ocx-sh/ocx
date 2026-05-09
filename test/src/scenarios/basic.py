"""Trivial single-package scenario."""
from __future__ import annotations

from src.scenarios import Scenario


class BasicPackage(Scenario):
    """One package, one tag, one entrypoint. Smoke-test baseline."""

    name = "BasicPackage"

    def setup(self) -> None:
        self.publish("hello", "1.0.0")
