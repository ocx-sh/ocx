# Research: end-to-end prototype of ocx packages as action inputs under Bazel remote execution

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, run (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN. Bazel 9.2.0 against Buildbarn in Docker (hardlinking worker, Ubuntu 22.04 runner), with a test build of ocx main as the package manager. A throwaway workspace of 258 lines of Starlark. The last section was run by the orchestrator outside Bazel.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. One machine, one worker, a warm content store. The launcher is a POSIX `sh` script, which the design under discussion does not allow on an executor. Environment entries are composed in set order by the prototype, not by ocx.

## Setup

- Bazel 9.2.0, always started with `--ignore_all_rc_files`, output root under `rbe/proto-out`, driver script `rbe/proto/b`. Remote runs use `--remote_executor=grpc://localhost:18980`, instance `hardlinking`, the two default exec properties of the earlier experiment, and `--allow_unresolved_symlinks`. Local runs use `--spawn_strategy=sandboxed,local` and no remote flag.
- Backend: the Buildbarn compose project `ocxrbe` (one hardlinking worker, Ubuntu 22.04 runner image). It is a single-machine Docker backend, not a production cluster; every remote number is a one-box number.
- Package manager: `rbe/../bin/ocx-main`, treated as opaque. Every call sets `OCX_HOME` and `HOME` to directories inside the repository being filled and `OCX_NO_CONFIG=1`. The real `~/.ocx` was not written.
- Packages: node 24.21.0 (4797 files, 3 links), python-build-standalone 3.14.7_20260814 (3521 files, 1047 links), shfmt 3.13.1, pipx 1.18 and its private dependency python-build-standalone 3.13.14 from ghcr.io (a different registry), identifiers taken from `ocx.toml`/`ocx.lock`, pipx from the task text.
- Workspace: `rbe/proto` with `MODULE.bazel`, `repo.bzl` (repository rule), `rules.bzl` (package rule, launcher rule, macro), `BUILD.bazel` (sets and consumers), `synthetic/` (T7 package), `b` and `t8.sh` (drivers), `res/` (raw outputs and execution logs).

## Design as built

- One repository per package and platform (`node_linux_amd64`, and so on). The rule pulls into a scratch home inside the repository, reads `metadata.json` entrypoints, moves `content` to `<repo>/content`, deletes the scratch home, records each symbolic link (path, target, type of target) and deletes them from disk, then writes `BUILD.bazel` with a `glob(["content/**"])`, the link table, the env report and the entrypoints. Link re-declaration is the "f6" form (`declare_symlink` plus `symlink(target_path=...)`) inside the runfiles.
- Env report: `package env --self` with every absolute package-root prefix replaced by `@@OCXROOT:<registry/repository>@@`. The plain report was not used: for pipx its only entry is `<root>/entrypoints`, whose launchers call `ocx launcher exec` (the package manager) and whose content holds no `bin/`.
- A set is a list of package targets. `<set>_<tool>` execs the bare name under the set environment; `<set>_run` execs argv. At run time the launcher finds `$0.runfiles` (else `RUNFILES_DIR`), and the generated script lines set each env entry with the token replaced by `$RF/<runfiles repo dir>`. `path` prepends, `constant` replaces; `list` appends with `:` (no `list` entry occurred).
- A tool that is an entrypoint in package metadata (pipx) is launched as its declared command plus fixed arguments.
- Consumers are plain `genrule`s with the launcher in `tools`.

## Results

Each cell is the decisive output line. "remote" ran on the Buildbarn worker, "local" in the Bazel sandbox on this host.

| Check | Remote | Local sandbox |
|---|---|---|
| T1 `npm --version` | pass: `11.19.0` | pass: `11.19.0` |
| T2a python ssl | pass: `(3, 14)` | pass: `(3, 14)` |
| T2b `pipx --version` (set = pipx + dependency) | pass: `1.18.19` | pass: `1.18.19` |
| T3 `shfmt --version` | pass: `v3.13.1` | pass: `v3.13.1` |
| T4 node, python, shfmt by bare name from one command | pass: `v24.21.0`, `(3, 14)`, `v3.13.1`, and `command -v node` resolves under `s_all3_run.runfiles/+ocx_package+node_linux_amd64/content/bin/node` | pass, same three lines |
| T5 execution log of T1 | 4804 inputs: 4801 files, 3 with a symlink target. `.../content/bin/npm` is listed as `"symlinkTargetPath": "../lib/node_modules/npm/bin/npm-cli.js"` with no digest. No input path ends in `/ocx` or contains `ocx-main`. | n/a |
| T6 network | runner container NetworkMode `none`; TCP connect to 1.1.1.1:443 prints `ERROR ENETUNREACH` | same action prints `CONNECTED` (host network) |
| T7 directory links | pass (details below) | pass |
| T8 | see cost table | see cost table |
| T9 second client | repository supplied from the repo contents cache, not re-run | n/a |
| T10 `bazel vendor` | pass: `11.19.0` with the package manager deleted | n/a |

### T2: pipx and its dependency

pipx's environment report (with `--self`) did refer to the dependency's root: its first entry is `PATH` = `<ghcr.io python root>/content/bin`, tokenized as `@@OCXROOT:ghcr.io/ocx-contrib/astral-sh/python-build-standalone@@/content/bin`. The mapping resolved it: the set `s_pipx` holds the dependency package target, whose token matches, and the generated launcher line is `PATH="$(printf %s ''"$RF/+ocx_package+pbs313_linux_amd64"'/content/bin')${PATH:+:$PATH}"`. Without `--self` the report names only `<pipx root>/entrypoints`. A set that lacked the dependency target fails at analysis with the "unresolved root token" error (check built into the launcher rule; not run as a negative case).

### T7: directory links

Synthetic package: `share/real/{a.txt,b.txt}`, `share/alias -> real` (directory link), `bin/up -> ../other/o.txt`, `bin/f1`, `bin/f2 -> f1`, `other/o.txt`. Remote output:

```
test -L share/alias: yes
readlink share/alias: real
ls share/alias/: a.txt b.txt
cat share/alias/a.txt: r1
test -L bin/up: yes -> ../other/o.txt; cat: other
test -L bin/f2: yes -> f1; cat: same
find -type l: ./bin/f2 ./bin/up ./share/alias
find -type f: ./bin/f1 ./other/o.txt ./share/real/a.txt ./share/real/b.txt
```

No file under the linked directory had to be excluded and no duplicate arose, because the rule deletes every link from disk before BUILD is read, so `glob` sees only `share/real/*` once. The link itself is an unresolved symlink artifact with target `real`. A control with the directory link left on disk (to see Bazel's glob duplicates) was not run. In the local sandbox `find -type f` prints nothing and `find -type l` lists every file, because the sandbox stages every input as a symlink; the `test -L` lines for the three links are therefore not distinguishing there, while `readlink` shows the right targets.

### T8 cost

One-box numbers: single Docker backend with a warm content store (the same input blobs were uploaded by earlier runs), 12-core host, `--jobs=4`, 24 distinct genrules per batch (each runs `node -e "process.exit(0)"` through a launcher; a salt in the command makes every action key distinct). Wall-clock seconds measured around the whole `bazel build` call (server already running, so analysis and startup of an incremental invocation are included). Remote batches r3 and r4, local batches r1 and r2; a new batch was used per run so no action-cache hit could occur (24 remote or 24 sandbox processes were reported each time).

| Set | Inputs per action | Remote batch 1 / 2 | Local sandbox batch 1 / 2 | Launcher first build, target config, after `clean` (remote run / local run) |
|---|---|---|---|---|
| (a) node only | 4804 (4801 files, 3 links) | 2.5 s / 1.8 s | 1.7 s / 0.8 s | 0.9 s / 0.8 s |
| (b) node, python, pipx, pbs313, shfmt | 14112 (12015 files, 2097 links) | 6.4 s / 4.1 s | 3.5 s / 1.4 s | 1.6 s / 1.6 s |
| (c) node under six repository names | 28804 (28786 files, 18 links) | 10.0 s / 9.7 s | 7.2 s / 4.2 s | 11.0 s (outlier) then 2.7 s, 3.4 s / 2.7 s then 2.7 s, 3.0 s |

Notes on the cost table:
- Input counts come from `--execution_log_json_file` (`res/t8-*/probe*.json`, `batch_*_r*.json`; for (a) and (b) all 24 actions of the batch show the same count). For (c) the count comes from the one-action probe log.
- "Launcher first build" is `bazel build //:s_<set>_node` after `bazel clean` (outputs removed, external repositories kept): analysis plus local runfiles-tree creation. The first c run measured 11.0 s and three later repeats 2.7 to 3.4 s; the 11.0 s was not reproduced and is reported as measured.
- First build of the exec-configuration launcher plus one action (probe, after clean, remote with `--noremote_accept_cached`): remote 1.0 s (a), 3.4 s (b), 4.2 s (c); local 0.9 s, 2.0 s, 5.3 s.
- Repository fetch of the sets (first time in an output base, includes the downloads through the package manager): set (b) packages 12.3 s remote-client and 12.4 s local-client, set (c) six node repositories 9.3 s and 9.7 s. The node repository alone had been fetched before (0.2 to 0.4 s). These fetch runs used the `fetch` command, which `b` ran without `--repository_cache`, so they used the output root's default repository cache.
- Remote was slower than local sandbox here in every cell; both include only this host and a local Docker worker.

### T9: second client

A second client with a fresh output base (`ob-c3`) on the same machine, same `--repository_cache=rbe/proto-out/repo-cache`, did not re-run the repository rule. Evidence: the rule writes `marker.txt` containing pid, nanosecond time; the first client's `node_linux_amd64/marker.txt` is `2846907-1795566267710161355-` and the fresh client's is the identical string. The fresh client's `external/+ocx_package+node_linux_amd64` is a symbolic link into `rbe/proto-out/repo-cache/contents/7e046ade.../24dd53dc-...`, i.e. Bazel's repository contents cache (`<repository_cache>/contents`, 3.2 GB for all repositories of this prototype), which was the cache directory in effect. The build took 8.2 s, none of it spent on a package manager pull. `repo_metadata(reproducible = True)` is in the rule (Bazel 9.2.0 has it); the experiment did not run a control without it.

### T10: bazel vendor

`bazel vendor --vendor_dir=rbe/proto-out/vendor-b //:t1` wrote 210 MB for the repositories T1 needs. Then the package manager binary (a copy referenced by the rule's `ocx` attribute) was deleted, and `bazel build //:t1 --vendor_dir=... ` on a fresh output base and an empty repository cache, remote, with `--noremote_accept_cached`, passed (`11.19.0`, one remote process); the repository was a symbolic link into the vendor directory.

## Deviations and failures

1. Platform: `-p linux/amd64` returned `package not found` for packages that publish only libc variants (pipx, python, node). The exact error: `failed to install package: ocx.sh/pypa/pipx:1.18 — package not found`. `package inspect` showed candidates `linux/amd64+libc.glibc` and `+libc.musl` only. Changed to `-p linux/amd64+libc.glibc` for all packages (shfmt resolves with either).
2. pipx has no `bin/pipx`. First T2b remote run failed: the bare name `pipx` resolved to the runner image's own `/bin/pipx`, giving `AttributeError: module 'enum' has no attribute 'StrEnum'` (image python 3.10 loading pipx's `site-packages` through `PYTHONPATH`). Cause: pipx is declared as an entrypoint in `metadata.json`, a command plus args, and its generated launcher calls `ocx launcher exec`. Changed: the repository rule records `entrypoints`, the launcher rule emits the declared command and arguments. After that: `1.18.19` on both places.
3. Env surface: the default `package env` report for pipx holds only the `entrypoints` directory; the `--self` report was used instead (see Design).
4. T10 as described (changing the tool path through an environment variable) does not pass. With the tool path read through `ctx.getenv("OCX_BIN")` (recorded as `ENV:OCX_BIN` in the vendor marker) and `--repo_env=OCX_BIN=/nonexistent/ocx`, Bazel re-ran the rule instead of using the vendored copy and failed: `execvp(/nonexistent/ocx, ...): No such file or directory`, then `ERROR: no such package '@@+ocx_package+node_linux_amd64//'`. Changed: the tool path is the rule attribute `ocx`; the rule's inputs were left unchanged between vendoring and building and the binary itself was deleted. That passed. The `repo.bzl` printed below has the attribute default restored to the original path; the vendoring run used a default pointing at a temporary copy.
5. The first `package pull` probes (before the rule existed) ran with the real `HOME`, before I set `HOME` and `OCX_NO_CONFIG=1`; the tool may have looked at user-level config on those few probe commands. The prototype rule and all measured runs set both.
6. `execution_log_json_file` output is pretty-printed concatenated objects, not lines; a stream parser was used (`res/xl.py`).
7. The remote 24-action batches ran against a warm worker content store; a cold-store figure was not measured (the backend could not be reset).

## What this does not show

- Cluster behaviour: one worker, one machine, localhost gRPC, no contention, no network cost between client and cluster. Numbers do not transfer to a production cluster.
- A cold content store, cache eviction, or a second worker.
- Other platforms, other Bazel versions, Windows, darwin, musl.
- Reproducibility of the tags: packages are pulled by tag; `reproducible = True` is a declaration, not a check; nothing compared two pulls byte for byte.
- Package metadata kinds beyond `path` and `constant`; `list` is implemented with a guessed `:` separator and was never exercised. Entry order across packages follows the set list, not the package manager's own topological order, and a dependency's entries are applied twice when the dependency is also in the set (harmless for `PATH`).
- Files with unusual names, empty directories (dropped by `glob`), links with absolute targets (none existed in these packages), and a `BUILD`/`BUILD.bazel` file inside a package (glob would stop at that directory; none existed in these packages, not checked for the 25k set beyond file-count equality for node: glob 4797 = find 4797).
- Executables that need the package manager at run time (entrypoints of the `ocx launcher exec` kind) are re-created only from metadata, not by ocx.
- The directory-link control (link left on disk), signature verification (the pull printed that no trust policy covered the packages), and the second client with a changed package set.

## Rule sources

### MODULE.bazel

```python
module(name = "ocxproto")

ocx_package = use_repo_rule("//:repo.bzl", "ocx_package")

# One repository per package and platform. Dependencies are declared by hand.
ocx_package(name = "node_linux_amd64", id = "ocx.sh/nodejs/node:24.21.0")
ocx_package(name = "python_linux_amd64", id = "ocx.sh/astral-sh/python-build-standalone:3.14.7_20260814")
ocx_package(name = "shfmt_linux_amd64", id = "ocx.sh/shfmt/shfmt:3.13.1")
ocx_package(name = "pipx_linux_amd64", id = "ocx.sh/pypa/pipx:1.18")
ocx_package(name = "pbs313_linux_amd64", id = "ghcr.io/ocx-contrib/astral-sh/python-build-standalone:3.13.14@sha256:0dd5ac8eb7f146720e674624548fac05f1e589c1f91792fff5dc8fb73ef38c12")

# T8(c): the same node content under six repository names, each with its own token.
[ocx_package(name = "node%d_linux_amd64" % i, id = "ocx.sh/nodejs/node:24.21.0", token = "node-copy-%d" % i) for i in range(1, 7)]

# T7: a hand-made package directory, fed through the same link re-declaration.
ocx_package(name = "syn_local", id = "local/synthetic:0", local_src = "synthetic")
```

### Repository rule (`repo.bzl`)

```python
"""Repository rule: one package, one platform."""

def _run(ctx, args, env = {}):
    r = ctx.execute(args, environment = env, timeout = 900)
    if r.return_code != 0:
        fail("{} failed (rc={}): {}".format(args, r.return_code, r.stderr))
    return r.stdout

def _token(ident):
    # registry/repository, tag and digest dropped
    return ident.split("@")[0].rsplit(":", 1)[0]

def _tokenize(value, roots):
    for root, tok in roots:
        value = value.replace(root, "@@OCXROOT:" + tok + "@@")
    return value

def _impl(ctx):
    token = ctx.attr.token or _token(ctx.attr.id)
    env_items = []
    entrypoints = {}
    if ctx.attr.local_src:
        # local-directory variant: no package manager involved
        src = ctx.path(Label("@@//:MODULE.bazel")).dirname.get_child(ctx.attr.local_src)
        _run(ctx, ["mkdir", "content"])
        _run(ctx, ["cp", "-a", str(src) + "/.", "content"])
        env_items = [{"key": "SYN_CONTENT", "value": "@@OCXROOT:" + token + "@@/content", "kind": "constant"}]
    else:
        ocx = ctx.getenv("OCX_BIN") or ctx.attr.ocx
        scratch = ctx.path("scratch-home")
        env = {
            "OCX_HOME": str(scratch),
            "HOME": str(scratch) + "/user-home",  # keep the tool away from the real home and its credential files
            "OCX_NO_CONFIG": "1",
        }
        _run(ctx, ["mkdir", "-p", str(scratch) + "/user-home"])
        base = [ocx, "--format", "json"]
        pl = ["-p", ctx.attr.platform]
        pulled = json.decode(_run(ctx, base + ["package", "pull"] + pl + [ctx.attr.id], env))
        root = pulled["paths"].values()[0]
        # every package root in this home: the package itself plus its dependencies
        deps = json.decode(_run(ctx, base + ["package", "deps", "--flat", "--self"] + pl + [ctx.attr.id], env))
        roots = [(root, token)]
        for d in deps["items"]:
            ident = d["identifier"]
            if _token(ident) == _token(ctx.attr.id):
                continue
            w = json.decode(_run(ctx, base + ["package", "which"] + pl + [ident], env))
            roots.append((w["paths"].values()[0]["path"], _token(ident)))
        # --self: the surface a tool needs to run itself (the public surface of a package with a private dependency names only `entrypoints/`, which calls ocx)
        rep = json.decode(_run(ctx, base + ["package", "env", "--self"] + pl + [ctx.attr.id], env))
        env_items = [{"key": i["key"], "value": _tokenize(i["value"], roots), "kind": i["kind"]} for i in rep["items"]]
        entrypoints = json.decode(ctx.read(root + "/metadata.json")).get("entrypoints", {})
        _run(ctx, ["mv", root + "/content", "content"])
        ctx.delete(scratch)

    # symbolic links: record, then delete from disk
    out = _run(ctx, ["find", "content", "-type", "l", "-printf", "%P\\t%l\\t%Y\\n"])
    links = {}
    dirlinks = []
    for line in out.splitlines():
        p, t, y = line.split("\t")
        links[p] = t
        if y == "d":
            dirlinks.append(p)
    _run(ctx, ["find", "content", "-type", "l", "-delete"])
    ctx.file("env.json", json.encode({"token": token, "items": env_items, "entrypoints": entrypoints}))
    ctx.file("links.json", json.encode(links))
    ctx.file("marker.txt", _run(ctx, ["sh", "-c", "echo $$-$(date +%s%N)-$RANDOM"]))
    ctx.file("BUILD.bazel", """package(default_visibility = ["//visibility:public"])
load("@@//:rules.bzl", "ocx_pkg")
ocx_pkg(
    name = "pkg",
    token = {token},
    files = glob(["content/**"]),
    links = {links},
    env_json = {env},
    entrypoints_json = {eps},
)
exports_files(["env.json", "links.json", "marker.txt"])
""".format(token = json.encode(token), links = json.encode(links), env = json.encode(json.encode({"items": env_items})), eps = json.encode(json.encode(entrypoints))))
    if hasattr(ctx, "repo_metadata"):
        return ctx.repo_metadata(reproducible = True)
    return None

ocx_package = repository_rule(
    _impl,
    attrs = {
        "id": attr.string(mandatory = True),
        "platform": attr.string(default = "linux/amd64+libc.glibc"),
        "token": attr.string(),
        "local_src": attr.string(),
        "ocx": attr.string(default = "<scratch>/bin/ocx-main"),
    },
    environ = ["OCX_BIN"],
)
```

### Package rule, launcher rule and macro (`rules.bzl`); the launcher template is the `script = """..."""` string in `_launcher_impl`

```python
"""Package target, tool-set launchers."""

PkgInfo = provider(fields = ["token", "repo", "env", "entrypoints"])

def _pkg_impl(ctx):
    # Unresolved symlink artifacts: the link itself is the input, never its target.
    links = []
    for rel, target in ctx.attr.links.items():
        l = ctx.actions.declare_symlink("content/" + rel)
        ctx.actions.symlink(output = l, target_path = target)
        links.append(l)
    return [
        DefaultInfo(runfiles = ctx.runfiles(files = ctx.files.files + links)),
        PkgInfo(token = ctx.attr.token, repo = ctx.label.repo_name, env = json.decode(ctx.attr.env_json)["items"], entrypoints = json.decode(ctx.attr.entrypoints_json)),
    ]

ocx_pkg = rule(_pkg_impl, attrs = {
    "token": attr.string(),
    "files": attr.label_list(allow_files = True),
    "links": attr.string_dict(),
    "env_json": attr.string(),
    "entrypoints_json": attr.string(default = "{}"),
})

def _sq(s):
    return "'" + s.replace("'", "'\\''") + "'"

def _render(value, root_of):
    # shell word: literal parts single-quoted, each root token replaced by "$RF/<runfiles dir of that package>"
    w = _sq(value)
    for tok, repo in root_of.items():
        w = w.replace("@@OCXROOT:" + tok + "@@", "'\"$RF/" + repo + "\"'")
    return w

def _launcher_impl(ctx):
    pkgs = [p[PkgInfo] for p in ctx.attr.packages]
    root_of = {p.token: p.repo for p in pkgs}
    lines = []
    for p in pkgs:
        for e in p.env:
            k, v, kind = e["key"], _render(e["value"], root_of), e["kind"]
            if "@@OCXROOT:" in v:
                fail("unresolved root token in %s of %s (set holds: %s)" % (k, p.token, root_of.keys()))
            if kind == "path":  # prepend
                lines.append('%s="$(printf %%s %s)${%s:+:$%s}"; export %s' % (k, v, k, k, k))
            elif kind == "constant":
                lines.append("%s=%s; export %s" % (k, v, k))
            else:  # list: append, ':' separator (prototype only; the report carries no separator)
                lines.append('%s="${%s:+$%s:}"%s; export %s' % (k, k, k, v, k))
    cmd = 'exec "$@"'
    if ctx.attr.tool:
        cmd = "exec %s \"$@\"" % ctx.attr.tool
        for p in pkgs:
            ep = p.entrypoints.get(ctx.attr.tool)
            if ep != None:  # an entrypoint declared in package metadata: command plus fixed arguments, resolved on the composed PATH
                cmd = "exec " + " ".join([_sq(ep.get("command", ctx.attr.tool))] + [_sq(a) for a in ep.get("args", [])]) + ' "$@"'
    script = """#!/bin/sh
# generated launcher for tool set {name}
if [ -d "$0.runfiles" ]; then RF=$0.runfiles
elif [ -n "${{RUNFILES_DIR:-}}" ] && [ -d "$RUNFILES_DIR" ]; then RF=$RUNFILES_DIR
else echo "launcher: no runfiles directory" >&2; exit 127; fi
case $RF in /*) ;; *) RF=$(pwd)/$RF ;; esac
{env}
{cmd}
""".format(name = ctx.label.name, env = "\n".join(lines), cmd = cmd)
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, script, is_executable = True)
    rf = ctx.runfiles().merge_all([p[DefaultInfo].default_runfiles for p in ctx.attr.packages])
    return [DefaultInfo(executable = exe, runfiles = rf)]

_launcher = rule(_launcher_impl, executable = True, attrs = {
    "packages": attr.label_list(providers = [PkgInfo]),
    "tool": attr.string(),
})

def ocx_toolset(name, packages, tools = []):
    """`<name>_<tool>` runs one bare command under the set's environment; `<name>_run` runs argv."""
    for t in tools:
        _launcher(name = name + "_" + t, packages = packages, tool = t)
    _launcher(name = name + "_run", packages = packages)
```

### BUILD.bazel (sets and consumers)

```python
load(":rules.bzl", "ocx_toolset")


ocx_toolset(name = "s_node", packages = ["@"+"node"+"_linux_amd64//:pkg"], tools = ["node", "npm"])
ocx_toolset(name = "s_py", packages = ["@"+"python"+"_linux_amd64//:pkg"], tools = ["python3"])
ocx_toolset(name = "s_pipx", packages = ["@"+"pipx"+"_linux_amd64//:pkg", "@"+"pbs313"+"_linux_amd64//:pkg"], tools = ["pipx", "python"])
ocx_toolset(name = "s_shfmt", packages = ["@"+"shfmt"+"_linux_amd64//:pkg"], tools = ["shfmt"])
ocx_toolset(name = "s_all3", packages = ["@"+"node"+"_linux_amd64//:pkg", "@"+"python"+"_linux_amd64//:pkg", "@"+"shfmt"+"_linux_amd64//:pkg"])
ocx_toolset(name = "s_b", packages = ["@"+"node"+"_linux_amd64//:pkg", "@"+"python"+"_linux_amd64//:pkg", "@"+"pipx"+"_linux_amd64//:pkg", "@"+"pbs313"+"_linux_amd64//:pkg", "@"+"shfmt"+"_linux_amd64//:pkg"], tools = ["node"])
ocx_toolset(name = "s_c", packages = ["@node%d_linux_amd64//:pkg" % i for i in range(1, 7)], tools = ["node"])
ocx_toolset(name = "s_syn", packages = ["@syn_local//:pkg"])

# Consumers: plain genrules, the launcher is a `tool`.
genrule(name = "t1", outs = ["t1.txt"], tools = [":s_node_npm"], cmd = "$(execpath :s_node_npm) --version > $@")
genrule(name = "t2a", outs = ["t2a.txt"], tools = [":s_py_python3"], cmd = "$(execpath :s_py_python3) -c 'import ssl, sys; print(sys.version_info[0:2])' > $@")
genrule(name = "t2b", outs = ["t2b.txt"], tools = [":s_pipx_pipx"], cmd = "$(execpath :s_pipx_pipx) --version > $@")
genrule(name = "t3", outs = ["t3.txt"], tools = [":s_shfmt_shfmt"], cmd = "$(execpath :s_shfmt_shfmt) --version > $@")
genrule(
    name = "t4",
    outs = ["t4.txt"],
    tools = [":s_all3_run"],
    cmd = "$(execpath :s_all3_run) sh -c 'node --version; python3 -c \"import sys; print(sys.version_info[0:2])\"; shfmt --version; command -v node python3 shfmt' > $@",
)
genrule(name = "t6", outs = ["t6.txt"], tools = [":s_node_run"], cmd = "$(execpath :s_node_run) node -e \"var s=require('net').connect({host:'1.1.1.1',port:443,timeout:5000});s.on('connect',function(){console.log('CONNECTED');process.exit(0)});s.on('timeout',function(){console.log('TIMEOUT');process.exit(0)});s.on('error',function(e){console.log('ERROR '+e.code);process.exit(0)})\" > $@")
genrule(
    name = "t7",
    outs = ["t7.txt"],
    tools = [":s_syn_run"],
    cmd = """$(execpath :s_syn_run) sh -c 'cd "$$SYN_CONTENT" && {
echo "test -L share/alias: $$(test -L share/alias && echo yes || echo no)"
echo "readlink share/alias: $$(readlink share/alias)"
echo "ls share/alias/: $$(ls share/alias/ | tr "\\n" " ")"
echo "cat share/alias/a.txt: $$(cat share/alias/a.txt)"
echo "test -L bin/up: $$(test -L bin/up && echo yes || echo no) -> $$(readlink bin/up); cat: $$(cat bin/up)"
echo "test -L bin/f2: $$(test -L bin/f2 && echo yes || echo no) -> $$(readlink bin/f2); cat: $$(cat bin/f2)"
echo "find -type l: $$(find . -type l | sort | tr "\\n" " ")"
echo "find -type f: $$(find . -type f | sort | tr "\\n" " ")"
}' > $@""",
)

# T8: 24 distinct consumers per batch; the salt makes every action key distinct.
[
    [
        genrule(
            name = "c_%s_%s_%d" % (s, b, i),
            outs = ["c_%s_%s_%d.txt" % (s, b, i)],
            tools = [":s_%s_node" % s],
            cmd = "$(execpath :s_%s_node) -e 'process.exit(0)' && echo %s-%d > $@" % (s, b, i),
        )
        for i in range(24)
    ]
    for s in ["node", "b", "c"]
    for b in ["r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8"]
]

[
    filegroup(name = "batch_%s_%s" % (s, b), srcs = [":c_%s_%s_%d" % (s, b, i) for i in range(24)])
    for s in ["node", "b", "c"]
    for b in ["r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8"]
]

# one consumer per set, to time the first build of the launcher in the exec configuration
[
    genrule(name = "probe_" + s, outs = ["probe_%s.txt" % s], tools = [":s_%s_node" % s], cmd = "$(execpath :s_%s_node) -e 'process.exit(0)' && echo probe > $@" % s)
    for s in ["node", "b", "c"]
]
```

## Set-level composition by ocx in a scratch home (run by the orchestrator, outside Bazel)

A scratch project declared node, shfmt and pipx under `[tools]`, python under `[group.extra.tools]`, one `[env]` constant and one `[group.extra.env]` constant. Every run used a scratch `OCX_HOME`.

- `ocx --format json env -p linux/amd64+libc.glibc -g default,extra` in a fresh home composed the whole set and installed the four packages and the one dependency on the way. It took 15.1 s online. With `--blob-cache-dir` it left 171 MB of compressed blobs behind.
- The same command in another fresh home with `--offline --blob-dir <that cache>` took 9.2 s and gave the same six items.
- `-p linux/amd64` fails for a project: `no 'linux/amd64' leaf for binding 'node' at the locked version`. The lock keys carry the libc feature, for example `linux/amd64+libc.glibc`.
- `--no-pull` leaves out every package that is not unpacked. So ocx composes an environment only from unpacked packages, not from metadata alone.
- The report holds four `PATH` items and the two constants from the project and the group. Every path item goes through `<project>/.ocx/toolchain/links/<group>/<tool>/` and resolves with `realpath` to `<home>/packages/<registry>/sha256/<2 hex>/<30 hex>/content/...`. The report itself does not name the package root.
- The `PATH` item of pipx is `<package root>/entrypoints`, not a directory under `content`. The files there are `sh` scripts of one line: `exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<absolute package root>' -- "${0##*/}" "$@"`. So a tool with entry points needs `ocx` at run time when it is started the way ocx installs it.
- The data to start such a tool without `ocx` is available today. The package's `metadata.json` declares each entry point as a command plus arguments (`pipx`: command `python`, arguments `-c <bootstrap>`). `ocx --format json package env --self <package>` reports the private environment the command runs in: `PATH` with the dependency's `content/bin`, `PYTHONPATH`, the package's own `bin`, and one constant. The help text of `--self` says it is meant for "building a launcher equivalent".
- Run by hand with exactly those items and no `ocx`: `pipx --version` prints `1.18.19` in 0.42 s. Through the generated launcher with `ocx` it prints the same in 0.44 s.
- The home's own `blobs/` directory holds only small blobs (140 KB after the pull); layers are kept unpacked under `layers/`. So an existing ocx home is not a source of compressed layers for `--blob-dir`. A directory filled through `--blob-cache-dir` is.

## What ocx reports from metadata alone (run by the orchestrator, outside Bazel)

- `ocx --format json package inspect --resolve --closure -p linux/amd64+libc.glibc <pipx digest>` in an empty scratch home took 2.1 s, left 84 KB in the home and unpacked nothing. Three packages in one call took 1.7 s and left 132 KB.
- Per package the report holds: the metadata (environment entries with `${installPath}` values and a visibility, dependencies with their digests, entry points with command and arguments), the layers (digest, media type, size), the resolution chain, and the closure.
- The closure holds two surfaces, `interface` and `private`. Each lists binaries, entry points and environment entries, and each entry names the package it comes from. An environment entry of a surface is `{key, kind, package}`. It carries no value; the value is in that package's own metadata.
- For pipx the private surface lists, in order: `PATH` from the python dependency, `PYTHONPATH` from pipx, `PATH` from pipx, one constant from pipx. That is the same order and content as `ocx package env --self` printed after unpacking.
- So the order and the origin of every environment entry are available without unpacking. Not available from one report: the values of a dependency's entries, and a project's composition across several tools with its `[env]` tables.
- Layers are `application/vnd.oci.image.layer.v1.tar+zstd`: 39.1 MB for node (one layer), 1.2 MB for shfmt (one), 0.8 MB for pipx (seven).

## The project form of the report: root `ocx inspect` (run by the orchestrator, outside Bazel)

Correction to the section above: a project scope exists. It is the root command `ocx inspect [-g group...] [NAME...]`, not `ocx package inspect`.

- Test project: three default bindings (node, shfmt, pipx), one group `extra` with python, a project `[env]` entry and a group `[env]` entry.
- `ocx --format json inspect -g default,extra` in an empty scratch home: 0.02 s, no registry contact. It reports each binding with its identifier and the platforms the lock pins (`candidates`), and a top-level `env` array with the project's and the group's entries in order.
- With `--resolve --closure -p linux/amd64+libc.glibc`: 1.9 s, 25794 bytes, 156 KB in the home, nothing unpacked. Each binding then carries the report of `ocx package inspect`: pinned digest, platform, metadata (environment entries with `${installPath}` values and a visibility, dependencies, entry points with command and arguments), layers, resolution chain and closure.
- Not in the report: one composed environment across the bindings, and a binary's path inside its package (a binary entry is a name and a package).
- The help text states that a warm `--closure` works under `--offline`, and that the command exits 78 without a lock and 65 when the lock no longer matches `ocx.toml`. Neither was run.
