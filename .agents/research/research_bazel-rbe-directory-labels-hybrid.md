# Research: link-free directories handed over as directory labels, beside files and declared links (local rig)

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, run (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN. Bazel 9.2.0 against Buildbarn in Docker (hardlinking worker), node and python distributions. The form "h" is compared with the form "f6" of `research_bazel-rbe-input-forms-matrix.md` at three set sizes.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. One machine, one worker, and other experiments ran on the same machine at the same time, so the remote times are noisy. Empty directories were not exercised.


All numbers come from commands run in this session on one machine. The remote backend is a single-machine Docker Buildbarn (grpc://localhost:18980, instance "hardlinking"), not a production cluster, so absolute remote times say nothing about a real cluster. Bazel 9.2.0, `--ignore_all_rc_files`, output roots under the scratch directory.

## What h is

Same repository rule as f6. The rule copies the distribution, lists links, deletes them, and computes the maximal directories that contain no link anywhere below. Each such directory becomes one directory label (`content/<dir>`); every other regular file is handed over through `glob(["content/**"], exclude = [<one pattern per directory label>])`; every link is a declared unresolved symlink as in f6. All go into the runfiles of one executable.

## Counts

| distribution | directory labels | individual files | declared links | largest directory (files) | smallest directory (files) |
|---|---|---|---|---|---|
| node (4797 files, 3 links) | 3 | 4 | 3 | include (2810) | share (3) |
| python (3780 files, 1048 links) | 8 | 1844 | 1048 | lib/python3.14 (1455) | lib/thread3.0.6 and share/terminfo/M (3 each) |

Node directories: include 2810, lib 1980, share 3 (sum 4793, plus 4 loose files in bin). Python directories: lib/python3.14 1455, include 284, lib/tcl9.0 105, lib/tk9.0 73, lib/itcl4.3.8 8, lib/tcl9 5, lib/thread3.0.6 3, share/terminfo/M 3 (sum 1936, plus 1844 loose files).

## Rule sources (inline)

### repos.bzl
```
def _run(ctx, args):
    r = ctx.execute(args, timeout = 600)
    if r.return_code != 0:
        fail("{} failed: {}".format(args, r.stderr))
    return r.stdout

def _parent(p):
    i = p.rfind("/")
    return p[:i] if i >= 0 else ""

def _impl(ctx):
    src = ctx.attr.src
    kind = ctx.attr.kind
    vis = 'package(default_visibility = ["//visibility:public"])\n'
    _run(ctx, ["mkdir", "content"])
    _run(ctx, ["cp", "-a", src + "/.", "content"])
    out = _run(ctx, ["find", "content", "-type", "l", "-printf", "%P\\t%l\\n"])
    links = {}
    for line in out.splitlines():
        p, t = line.split("\t", 1)
        links[p] = t
    files = _run(ctx, ["find", "content", "-type", "f", "-printf", "%P\\n"]).splitlines()
    dirs = _run(ctx, ["find", "content", "-mindepth", "1", "-type", "d", "-printf", "%P\\n"]).splitlines()
    _run(ctx, ["find", "content", "-type", "l", "-delete"])
    if kind == "f6":
        ctx.file("BUILD.bazel", vis + """
load("@@//:f6.bzl", "dist_exe")
dist_exe(
    name = "exe",
    files = glob(["content/**"]),
    links = LINKS,
)
""".replace("LINKS", json.encode(links)))
    elif kind == "h":
        tainted = {}
        for l in links:
            p = _parent(l)
            for _ in range(64):
                if p == "" or p in tainted:
                    break
                tainted[p] = True
                p = _parent(p)
        clean = [d for d in dirs if d not in tainted and (_parent(d) == "" or _parent(d) in tainted)]
        cleanset = {d: True for d in clean}
        loose = [f for f in files if _parent(f) == "" or _parent(f) in tainted]
        sizes = {d: 0 for d in clean}
        for f in files:
            p = _parent(f)
            for _ in range(64):
                if p == "":
                    break
                if p in cleanset:
                    sizes[p] += 1
                    break
                p = _parent(p)
        ss = sorted(sizes.items(), key = lambda kv: kv[1])
        ctx.file("h_stats.json", json.encode({
            "dir_labels": len(clean),
            "loose_files": len(loose),
            "links": len(links),
            "files_total": len(files),
            "smallest": ss[0] if ss else None,
            "largest": ss[-1] if ss else None,
            "in_dirs": len(files) - len(loose),
        }))
        ctx.file("BUILD.bazel", vis + """
load("@@//:h.bzl", "dist_h")
dist_h(
    name = "exe",
    files = glob(["content/**"], exclude = EXCL),
    dirs = DIRS,
    links = LINKS,
)
""".replace("EXCL", json.encode(["content/%s/**" % d for d in clean])).replace("DIRS", json.encode(["content/" + d for d in clean])).replace("LINKS", json.encode(links)))
    else:
        fail(kind)

dist = repository_rule(_impl, attrs = {"src": attr.string(), "kind": attr.string()})
```

### h.bzl
```
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
        runfiles = ctx.runfiles(files = ctx.files.files + ctx.files.dirs + links),
    )]

dist_h = rule(_impl, attrs = {
    "files": attr.label_list(allow_files = True),
    "dirs": attr.label_list(allow_files = True),
    "links": attr.string_dict(),
}, executable = True)
```

### f6.bzl (unchanged from the existing workspace)
```
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

### defs.bzl (consumer, set executable; the consumer script is the existing one plus three count fields)
```
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
if [ "$MODE" = light ]; then
  echo "host=$(hostname) n=$N" > "$OUT"
  PATH=$ROOT/bin:/usr/bin:/bin sh -c "$RUNCMD" >/dev/null 2>&1 || { echo "light run failed" >&2; exit 1; }
  exit 0
fi
P=$ROOT/$PROBE
{
  echo "host=$(hostname)"
  echo "where=$WHERE n=$N"
  if [ $WHERE = remote ] || [ -n "$ALWAYS" ]; then echo "links_seen=$(find "$ROOT" -type l 2>/dev/null | wc -l)"; echo "files_seen=$(find "$ROOT" -type f 2>/dev/null | wc -l)"; echo "exec_seen=$(find "$ROOT" -type f -perm -u+x 2>/dev/null | wc -l)"; echo "emptydirs_seen=$(find "$ROOT" -type d -empty 2>/dev/null | wc -l)"; else echo "links_seen=n/a (sandbox)"; fi
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
    if False:
        pass
    else:
        exe = ctx.attr.src[DefaultInfo].files_to_run
        tools = [exe]
        repo = ctx.attr.repo.label.repo_name if ctx.attr.repo else ctx.attr.src.label.repo_name
        root = exe.executable.path + ".runfiles/" + repo + "/content"
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
    "repo": attr.label(),
})


def _set_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(
        executable = exe,
        runfiles = ctx.runfiles().merge_all([d[DefaultInfo].default_runfiles for d in ctx.attr.deps]),
    )]

