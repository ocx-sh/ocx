#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# A stand-in for `ocx` that the SDK's runtime tests spawn: a fixed behaviour per `FAKE_MODE` (or per first argument
# when that is unset), so the spawn rules can be observed without a registry. POSIX `sh` only.
#
#   FAKE_LOG      file; every invocation appends its argv as one line
#   FAKE_HANDSHAKE the answer to `ocx --format json version`
#   FAKE_STDOUT, FAKE_EXIT   what any other invocation prints and exits with
#   FAKE_PID_FILE modes that run on write their pid here once they are in their final state
#
# Modes: `stdin` says whether stdin is /dev/null, `env` dumps the environment, `flood` writes without end, `graceful`
# notes the interrupt in `$FAKE_PID_FILE.interrupted` and exits, `stubborn` ignores the interrupt, `daemon` exits at
# once while a background sleeper keeps stdout open.

if [ -n "${FAKE_LOG:-}" ]; then
    printf '%s\n' "$*" >>"$FAKE_LOG"
fi

if [ "$*" = "--format=json version" ]; then
    printf '%s' "$FAKE_HANDSHAKE"
    exit 0
fi

case "${FAKE_MODE:-${1:-}}" in
    stdin)
        if [ /dev/stdin -ef /dev/null ]; then
            echo null
        else
            echo piped
            cat
        fi
        exit 0
        ;;
    env)
        env
        exit 0
        ;;
    flood)
        echo $$ >"$FAKE_PID_FILE"
        exec yes
        ;;
    graceful)
        trap 'kill $! 2>/dev/null; echo interrupted > "$FAKE_PID_FILE.interrupted"; exit 0' INT
        echo $$ >"$FAKE_PID_FILE.tmp"
        mv "$FAKE_PID_FILE.tmp" "$FAKE_PID_FILE"
        while :; do
            sleep 60 &
            wait $!
        done
        ;;
    daemon)
        sleep 5 &
        exit 0
        ;;
    stubborn)
        trap '' INT
        echo $$ >"$FAKE_PID_FILE.tmp"
        mv "$FAKE_PID_FILE.tmp" "$FAKE_PID_FILE"
        while :; do
            sleep 1
        done
        ;;
esac

printf '%s' "${FAKE_STDOUT:-}"
exit "${FAKE_EXIT:-0}"
