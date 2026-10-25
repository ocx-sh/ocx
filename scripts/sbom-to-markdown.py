#!/usr/bin/env python3
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

"""Convert a CycloneDX JSON SBOM to a Markdown page for the website."""

import argparse
import json
import re
import sys
from pathlib import Path
from urllib.parse import quote


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="CycloneDX JSON file")
    parser.add_argument("--output", required=True, type=Path, help="Output Markdown file")
    return parser.parse_args()


def extract_license(component: dict) -> str:
    licenses = component.get("licenses", [])
    parts = []
    for entry in licenses:
        if "license" in entry:
            lic = entry["license"]
            parts.append(lic.get("id") or lic.get("name", ""))
        elif "expression" in entry:
            parts.append(entry["expression"])
    return " OR ".join(filter(None, parts)) or ""


def extract_links(component: dict) -> dict:
    """Derive crates.io, docs.rs, repository, and website links from SBOM data."""
    links = {}

    purl = component.get("purl", "")
    if purl.startswith("pkg:cargo/"):
        # Extract crate name from purl (before @ version)
        crate_part = purl[len("pkg:cargo/"):]
        crate_name = crate_part.split("@")[0].split("?")[0]
        # Only link to crates.io for registry crates (not local path deps)
        if "download_url=file://" not in purl:
            links["cratesIo"] = f"https://crates.io/crates/{quote(crate_name)}"

    for ref in component.get("externalReferences", []):
        url = ref.get("url", "")
        ref_type = ref.get("type", "")
        if ref_type == "documentation" and "docs.rs" in url:
            links["docsRs"] = url
        elif ref_type == "vcs" and url:
            links["repository"] = url
        elif ref_type == "website" and url:
            links["website"] = url

    return links


def clean_author(raw: str) -> str:
    """Strip email addresses and truncate to first 3 names."""
    if not raw:
        return ""
    # Remove email addresses in angle brackets
    cleaned = re.sub(r"\s*<[^>]+>", "", raw)
    names = [n.strip() for n in cleaned.split(",") if n.strip()]
    if len(names) > 3:
        return ", ".join(names[:3]) + " et al."
    return ", ".join(names)


def build_component(comp: dict) -> dict:
    return {
        "name": comp.get("name", ""),
        "version": comp.get("version", ""),
        "license": extract_license(comp),
        "description": comp.get("description", ""),
        "author": clean_author(comp.get("author", "")),
        "scope": comp.get("scope", ""),
        "links": extract_links(comp),
    }


def generate_markdown(components: list[dict]) -> str:
    """One table row per license, the components listed in the last cell.

    A row per component would exceed the per-page DOM budget (`website/scripts/check_budget.py`).
    """
    by_license: dict[str, list[str]] = {}
    for c in components:
        by_license.setdefault(c["license"] or "unknown", []).append(f"{c['name']} {c['version']}".strip())
    rows = "\n".join(
        f"| {lic.replace('|', '\\|')} | {len(names)} | {', '.join(sorted(names))} |"
        for lic, names in sorted(by_license.items(), key=lambda kv: (-len(kv[1]), kv[0]))
    )
    return f"""---
title: Dependencies
---
<!-- doc_type: reference -->

Third-party dependencies compiled into the `ocx` binary, generated from the [CycloneDX][cyclonedx] SBOM at build time.

## Components by license {{#components-by-license}}

| License | Count | Components |
|---|---|---|
{rows}

[cyclonedx]: https://cyclonedx.org/
"""


def main() -> None:
    args = parse_args()

    if not args.input.exists():
        print(f"Error: input file not found: {args.input}", file=sys.stderr)
        sys.exit(1)

    bom = json.loads(args.input.read_text())
    raw_components = sorted(bom.get("components", []), key=lambda c: c.get("name", ""))
    components = [build_component(c) for c in raw_components]

    # Generate markdown page
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generate_markdown(components))
    print(f"Generated {args.output}")


if __name__ == "__main__":
    main()
