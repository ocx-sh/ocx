"""Convert .cast recordings to animated GIFs using agg."""
from __future__ import annotations

import argparse
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

from rich.progress import (
    BarColumn,
    MofNCompleteColumn,
    Progress,
    SpinnerColumn,
    TextColumn,
)


def _convert_one(cast: Path, gif: Path, font_dir: Path, font_family: str, font_size: int) -> Path:
    """Convert a single .cast file to .gif.  Returns the .gif path on success."""
    proc = subprocess.run(
        [
            "agg",
            "--font-dir", str(font_dir),
            "--font-family", font_family,
            "--font-size", str(font_size),
            str(cast), str(gif),
        ],
        capture_output=True,
        text=True,
        # Explicit, and the opposite of what this used to pass. `check=True`
        # raises a CalledProcessError whose message carries the exit code and
        # nothing else; the returncode is inspected below so agg's own
        # diagnostic reaches the reader.
        check=False,
    )
    if proc.returncode != 0:
        # agg's own diagnostic rather than a bare CalledProcessError.  The
        # failure this actually hits is "no faces matching font family
        # options", and a traceback that drops the message sends the reader
        # looking at the cast instead of at the font.
        raise RuntimeError(
            f"agg exited {proc.returncode} on {cast}: "
            f"{proc.stderr.strip() or '<no stderr>'}"
        )
    if not gif.is_file() or gif.stat().st_size == 0:
        raise RuntimeError(f"agg exited 0 but wrote no bytes to {gif}")
    return gif


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("casts_dir", type=Path, help="Directory containing .cast files")
    parser.add_argument("gifs_dir", type=Path, help="Output directory for .gif files")
    parser.add_argument("--font-dir", type=Path, required=True, help="Path to font directory")
    parser.add_argument("--font-family", default="CaskaydiaCove Nerd Font")
    parser.add_argument("--font-size", type=int, default=32)
    args = parser.parse_args()

    args.gifs_dir.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []

    # `rglob`, not `glob`.  The casts are written to `<doc>/<name>.cast` — a
    # nested layout — so the flat `*.cast` this used to spell matched ZERO
    # files and this script exited 1 on every invocation it ever got.
    casts = sorted(args.casts_dir.rglob("*.cast"))
    if not casts:
        print(f"No .cast files found under {args.casts_dir}", file=sys.stderr)
        sys.exit(1)

    # Mirror the cast tree rather than flattening to `<stem>.gif`.  Flattening
    # is a silent overwrite waiting for the first two slugs that share a stem
    # (`user-guide/deps` and some later `reference/deps`): both would render
    # onto one path, and a count of *conversions* would still say 39 while 38
    # files existed.  Mirroring makes the output paths unique by construction.
    jobs: list[tuple[Path, Path]] = []
    for cast in casts:
        gif = args.gifs_dir / cast.relative_to(args.casts_dir).with_suffix(".gif")
        gif.parent.mkdir(parents=True, exist_ok=True)
        jobs.append((cast, gif))

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        BarColumn(),
        MofNCompleteColumn(),
    ) as progress:
        overall = progress.add_task("Converting .cast → .gif", total=len(jobs))

        with ProcessPoolExecutor() as pool:
            futures = {
                pool.submit(
                    _convert_one,
                    cast,
                    gif,
                    args.font_dir,
                    args.font_family,
                    args.font_size,
                ): gif
                for cast, gif in jobs
            }
            for future in as_completed(futures):
                written.append(future.result())
                progress.console.print(
                    f"  [green]✓[/green] {written[-1].relative_to(args.gifs_dir)}"
                )
                progress.advance(overall)

    # The count, asserted — not "it ran".  `future.result()` re-raises, so a
    # failed render cannot reach here today; a filtered submission or a
    # `continue` added later could, and an under-read loop that printed a tick
    # per success is indistinguishable from a complete one without this line.
    if len(written) != len(jobs):
        print(
            f"converted {len(written)} of {len(jobs)} casts - the run was partial",
            file=sys.stderr,
        )
        sys.exit(1)
    print(f"{len(written)} GIFs written to {args.gifs_dir}")


if __name__ == "__main__":
    main()
