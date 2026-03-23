#!/usr/bin/env bash
# title: Working with variants
# setup: variants
ocx index list python --variants
ocx install python:debug-3.13
ocx exec python:debug-3.13 -- python3 --version
