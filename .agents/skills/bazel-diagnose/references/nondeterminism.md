# Non-hermetic and nondeterministic actions

Read this while running section D. It holds what each sandbox strategy actually
isolates, the hermetic-sandbox mount set, the environment tiers and their version
split, the mount-pair trap that silently serves stale bytes, and the glob failure
family.

Contents: [Two different questions](#two-different-questions) ·
[Which strategy ran](#which-strategy-ran) ·
[What the default sandbox does not isolate](#what-the-default-sandbox-does-not-isolate) ·
[The hermetic sandbox](#the-hermetic-sandbox) ·
[The mount-pair trap](#the-mount-pair-trap) ·
[Network](#network) ·
[Environment tiers](#environment-tiers) ·
[Globs](#globs)

Measured on Bazel 8.7.0 and 9.2.0 on a Linux host with working unprivileged user
namespaces, where strategy selection lands on `linux-sandbox`. **The mechanisms
are Bazel source behaviour; the exact mount lists and the strategy actually
selected are host-shaped.** A CI runner in a container without user namespaces,
macOS, and Windows each behave differently, and none was measured here.

## Two different questions

They are separated because they need different instruments and one does not
imply the other:

| Question | Instrument | Rule |
|---|---|---|
| Did the action's **declared** inputs, args or environment change? | Execution-log diff | BZL-HERM-15, BZL-CACHE-16 |
| Did the **output bytes** change for unchanged declared inputs? | Two-run `sha256sum` over `bazel-out` | BZL-HERM-30 |

Measured directly: two runs whose `commandArgs`, `inputs[].digest` and
`environmentVariables` were byte-identical produced three output files with
different SHA-256s (measured 8.7.0, cross-checked 9.2.0 — BZL-HERM-30). An
empty log diff is never, on its own, a determinism claim. Conversely a *stable action key* is not portability: the key is
independent of the workspace's absolute path, so an action reading ambient state
distributes that state to everyone the cache serves (BZL-HERM-31, BZL-CACHE-34).

## Which strategy ran

Per action, not per build:

```
bazel build --sandbox_debug --subcommands <target> 2>&1 | grep -i sandbox
```

Confirms the runner name and prints the sandbox's own mount and remount lines.
EMPTY means no sandboxed spawn ran at all — either everything hit a cache, or
every action is tagged out of the sandbox. Never commit this flag to any rc file
or workflow: it leaves sandbox roots on disk by design (BZL-HERM-03).

The strategy is also readable, per action, from the execution log's `runner`
field. Both are required by BZL-HERM-26 before calling a build sandboxed:

| Runner | Isolation |
|---|---|
| `linux-sandbox` | Full: mount namespace, network namespace available to revoke |
| `darwin-sandbox` | Denies writes and (conditionally) the network; **does not isolate host reads**. Whether its localhost allowance behaves like Linux's loopback is unmeasured here — an open darwin gap |
| `processwrapper-sandbox` | The cross-platform fallback; cannot revoke a network namespace |
| `local` | None. `no-sandbox` and `local` tags both route here |

Windows has **no working sandbox**: an experimental `windows-sandbox` code path
exists, is off by default, and needs a binary Bazel does not ship. A green
Windows leg is not evidence that any isolation flag held (BZL-HERM-02,
BZL-HERM-26).

## What the default sandbox does not isolate

`--sandbox_debug` on this host shows `linux-sandbox-pid1` remounting the **entire
host filesystem root read-only** (~130 mount points), then re-mounting a narrow
writable set: `/dev/shm`, `/tmp`, and the action's own execroot. A genrule under
the default sandbox reads the real `/usr/lib`, the real `/home` entries and
the real `/etc/hostname`.

So "the build is sandboxed" never means "the action cannot see the host". An
action's output must not vary by machine identity (BZL-HERM-10);
`--sandbox_fake_hostname=true` and `--sandbox_fake_username=true` are the
mitigation for that specific class. The general case is the hermetic sandbox
below.

`HOME` is **unset** in the action environment in every configuration tested, on
both majors — genuinely absent, not empty. It is not evidence for or against the
strict-environment flag either way.

## The hermetic sandbox

`--experimental_use_hermetic_linux_sandbox` (default `false`) runs on both 8.7.0
and 9.2.0. With no mount pairs it mounts only `dev/{null,random,urandom,zero,full}`,
`dev/shm`, `/tmp` and the execroot — so even the shell is missing:

```
src/main/tools/linux-sandbox-pid1.cc:566: "execvp(/bin/bash, …)": No such file or directory
exit=1
```

On a usrmerge host the minimal working set is **all four** of:

```
--sandbox_add_mount_pair=/usr --sandbox_add_mount_pair=/bin \
--sandbox_add_mount_pair=/lib --sandbox_add_mount_pair=/lib64
```

`/usr` alone is **not** sufficient — mounting it does not create the top-level
`/bin`, `/lib`, `/lib64` symlink nodes, and the failure is the same `execvp`
line. With the four mounts, `/home` and `/etc/hostname` genuinely disappear from
the action's view: that is the isolation the default sandbox does not give.

The same four mounts, and nothing more, were enough for a network-fetched
rules_python 2.3.3 hermetic toolchain, its `py_binary`/`py_test` machinery and a
`$(PYTHON3)`-driven genrule — identical on both majors. **Not measured, and
therefore not claimable:** anything touching DNS, TLS or a CA bundle from inside
the sandbox, and any toolchain depending on shared libraries beyond glibc. Treat
those as open when a diagnosis reaches them.

## The mount-pair trap

`--sandbox_add_mount_pair` is **not part of the action key at any layer** —
neither the in-memory Skyframe graph, nor the local action cache, nor the disk
cache. Measured three ways on 8.7.0:

| Experiment | Result |
|---|---|
| Same session, change the mount source only | `1 process: 1 internal` — a null build, no re-check, **stale output** |
| `clean --expunge`, same `--disk_cache`, change the mount source | `1 disk cache hit` — **stale output**, from an entry computed under the other mount |
| Fresh execution, fake mount, no cache at all | Reads the fake file correctly — the sandbox is not ignoring the mount |

So a build's isolation level is invisible to every cache layer, and
`bazel clean --expunge` is **not** sufficient once a disk or remote cache holds
an entry from the other configuration. Anyone varying host-visible filesystem
state across builds — a scratch toolchain mounted during development — must not
read a cache hit as reflecting the current mounts (BZL-HERM-26). This is the same
family as BZL-HERM-19: the tracked inputs do not describe what the action reads.

## Network

`--sandbox_default_allow_network` defaults `true` on 8.7.0, 8.8.0 and 9.2.0.
Measured behaviour with it set to `false`:

| Configuration | Network reachable | Strategy |
|---|---|---|
| Default (flag unset) | yes | `linux-sandbox` |
| `--sandbox_default_allow_network=false` | **no** | `linux-sandbox` |
| Same flag, `tags=["requires-network"]` | yes | `linux-sandbox` — the tag overrides while staying sandboxed |
| Same flag, `tags=["no-sandbox"]` | yes | `local` — no namespace to revoke, so the flag is a no-op |

Two boundaries that are load-bearing (BZL-HERM-02, BZL-FLAG-24):

- The flag **cannot reach a repository rule**. It is execution-tagged and governs
  sandboxed build and test actions; `repository_ctx.download`/`.execute` run in
  the loading phase, which has never been sandboxed. Citing it as a control over
  a fetch is wrong on its face.
- A green leg on Windows, on `processwrapper-sandbox`, or on any action routed to
  `local` is not evidence the flag held.

Diagnosing "the build reached the network": `grep -rn 'requires-network\|no-sandbox\|no-remote-exec' --include='BUILD*' .`
names the targets exempted. EMPTY means no target opted out — the reach is then
in a repository rule (section F) or in an action nobody tagged, and the sandbox
flag was never set at all.

## Environment tiers

Three tiers, stated separately whenever a hermeticity posture is written down
(BZL-HERM-29): build and host actions; repository rules and module extensions;
test actions. `--action_env` governs the first, `--repo_env` the second
(BZL-HERM-04). `--repo_env` does **not** restrict what a repository rule may
read — only `--experimental_strict_repo_env` does, and it is off by default on
both majors (BZL-HERM-05).

Measured action environment, both majors:

| Configuration | `PATH` | `LD_LIBRARY_PATH` | An arbitrary exported var |
|---|---|---|---|
| 8.7.0 default (`--incompatible_strict_action_env` false) | full client value | leaks | **does not leak** |
| 8.7.0 with the flag true | `/bin:/usr/bin:/usr/local/bin` | absent | does not leak |
| 9.2.0 default (flag true) | `/bin:/usr/bin:/usr/local/bin` | absent | does not leak |

Non-strict mode is a **named allowlist**, not a full client-environment
passthrough. 9.2.0's default output is byte-identical to 8.7.0 under the explicit
flag. Both forms of `--action_env` (`NAME=VALUE` and bare `NAME`) behave
identically across majors.

Never reach for `--noincompatible_strict_action_env` to settle environment
flakiness: on 8.x it looks like a no-op and pins the permissive behaviour across
a future bump; on 9.x it reintroduces the cache-key-invisible class outright
(BZL-CACHE-19). The fix is a named `--action_env=VAR`, and only for a variable
whose value does not differ between machines sharing the cache (BZL-HERM-06).

## Globs

Two checkers, two disjoint bug classes — measured against four fixtures:

| Fixture | `constant-glob` (buildifier) | `--incompatible_disallow_empty_glob` (default true, both majors) |
|---|---|---|
| Literal pattern, no match | fires | errors: "didn't match anything" |
| **Wildcard pattern, no match** | **silent** | errors — the silent-zero-sources case |
| Literal pattern, matches | fires (style) | passes |
| `glob([])` | silent | errors: "all files in the glob have been excluded" |

The wildcard/empty row is the one that matters and buildifier structurally cannot
see it: it is a lexical pattern-shape check with no filesystem access, while the
flag is a runtime match-count check. Neither subsumes the other.

Neither catches the shrinking-match-set failure: a `glob()` never matches into a
subpackage, so the day a `BUILD` file appears beneath a globbed tree the match set
silently shrinks with no error. Re-run `bazel query 'kind("source file", deps(//the:target))'`
before and after any change that adds, moves or removes a `BUILD` file under a
globbed directory, and diff the two outputs (BZL-HERM-19, BZL-ARCH-02). EMPTY
output from that query means the target has no source dependencies at all — which
on a target that is supposed to compile something *is* the finding.