set_exe = rule(_set_impl, attrs = {"deps": attr.label_list()}, executable = True)

def repos(f, s):
    if s == "a":
        return ["@node_%s//:exe" % f]
    if s == "b":
        return ["@node_%s//:exe" % f, "@py_%s//:exe" % f]
    return ["@node_%s//:exe" % f] + ["@node_%s_%d//:exe" % (f, i) for i in range(2, 7)]

```

Drivers: `/tmp/.../rbe/b8` (flags as in the existing `b`, plus `--remote_download_outputs=toplevel` so the consumer's text file is available, and `--symlink_prefix=/`), `m8.sh` (rows), `cost8.sh` (cost), all next to this directory. Raw rows: `results/rows.tsv`, `results/cost.tsv`, `results/cost_a.tsv`, logs under `results/`.

## Rows

Fields: host, links seen, files seen, executable-bit files seen, empty dirs seen, probe is a link, readlink, resolved path (relative), probe size, run result, run exit code; then Bazel's process summary. The three counts files/exec/empty were added to the consumer for the source comparison.

| dist | form | row | seen by the action | Bazel summary | elapsed s |
|---|---|---|---|---|---|
| node | h | R-cold | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 1.647 |
| node | h | R-cached | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 0.980 |
| node | h | R-second-client | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 1.539 |
| node | h | L-cold | ocx.sh ; n/a (sandbox) ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; $W/ob-h3/external/+dist+node_h/content/lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 processwrapper-sandbox. | 0.635 |
| node | f6 | R-cold | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 1.671 |
| node | f6 | R-cached | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 1.121 |
| node | f6 | R-second-client | c74fde5700f6 ; 3 ; 4797 ; 4797 ; 0 ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 remote. | 1.617 |
| node | f6 | L-cold | ocx.sh ; n/a (sandbox) ; yes ; ../lib/node_modules/npm/bin/npm-cli.js ; $W/ob-h3/external/+dist+node_f6/content/lib/node_modules/npm/bin/npm-cli.js ; 54 ; 11.19.1 ; 0 | 10 processes: 9 internal, 1 processwrapper-sandbox. | 0.834 |
| py | f6 | R-cold | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 6.688 |
| py | f6 | R-cached | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 0.923 |
| py | f6 | R-second-client | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 1.961 |
| py | f6 | L-cold | ocx.sh ; n/a (sandbox) ; yes ; python3.14 ; $W/ob-h3/external/+dist+py_f6/content/bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 processwrapper-sandbox. | 0.842 |
| py | h | R-cold | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 0.923 |
| py | h | R-cached | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 0.881 |
| py | h | R-second-client | c74fde5700f6 ; 1048 ; 3780 ; 3780 ; 0 ; yes ; python3.14 ; bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 remote. | 1.611 |
| py | h | L-cold | ocx.sh ; n/a (sandbox) ; yes ; python3.14 ; $W/ob-h3/external/+dist+py_h/content/bin/python3.14 ; 78816800 ; (3, 14) ; 0 | 1055 processes: 1054 internal, 1 processwrapper-sandbox. | 0.533 |

Reading the table. The remote rows (R-cold, R-cached, R-second-client) are identical between f6 and h for both distributions in every recorded field except Bazel's elapsed time: node sees 3 links, 4797 files, probe `bin/npm` is a link to `../lib/node_modules/npm/bin/npm-cli.js`, npm reports 11.19.1; python sees 1048 links, 3780 files, `bin/python3` is a link to `python3.14`, size 78816800, run result (3, 14). The local sandbox rows (L-cold) also agree between the forms: links seen is not measurable there (every path is a sandbox symlink), the probe is a link with the same readlink, and the resolved path points into the external repository's `content` directory. In every row the exit code is 0.

Caveats on the rows. (1) All repositories were fetched before each timed row (a `--nobuild` pre-step), so repository-rule time is excluded. (2) The consumer actions are new (distinct numbers 8001 to 8005), but file blobs and the executable's inputs are the same bytes the earlier experiments already uploaded to the same backend instance, so "cold" means the action ran in this invocation (1 remote process in every summary), not that blobs were absent from the backend. (3) `--remote_download_outputs=toplevel` was used for the remote rows. (4) R-cached is "clean, then a new consumer number", as in the existing matrix.

## Comparison with the source directory (remote rows)

| | source files | source links | executable-bit files | empty directories |
|---|---|---|---|---|
| node source | 4797 | 3 | 66 | 0 |
| node, f6, remote action | 4797 | 3 | 4797 | 0 |
| node, h, remote action | 4797 | 3 | 4797 | 0 |
| python source | 3780 | 1048 | 21 | 0 |
| python, f6, remote action | 3780 | 1048 | 3780 | 0 |
| python, h, remote action | 3780 | 1048 | 3780 | 0 |

File and link counts match in both forms. The executable-bit count differs from the source in both forms (every file has the bit in the remote input tree), identically for f6 and h. The source has no empty directories, so empty-directory loss was not exercised.

## Execution log (remote node row, `--execution_log_json_file`)

- f6: the Consume spawn lists 4804 inputs; h: 4804 inputs. The two sets of paths are identical after renaming the repository, and all digests agree except `_repo_mapping` (it contains the repository name).
- A directory label does not appear as an entry. No input ends in `content/lib`, `content/include` or `content/share`; the directories are expanded to their files, all under `exe.sh.runfiles/+dist+node_h/content/...`. f6 lists the same 4804 entries.
- `bin/npm` is listed as one entry with `symlinkTargetPath` `../lib/node_modules/npm/bin/npm-cli.js`, no digest, `isTool` true, in both forms.

## Change detection inside a directory label

Edit target: `lib/node_modules/npm/index.js`, which sits inside the directory label `lib` for h and in the glob for f6. Local sandboxed builds of one node consumer per form (fresh output base), process summaries:

| step | summary |
|---|---|
| first build | 19 processes: 17 internal, 2 sandbox |
| rebuild, nothing changed | 1 process: 1 internal |
| one byte appended in place in the fetched repository directory, no refetch | 1 process: 18 action cache hit, 1 internal (consumers not re-run, for either form) |
| one byte appended in the private source copy, no refetch | 1 process: 1 internal (nothing happens, for either form) |
| `bazel fetch --force` of both repositories, then build | 5 processes: 14 action cache hit, 3 internal, 2 sandbox (both consumers re-ran) |
| rebuild again | 1 process: 1 internal |
| refetch after restoring the source (change back), then build | 5 processes: 2 sandbox (both re-ran) |
| refetch with nothing changed, then build | 1 process: 18 action cache hit (no re-run) |

Finding: after a repository refetch, a changed file inside a directory label re-runs the consumer exactly as a changed globbed file does, and an unchanged refetch does not re-run it, so the directory label is tracked by content, at least when the repository is refetched. Without a refetch, an in-place edit of the fetched files was not noticed by either form in this test (the in-place test is a single try, not an exploration of Bazel's external-file check flags).

## Cost (single-machine Docker backend, not a production cluster)

Wall-clock seconds, two runs each. "24 consumers": 24 distinct consumers, `--jobs=4`, each runs `node -e "process.exit(0)"` through the set executable's runfiles, everything else built and uploaded first. "First build of the executable": after `bazel clean`, local, no remote. Local runs use `--spawn_strategy=sandboxed,local`.

Size a: node alone. Size b: node and python in one executable. Size c: node under six repository names (28,800 files; 28,809 links in f6's runfiles tree).

| size | form | remote 24 | local 24 | exe first build | runfiles links / regular files | 20 executables: wall, links in their runfiles | nobuild analysis (s) |
|---|---|---|---|---|---|---|---|
| a | f6 | 2.19, 2.56 | 1.35, 0.98 | 0.78, 0.99 | 4804 / 0 | 5.14, 96100 | 3.32, 7.07 |
| a | h | 2.13, 2.03 | 0.44, 0.34 | 0.37, 0.35 | 14 / 0 | 0.40, 300 | 2.69, 2.57 |
| b | f6 | 3.79, 3.25 | 1.43, 1.54 | 1.39, 1.21 | 9633 / 0 | 7.65, 192680 | 4.37, 3.09 |
| b | h | 4.06, 3.37 | 1.35, 0.70 | 0.85, 0.69 | 2915 / 0 | 1.89, 58320 | 3.06, 3.88 |
| c | f6 | 15.06, 13.93 | 7.45, 9.85 | 3.02, 2.67 | 28809 / 0 | 33.06, 576200 | 4.30, 4.02 |
| c | h | 15.58, 12.32 | 0.53, 0.50 | 0.88, 0.74 | 69 / 0 | 0.81, 1400 | 3.53, 3.16 |

Notes. The runfiles tree on disk is all symbolic links in both forms (zero regular files). In h a directory label becomes one symbolic link to the source directory, which is why links drop from 4804 to 14 for node. Total links under `bazel-bin` after the 20-executable build: a f6 96103, a h 303, b f6 193731, b h 59371, c f6 576218, c h 1418. "Analysis" is `build --nobuild` of the 24 consumers after `bazel shutdown`; it includes loading the one shared BUILD file with every target of this experiment (all sizes), and the run-to-run spread (a f6: 3.3 vs 7.1) shows the noise. Remote 24 times are about equal between forms at every size; the remote input tree is expanded to files in both.

## Deviations and not done

- Repositories pre-fetched before each timed row, so fetch time is excluded; the backend instance already held the file blobs from earlier experiments.
- `--remote_download_outputs=toplevel` and `--symlink_prefix=/` were added to the existing flags; output directories were read from the output base, not `bazel-bin`.
- Cost sizes used a set executable wrapping the repositories' executables (a wrapper of one for size a), identical in both forms.
- The in-place edit test was run once; no external-file-check flags were varied. Empty-directory handling not exercised (source has none). Executable-bit loss not investigated beyond the count. No second distribution or non-Linux platform was tried. No production cluster. Nothing was pushed or modified outside the scratch directory.
