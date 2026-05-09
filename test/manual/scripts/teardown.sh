#!/usr/bin/env bash
# Wipe the manual-testing OCX_HOME after asking for confirmation.
set -euo pipefail

_ocx_manual_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${OCX_HOME:-${_ocx_manual_dir}/.ocx-home}"
if [[ ! -d "$home" ]]; then
    echo "no $home — nothing to remove"
    exit 0
fi

read -r -p "delete $home and everything beneath it? [y/N] " ans
case "${ans:-N}" in
    y|Y|yes|YES)
        rm -rf -- "$home"
        echo "removed $home"
        ;;
    *)
        echo "aborted"
        exit 1
        ;;
esac
