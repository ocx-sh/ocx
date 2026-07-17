"""Three-layer package scenario.

`pkg` is published with three layers:
- layer-base: `lib/liba.so` (placeholder content)
- layer-libs: `lib/libb.so` (placeholder content)
- layer-app:  `bin/myapp`   (executable echoing the marker)

PATH points at `${installPath}/bin` so `exec myapp` resolves the top layer.
"""
from __future__ import annotations

import json
import stat
from pathlib import Path
from uuid import uuid4

from src.runner import PackageInfo, current_platform
from src.scenarios import Scenario


def _make_layer(tmp_path: Path, name: str, files: dict[str, str]) -> Path:
    layer_dir = tmp_path / f"layer-{name}"
    for rel, content in files.items():
        p = layer_dir / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        if rel.startswith("bin/"):
            p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return layer_dir


class MultiLayer(Scenario):
    name = "MultiLayer"

    def setup(self) -> None:
        marker = f"marker-{uuid4().hex[:12]}"
        repo = self.repo("multilayer")

        base = _make_layer(self.tmp_path, "base", {"lib/liba.so": "liba\n"})
        libs = _make_layer(self.tmp_path, "libs", {"lib/libb.so": "libb\n"})
        app = _make_layer(
            self.tmp_path, "app",
            {"bin/myapp": f"#!/bin/sh\necho {marker}\n"},
        )

        plat = current_platform()
        metadata_path = self.tmp_path / f"meta-{repo}.json"
        metadata_path.write_text(json.dumps({
            "type": "bundle",
            "version": 1,
            "platform": plat,
            "env": [
                {
                    "key": "PATH", "type": "path", "required": True,
                    "value": "${installPath}/bin", "visibility": "public",
                },
            ],
        }))

        bundles: list[Path] = []
        for layer_dir in (base, libs, app):
            bundle = self.tmp_path / f"{layer_dir.name}.tar.gz"
            self.ocx.plain(
                "package", "create",
                "-m", str(metadata_path), "-o", str(bundle), str(layer_dir),
            )
            bundles.append(bundle)

        fq = f"{self.ocx.registry}/{repo}:1.0.0"
        push_args = [
            "package", "push", "-p", plat, "-m", str(metadata_path),
            "-n", "--cascade", "-i", fq,
            *[str(b) for b in bundles],
        ]
        self.ocx.plain(*push_args)
        self.ocx.plain("index", "update", repo)

        self.packages["pkg"] = PackageInfo(
            repo=repo, tag="1.0.0",
            short=f"{repo}:1.0.0", fq=fq,
            content_dir=app, marker=marker, platform=plat,
        )
