# title: Managing shell profile packages
# setup: full-catalog
ocx install --select cmake:3.28
ocx install --select clang:18
ocx shell profile add cmake:3.28 clang:18
ocx shell profile list
ocx shell profile load --shell bash
ocx shell profile remove clang:18
ocx shell profile list
