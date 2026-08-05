#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: user-guide/managed-config-test
# title: Preview a candidate config before publishing
# description: Catch a typo'd key and preview the effective merge locally, before anything reaches the registry.
set -euo pipefail
cd "$SCENARIO_TMP"

# region cast
printf '[patches]\nregistry = "corp.example.com/ocx-patches"\n[registry]\ndefalt = "corp.example.com"\n' >candidate.toml
ocx config test candidate.toml
# endregion cast

report=$(ocx --format json config test candidate.toml)
echo "$report" | grep -q '"corp.example.com/ocx-patches"'
echo "$report" | grep -q '"registry.defalt"'
echo "$report" | grep -q '"registry_default": null'
