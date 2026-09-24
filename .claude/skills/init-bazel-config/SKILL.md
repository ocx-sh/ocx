---
name: init-bazel-config
description: Use when Bazel has never been built on this machine, or when a build fails for a reason the workspace cannot explain — no such target '//:ZGV2…', a link error about libstdc++, every action missing the remote cache, the output root under /tmp. Checks the same host preconditions `task bazel:doctor` does, then asks for the one thing a check cannot supply (the dev-read cache password, the maintainer opt-in) and writes the answer to ~/.bazelrc.
user-invocable: true
disable-model-invocation: true
argument-hint: "[--check-only]"
triggers:
  - "bazel setup"
  - "first bazel build"
  - "bazel doctor"
  - "bazel cache credential"
  - "set up bazel on this machine"
---

# init-bazel-config

`.bazelrc` and `MODULE.bazel` are committed, so every checkout arrives with the
workspace half of a working build already correct. The other half lives on the
host — output root, RAM envelope, the C++ link prerequisite, the remote cache
reader credential — is committed nowhere, and is what a first build gets wrong.
No gate in this repository can see it.

**This skill is the prompting. `scripts/bazel_doctor.py` is the checks**, and
`task bazel:doctor` runs the same script with no questions. Never re-implement a
check here: two answers to "is this host ready" drift inside one release.

Idempotent. Re-run it any time; every write is a replace between markers.

## Step 0 — read the host

```
task bazel:doctor --force
```

Every check prints `PASS`, `WARN` or `FAIL` with its fix beside it, and the
failing **codes** are listed at the end. Route on the code, never on the prose.
Exit 0 means no `FAIL`; a `WARN` is a standing decision, not a defect.

`--check-only` was passed → stop here and report. Otherwise work the codes below
in order; each fix changes what the next check sees, so re-run Step 0 after each.

## Step 1 — toolchain (`toolchain-*`)

| Code | Fix |
|---|---|
| `toolchain-pin` | `ocx.toml` has no `[tools] bazel` line. Add it, then `ocx lock`. |
| `toolchain-lock` | `ocx lock`, and commit `ocx.lock`. |
| `toolchain-drift` | `.bazelversion` and the binary disagree — reconcile, then `ocx pull`. |
| `toolchain-pin-drift` | The running binary is not the `ocx.toml` pin. Same fix. |
| `toolchain-bazelisk` | bazelisk on PATH resolves *its own* version. Take it off PATH. |

Never `bazel` bare: every call in this repository is `ocx exec bazel -- <cmd>`.

## Step 2 — the host block (`hostrc-*`, `userrc-host-state`)

**Host state goes in `~/.bazelrc`, never `.bazelrc.user`.** This machine carries
four fixed worktrees plus one per agent task; `.bazelrc.user` is per-checkout, so
a credential there has to be re-created in every one of them, and the checkout
that misses it fails in a way that reads as a cache outage.

Show the block before writing it, then ask:

```
python3 scripts/bazel_doctor.py --print-host-block
```

`AskUserQuestion` — "Write this block to ~/.bazelrc?" / Write it · Show me the
file first · Skip. On *Write it*:

```
python3 scripts/bazel_doctor.py --write-host-block
```

It replaces only what is between its two marker comments. Anything a human put
in that file outside them survives, and a second run is a no-op.

`userrc-host-state` means `.bazelrc.user` carries host state: move those lines
into `~/.bazelrc` yourself and say which ones you moved.

## Step 3 — the C++ link prerequisite (`cxx-missing`)

`gcc -print-file-name=libstdc++.so` must print a path that **exists**. gcc echoes
the bare name back when it cannot find the file, so a non-empty answer proves
nothing. Without it, rules_rust's `process_wrapper` fails to link.

The doctor prints the distro command. On Fedora it is `sudo dnf install gcc-c++`
— **not** `libstdc++-devel`, which does not ship `libstdc++.so` (measured, F43,
and it is the obvious wrong answer). Debian and Ubuntu: `sudo apt install g++`.

No root? Offer the symlink farm instead — it is green as `cxx-farm` (WARN), not
a workaround the doctor pretends is a toolchain:

```sh
mkdir -p ~/.cache/ocx/libdir
ln -sf "$(gcc -print-file-name=libstdc++.so.6)" ~/.cache/ocx/libdir/libstdc++.so
```

