#!/usr/bin/env bash
# Wipe the patches manual exploration state (OCX_HOME only; does not touch
# the registry which remains running for other manual tests).
#
# Pass --force to skip the confirmation prompt (useful in CI or scripted demos).
set -euo pipefail
IFS=$'\n\t'

FORCE=false
for arg in "$@"; do
    case "${arg}" in
        --force) FORCE=true ;;
        *)
            echo "unknown argument: ${arg}" >&2
            exit 64
            ;;
    esac
done

_manual_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${OCX_HOME:-${_manual_dir}/.ocx-home}"

if [[ ! -d "${home}" ]]; then
    echo "no ${home} — nothing to remove"
    exit 0
fi

if [[ "${FORCE}" == false ]]; then
    read -r -p "delete ${home} and all package out/ dirs under packages/patches/? [y/N] " ans
    case "${ans:-N}" in
        y | Y | yes | YES) ;;
        *)
            echo "aborted"
            exit 1
            ;;
    esac
fi

rm -rf -- "${home}"
echo "removed ${home}"

# Also wipe the generated build trees under packages/patches/ (gitignored).
patches_pkg_root="${_manual_dir}/packages/patches"
if [[ -d "${patches_pkg_root}" ]]; then
    find "${patches_pkg_root}" -name 'out' -type d -exec rm -rf {} + 2>/dev/null || true
    find "${patches_pkg_root}" -name 'build' -type d -exec rm -rf {} + 2>/dev/null || true
    find "${patches_pkg_root}" -name 'metadata.json' -type f -exec rm -f {} + 2>/dev/null || true
    echo "cleaned packages/patches/ build artifacts"
fi
