"""Convert .cast recordings to animated GIFs using agg."""
from __future__ import annotations

import argparse
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

from rich.progress import Progress, SpinnerColumn, TextColumn, BarColumn, MofNCompleteColumn


def _convert_one(cast: Path, gif: Path, font_dir: Path, font_family: str, font_size: int) -> str:
    """Convert a single .cast file to .gif.  Returns the cast stem on success."""
    subprocess.run(
        [
            "agg",
            "--font-dir", str(font_dir),
            "--font-family", font_family,
            "--font-size", str(font_size),
            str(cast), str(gif),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return cast.stem


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

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        BarColumn(),
        MofNCompleteColumn(),
    ) as progress:
        overall = progress.add_task("Converting .cast → .gif", total=len(casts))

        with ProcessPoolExecutor() as pool:
            futures = {
                pool.submit(
                    _convert_one,
                    cast,
                    args.gifs_dir / f"{cast.stem}.gif",
                    args.font_dir,
                    args.font_family,
                    args.font_size,
                ): cast
                for cast in casts
            }
            for future in as_completed(futures):
                name = future.result()
                progress.console.print(f"  [green]✓[/green] {name}")
                progress.advance(overall)


if __name__ == "__main__":
    main()