then two lines into `~/.bazelrc`, `build --linkopt=-L$HOME/.cache/ocx/libdir` and
the same with `--host_linkopt`. Say that they retire once gcc-c++ lands.

## Step 4 — the cache reader credential (`cache-credential-*`)

Reads only. **Refuse to place a writer credential on a developer machine** — the
write lane is CI's, and the org secret exists so no laptop needs one. If asked,
say no and point at Step 6.

`credential-unquoted` first, before anything else: the rc tokenizer splits on
whitespace, so `--remote_header=authorization=Basic <b64>` makes Bazel read the
base64 as a build target (`no such target '//:ZGV2…'`) and *every* command in the
workspace dies. The doctor names the line. `--set-reader-credential` below
rewrites it; a hand-fix is wrapping the whole value in double quotes.

`cache-credential-absent` or `cache-credential-401` → ask for the password.
`AskUserQuestion`, free text: "Password for the `dev-read` realm on
bazel-cache.ocx.sh (ask the maintainer if you do not have it)". Then, **stdin,
never argv** — argv is world-readable through `ps -eo args`:

```sh
python3 scripts/bazel_doctor.py --set-reader-credential <<'OCXPW'
<the password>
OCXPW
```

Then re-run Step 0 for the verdict. Say nothing about the value afterwards — not
its length, not a prefix, not "it looks right". The tool prints only that a
quoted line was written.

Offer the alternative in the same breath: a password typed into an
`AskUserQuestion` is in the transcript by construction, so anyone who would
rather not put it there runs that heredoc themselves and you re-run Step 0.

`cache-credential-blocked` (HTTP 403) is **not** an auth failure — the edge
refused before the realm saw the request. Do not touch the credential; retry
from another network.

## Step 5 — caches (`disk-cache-*`, `output-root-*`)

Nothing under `/tmp`: it is a tmpfs on this host and an hourly reaper walks it,
so a cache there is RAM that vanishes mid-build. `--write-host-block` fixes the
output root; `--disk_cache` comes from the committed `.bazelrc`, so a red there
means you are not running from the repository root.

`disk-cache-gc-parked` (WARN) is a decision, not a defect: `.bazelrc` explains
why the GC flags are off on this pin, and the doctor turns it into a `FAIL` by
itself once the pin crosses 9.3.0. Do not "fix" it.

## Step 6 — maintainer path (`maintainer-secrets`) — opt-in only

Only when the doctor reports org admin on `ocx-sh`. Never automatic: rotating an
org secret is a decision. `AskUserQuestion` — "Set the org cache secrets now?" /
Set both · Read only · Skip. Per secret, free text for the password, then:

```sh
printf '%s' "Basic $(printf '%s' 'dev-read:<pw>' | base64 -w0)" |
  gh secret set BAZEL_CACHE_READ_AUTH --org ocx-sh --visibility selected --repos ocx,rules_ocx
```

and `BAZEL_CACHE_WRITE_AUTH` the same way with the writer identity. Never echo
either value and never write either into a local file.

## Step 7 — one warm-cache smoke

```
ocx exec bazel -- bazel test //crates/ocx_exit:all
```

Report the summary's cache line — `(cached) PASSED`, or the `remote cache hit`
count. A run that builds everything from scratch after Step 4 went green means
the credential reaches the realm but the actions do not key to it; say so rather
than calling the setup done.

## Stop condition

`task bazel:doctor --force` exits 0, and Step 7's summary is reported. Report
each check you changed and each `WARN` you deliberately left. Never edit a check
to make it pass.

## What agents get wrong

- **Probing the cache with a bare `urllib`.** `Python-urllib/*` is 403'd at the
  edge for `*.ocx.sh`, and "any non-401 is fine" reads that as a pass. The
  doctor sends a real User-Agent and treats 403 as its own finding.
- **Grepping the rc for a broken credential.** `~/.bazelrc` keeps a
  commented-out unquoted example above the live line; the doctor tokenizes with
  `shlex`, which is what Bazel does, so a comment is a comment.
- **`fc-list | grep -qi mono`.** It is at `/usr/sbin/fc-list` here, off a plain
  PATH, so that answers "no font" for the wrong reason.
- **Re-implementing a check in this file.** Add it to `scripts/bazel_doctor.py`
  with a red/green pair in its tests (`scripts/tests/test_bazel_doctor.py`), which `task scripts:verify` runs.
