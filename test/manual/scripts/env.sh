#!/usr/bin/env bash
# Point the shell at the local zot registry from `cd test && uv run python -m src.zot`, on
# the `OCX_TEST_REGISTRY_PORT` it binds; use as `source test/manual/scripts/env.sh`.
# Sourced, so no `set -u`: it would leak into the interactive shell and break prompts
# that read unset variables (VSCode's `RPROMPT` integration).
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
