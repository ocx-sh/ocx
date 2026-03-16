#!/usr/bin/env bash
# title: Switching and removing the active version
# setup: multi-version
ocx install corretto:21
ocx install corretto:25
ocx select corretto:21
ocx find --current corretto
ocx select corretto:25
ocx find --current corretto
ocx deselect corretto
