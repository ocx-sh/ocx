# Research: how a native launcher finds its sidecar and root inside Bazel (spike 6)

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: hex-architect high, rules_ocx under remote execution · Domain: launcher

Run by the orchestrator on this host after two delegated workers went silent. Bazel 9.2.0 with `--ignore_all_rc_files` and a scratch output user root. The launcher is a throwaway C program built with `cc -O2` (glibc, dynamic). `linux-sandbox` is not available on this host, so the sandboxed legs use `processwrapper-sandbox`.

## Question

The launcher is one binary, copied under each tool's name, with a sidecar `<name>.launch` beside it. Where must the launcher look so that it finds the sidecar and the vendored root, with the package's links present? It has to work:

- inside a Bazel action (local, sandboxed, `bazel run`, `bazel test`);
- in a plain vendored directory that has been moved;
- when one tool starts another by bare name through `PATH`.

## What was run

The sidecar has three lines: `self`, the launcher's path relative to the root; `target`, relative to the root; and a `PATH` prefix relative to the root. The candidates are switched by `LOOKUP`:

- **A**: the unresolved started path. `argv[0]`, made absolute against the cwd; a bare name is resolved by a `PATH` search. Read `<P>.launch`; root = `P` minus `self` when `P` ends with `self`.
- **R**: read the sidecar `<P>.launch` beside `P`, then find the root as `<P>.runfiles/<repo>/` (or `$RUNFILES_DIR/<repo>/`), the one entry holding `<self>.launch`.
- **D**: canonical `/proc/self/exe`, then the same as A.

The core of the launcher, trimmed:

```c
if (*m == 'A') { snprintf(f, sizeof f, "%s.launch", p); if (rd(f) && strip_self(p, root)) how = 'A'; }
if (*m == 'R') { snprintf(f, sizeof f, "%s.launch", p); if (!rd(f)) continue;
  snprintf(rf, sizeof rf, "%s.runfiles", p); DIR *dd = opendir(rf);           /* or $RUNFILES_DIR */
  for (e = readdir(dd) ...) { snprintf(c, sizeof c, "%s/%s/%s.launch", rf, e->d_name, self_rel);
    if (ex(c)) { snprintf(root, sizeof root, "%s/%s", rf, e->d_name); break; } } }
if (*m == 'D') { realpath("/proc/self/exe", c); snprintf(f, sizeof f, "%s.launch", c); if (rd(f) && strip_self(c, root)) how = 'D'; }
/* then: PATH = root/<path rel>:$PATH; execv(root/<target rel>, argv) */
```

The repository rule writes `manifest.json`, `lib/real-tool`, `lib/other-real`, `launchers/default/{tool,other}` (copies of the launcher) and their `.launch` sidecars. The link `bin/tool -> ../lib/real-tool` is not on disk. The repository's own BUILD file declares the tool target:

```starlark
def _impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = exe, target_file = ctx.file.launcher, is_executable = True)
    sc = ctx.actions.declare_file(ctx.label.name + ".launch")      # ocx's sidecar placed beside the executable, not written
    ctx.actions.symlink(output = sc, target_file = ctx.file.sidecar)
    links = []
    for rel, tgt in ctx.attr.links.items():                         # declared in the repository's own package
        l = ctx.actions.declare_symlink(rel); ctx.actions.symlink(output = l, target_path = tgt); links.append(l)
    return [DefaultInfo(executable = exe, files = depset([exe, sc]),
                        runfiles = ctx.runfiles(files = ctx.files.data + [sc] + links))]
```

A first form put the links into `root_symlinks` under `<repo>/…` from a rule in the main repository. It crashed Bazel 9.2.0 when the tool was in the exec configuration: `NullPointerException: missing metadata for File:[…]tool_links/bin/tool` in `SourceManifestAction`. The links must be declared by a rule in the package's own repository. That is also how the 2026-11-25 prototype declared them.

## Results

The target `bin/tool` is reachable only through the declared link.

| Leg | A | R | D | tool → `other` by bare name |
|---|---|---|---|---|
| genrule `tools=`, processwrapper sandbox | fail 78 (no root) | pass | fail 74 (exec of `bin/tool` under the canonical repository path: link absent) | pass (outer R, inner `other` by A) |
| genrule, `--spawn_strategy=local` | fail 78 | pass | fail 74 | pass |
| genrule, sandbox, `--nobuild_runfile_links` | fail 78 | pass | fail 74 | pass |
| genrule, local, `--nobuild_runfile_links` | fail 78 | pass | fail 74 | pass |
| `bazel run @pkg//:tool` | fail | pass | fail | not run |
| `bazel test` (test rule, sandbox) | not run | pass | fail 74 | not run |
| vendored directory copied out and moved, link on disk | pass | fail 78 (no `.runfiles`) | wrong root: resolved into Bazel's repo contents cache | pass (bare `tool` through `PATH`, cwd `/`) |

- `--nobuild_runfile_links` did not change what a genrule tool sees: Bazel still staged the tool's runfiles tree for the action.
- In the moved-directory leg, D resolved `/proc/self/exe` into `<output_user_root>/cache/repos/v1/contents/…`. The copied files were links into Bazel's repo contents cache, so canonicalisation picked a root that is not the vendored directory.

## Not run and why

- Windows (`GetModuleFileNameW`, manifest-only runfiles by default): no Windows host. Named CI check: rules_ocx `verify-deep` on `windows-latest` builds a genrule over a vendored tool with default runfiles settings, and the launcher must find its root through `<exe>.runfiles_manifest` or `RUNFILES_MANIFEST_FILE`. This spike did not implement a manifest lookup.
- macOS: no host. Named CI check: the same genrule on `macos-latest`.
- Remote executor: not in this spike. The runfiles tree reaches the executor as regular files plus declared links. That was shown for an `sh` launcher on 2026-11-25 (`research_bazel-rbe-rules-ocx-prototype.md`). The rig repeats this for the native launcher.
- A static musl build, and an image without a shell.

## Answer

1. **Never canonicalise the launcher's own path.** D fails in every Bazel leg, because the declared links exist only in the runfiles tree. It also picks a wrong root in a copied directory. `/proc/self/exe`, and `std::env::current_exe` on Linux, must not be a candidate.
2. **Started path, then runfiles.** First the unresolved started path: when it ends with the sidecar's own `self` path, root = started path minus `self`. This covers a vendored directory at any location and a bare-name start through a `PATH` that holds a launcher directory, inside or outside Bazel. Otherwise, `<started path>.runfiles/<entry>/` (or `$RUNFILES_DIR/<entry>/`) is the root when it holds `<self>.launch`. This covers a Bazel tool target, whose executable sits outside the runfiles tree.
3. **The rule places ocx's sidecar beside the executable.** It symlinks the file and writes no data. The launcher then reads `self` before it knows the root. A rule in the set's own repository declares the links.
4. The manifest-only runfiles lookup is still owed for Windows, as the CI check above.

## How shown red and green

- Red control: D, and A for a tool target, exit 78 or 74 in every Bazel leg above. Green: R in every Bazel leg, and A in the moved vendored directory.
- Bare-name chain: green as shown. Red would be a `PATH` prefix that names the repository path instead of the runfiles path; that was not run.
