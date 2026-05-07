#!/usr/bin/env bash
# title: Publishing a multi-platform package
# setup: publisher
ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
ocx package create build -i mytool:1.0.0 -p linux/arm64 -m metadata.json -o .
ocx package push -n -c -p linux/amd64 mytool:1.0.0 mytool-1.0.0-linux-amd64.tar.xz
ocx package push -c -p linux/arm64 mytool:1.0.0 mytool-1.0.0-linux-arm64.tar.xz
ocx index update mytool
ocx index list mytool --platforms
ocx install mytool:1.0.0
ocx exec mytool:1.0.0 -- mytool
