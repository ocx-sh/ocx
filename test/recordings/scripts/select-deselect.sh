# title: Switching and removing the active version
# setup: multi-version
ocx install python:3.12
ocx install python:3.11
ocx select python:3.12
ocx find --current python
ocx select python:3.11
ocx find --current python
ocx deselect python
