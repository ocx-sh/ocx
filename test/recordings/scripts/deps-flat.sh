#!/usr/bin/env bash
# title: Resolved dependency order
# setup: deps-export
ocx install --select webapp:2.0
ocx deps --flat webapp:2.0
