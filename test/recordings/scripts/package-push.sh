#!/usr/bin/env bash
# title: Publishing a package
# setup: publisher
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
ocx index update mytool
ocx index list mytool
