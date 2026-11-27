# Research: tool files as action outputs with declared links, two forms (local rig)

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, run (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN. Bazel 9.2.0 against NativeLink v1.9.0 in Docker, 48 rows of (form, distribution, consumer, situation), with a short repeat on Buildbarn (hardlinking worker). The forms are named G1 and G2 and are compared with F1 and F6 of `research_bazel-rbe-input-forms-matrix.md`.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. One machine, one worker per backend. The Buildbarn failure of G2 was recorded and not investigated. Empty directories were not exercised.

G1: the repository holds the files with every link deleted. A rule copies each file to a declared output and declares one unresolved symlink per link beside them. G2: the repository holds one archive and the lists of files and links. One action unpacks the archive and declares every file and every link as its own output.


Single-machine Docker backends, not a production cluster. Every number below comes from a command run in `ws7`; all times are wall clock on this one 12-core host, with Bazel 9.2.0 and a warm server unless stated.

## Rule sources

### repos.bzl (repository rule, kinds g1 and g2 are new)
```
def _run(ctx, args):
    r = ctx.execute(args, timeout = 600)
    if r.return_code != 0:
        fail("{} failed: {}".format(args, r.stderr))
    return r.stdout

def _links(ctx, root):
    out = _run(ctx, ["find", root, "-type", "l", "-printf", "%P\\t%l\\n"])
    links = {}
    for line in out.splitlines():
        p, t = line.split("\t", 1)
        links[p] = t
    return links

def _impl(ctx):
    src = ctx.attr.src
    kind = ctx.attr.kind
    vis = 'package(default_visibility = ["//visibility:public"])\n'
    if kind == "files":
        _run(ctx, ["mkdir", "content"])
        _run(ctx, ["cp", "-a", src + "/.", "content"])
        ctx.file("BUILD.bazel", vis + """
filegroup(name = "all_files", srcs = glob(["content/**"]))
""")
    elif kind in ("f6", "g1"):
        _run(ctx, ["mkdir", "content"])
        _run(ctx, ["cp", "-a", src + "/.", "content"])
        links = _links(ctx, "content")
        _run(ctx, ["find", "content", "-type", "l", "-delete"])
        if kind == "f6":
            ctx.file("BUILD.bazel", vis + """
load("@@//:f6.bzl", "dist_exe")
dist_exe(name = "exe", files = glob(["content/**"]), links = LINKS)
""".replace("LINKS", json.encode(links)))
        else:
            ctx.file("BUILD.bazel", vis + """
load("@@//:g.bzl", "g1")
g1(name = "d", files = glob(["content/**"]), links = LINKS)
""".replace("LINKS", json.encode(links)))
    elif kind == "g2":
        links = _links(ctx, src)
        files = _run(ctx, ["find", src, "-type", "f", "-printf", "%P\\n"]).splitlines()
        # sorted, no gzip timestamp: the same bytes in every output base
        _run(ctx, ["sh", "-c", 'tar --sort=name -cf - -C "$1" . | gzip -n > dist.tgz', "sh", src])
        ctx.file("lists.bzl", "FILES = {}\nLINKS = {}\n".format(json.encode(sorted(files)), json.encode(links)))
        ctx.file("BUILD.bazel", vis + """
load("@@//:g.bzl", "g2")
load(":lists.bzl", "FILES", "LINKS")
exports_files(["dist.tgz"])
g2(name = "d", archive = "dist.tgz", files = FILES, links = LINKS)
g2(name = "dl", archive = "dist.tgz", files = FILES, links = LINKS, local = True)
""")
    else:
        fail(kind)

dist = repository_rule(_impl, attrs = {"src": attr.string(), "kind": attr.string()})
```

### g.bzl (the G1 and G2 rules)
```
DistInfo = provider(fields = ["root"])

def _exe(ctx, outs):
    exe = ctx.actions.declare_file("exe.sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return exe

def _result(ctx, outs, rels):
    exe = _exe(ctx, outs)
    root = outs[0].path[:-len(rels[0]) - 1] + "/dist"
    return [
        DefaultInfo(files = depset(outs), executable = exe, runfiles = ctx.runfiles(files = outs)),
        DistInfo(root = root),
    ]

def _g1_impl(ctx):
    prefix = "../" + ctx.label.repo_name + "/content/"
    outs, rels = [], []
    for f in ctx.files.files:
        sp = f.short_path
        if not sp.startswith(prefix):
            fail("unexpected path " + sp)
        rel = sp[len(prefix):]
        o = ctx.actions.declare_file("dist/" + rel)
        ctx.actions.symlink(output = o, target_file = f)
        outs.append(o)
        rels.append("dist/" + rel)
    for rel, target in ctx.attr.links.items():
        o = ctx.actions.declare_symlink("dist/" + rel)
        ctx.actions.symlink(output = o, target_path = target)
        outs.append(o)
        rels.append("dist/" + rel)
    return _result(ctx, outs, rels)

g1 = rule(_g1_impl, attrs = {"files": attr.label_list(allow_files = True), "links": attr.string_dict()}, executable = True)

def _g2_impl(ctx):
    outs, rels = [], []
    for rel in ctx.attr.files:
        outs.append(ctx.actions.declare_file("dist/" + rel))
        rels.append("dist/" + rel)
    for rel in ctx.attr.links:
        outs.append(ctx.actions.declare_symlink("dist/" + rel))
        rels.append("dist/" + rel)
    root = outs[0].path[:-len(rels[0]) - 1] + "/dist"
    reqs = {"no-remote-exec": "1"} if ctx.attr.local else {}
    arch = ctx.file.archive
    ctx.actions.run_shell(
        inputs = [arch],
        outputs = outs,
        command = 'tar -xzf "$1" -C "$2" # salt={}'.format(ctx.var.get("salt", "0")),
        arguments = [arch.path, root],
        execution_requirements = reqs,
        mnemonic = "Unpack",
    )
    exe = _exe(ctx, outs)
    return [
        DefaultInfo(files = depset(outs), executable = exe, runfiles = ctx.runfiles(files = outs)),
        DistInfo(root = root),
    ]

g2 = rule(_g2_impl, attrs = {
    "archive": attr.label(allow_single_file = True),
    "files": attr.string_list(),
    "links": attr.string_dict(),
    "local": attr.bool(),
}, executable = True)
```

### defs.bzl (consume rule: kinds files, f6, raw, runfiles; mode count is new)
```
load(":g.bzl", "DistInfo")

SCRIPT = r"""
OUT=$1; ROOT=$2; PROBE=$3; ARCH=$4; N=$5; MODE=$6; RUNCMD=$7
export HOME=${TMPDIR:-/tmp}
ALWAYS=
if [ -n "$ARCH" ]; then
  S=$(mktemp -d) && tar -xzf "$ARCH" -C "$S" || exit 1
  ROOT=$S; ALWAYS=1
fi
case $ROOT in /*) ;; *) ROOT=$PWD/$ROOT ;; esac
case $PWD in /worker/*) WHERE=remote ;; *) WHERE=local ;; esac
if [ "$MODE" = count ]; then
  {
    echo "host=$(hostname)"
    echo "files=$(find "$ROOT" -type f | wc -l)"
    echo "links=$(find "$ROOT" -type l | wc -l)"
    echo "files_followlinks=$(find -L "$ROOT" -type f | wc -l)"
    echo "exec_files=$(find "$ROOT" -type f -perm -u+x | wc -l)"
    echo "exec_files_followlinks=$(find -L "$ROOT" -type f -perm -u+x | wc -l)"
    echo "empty_dirs=$(find "$ROOT" -type d -empty | wc -l)"
  } > "$OUT"
  exit 0
fi
if [ "$MODE" = light ]; then
  echo "host=$(hostname) n=$N" > "$OUT"
  PATH=$ROOT/bin:/usr/bin:/bin sh -c "$RUNCMD" >/dev/null 2>&1 || { echo "light run failed" >&2; exit 1; }
  exit 0
fi
P=$ROOT/$PROBE
{
  echo "host=$(hostname)"
  echo "where=$WHERE n=$N"
  if [ $WHERE = remote ] || [ -n "$ALWAYS" ]; then echo "links_seen=$(find "$ROOT" -type l 2>/dev/null | wc -l)"; else echo "links_seen=n/a (sandbox)"; fi
  echo "probe_is_link=$([ -L "$P" ] && echo yes || echo no)"
  echo "probe_readlink=$(readlink "$P" 2>/dev/null || echo -)"
  rp=$(realpath "$P" 2>/dev/null); rr=$(realpath "$ROOT" 2>/dev/null)
  case $rp in "$rr"/*) rp=${rp#"$rr"/} ;; esac
  echo "probe_realpath=${rp:-unresolved}"
  echo "probe_size=$(stat -L -c %s "$P" 2>/dev/null || echo missing)"
  o=$(PATH=$ROOT/bin:/usr/bin:/bin sh -c "$RUNCMD" 2>&1); rc=$?
  echo "run=$(echo "$o" | tail -n 1)"
  echo "run_rc=$rc"
  if [ "$MODE" = diag ]; then printf "run_full<<\n%s\n" "$o"; fi
} > "$OUT"
"""

def _consume_impl(ctx):
    form = ctx.attr.form
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    inputs = []
    tools = []
    arch = ""
    if form == "files":
        inputs = ctx.files.src
        root = ctx.attr.src.label.workspace_root + "/content"
    elif form == "raw":
        inputs = ctx.attr.src[DefaultInfo].files
        root = ctx.attr.src[DistInfo].root
    elif form == "runfiles":
        exe = ctx.attr.src[DefaultInfo].files_to_run
        tools = [exe]
        root = exe.executable.path + ".runfiles/" + ctx.attr.src.label.repo_name + "/dist"
    elif form == "f6":
        exe = ctx.attr.src[DefaultInfo].files_to_run
        tools = [exe]
        root = exe.executable.path + ".runfiles/" + ctx.attr.src.label.repo_name + "/content"
    ctx.actions.run_shell(
        inputs = inputs,
        tools = tools,
        outputs = [out],
        command = SCRIPT + " # form=" + form,
        arguments = [out.path, root, ctx.attr.probe, arch, str(ctx.attr.n), ctx.attr.mode, ctx.attr.run],
        mnemonic = "Consume",
        progress_message = "consume " + str(ctx.label),
    )
    return [DefaultInfo(files = depset([out]))]

consume = rule(_consume_impl, attrs = {
    "form": attr.string(),
    "src": attr.label(allow_files = True, cfg = "exec"),
    "n": attr.int(),
    "probe": attr.string(),
    "run": attr.string(),
    "mode": attr.string(default = "full"),
})

NODE_RUN = "npm --version"
PY_RUN = "python3 -c \"import ssl, sys; print(sys.version_info[0:2])\""

D = {
    "node": struct(g1 = "@node_g1//:d", g2 = "@node_g2//:d", g2l = "@node_g2//:dl", probe = "bin/npm", run = NODE_RUN),
    "py": struct(g1 = "@py_g1//:d", g2 = "@py_g2//:d", g2l = "@py_g2//:dl", probe = "bin/python3", run = PY_RUN),
}
```

## Rows (NativeLink, instance main)

Each row is one build with a fresh consumer integer and a fresh salt (the salt sits in the unpack command, so a G2 cold row is cold for the unpack action too). Run command: `npm --version` (node), the python import command of the existing workspace (python). Local rows: links cannot be counted inside the sandbox, so judge by the probe fields and the run. In every row the probe was a link, `npm --version` printed 11.19.1 (node) or python printed (3, 14), and the exit code was 0. Remote rows saw 3 links (node) and 1048 links (python); the probe resolved to `lib/node_modules/npm/bin/npm-cli.js` (node) and `bin/python3.14` (python), size 54 and 78816800.

G1 has no cacheable producer: the producers are 4800 in-process symlink actions, so no row can show a producer cache hit. Its summaries are `4802 processes: 4801 internal, 1 remote` (node raw) and the like, in every remote situation, and `1 processwrapper-sandbox` for local rows. G2 remote rows: R-cold ran the unpack remotely (`1 internal, 2 remote`); R-cached-default and R-second-client show the unpack as `1 remote cache hit` and the consumer as `1 remote`. G2 L-cached-default rows show `1 remote cache hit` (unpack) plus `1 processwrapper-sandbox` (consumer), after a warm build of a different consumer through the same exec-configuration action.

| dist | form | situation | host | links seen | probe is link | readlink | resolved (rel. root) | size | run | rc | Bazel summary |
|---|---|---|---|---|---|---|---|---|---|---|---|
| node | g1raw | R-cold | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4802 processes: 4801 internal, 1 remote. |
| node | g1raw | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4802 processes: 4801 internal, 1 remote. |
| node | g1raw | R-second-client | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4802 processes: 4801 internal, 1 remote. |
| node | g1raw | L-cold | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4802 processes: 4801 internal, 1 processwrapper-sandbox. |
| node | g1raw | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4802 processes: 4801 internal, 1 processwrapper-sandbox. |
| node | g1run | R-cold | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4807 processes: 4806 internal, 1 remote. |
| node | g1run | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4807 processes: 4806 internal, 1 remote. |
| node | g1run | R-second-client | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4807 processes: 4806 internal, 1 remote. |
| node | g1run | L-cold | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4807 processes: 4806 internal, 1 processwrapper-sandbox. |
| node | g1run | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 4807 processes: 4806 internal, 1 processwrapper-sandbox. |
| node | g2raw | R-cold | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 internal, 2 remote. |
| node | g2raw | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| node | g2raw | R-second-client | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| node | g2raw | L-cold | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 internal, 2 processwrapper-sandbox. |
| node | g2raw | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 processwrapper-sandbox. |
| node | g2run | R-cold | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 6 internal, 2 remote. |
| node | g2run | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| node | g2run | R-second-client | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| node | g2run | L-cold | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 6 internal, 2 processwrapper-sandbox. |
| node | g2run | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 processwrapper-sandbox. |
| node | g2Lraw | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| node | g2Lraw | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 processwrapper-sandbox. |
| node | g2Lrun | R-cached-default | nl-worker | 3 | yes | ../lib/node_modules/npm/bin/npm-cli.js | lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| node | g2Lrun | L-cached-default | ocx.sh | n/a (sandbox) | yes | ../lib/node_modules/npm/bin/npm-cli.js | <root>/lib/node_modules/npm/bin/npm-cli.js | 54 | 11.19.1 | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 processwrapper-sandbox. |
| py | g1raw | R-cold | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4830 processes: 4829 internal, 1 remote. |
| py | g1raw | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4830 processes: 4829 internal, 1 remote. |
| py | g1raw | R-second-client | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4830 processes: 4829 internal, 1 remote. |
| py | g1raw | L-cold | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 4830 processes: 4829 internal, 1 processwrapper-sandbox. |
| py | g1raw | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 4830 processes: 4829 internal, 1 processwrapper-sandbox. |
| py | g1run | R-cold | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4835 processes: 4834 internal, 1 remote. |
| py | g1run | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4835 processes: 4834 internal, 1 remote. |
| py | g1run | R-second-client | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 4835 processes: 4834 internal, 1 remote. |
| py | g1run | L-cold | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 4835 processes: 4834 internal, 1 processwrapper-sandbox. |
| py | g1run | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 4835 processes: 4834 internal, 1 processwrapper-sandbox. |
| py | g2raw | R-cold | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 internal, 2 remote. |
| py | g2raw | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| py | g2raw | R-second-client | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| py | g2raw | L-cold | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 internal, 2 processwrapper-sandbox. |
| py | g2raw | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 processwrapper-sandbox. |
| py | g2run | R-cold | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 6 internal, 2 remote. |
| py | g2run | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| py | g2run | R-second-client | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| py | g2run | L-cold | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 6 internal, 2 processwrapper-sandbox. |
| py | g2run | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 processwrapper-sandbox. |
| py | g2Lraw | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 remote. |
| py | g2Lraw | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 3 processes: 1 remote cache hit, 1 internal, 1 processwrapper-sandbox. |
| py | g2Lrun | R-cached-default | nl-worker | 1048 | yes | python3.14 | bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 remote. |
| py | g2Lrun | L-cached-default | ocx.sh | n/a (sandbox) | yes | python3.14 | <root>/bin/python3.14 | 78816800 | (3, 14) | 0 | 8 processes: 1 remote cache hit, 6 internal, 1 processwrapper-sandbox. |

## Execution log (node, R-cold, from `--execution_log_json_file`)

```
g1raw inputs= 4800 {'file_with_digest': 4797, 'symlink': 3, 'other': 0} runner= remote cacheHit= None
    node_g1/dist/bin/npm NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g1/dist/lib/node_modules/npm/bin/npm digest symlinkTarget= ''
g1run inputs= 9604 {'file_with_digest': 9597, 'symlink': 6, 'other': 1} runner= remote cacheHit= None
    node_g1/dist/bin/npm NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g1/dist/lib/node_modules/npm/bin/npm digest symlinkTarget= ''
    node_g1/exe.sh.runfiles/ NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g1/exe.sh.runfiles/ digest symlinkTarget= ''
g2raw inputs= 4800 {'file_with_digest': 4797, 'symlink': 3, 'other': 0} runner= remote cacheHit= None
    node_g2/dist/bin/npm NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g2/dist/lib/node_modules/npm/bin/npm digest symlinkTarget= ''
g2run inputs= 9604 {'file_with_digest': 9597, 'symlink': 6, 'other': 1} runner= remote cacheHit= None
    node_g2/dist/bin/npm NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g2/dist/lib/node_modules/npm/bin/npm digest symlinkTarget= ''
    node_g2/exe.sh.runfiles/ NO-digest symlinkTarget= '../lib/node_modules/npm/bin/npm-cli.js'
    node_g2/exe.sh.runfiles/ digest symlinkTarget= ''
```

Reading: for the raw consumers the action lists 4800 inputs: 4797 files with a digest and 3 symlinks that carry a target path and no digest. `bin/npm` is a symlink with target `../lib/node_modules/npm/bin/npm-cli.js`; its neighbour `lib/node_modules/npm/bin/npm` is a file with digest. The runfiles consumers list 9604 inputs (the tree twice: once as outputs, once under `exe.sh.runfiles/`), 6 symlinks, and one extra empty-file entry `_main/.runfile` with neither digest nor target.

## Extra observations (one remote node action per form, `res/counts.txt`)

Source directory on this machine: 4797 regular files, 3 links, 66 files with the executable bit, 0 empty directories.

```
== f1 rc=0 INFO: 2 processes: 1 internal, 1 remote.
host=nl-worker files=4800 links=0 files_followlinks=4800 exec_files=4800 exec_files_followlinks=4800 empty_dirs=0 
== f6 rc=0 INFO: 10 processes: 9 internal, 1 remote.
host=nl-worker files=4797 links=3 files_followlinks=4800 exec_files=4797 exec_files_followlinks=4800 empty_dirs=0 
== g1raw rc=0 INFO: 4802 processes: 4801 internal, 1 remote.
host=nl-worker files=4797 links=3 files_followlinks=4800 exec_files=4797 exec_files_followlinks=4800 empty_dirs=0 
== g1run rc=0 INFO: 7 processes: 6 internal, 1 remote.
host=nl-worker files=4797 links=3 files_followlinks=4800 exec_files=4797 exec_files_followlinks=4800 empty_dirs=0 
== g2raw rc=0 INFO: 3 processes: 1 internal, 2 remote.
host=nl-worker files=4797 links=3 files_followlinks=4800 exec_files=4797 exec_files_followlinks=4800 empty_dirs=0 
== g2run rc=0 INFO: 7 processes: 6 internal, 1 remote.
host=nl-worker files=4797 links=3 files_followlinks=4800 exec_files=4797 exec_files_followlinks=4800 empty_dirs=0 
```

Every form delivers all 4797 files with the executable bit set (source: 66), including f1. f1 shows 4800 files and 0 links, so the 3 links became regular files (as in the earlier matrix). The empty-directory count is 0 everywhere, but the source has 0 as well, so this run cannot tell whether an empty directory survives any form.

## Cost (NativeLink, single-machine Docker, not a production cluster)

24 distinct light consumers (`node -e "process.exit(0)"`), `--jobs=4`, two runs (a and b use different consumer sets), producers built beforehand in a separate warm build.

| mode | form | run | rc | wall s | Bazel summary |
|---|---|---|---|---|---|
| remote | f1 | a | 0 | 4.42 | 25 processes: 1 internal, 24 remote. |
| remote | f1 | b | 0 | 4.45 | 25 processes: 1 internal, 24 remote. |
| remote | f6 | a | 0 | 3.77 | 25 processes: 1 internal, 24 remote. |
| remote | f6 | b | 0 | 4.12 | 25 processes: 1 internal, 24 remote. |
| remote | g1raw | a | 0 | 3.97 | 25 processes: 1 internal, 24 remote. |
| remote | g1raw | b | 0 | 3.62 | 25 processes: 1 internal, 24 remote. |
| remote | g1run | a | 0 | 6.41 | 25 processes: 1 internal, 24 remote. |
| remote | g1run | b | 0 | 6.19 | 25 processes: 1 internal, 24 remote. |
| remote | g2raw | a | 0 | 3.73 | 25 processes: 1 internal, 24 remote. |
| remote | g2raw | b | 0 | 3.55 | 25 processes: 1 internal, 24 remote. |
| remote | g2run | a | 0 | 6.04 | 25 processes: 1 internal, 24 remote. |
| remote | g2run | b | 0 | 5.83 | 25 processes: 1 internal, 24 remote. |
| local | f1 | a | 0 | 0.89 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | f1 | b | 0 | 0.67 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | f6 | a | 0 | 0.84 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | f6 | b | 0 | 0.68 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g1raw | a | 0 | 1.00 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g1raw | b | 0 | 0.84 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g1run | a | 0 | 1.37 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g1run | b | 0 | 1.82 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g2raw | a | 0 | 0.82 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g2raw | b | 0 | 0.64 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g2run | a | 0 | 2.38 | 25 processes: 1 internal, 24 processwrapper-sandbox. |
| local | g2run | b | 0 | 1.03 | 25 processes: 1 internal, 24 processwrapper-sandbox. |

First build of each producer (after `bazel clean`, warm server, fresh salt, remote mode; `res/producer.tsv`):

```
@node_g1//:d	run1	rc=0	0.70s	4806 processes: 4806 internal.	Elapsed time: 0.685s, Critical Path: 0.42s
@py_g1//:d	run1	rc=0	0.72s	4834 processes: 4834 internal.	Elapsed time: 0.688s, Critical Path: 0.28s
@node_g2//:d	run1	rc=0	4.38s	7 processes: 6 internal, 1 remote.	Elapsed time: 4.366s, Critical Path: 4.19s
@py_g2//:d	run1	rc=0	4.61s	7 processes: 6 internal, 1 remote.	Elapsed time: 4.593s, Critical Path: 4.38s
@node_g1//:d	run2	rc=0	0.70s	4806 processes: 4806 internal.	Elapsed time: 0.689s, Critical Path: 0.44s
@py_g1//:d	run2	rc=0	0.66s	4834 processes: 4834 internal.	Elapsed time: 0.645s, Critical Path: 0.31s
@node_g2//:d	run2	rc=0	4.43s	7 processes: 6 internal, 1 remote.	Elapsed time: 4.392s, Critical Path: 4.18s
@py_g2//:d	run2	rc=0	4.66s	7 processes: 6 internal, 1 remote.	Elapsed time: 4.639s, Critical Path: 4.44s
```

Loading and analysis, `build --nobuild` after `bazel shutdown` (wall time includes server start; `res/load.tsv`):

```
node f1 x24	1	rc=0	3.47s	Elapsed time: 3.409s, Critical Path: 0.00s
node f1 x24	2	rc=0	3.09s	Elapsed time: 3.015s, Critical Path: 0.00s
node f6 x24	1	rc=0	3.14s	Elapsed time: 3.073s, Critical Path: 0.00s
node f6 x24	2	rc=0	3.14s	Elapsed time: 3.074s, Critical Path: 0.00s
node g1raw x24	1	rc=0	3.07s	Elapsed time: 3.004s, Critical Path: 0.00s
node g1raw x24	2	rc=0	3.05s	Elapsed time: 2.986s, Critical Path: 0.00s
node g1run x24	1	rc=0	3.18s	Elapsed time: 3.090s, Critical Path: 0.00s
node g1run x24	2	rc=0	3.06s	Elapsed time: 2.988s, Critical Path: 0.00s
node g2raw x24	1	rc=0	2.37s	Elapsed time: 2.304s, Critical Path: 0.00s
node g2raw x24	2	rc=0	2.42s	Elapsed time: 2.358s, Critical Path: 0.00s
node g2run x24	1	rc=0	2.45s	Elapsed time: 2.383s, Critical Path: 0.00s
node g2run x24	2	rc=0	2.46s	Elapsed time: 2.392s, Critical Path: 0.00s
py g1raw x1	1	rc=0	2.81s	Elapsed time: 2.742s, Critical Path: 0.00s
py g1raw x1	2	rc=0	2.84s	Elapsed time: 2.766s, Critical Path: 0.00s
py g2raw x1	1	rc=0	2.41s	Elapsed time: 2.330s, Critical Path: 0.00s
py g2raw x1	2	rc=0	2.33s	Elapsed time: 2.269s, Critical Path: 0.00s
node g1raw x1	1	rc=0	3.04s	Elapsed time: 2.973s, Critical Path: 0.00s
node g1raw x1	2	rc=0	3.11s	Elapsed time: 3.050s, Critical Path: 0.00s
node g2raw x1	1	rc=0	2.42s	Elapsed time: 2.345s, Critical Path: 0.00s
node g2raw x1	2	rc=0	2.41s	Elapsed time: 2.347s, Critical Path: 0.00s
```

## Buildbarn light confirmation (node, R-cold, instance hardlinking)

```
== g1raw rc=0 INFO: 4802 processes: 4801 internal, 1 remote.
host=c74fde5700f6 where=remote n=101 links_seen=3 probe_is_link=yes probe_readlink=../lib/node_modules/npm/bin/npm-cli.js probe_realpath=lib/node_modules/npm/bin/npm-cli.js probe_size=54 run=11.19.1 run_rc=0 
== g1run rc=0 INFO: 7 processes: 4800 action cache hit, 6 internal, 1 remote.
host=c74fde5700f6 where=remote n=101 links_seen=3 probe_is_link=yes probe_readlink=../lib/node_modules/npm/bin/npm-cli.js probe_realpath=lib/node_modules/npm/bin/npm-cli.js probe_size=54 run=11.19.1 run_rc=0 
== g2raw rc=34 INFO: 2 processes: 2 internal.
ERROR: <scratch>/rbe/ob-gbc1/external/+dist+node_g2/BUILD.bazel:6:3: Unpack external/+dist+node_g2/dist/CHANGELOG.md [for tool] failed: (Exit 34): Remote Execution Failure:
ERROR: <scratch>/rbe/ws7/BUILD.bazel:17:12 consume @@//:g2raw_node_101 failed: (Exit 34): Remote Execution Failure:
ERROR: Build did NOT complete successfully
== g2run rc=34 INFO: 4 processes: 4 internal.
ERROR: <scratch>/rbe/ob-gbc1/external/+dist+node_g2/BUILD.bazel:6:3: Unpack external/+dist+node_g2/dist/CHANGELOG.md [for tool] failed: (Exit 34): Remote Execution Failure:
ERROR: <scratch>/rbe/ob-gbc1/external/+dist+node_g2/BUILD.bazel:6:3 Creating source manifest for @@+dist+node_g2//:d [for tool] failed: (Exit 34): Remote Execution Failure:
ERROR: Build did NOT complete successfully

```
G1 raw and G1 runfiles pass with all fields identical to NativeLink. The g1run line above reused the local symlink outputs of the preceding g1raw build in the same fresh output base (`4800 action cache hit`); I reran it after `bazel clean` as n=102 and it gave `4807 processes: 4806 internal, 1 remote`, same probe fields, run 11.19.1, rc 0. G2 raw and G2 runfiles both failed (exit 34) with this exact text:

```
Remote Execution Failure:
Invalid Argument: Failed to store cached action result: Shard 0: Blob is 941839 bytes in size, while this backend is only capable of storing blobs of up to 581632 bytes in size
```
I stopped on those two forms and did not investigate the limit.

## Deviations and what was not done

- The matrix ran four times. Attempt 1 reused one salt across forms, so the G2 unpack action was a cache hit in later forms, and its L-cached warm build used the target configuration while the consumer uses the exec configuration, so the producer was not a hit. Attempt 2 and 3 reused consumer integers or salts from earlier attempts (the consumer command carries no salt), which turned cold consumers into cache hits. The reported table is attempt 4, with fresh salt and integers per attempt; earlier logs are in `res/attempt1`, `attempt2`, `attempt3`.
- R-cached-all was not run (not requested). The forced-local G2 variant has only the two cached rows, as asked; its R-cached row primes through a remote-mode build whose unpack ran locally (`1 processwrapper-sandbox`).
- G1 cannot show a "producer cache hit"; there is no spawn to cache.
- Extra observations ran for node only. Source directory has no empty directories, so the empty-directory comparison carries no information.
- Cost runs used one run per set (a, b) as "two runs"; f1 and f6 baselines were re-measured in `ws7` on the same backend, not taken from the earlier run.
- No container was touched; Buildbarn saw only the four (plus one rerun) single-action builds.
