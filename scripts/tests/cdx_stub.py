"""A stand-in for `cargo cyclonedx` that writes what the pinned 0.5.9 writes.

Observed with the real tool: `--override-filename x.cdx --format json` writes
`<manifest dir>/x.cdx.json`, and `--format xml` writes `x.cdx.xml`. The tool
appends the format extension to the override, so a stem without `.cdx` yields
`x.json`. A stub that names the file any other way hides a wrong name.
"""

from __future__ import annotations

from pathlib import Path


def run_cyclonedx(argv: list[str]) -> Path:
    """Write the file the real tool writes for `argv` (a `cargo cyclonedx ...` line) and return it."""
    fmt = argv[argv.index("--format") + 1]
    manifest = Path(argv[argv.index("--manifest-path") + 1])
    override = argv[argv.index("--override-filename") + 1]
    out = manifest.parent / f"{override}.{fmt}"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(f"sbom {fmt}", encoding="utf-8")
    return out
