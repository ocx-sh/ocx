# Common Environment Variables by Tool Type

| Tool Type | Env Vars |
|-----------|----------|
| CLI tool (single binary) | `PATH` |
| SDK/toolchain | `PATH`, `{TOOL}_HOME` (e.g. `JAVA_HOME`, `GOROOT`) |
| Library | `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, `PKG_CONFIG_PATH` |
| Tool with man pages | `PATH`, `MANPATH` |
