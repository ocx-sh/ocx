#!/usr/bin/env bash
# Wipe the integrations exploration state: the disposable OCX_HOME, the
# generated build/out/metadata trees for the `custom-*` packages, the staged
# managed-config payload, and the two project locks. The registry keeps
# running for the other manual rigs.
#
# Pass --force to skip the confirmation prompt.
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

MANUAL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${OCX_HOME:-${MANUAL_ROOT}/.ocx-home}"

if [[ "${FORCE}" == false ]]; then
    read -r -p "delete ${home}, the custom-* build artifacts and the project locks? [y/N] " ans
    case "${ans:-N}" in
        y | Y | yes | YES) ;;
        *)
            echo "aborted"
            exit 1
            ;;
    esac
fi

if [[ -d "${home}" ]]; then
    rm -rf -- "${home}"
    echo "removed ${home}"
else
    echo "no ${home} — nothing to remove there"
fi

for pkg in custom-leaf custom-private custom-tool custom-other; do
    dir="${MANUAL_ROOT}/packages/${pkg}"
    [[ -d "${dir}" ]] || continue
    rm -rf -- "${dir}/build" "${dir}/out" "${dir}/metadata.json"
done
echo "cleaned packages/custom-*/ build artifacts"

rm -rf -- "${MANUAL_ROOT}/managed-config"
rm -f -- "${MANUAL_ROOT}/projects/integrations/ocx.lock" \
    "${MANUAL_ROOT}/projects/integrations-no-patches/ocx.lock"
echo "cleaned managed-config/ and the project locks"

echo
echo "The patches rig shares this OCX_HOME — run scripts/teardown-patches.sh"
echo "too if you also want packages/patches/ build artifacts gone."
