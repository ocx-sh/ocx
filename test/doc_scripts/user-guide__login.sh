#!/usr/bin/env bash
# state: setup:basic
# doc: user-guide/login
# title: Store registry credentials
# description: Log in to a registry and store credentials for subsequent commands.
set -euo pipefail

# Alias the test registry to a name that is not in the runner-harness namespace
# so the display region can show the commands without leaking $REGISTRY.
DEMO_REGISTRY="$REGISTRY"

# region cast
echo "test-token" | ocx login -u ci --password-stdin --allow-insecure-store "$DEMO_REGISTRY"
ocx logout "$DEMO_REGISTRY"
# endregion cast
