# Research: Do rules_ocx `@tools` launcher bytes depend on OCX_HOME?

## Metadata

**Date:** 2026-11-08
**Domain:** devops
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (disputed fact: is hermetic Bazel a present-day gain for ocx's own build?)
**Expires:** 2027-02-08 (tied to rules_ocx 0.5.0 launcher rendering)

## Direct Answer

Yes, measured. `launchers/zot.sh` in `@tools` embeds the absolute `OCX_HOME`, the absolute `PATH` content entries, the absolute `exec` target, and `OCX_BINARY_PIN` = an absolute path under the Bazel output base. Fetching `@tools` with two different homes produced launchers differing on lines 4–7; the package digest segment was identical. Not measured: that the launcher's bytes change an acceptance test's action key (needs `bazel aquery` or an execution-log diff).

## Code read

- `rules_ocx/ocx/private/project.bzl:269` — `write_launchers(ctx, discovered.bins, entries, ocx_env.home, str(binary), ...)`.
- `rules_ocx/ocx/private/repo_utils.bzl:1643-1660` — `ctx.file("launchers/<name>.sh", render_launcher(...), executable=True)`: real file content, not a symlink.
- `repo_utils.bzl:1519-1520` — POSIX launcher bakes `export OCX_HOME=<home>`, `export OCX_BINARY_PIN=<ocx>`; `.bat` variant ~`:1508`.
- `repo_utils.bzl:1523-1527` — `PATH` entries `<home>/packages/.../content`; final `exec '<home>/packages/.../content/zot'`.
- `repo_utils.bzl:679-686` — home = `ctx.getenv("OCX_HOME")`, else `$HOME/.ocx`.
- `ocx/MODULE.bazel:140-144` — `ocx.project(name = "tools", ...)` without `isolated_home`.
- `OCX_BINARY_PIN` = `str(binary)` under `<output_base>/external/rules_ocx++ocx+ocx_tool/ocx` — also output-base dependent.

## CI vs local home

- Local: `OCX_HOME` unset → `/home/mherwig/.ocx`.
- CI: no `OCX_HOME` in Bazel/acceptance workflows (only `.github/workflows/shell-activation-deep.yml:78,151`, non-Bazel); `.bazelrc` sets no `repo_env`/`action_env` for it → `$HOME/.ocx` on the runner (inferred `/home/runner/.ocx`; `ocx-sh/setup-ocx` source not read).

## Measurement

`ocx exec bazel -- bazel --output_base=<ob1> fetch --repo=@tools` (default home) and the same with `OCX_HOME=<scratch>/home2`, `--output_base=<ob2>`. A Python `difflib` comparison of `launchers/zot.sh`:

```
-export OCX_HOME='/home/mherwig/.ocx'
+export OCX_HOME='.../measure/home2'
-export PATH='/home/mherwig/.ocx/packages/ocx.sh/sha256/79/9d83.../content:...'
+export PATH='.../home2/packages/ocx.sh/sha256/79/9d83.../content:...'
-exec '/home/mherwig/.ocx/packages/.../content/zot' "$@"
+exec '.../home2/packages/.../content/zot' "$@"
```

The proxied `diff` reported "Files are identical" for files of 1.6K vs 3.0K — false; only the Python comparison is evidence. Throwaway servers shut down, scratch deleted.

## negative

- Content digests identical across homes — only the path prefix varies.
- Affects CI↔local remote-cache sharing only; runs with the same home and output base still match each other.
- Launcher-as-action-input not confirmed via aquery.

## leads

- `bazel aquery` / execution-log diff on one acceptance target across two homes — confirms the action-key effect.
- Fix lives in `render_launcher`: stop baking absolute paths (runtime resolution or runfiles-relative).
- `ocx-sh/setup-ocx` source — settle the CI home.
