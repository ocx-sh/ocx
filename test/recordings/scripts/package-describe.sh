#!/usr/bin/env bash
# title: Attaching package descriptions
# setup: publisher
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
ocx package describe --readme README.md --title "mytool" --description "A small example tool" mytool
ocx package info mytool
