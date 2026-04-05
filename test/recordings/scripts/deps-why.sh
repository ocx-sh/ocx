#!/usr/bin/env bash
# title: Tracing why a dependency is pulled in
# setup: deps-export
ocx install --select webapp:2.0
ocx deps --why nodejs webapp:2.0
