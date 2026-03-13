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

"""Convert a CycloneDX JSON SBOM to a Markdown page and JSON data file for the website."""

import argparse
import json
import re
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="CycloneDX JSON file")
    parser.add_argument("--output", required=True, type=Path, help="Output Markdown file")
    parser.add_argument("--json-output", type=Path, help="Output JSON data file for the Vue component")
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


def build_summary(components: list[dict]) -> dict:
    license_counts: Counter = Counter()
    scope_counts: Counter = Counter()
    for c in components:
        if c["license"]:
            license_counts[c["license"]] += 1
        scope_counts[c["scope"] or "unknown"] += 1

    return {
        "total": len(components),
        "required": scope_counts.get("required", 0),
        "excluded": scope_counts.get("excluded", 0),
        "uniqueLicenses": len(license_counts),
        "licenses": dict(license_counts.most_common()),
    }


def extract_target(bom: dict) -> str:
    """Extract the Rust target triple from SBOM metadata properties."""
    for prop in bom.get("metadata", {}).get("properties", []):
        if prop.get("name") == "cdx:rustc:sbom:target:triple":
            return prop.get("value", "")
    return ""


def extract_binary_info(bom: dict) -> dict:
    """Extract binary name, version, and license from the top-level metadata component."""
    meta_comp = bom.get("metadata", {}).get("component", {})
    return {
        "name": meta_comp.get("name", "unknown"),
        "version": meta_comp.get("version", ""),
        "license": extract_license(meta_comp),
    }


def generate_markdown() -> str:
    return "\n".join([
        "---",
        "title: Dependencies",
        "---",
        "",
        "# Dependencies {#dependencies}",
        "",
        "Third-party dependencies compiled into the `ocx` binary,",
        "generated from the [CycloneDX][cyclonedx] SBOM at build time.",
        "",
        "<DependencyExplorer />",
        "",
        "[cyclonedx]: https://cyclonedx.org/",
        "",
    ])


def main() -> None:
    args = parse_args()

    if not args.input.exists():
        print(f"Error: input file not found: {args.input}", file=sys.stderr)
        sys.exit(1)

    bom = json.loads(args.input.read_text())
    raw_components = sorted(bom.get("components", []), key=lambda c: c.get("name", ""))
    components = [build_component(c) for c in raw_components]

    now = datetime.now(timezone.utc).strftime("%Y-%m-%d")

    # Generate markdown page
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generate_markdown())
    print(f"Generated {args.output}")

    # Generate JSON data if requested
    if args.json_output:
        binary = extract_binary_info(bom)
        target = extract_target(bom)
        summary = build_summary(components)

        data = {
            "generated": now,
            "binaries": {
                binary["name"]: {
                    "version": binary["version"],
                    "license": binary["license"],
                    "target": target,
                    "summary": summary,
                    "components": components,
                }
            },
        }

        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(json.dumps(data, indent=2) + "\n")
        print(f"Generated {args.json_output} ({len(components)} components)")


if __name__ == "__main__":
    main()
