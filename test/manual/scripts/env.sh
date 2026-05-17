#!/usr/bin/env bash
# Configure the manual-testing shell to publish + consume against the local
# `registry:2` from `test/docker-compose.yml`. Source this file:
#
#   source test/manual/scripts/env.sh
#
# Start the registry first: `cd test && docker compose up -d`.
#
# This script is sourced into the user's interactive shell, so it does NOT
# enable `set -euo pipefail` — `set -u` in particular leaks into the parent
# shell and breaks prompts that reference unset variables (e.g. VSCode's
# `RPROMPT` integration).

export OCX_DEFAULT_REGISTRY=localhost:5000
export OCX_INSECURE_REGISTRIES=localhost:5000

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
