#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/env
# title: See the package environment
# description: List the environment variables an installed package declares.
set -euo pipefail

ocx package install $PKG_NODEJS_NODE
# region cast
ocx package env $PKG_NODEJS_NODE
# endregion cast
