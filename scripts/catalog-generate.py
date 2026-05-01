#!/usr/bin/env python3
# /// script
# requires-python = ">=3.13"
# dependencies = ["jinja2"]
# ///
# Copyright 2026 The OCX Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Generate package catalog data and pages for the OCX website.

Invokes the OCX CLI to gather package metadata, logos, READMEs, tags, and
platform information from a registry, then writes:
  - JSON data files for the Vue catalog component
  - Per-package markdown pages (with README appended) for VitePress
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from jinja2 import Environment, FileSystemLoader

SCRIPT_DIR = Path(__file__).resolve().parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ocx-binary",
        default="ocx",
        help="Path to the ocx binary (default: ocx)",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Output directory for catalog JSON data",
    )
    parser.add_argument(
        "--pages-dir",
        type=Path,
        required=True,
        help="Output directory for generated markdown pages",
    )
    parser.add_argument(
        "--registry",
        default="ocx.sh",
        help="Registry to query (default: ocx.sh)",
    )
    parser.add_argument(
        "--remote",
        action="store_true",
        help="Query remote registry directly (sets OCX_REMOTE=1)",
    )
    parser.add_argument(
        "--package",
        action="append",
        dest="packages",
        help="Only refresh specific packages (can be repeated)",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Force regeneration even if catalog.json exists",
    )
    return parser.parse_args()


def run_ocx(binary: str, args: list[str], *, remote: bool = False) -> str:
    """Run an ocx command and return stdout."""
    cmd = [binary, "--format", "json", *args]
    env = os.environ.copy()
    if remote:
        env["OCX_REMOTE"] = "1"
    result = subprocess.run(cmd, capture_output=True, text=True, check=True, env=env)
    return result.stdout


def get_catalog_with_tags(binary: str, *, remote: bool) -> dict[str, list[str]]:
    """Get all repos and their tags from the registry."""
    output = run_ocx(binary, ["index", "catalog", "--tags"], remote=remote)
    data = json.loads(output)
    # JSON wraps in {"repositories": {...}}
    repos = data.get("repositories", data) if isinstance(data, dict) else data
    return repos


def get_package_info(
    binary: str, repo: str, pkg_dir: Path, *, remote: bool
) -> dict | None:
    """Get package description metadata, saving readme and logo."""
    try:
        output = run_ocx(
            binary,
            [
                "package",
                "info",
                "--save-readme",
                str(pkg_dir),
                "--save-logo",
                str(pkg_dir),
                repo,
            ],
            remote=remote,
        )
        result = json.loads(output)
        # Returns null when no description is published
        return result if result else None
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"  Warning: failed to get info for {repo}: {e}", file=sys.stderr)
        return None


def get_package_platforms(
    binary: str, repo: str, latest_tag: str, *, remote: bool
) -> list[str]:
    """Get supported platforms for the latest tag of a package."""
    if not latest_tag:
        return []
    try:
        output = run_ocx(
            binary,
            ["index", "list", "--platforms", f"{repo}:{latest_tag}"],
            remote=remote,
        )
        data = json.loads(output)
        # Output is keyed by "repo:tag", value is a list of platform strings
        key = f"{repo}:{latest_tag}"
        platforms = data.get(key) or data.get(repo) or next(iter(data.values()), [])
        return sorted(platforms) if platforms else []
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"  Warning: failed to get platforms for {repo}: {e}", file=sys.stderr)
        return []


def detect_logo(pkg_dir: Path) -> tuple[bool, str]:
    """Check if a logo was saved and return (has_logo, extension)."""
    for ext in ("svg", "png"):
        if (pkg_dir / f"logo.{ext}").exists():
            return True, ext
    return False, ""


def build_package_data(
    binary: str,
    repo: str,
    tags: list[str],
    output_dir: Path,
    *,
    remote: bool,
    registry: str,
) -> dict:
    """Build complete data for a single package."""
    name = repo.split("/")[-1] if "/" in repo else repo
    pkg_dir = output_dir / "packages" / name
    pkg_dir.mkdir(parents=True, exist_ok=True)

    print(f"  Fetching info for {repo}...")

    # Get description metadata
    info = get_package_info(binary, repo, pkg_dir, remote=remote)

    # Prefer the explicit "latest" tag (set by --cascade publishing),
    # fall back to lexicographic last tag as a rough heuristic.
    latest_tag = "latest" if "latest" in tags else (tags[-1] if tags else "")

    # Get platforms for the latest tag only
    platforms = get_package_platforms(binary, repo, latest_tag, remote=remote)

    # Detect logo
    has_logo, logo_ext = detect_logo(pkg_dir)

    # Check if README was saved
    has_readme = (pkg_dir / "README.md").exists()

    # Find the highest version tag (default variant) for display
    latest_version = find_latest_version(tags) or latest_tag

    # Build summary for catalog.json
    summary = {
        "name": name,
        "registry": registry,
        "repository": repo,
        "title": (info or {}).get("title") or name,
        "description": (info or {}).get("description") or "",
        "keywords": parse_keywords((info or {}).get("keywords")),
        "hasLogo": has_logo,
        "logoExt": logo_ext,
        "hasReadme": has_readme,
        "tagCount": len(tags),
        "platforms": platforms,
        "latestTag": latest_tag,
        "latestVersion": latest_version,
    }

    # Build detail info.json
    detail = {
        **summary,
        "tags": tags,
    }

    # Write per-package info.json
    (pkg_dir / "info.json").write_text(json.dumps(detail, indent=2) + "\n")

    return summary


