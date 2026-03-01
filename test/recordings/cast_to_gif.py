"""Convert .cast recordings to animated GIFs using agg."""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("casts_dir", type=Path, help="Directory containing .cast files")
    parser.add_argument("gifs_dir", type=Path, help="Output directory for .gif files")
    parser.add_argument("--font-dir", type=Path, required=True, help="Path to font directory")
    parser.add_argument("--font-family", default="CaskaydiaCove Nerd Font")
    parser.add_argument("--font-size", type=int, default=32)
    args = parser.parse_args()

    args.gifs_dir.mkdir(parents=True, exist_ok=True)

    casts = sorted(args.casts_dir.glob("*.cast"))
    if not casts:
        print(f"No .cast files found in {args.casts_dir}", file=sys.stderr)
        sys.exit(1)

    for cast in casts:
        gif = args.gifs_dir / f"{cast.stem}.gif"
        print(f"Converting {cast.name} → {gif.name}")
        subprocess.run(
            [
                "agg",
                "--font-dir", str(args.font_dir),
                "--font-family", args.font_family,
                "--font-size", str(args.font_size),
                str(cast), str(gif),
            ],
            check=True,
        )


if __name__ == "__main__":
    main()
