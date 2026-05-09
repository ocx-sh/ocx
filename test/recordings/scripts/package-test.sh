#!/usr/bin/env bash
# title: Test a package locally before pushing
# setup: publisher
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package test -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool
ocx package test -p linux/amd64 -m metadata.json --keep -i mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool
ocx package push -n -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