def generate_package_page(
    template, summary: dict, pkg_dir: Path, pages_dir: Path
) -> None:
    """Generate a markdown page for a single package."""
    readme = ""
    readme_path = pkg_dir / "README.md"
    if readme_path.exists():
        readme = readme_path.read_text(encoding="utf-8").strip()

    content = template.render(
        title=summary["title"],
        description=summary["description"],
        keywords=summary["keywords"],
        readme=readme,
    )

    pages_dir.mkdir(parents=True, exist_ok=True)
    page_path = pages_dir / f"{summary['name']}.md"
    page_path.write_text(content, encoding="utf-8")


def parse_keywords(keywords_str: str | None) -> list[str]:
    """Parse comma-separated keywords string into a list."""
    if not keywords_str:
        return []
    return [k.strip() for k in keywords_str.split(",") if k.strip()]


_VERSION_RE = re.compile(
    r"^(?:([a-z][a-z0-9.]*)-)?((0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*))?)?)$"
)


def find_latest_version(tags: list[str]) -> str | None:
    """Find the highest version tag from a list of tags (default variant only).

    Returns the full tag string of the highest version, or None if no
    parseable version tags exist in the default variant.
    """
    best_tag: str | None = None
    best_parts: tuple[int, ...] = ()

    for tag in tags:
        if tag == "latest":
            continue
        m = _VERSION_RE.match(tag)
        if not m:
            continue
        # Skip variant-prefixed tags — only consider default variant
        if m.group(1) is not None:
            continue
        parts = tuple(int(x) for x in m.group(2).split(".") if x)
        if parts > best_parts:
            best_parts = parts
            best_tag = tag

    return best_tag


def main() -> None:
    args = parse_args()
    output_dir = args.output_dir
    pages_dir = args.pages_dir

    env = Environment(
        loader=FileSystemLoader(SCRIPT_DIR / "templates"),
        keep_trailing_newline=True,
    )
    template = env.get_template("package-page.md.j2")

    # Load existing catalog if doing selective refresh
    existing_catalog = None
    catalog_path = output_dir / "catalog.json"
    if args.packages and catalog_path.exists() and not args.force:
        existing_catalog = json.loads(catalog_path.read_text())

    # Get repos and tags from registry
    if args.packages and existing_catalog:
        print(f"Selective refresh for: {', '.join(args.packages)}")
        all_repos = get_catalog_with_tags(args.ocx_binary, remote=args.remote)
        repos_to_process = {
            repo: tags
            for repo, tags in all_repos.items()
            if any(
                pkg == repo or pkg == repo.split("/")[-1]
                for pkg in args.packages
            )
        }
    else:
        print("Fetching full catalog...")
        all_repos = get_catalog_with_tags(args.ocx_binary, remote=args.remote)
        repos_to_process = all_repos

    if not repos_to_process:
        print("No packages to process.", file=sys.stderr)
        sys.exit(1)

    output_dir.mkdir(parents=True, exist_ok=True)

    # Process each package
    summaries: list[dict] = []
    if existing_catalog and args.packages:
        existing_by_repo = {
            p["repository"]: p for p in existing_catalog.get("packages", [])
        }
        for repo, tags in all_repos.items():
            if repo in repos_to_process:
                summary = build_package_data(
                    args.ocx_binary, repo, tags, output_dir, remote=args.remote,
                    registry=args.registry,
                )
                summaries.append(summary)
            elif repo in existing_by_repo:
                summaries.append(existing_by_repo[repo])
    else:
        for repo, tags in repos_to_process.items():
            summary = build_package_data(
                args.ocx_binary, repo, tags, output_dir, remote=args.remote,
                registry=args.registry,
            )
            summaries.append(summary)

    # Sort by name
    summaries.sort(key=lambda s: s["name"].lower())

    # Write catalog.json
    catalog = {
        "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "registry": args.registry,
        "packages": summaries,
    }
    catalog_path.write_text(json.dumps(catalog, indent=2) + "\n")
    print(f"\nGenerated {catalog_path} ({len(summaries)} packages)")

    # Generate per-package markdown pages
    for summary in summaries:
        pkg_dir = output_dir / "packages" / summary["name"]
        generate_package_page(template, summary, pkg_dir, pages_dir)
    print(f"Generated {len(summaries)} package pages in {pages_dir}")

    # Clean up stale data dirs
    packages_dir = output_dir / "packages"
    if packages_dir.exists():
        catalog_names = {s["name"] for s in summaries}
        for pkg_dir in packages_dir.iterdir():
            if pkg_dir.is_dir() and pkg_dir.name not in catalog_names:
                shutil.rmtree(pkg_dir)
                print(f"  Removed stale package dir: {pkg_dir.name}")

    # Clean up stale page files
    if pages_dir.exists():
        catalog_names = {s["name"] for s in summaries}
        for page in pages_dir.glob("*.md"):
            if page.stem not in catalog_names:
                page.unlink()
                print(f"  Removed stale page: {page.name}")


if __name__ == "__main__":
    main()
