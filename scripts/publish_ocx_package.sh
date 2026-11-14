#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Package one ocx binary and push it to an OCI registry.
#
#   scripts/publish_ocx_package.sh --identifier REG/NS/REPO:TAG --platform P --binary PATH
#       [--sbom PATH] [--sign] [--no-keep-tag] [--cascade] [--tags-file F]
#       [--annotation KEY=VALUE]... [--metadata FILE]
#
# The `ocx` on PATH is the binary under release. Layout: bin/ocx (bin/ocx.exe for a
# windows/* platform) plus the two licence files at the package root, as the release
# archives carry them. Registry credentials and signing identity come from the environment
# of `ocx` itself; this script reads none and prints none. Exit 64 on a usage error,
# otherwise the exit code of the failing ocx step.
set -euo pipefail

usage() {
    sed -n '5,9p' "$0" >&2
    exit 64
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
identifier="" platform="" binary="" sbom="" metadata="$root/packaging/ocx/metadata.json"
push_flags=()

while (($#)); do
    case "$1" in
        --identifier)
            identifier="${2:?--identifier needs a value}"
            shift 2
            ;;
        --platform)
            platform="${2:?--platform needs a value}"
            shift 2
            ;;
        --binary)
            binary="${2:?--binary needs a value}"
            shift 2
            ;;
        --sbom)
            sbom="${2:?--sbom needs a value}"
            shift 2
            ;;
        --metadata)
            metadata="${2:?--metadata needs a value}"
            shift 2
            ;;
        --tags-file)
            push_flags+=(--tags-file "${2:?--tags-file needs a value}")
            shift 2
            ;;
        --annotation)
            push_flags+=(--annotation "${2:?--annotation needs a value}")
            shift 2
            ;;
        --sign)
            push_flags+=(--sign)
            shift
            ;;
        --no-keep-tag)
            push_flags+=(--no-keep-tag)
            shift
            ;;
        --cascade)
            push_flags+=(--cascade)
            shift
            ;;
        -h | --help) usage ;;
        *)
            echo "publish_ocx_package: unknown argument '$1'" >&2
            usage
            ;;
    esac
done

[[ -n "$identifier" && -n "$platform" && -n "$binary" ]] || usage
[[ -f "$binary" ]] || {
    echo "publish_ocx_package: no such binary: $binary" >&2
    exit 64
}
[[ -z "$sbom" ]] || push_flags+=(--sbom "$sbom")

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/stage/bin"
staged=ocx
[[ "$platform" == windows/* ]] && staged=ocx.exe
install -m 0755 "$binary" "$work/stage/bin/$staged"
install -m 0644 "$root/LICENSE" "$work/stage/LICENSE"
install -m 0644 "$root/LICENSE-THIRD-PARTY.md" "$work/stage/LICENSE-THIRD-PARTY.md"

# `push` derives the metadata sidecar from the bundle `create` wrote, so no --metadata there.
ocx package create "$work/stage" -m "$metadata" -o "$work/ocx.tar.xz" -p "$platform"
echo "==> Publishing ${identifier} (${platform})" >&2
ocx package push --identifier "$identifier" --platform "$platform" ${push_flags[@]+"${push_flags[@]}"} "$work/ocx.tar.xz"
