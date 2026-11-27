# Research: input forms for a prebuilt tool distribution under Bazel remote execution (local rig)

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, run (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN. Bazel 9.2.0 against Buildbarn (bb-deployments at `7cd419f`) in Docker on one machine. Hardlinking worker, runner image `ghcr.io/catthehacker/ubuntu:act-22.04` (glibc 2.35). 78 rows of (form, distribution, situation). Eight of the remote rows were repeated on NativeLink v1.9.0. The measurements were made by subagents; this file was assembled by the orchestrator from their raw result files, because the first subagent could not write here.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Two backends, one worker type each, one machine. A single-machine Docker backend is not a production cluster, so only the relative order of the cost numbers means anything.

## Setup

Two unpacked tool distributions were used, both copied from an installed ocx home:

- node: 4797 files, 3 relative symbolic links. `bin/npm` is a link to `../lib/node_modules/npm/bin/npm-cli.js`.
- python-build-standalone 3.14: 3780 files, 1048 relative links. All 1048 point at files; 300 have `../` in the target. This distribution has no link to a directory.

Six forms of handing a distribution to an action:

| Form | What the using action takes as input |
|---|---|
| F1 repository files | Files of an external repository, unpacked by a repository rule with links preserved; `glob(["content/**"])`. |
| F2 repository directory | The same repository; the input is the directory label `content` (a source directory). |
| F3 tree built remotely | A tree artifact (`declare_directory`) unpacked from an archive by an action on the executor. |
| F4 tree built locally | The same unpack action forced onto the client (`no-remote-exec`), output uploaded. |
| F5 archive at use | The archive file; the using action unpacks it into a scratch directory before it runs the tool. |
| F6 files plus declared links | The regular files as in F1 with every link deleted from disk, plus one unresolved symlink artifact per link (`ctx.actions.declare_symlink` and `ctx.actions.symlink(target_path = ...)`), all in the runfiles of one executable. |

Situations: remote execution with everything built in the invocation (R-cold); remote execution with the producer a cache hit after `bazel clean`, default flags (R-cached-default) and with `--remote_download_outputs=all` (R-cached-all); a fresh output base against the warm cache (R-second-client); local sandboxed execution with no cache (L-cold); local sandboxed execution with the remote cache attached and the producer a cache hit (L-cached-default).

Bazel flags on every run: startup `--ignore_all_rc_files`; `--remote_instance_name=hardlinking`, `--remote_default_exec_properties=OSFamily=linux`, `--remote_default_exec_properties=container-image=docker://ghcr.io/catthehacker/ubuntu:act-22.04@sha256:dd7654ffb01d5b7b54b23b9ce928a1f7f2d08c7b3d7e320b6574b55d7ccde78b`. Remote runs add `--remote_executor=grpc://localhost:18980`. Local runs use `--spawn_strategy=sandboxed,local`, which is the processwrapper sandbox on this host.

Remote execution was real: every remote consumer wrote the runner container's id as its host name, and Bazel's summary counted the actions as remote.

## Results

The six situations agree within each form, so the table gives one line per form. `npm --version` prints `11.19.1` when it works. When it fails, node reports `Cannot find module`, because `bin/npm` is then a 54-byte copy of `npm-cli.js` and resolves `../lib/cli.js` from `bin/`.

| Form | Links the remote action sees (node / python) | `npm --version`, remote | `npm --version`, local sandbox | How the execution log lists `bin/npm` | Inputs per action (node / python) |
|---|---|---|---|---|---|
| F1 repository files | 0 / 0 | fails | works | file with the target's digest | 4800 / 4828 |
| F2 repository directory | 0 / 0 | fails | works | file with the target's digest | 4800 / 4828 |
| F3 tree built remotely | 0 / 0 | fails | works when built in the same invocation; fails after a cache hit | file with the target's digest | 4800 / 4828 |
| F4 tree built locally | 0 / 0 | fails | works when built in the same invocation; fails after a cache hit | file with the target's digest | 4800 / 4828 |
| F5 archive at use | 3 / 1048 | works | works | not listed; the archive is the only input | 1 / 1 |
| F6 files plus declared links | 3 / 1048 | works | works | symlink with the original relative target | 4804 / 4832 |
| Control: F1 with links copied (`cp -aL`) | 0 | fails | fails | not logged | not logged |

Further observations:

- python ran in every form and every situation, because `bin/python3` works as a copy of `python3.14`. Only the link count tells the forms apart for python.
- F6 with 1048 declared links built and ran with no error.
- For F3 and F4, `--remote_download_outputs=all` puts the 3 links back on the client's disk, and the remote consumer still sees 0 links. So the loss for a remote action does not depend on the download mode.
- For F3 and F4, a local sandboxed consumer loses the links after a cache hit under the default download mode. F1, F2 and F6 do not have this failure, because their files are on the client's disk from the repository rule.
- The control fails in all six situations, including the local sandbox where plain F1 passes. So the check can be red and green.

## Cost (node only)

Wall-clock seconds of one Bazel invocation that runs 24 distinct consumers at `--jobs=4`, each `node -e "process.exit(0)"`, with everything else already built and uploaded. Two runs each.

| Form | Remote, run 1 / run 2 | Local sandbox, run 1 / run 2 |
|---|---|---|
| F1 | 2.00 / 1.94 | 1.03 / 0.70 |
| F2 | 1.74 / 1.81 | 0.36 / 0.31 |
| F3 | 1.97 / 2.04 | 1.12 / 0.68 |
| F5 | 10.97 / 13.52 | 14.15 / 48.38 |
| F6 | 1.80 / 2.16 | 2.19 / 0.66 |

The first local F6 run includes building the launcher's runfiles tree once. The second local F5 number is noisy and was not investigated. F5 unpacked with plain `tar` and gzip here; the cost of unpacking with `ocx` itself is in `research_ocx-content-tree-env-offline-facts.md`.

Loading and analysis, `bazel build --nobuild` after `bazel shutdown`, seconds, two runs:

| Targets | Run 1 / run 2 |
|---|---|
| node F1, 24 targets | 2.99 / 2.97 |
| node F2, 24 targets | 2.70 / 2.58 |
| node F3, 24 targets | 2.26 / 2.39 |
| node F5, 24 targets | 2.25 / 2.28 |
| node F6, 24 targets | 2.89 / 2.92 |
| python F6, 1 target, 1048 declared links | 2.62 / 2.67 |
| python F1, 1 target | 2.80 / 2.79 |

First upload after the backend's storage was wiped, one remote consumer:

| Form | Wall (s) | Bytes sent by the client |
|---|---|---|
| F1 | 7.85 | 144 MB in 3385 chunk writes; 2344 distinct digests |
| F3 | 10.0 | 59 MB; one 58.7 MB archive, the tree is built on the executor |

## Second backend: NativeLink

The remote rows for F1, F3, F5 and F6 were repeated on NativeLink v1.9.0 (image `ghcr.io/tracemachina/nativelink:v1.9.0`), with everything built in the invocation and default flags. The worker image is the same Ubuntu 22.04 image with the NativeLink binary copied in. Every consumer wrote the worker container's name as its host name, and Bazel counted the actions as remote.

| Distribution | Form | Links seen | Probe is a link | Run | Exit code |
|---|---|---|---|---|---|
| node | F1 | 0 | no | `npm` fails | 1 |
| node | F3 | 0 | no | `npm` fails | 1 |
| node | F5 | 3 | yes | `11.19.1` | 0 |
| node | F6 | 3 | yes | `11.19.1` | 0 |
| python | F1 | 0 | no | `(3, 14)` | 0 |
| python | F3 | 0 | no | `(3, 14)` | 0 |
| python | F5 | 1048 | yes | `(3, 14)` | 0 |
| python | F6 | 1048 | yes | `(3, 14)` | 0 |

All eight rows match Buildbarn. The execution log again lists `bin/npm` as a regular file with a digest for F1, and as a symlink with the target `../lib/node_modules/npm/bin/npm-cli.js` and no digest for F6.

The `GetCapabilities` call reports `symlinkAbsolutePathStrategy = DISALLOWED` for NativeLink and `ALLOWED` for Buildbarn. That field covers absolute link targets only. NativeLink delivered the relative links of F5 and F6 intact.

Not done on NativeLink: the cached, second-client and local rows, and the cost runs. How its worker builds the input root was not verified.

## What this shows

- Every form in which Bazel hands over a directory or globbed files (F1 to F4) reaches a remote executor with zero symbolic links. Bazel lists each former link as a regular file with the target's digest. This matches the source read in `research_bazel-9.2-symlink-input-mechanics.md`.
- A tool that depends on a link's position breaks remotely in all four of those forms. Here that is `npm`.
- Trees (F3, F4) also break locally after a cache hit. Repository files (F1, F2) do not.
- Declared links (F6) and the archive (F5) are the two forms that keep links on the executor.
- F6 costs the same as F1 and F3 per action in this rig. F5 costs about six times more per action with plain `tar`.
- F2 is the cheapest form in the local sandbox, because the sandbox stages one link for the whole directory.
- 1048 declared links add no visible loading or analysis time.

## What this does not show

- Any backend other than Buildbarn with the hardlinking worker and NativeLink. No hosted service was tested, and no worker with a virtual file system.
- Declared links that point at directories. The python distribution has none.
- A set of several packages in one launcher, or input roots of tens of thousands of files.
- Production numbers. The rig is one machine.
- Download modes other than the default and `all`.
- macOS or Windows.

## Reproduction

The backend is the unmodified `docker-compose` directory of `bb-deployments`, started with `COMPOSE_PROJECT_NAME=ocxrbe docker compose up -d` and with the frontend published on port 18980.

`MODULE.bazel` declares one repository per form and distribution with a local repository rule:

```starlark
dist = use_repo_rule("//:repos.bzl", "dist")
dist(name = "node_files", src = NODE, kind = "files")
dist(name = "node_filesL", src = NODE, kind = "filesL")   # control
dist(name = "node_arch", src = NODE, kind = "archive")
dist(name = "node_f6", src = NODE, kind = "f6")
```

`repos.bzl`, the part that matters for F6:

```starlark
elif kind == "f6":
    _run(ctx, ["mkdir", "content"])
    _run(ctx, ["cp", "-a", src + "/.", "content"])
    out = _run(ctx, ["find", "content", "-type", "l", "-printf", "%P\\t%l\\n"])
    links = {}
    for line in out.splitlines():
        p, t = line.split("\t", 1)
        links[p] = t
    _run(ctx, ["find", "content", "-type", "l", "-delete"])
    ctx.file("BUILD.bazel", vis + """
load("@@//:f6.bzl", "dist_exe")
dist_exe(
    name = "exe",
    files = glob(["content/**"]),
    links = LINKS,
)
""".replace("LINKS", json.encode(links)))
```

`f6.bzl`:

```starlark
def _impl(ctx):
    links = []
    for rel, target in ctx.attr.links.items():
        l = ctx.actions.declare_symlink("content/" + rel)
        ctx.actions.symlink(output = l, target_path = target)
        links.append(l)
    exe = ctx.actions.declare_file("exe.sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(
        executable = exe,
        runfiles = ctx.runfiles(files = ctx.files.files + links),
    )]

dist_exe = rule(_impl, attrs = {"files": attr.label_list(allow_files = True), "links": attr.string_dict()}, executable = True)
```

The consumer takes the executable as a tool and uses `<executable>.runfiles/<repository name>/content` as the distribution root. For the other forms the root is the repository's `content` directory (F1, F2), the tree artifact (F3, F4), or a scratch directory the action unpacks into (F5). The consumer counts links under the root, reads the probe path with `readlink` and `realpath`, and runs the tool with the root's `bin` first on `PATH`.

The unpack action for F3 and F4 is `tar -xzf <archive> -C <declared directory>`, with `execution_requirements = {"no-remote-exec": "1"}` for F4.
