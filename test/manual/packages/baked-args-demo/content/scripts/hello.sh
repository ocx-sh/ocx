#!/bin/sh
# Minimal demonstration script for the baked-args-demo manual fixture.
# Invoked as: sh <installPath>/scripts/hello.sh [user-args...]
# The launcher prepends this path as a baked arg via ${installPath} interpolation.
echo "hello from baked-args-demo"
echo "script: $0"
if [ $# -gt 0 ]; then
    echo "user args: $*"
fi
