#!/usr/bin/env bash
# title: Managing shell profile packages
# setup: full-catalog
ocx install --select cmake:3.31
ocx install --select llvm:22.1
ocx shell profile add cmake:3.31 llvm:22.1
ocx shell profile list
ocx shell profile load --shell bash
ocx shell profile remove llvm:22.1
ocx shell profile list
