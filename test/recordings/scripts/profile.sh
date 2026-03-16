#!/usr/bin/env bash
# title: Managing shell profile packages
# setup: full-catalog
ocx install --select uv:0.10
ocx install --select node:22
ocx shell profile add uv:0.10 node:22
ocx shell profile list
ocx shell profile load --shell bash
ocx shell profile remove node:22
ocx shell profile list
