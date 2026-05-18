#!/usr/bin/env bash
# state: setup:basic
# doc: entry-points/path-integration
# title: PATH integration — install, select, and inspect package env
# description: Install a package with --select and inspect the env it contributes to PATH.
set -euo pipefail

ocx package install --select "$PKG_UV"

# Verify the current symlink was created (--select happened during install).
ocx package which --current "$PKG_UV" >/dev/null

# Print the resolved env for the installed package (the eval-safe form).
# This is the per-package equivalent of shell profile activation.
ocx package env --shell=bash "$PKG_UV" >/dev/null
