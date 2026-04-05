#!/usr/bin/env bash
# title: Inspecting the dependency tree
# setup: deps-export
ocx install --select webapp:2.0
ocx deps webapp:2.0
