# Research: do relative symlinks in a tree artifact survive Bazel's cache? (local spike)

## Metadata

**Date:** 2026-11-24
**Lane:** disputed-fact check, run inline (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN, not read. Bazel 9.2.0 (this repository's pinned binary), Linux, ext4, one machine. A scratch workspace outside the repository. Trees are copies of two installed ocx packages: node 24 (4797 files, 3 relative links) and python-build-standalone 3.14 (3780 files, 1048 relative links).
**Expires:** 2027-05-24
**Caveat:** findings only, no recommendation. A disk cache stands in for a remote cache, and a local sandboxed action stands in for a remote executor. No remote executor was used.

## Setup

Two rules in a scratch workspace with an empty `MODULE.bazel`:

- `tree`: `ctx.actions.declare_directory`, then `cp -a <package content>/. <out>/`. A variant uses `cp -aL`, which replaces every link by a copy of its target. That variant is the control.
- `use`: takes the tree as an input and runs the tool from it (`bin/npm --version`, or `bin/python3 -c 'import ssl, sys; ...'`) with the tree's `bin` first on `PATH`.

Flags on every build: `--spawn_strategy=sandboxed,local --disk_cache=<dir>`. `linux-sandbox` is not available on this machine, so the sandbox is `processwrapper-sandbox`.

## Results

| Step | node links on disk | python links on disk | `npm --version` | python |
|---|---|---|---|---|
| 1. First build; the tree action runs locally | 3 of 3 | 1048 of 1048 | `11.19.1` | works |
| 1c. Control: tree built with `cp -aL` | 0 | not built | fails | not run |
| 2. `bazel clean`, rebuild; tree is a disk cache hit; default `--remote_download_outputs` (`toplevel`) | 0 of 3 | 291 of 1048 | fails | works |
| 3. `bazel clean`, rebuild; tree is a disk cache hit; `--remote_download_outputs=all` | 3 of 3 | 1048 of 1048 | `11.19.1` | works |

Details of step 2:

- `bin/npm` came back as a regular file of 54 bytes, which is the content of its target `lib/node_modules/npm/bin/npm-cli.js`. So the link was replaced by a copy. npm then fails, because it resolves `../lib/cli.js` from `bin/`.
- python: 757 links came back as regular files (file count 3780 before, 4537 after). All 291 survivors are in `share/terminfo`, point at a file in the same directory, and have a target that sorts before the link. Links in `bin/`, `lib/` and every link with `../` in its target were replaced.
- python still runs, because `bin/python3` works as a copy of `python3.14`.

## What this shows

- The cache stores the links: step 3 restores all of them from the same cache entries that step 2 used.
- The links are lost when Bazel 9.2.0 does not download the tree as a whole and instead materialises it as the inputs of a later action. That is the default mode.
- The control proves the check can fail: a tree without links breaks npm.

## What this does not show

- Why 291 links survive in step 2. The mechanism was not read in Bazel's source.
- Whether a remote executor receives links or copies. With remote execution the client builds the action's input tree from the same metadata, so the same loss is plausible. It was not tested.
- Whether a gRPC remote cache behaves like the disk cache.
- Whether Bazel 9.3.x behaves differently, or whether an issue is filed for this.
- Windows and macOS.

## Reproduction

`defs.bzl`:

```starlark
def _tree_impl(ctx):
    out = ctx.actions.declare_directory(ctx.label.name)
    flag = "-aL" if ctx.attr.dereference else "-a"
    ctx.actions.run_shell(
        outputs = [out],
        command = "cp {} {}/. {}/".format(flag, ctx.attr.src, out.path),
        execution_requirements = {"no-sandbox": "1"},  # reads a path outside the workspace
    )
    return [DefaultInfo(files = depset([out]))]

tree = rule(_tree_impl, attrs = {"src": attr.string(), "dereference": attr.bool()})

def _use_impl(ctx):
    t = ctx.files.tree[0]
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.run_shell(
        inputs = [t],
        outputs = [out],
        command = """
T=$PWD/{t}
{{ echo "probe_is_link=$([ -L $T/{probe} ] && echo yes || echo no)"
  echo "run=$(cd $T && HOME=$TMPDIR PATH=$T/bin:$PATH {cmd} 2>&1 | tail -1)"
}} > {o}
""".format(t = t.path, o = out.path, probe = ctx.attr.probe, cmd = ctx.attr.cmd),
    )
    return [DefaultInfo(files = depset([out]))]

use = rule(_use_impl, attrs = {"tree": attr.label(), "probe": attr.string(), "cmd": attr.string()})
```

`BUILD.bazel` declares `tree(name = "node", src = <absolute path of the package content>)`, the `dereference = True` control, the python tree, and three `use` targets per tree (`node_use1` to `node_use3`) so that each step runs a consumer that is not yet cached.

Steps: build `//:node_use1 //:node_copies_use //:python_use1`; `bazel clean`; build `//:node_use2 //:python_use2`; `bazel clean`; build `//:node_use3 //:python_use3 --remote_download_outputs=all`. After each step count links with `find bazel-bin/<tree> -type l | wc -l` and read `bazel-bin/<use>.txt`.

Note: inside the `processwrapper-sandbox` every input is itself a link into the output base, so counting links inside the action says nothing. Count them in `bazel-bin`.
