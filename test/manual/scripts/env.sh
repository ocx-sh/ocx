#!/usr/bin/env bash
# Configure the manual-testing shell to publish + consume against the local
# zot registry that `cd test && uv run python -m src.zot` serves. Source this file:
#
#   source test/manual/scripts/env.sh
#
# Start the registry first, in its own terminal: `cd test && uv run python -m src.zot`
# (needs `zot` on PATH or OCX_TEST_ZOT; Ctrl-C stops it and drops its data).
#
# This script is sourced into the user's interactive shell, so it does NOT
# enable `set -euo pipefail` — `set -u` in particular leaks into the parent
# shell and breaks prompts that reference unset variables (e.g. VSCode's
# `RPROMPT` integration).

# Port follows the same knob `src.zot` binds with (`OCX_TEST_REGISTRY_PORT`, or its
# first argument), so a shell configured for a registry moved off 5000 talks to the
# registry that was actually started rather than to whatever else holds 5000.
export OCX_DEFAULT_REGISTRY="localhost:${OCX_TEST_REGISTRY_PORT:-5000}"
export OCX_INSECURE_REGISTRIES="${OCX_DEFAULT_REGISTRY}"

# Disposable OCX_HOME under test/manual/ (gitignored) — manual experiments
# never collide with the user's daily ~/.ocx state.
# Resolve this script's directory under both bash (`BASH_SOURCE`) and zsh (`$0`).
if [ -n "${ZSH_VERSION:-}" ]; then
    _ocx_src="$0"
else
    _ocx_src="${BASH_SOURCE[0]}"
fi
OCX_HOME="$(cd "$(dirname "${_ocx_src}")/.." && pwd)/.ocx-home"
export OCX_HOME
unset _ocx_src
mkdir -p "${OCX_HOME}"

cat >&2 <<EOF
Configured environment for manual testing with local registry:

registry  : ${OCX_DEFAULT_REGISTRY} (insecure)
OCX_HOME  : ${OCX_HOME}

tip: run \`ocx index update\` after publishing if you want \`ocx package which\` lookups to work.
EOF
