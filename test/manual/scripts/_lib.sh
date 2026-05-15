# Shared helpers for manual-testing scripts. Sourced from bootstrap.sh,
# teardown.sh, and any future scripts that wrap `ocx`. Provides:
#   - ANSI color palette (gated on $stdout being a tty)
#   - `ocx` wrapper that echoes each invocation before running the binary
#   - `ocx_step` / `ocx_warn` / `ocx_done` log helpers
#
# Callers must set `OCX_BIN` to an absolute path before invoking `ocx`,
# otherwise the wrapper falls back to a `PATH` lookup.

# shellcheck shell=bash

# ANSI colors when stdout is a tty.
if [[ -t 1 ]]; then
    OCX_C_STEP=$'\033[1;36m' # bold cyan — section headers
    OCX_C_CMD=$'\033[0;90m'  # bright-black — echoed commands
    OCX_C_WARN=$'\033[1;33m' # bold yellow
    OCX_C_DONE=$'\033[1;32m' # bold green
    OCX_C_RESET=$'\033[0m'
else
    OCX_C_STEP=""
    OCX_C_CMD=""
    OCX_C_WARN=""
    OCX_C_DONE=""
    OCX_C_RESET=""
fi

# Quote a single argv element for human-readable echo.
ocx_quote() {
    case "$1" in
        '' | *[[:space:]\'\"\\\$\`\&\|\;\<\>\(\)\#\*\?]*) printf "'%s'" "${1//\'/\'\\\'\'}" ;;
        *) printf '%s' "$1" ;;
    esac
}

# Print `$ ocx <args>` (in $OCX_C_CMD) to stderr, then run the real binary.
ocx() {
    local out="" arg
    for arg in "$@"; do
        out+=" $(ocx_quote "$arg")"
    done
    printf '%s$ ocx%s%s\n' "$OCX_C_CMD" "$out" "$OCX_C_RESET" >&2
    "${OCX_BIN:-ocx}" "$@"
}

# Section header.
ocx_step() {
    printf '%s→ %s%s\n' "$OCX_C_STEP" "$*" "$OCX_C_RESET"
}

# Echo `$ cd <dir>` then chdir. Use inside a subshell so the cwd change
# does not leak back to the caller.
ocx_cd() {
    printf '%s$ cd %s%s\n' "$OCX_C_CMD" "$(ocx_quote "$1")" "$OCX_C_RESET" >&2
    cd "$1" || return
}

# Warning to stderr.
ocx_warn() {
    printf '%swarning: %s%s\n' "$OCX_C_WARN" "$*" "$OCX_C_RESET" >&2
}

# Success summary.
ocx_done() {
    printf '%s%s%s\n' "$OCX_C_DONE" "$*" "$OCX_C_RESET"
}

# Write an executable `bin/<name>` script under <pkg_root>/<src>. Body read
# from stdin. Parents created; mode 0755.
scaffold_bin() {
    local dest="$1/bin/$2"
    mkdir -p "$(dirname "$dest")"
    cat >"$dest"
    chmod 0755 "$dest"
}

# Materialize the per-package source trees `ocx package create` consumes.
# These trees are NOT committed (only `metadata.in.json` and the
# `multi-layer-app` `.so` layer stubs are) — they are generated build
# artifacts, gitignored alongside `metadata.json` and `out/`. Idempotent:
# every run overwrites. Arg: the `packages/` root.
#
# Each entrypoint resolves through the composed PATH to
# `${installPath}/bin/<entry>`, so the dispatch target for entrypoint `X`
# is `<src>/bin/X`.
scaffold_payloads() {
    local r="$1" t

    scaffold_bin "$r/single-layer-hello/build" hello <<'EOF'
#!/usr/bin/env bash
echo "hello from single-layer-hello (HELLO_HOME=${HELLO_HOME:-unset})"
EOF

    for t in tool-a tool-b tool-c tool-d; do
        scaffold_bin "$r/multi-entry-toolkit/build" "$t" <<'EOF'
#!/usr/bin/env bash
echo "multi-entry-toolkit: $(basename "$0")"
EOF
    done

    scaffold_bin "$r/deps-leaf-a/build" leaf-a <<'EOF'
#!/usr/bin/env bash
echo "leaf-a (LEAF_A_HOME=${LEAF_A_HOME:-unset})"
EOF

    scaffold_bin "$r/deps-leaf-b/build" leaf-b <<'EOF'
#!/usr/bin/env bash
echo "leaf-b (LEAF_B_HOME=${LEAF_B_HOME:-unset})"
EOF

    # Interface dep leaf-a is on the composed PATH.
    scaffold_bin "$r/deps-mid/build" mid <<'EOF'
#!/usr/bin/env bash
echo "mid (MID_HOME=${MID_HOME:-unset}) -> $(leaf-a)"
EOF

    # mid is interface (on PATH); leaf-b is private (NOT on PATH) — calling
    # `leaf-b` here would fail by design, which is the visibility exercise.
    scaffold_bin "$r/deps-app/build" app <<'EOF'
#!/usr/bin/env bash
echo "app (APP_HOME=${APP_HOME:-unset}) -> $(mid)"
EOF

    scaffold_bin "$r/cross-layer-entrypoint/build" wrap-leaf-a <<'EOF'
#!/usr/bin/env bash
echo "wrap-leaf-a -> $(leaf-a)"
EOF

    # multi-layer-app: layer-base/layer-libs ship committed `.so` stubs;
    # only the layer-app entrypoint tree is generated.
    scaffold_bin "$r/multi-layer-app/layer-app" myapp <<'EOF'
#!/usr/bin/env bash
echo "myapp from multi-layer-app"
EOF
}
