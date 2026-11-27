# Research: hub fetch behaviour and facts size (spike 5)

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: hex-architect high, rules_ocx under remote execution · Domain: bazel module extensions

Run by the orchestrator on this host after the delegated worker went silent. Bazel 9.2.0, `--ignore_all_rc_files`, a scratch output user root, `--repo_contents_cache=` (empty, so a repository rule that runs is visible), `bazel clean --expunge` before every leg. ocx 0.6.8 with a scratch `OCX_HOME`, a scratch `HOME` and `OCX_NO_CONFIG=1`, run in a scratch copy of this repository's `ocx.toml` and `ocx.lock`.

## Question

1. Does a hub with one `toolchain()` per platform, or with a `select()` alias, fetch only the repository of the platform the build uses?
2. How large are the facts the hub needs, and what does a facts hit save?
3. How many of this repository's tools declare neither binaries nor entry points?

## What was run

A stand-in with no ocx. The extension declares four set repositories (`set_linux_amd64`, `set_linux_arm64`, `set_darwin_arm64`, `set_windows_amd64`). Each set rule appends its name to a log file outside the output base and writes an executable `tool.sh`, and it returns `repo_metadata(reproducible = True)`. A hub repository holds one `toolchain()` per platform:

```starlark
tool_toolchain(name = "impl_linux_arm64", tool = "@set_linux_arm64//:tool.sh")
toolchain(name = "tc_linux_arm64",
          exec_compatible_with = ["@platforms//os:linux", "@platforms//cpu:aarch64"],
          toolchain = ":impl_linux_arm64", toolchain_type = ":type")
resolved_tool(name = "tool")  # rule with toolchains = ["@hub//:type"]; symlinks the resolved tool, executable = True
alias(name = "tool_select", actual = select({":is_arm": "@set_linux_arm64//:tool.sh",
                                             "//conditions:default": "@set_linux_amd64//:tool.sh"}))
```

The extension reads a facts JSON file and returns it as `extension_metadata(reproducible = True, facts = {key: json})`. It logs `extension-call` only when the key is missing from `module_ctx.facts`, which stands in for the call to `ocx vendor --manifest-only`.

Facts input: `ocx --format json inspect -g default,docs,lint,release,test --resolve --closure -p <key>` for each platform key in the lock. The result was trimmed to: per platform, per binding, the name, the pinned digest, the binary names, the entry-point names and `binaries_complete`.

## Results

| Leg | Repositories fetched |
|---|---|
| `build //:use_tc` (genrule `tools = ["@hub//:tool"]`, host linux/amd64) | `set_linux_amd64` only (plus `extension-call` on the very first run) |
| `build //:use_sel` (`select` alias) | `set_linux_amd64` only |
| `build //:use_tc_arm` with `exec_compatible_with = [cpu:aarch64]` and `--extra_execution_platforms=//:arm64_exec` | `set_linux_arm64` only |
| `fetch //...` with the extra execution platform | `set_linux_arm64` only (every genrule took the extra platform first) |
| `fetch //:use_tc` | `set_linux_amd64` only |
| `vendor --vendor_dir=… //:use_tc` | `set_linux_amd64` only |
| `vendor --vendor_dir=…` with no target | all four set repositories |
| `MODULE.bazel.lock` deleted, then `build //:use_tc` (red control) | `extension-call` + `set_linux_amd64` |
| lock restored, `build //:use_tc` (green) | `set_linux_amd64`, no `extension-call` |

| Facts measurement | Value |
|---|---|
| Bindings in this repository's lock | 26 entries, 20 distinct bindings across 5 groups |
| `inspect --resolve --closure`, one platform | 66 to 71 KB, 0.7 to 1.05 s |
| Platforms for which the whole set resolves | 4 of 10 lock keys: `darwin/arm64`, `linux/amd64+libc.glibc`, `linux/arm64+libc.glibc`, `windows/amd64` |
| Untrimmed reports, 4 platforms | 278928 bytes |
| Trimmed facts JSON, 4 platforms | 13263 bytes |
| `facts` entry in `MODULE.bazel.lock` (escaped) | 14714 bytes of a 64511-byte lock |
| Bindings with neither binaries nor entry points | 0 of 20; `binaries_complete` is true for all 20 |

A set across all groups fails on the other six platform keys with exit 78, for example `no 'darwin/amd64' leaf for binding 'pnpm' at the locked version`. A plain `linux/amd64` fails the same way for a lock whose keys carry libc features.

## Not run and why

- `bazel vendor` with no target also tripped over an unrelated `rules_jvm_external` repository (exit 8). The set repositories were fetched before that error.
- Fetch logs on a remote executor were not taken. The selection happens at analysis on the client, so the executor does not matter here.
- Facts pruning after a key leaves `MODULE.bazel`, carried from the earlier facts research.

## Answers

1. Both hub shapes fetch only the selected platform's repository, as long as the hub BUILD file names the set repositories only in labels that analysis follows. Toolchain resolution analyses only the selected toolchain's implementation. `bazel fetch <target>` and `bazel vendor <target>` follow the same rule. Only `bazel vendor` with no target fetches every platform.
2. The facts for this repository's 20 tools on 4 platforms are about 13 KB of JSON, about 15 KB in the lock. With the lock present the extension makes no call. With it deleted, it makes one.
3. None. The `NotEnumerable` case does not occur in this repository. The design still has to state what happens when it does.

Design consequence: a set resolves only on the platforms where every binding in it has a leaf. The extension must take the platform list as the intersection, or from an explicit attribute. A platform outside that list must be an error that names the binding, never a silent omission.

## How each check is shown red and green

- Fetch scope: green is the one-repository log above. Red is a hub that names every set repository in an analysed attribute (for example a `filegroup` over all of them), which fetches all four. That form was not run.
- Facts hit: red is the deleted-lock leg (`extension-call` logged), green is the restored-lock leg (no call).
