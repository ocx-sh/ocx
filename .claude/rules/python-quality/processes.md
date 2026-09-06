# Processes

How Python spawns, drains, kills and decodes a child process. Loads while
editing anything that calls `subprocess`, `asyncio.create_subprocess_*` or
`pexpect` — the pytest acceptance harnesses that drive the compiled CLIs,
`ocx-sdk-python`'s process seam, and any single-file tool that shells out.

Contents: [Spawning and Decoding](#spawning-and-decoding) ·
[Draining and Teardown](#draining-and-teardown) ·
[Reading the Exit Status](#reading-the-exit-status) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

**Not here: the runtime bound.** No per-call `timeout=` is required, and
adding one across the ~1,141 call sites is not an improvement — pip's
functional suite, pytest's own `Pytester.run()` and uv's `TestContext` each
ship exactly zero. The bound is `pytest-timeout` at the session level plus a
`timeout-minutes` on the CI job, and that lives in the testing rules. What
follows is everything a timeout does *not* solve: a process group that
outlives the kill, a pipe that fills before the kill can happen, an exit
status nobody reads, and bytes nobody agreed how to decode.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

Scope, where it is not everything: PY-PROC-06 binds only the two async
spawners — `ocx-sdk-python` and the bench harness. PY-PROC-02 and PY-PROC-05
bind wherever a child is killed early, which today is the pytest harnesses
and the SDK; a single-file stdlib tool that shells out once and waits has
nothing to tear down. Everything else binds all four shapes.

## Spawning and Decoding

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PROC-01 | Every text `open()`, `read_text()` and `write_text()` names its `encoding=`, and every captured subprocess names `encoding=` **and** `errors=`. Both default to `locale.getpreferredencoding(False)`, not UTF-8, so one non-ASCII byte raises `UnicodeDecodeError` on a `LANG=C` image and nowhere else — a failure indistinguishable from flake, reproducing on one CI runner and not the next. `errors="replace"` in a black-box harness, which asserts on behaviour and can tolerate a mangled character; `errors="strict"` (the default) in `ocx-sdk-python`, where substituting U+FFFD silently corrupts bytes the caller re-emits. Binary mode is exempt, having nothing to decode. | `ruff check --isolated --preview --select PLW1514 --output-format concise .` — any diagnostic is a finding, `All checks passed!` is the pass. **`--preview` is load-bearing**: without it `PLW1514` is inert and the same file reads clean, and `--isolated` keeps a project ignore from silencing it. That rule covers files only, never `subprocess`, so also `rg -n --glob '*.py' --glob '!**/.venv/**' -e 'text=True' -e 'universal_newlines=True' .` — every hit is a candidate, and a hit whose own call passes no `encoding=` is the finding. Both harness runners are hits today | MUST |
| PY-PROC-04 | Commands are argv lists. Never `shell=True`, never `os.system`, never `os.popen` — the argv form is the only thing that makes "all characters, including shell metacharacters, can safely be passed to child processes" true, and a shell in the loop turns every interpolated tag, path or package name into an injection site. PEP 750 t-strings do not change this: PEP 787, the t-string-safe `subprocess`/`shlex.sh()` proposal, is **Deferred** to 3.15 at the earliest, so do not soften the rule in anticipation of it. | `ruff check --isolated --select S602,S604,S605 --output-format concise .` — any printed diagnostic is a finding; `All checks passed!` is the pass. `S605` covers both `os.system` and `os.popen`; `S602`/`S604` cover `shell=True`. `--isolated` is load-bearing — a project `per-file-ignores` entry for `S` silences the whole gate and it reads clean. Both harnesses are at zero today, so this is a don't-regress check | MUST |

## Draining and Teardown

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PROC-02 | A child that may have to be killed early is spawned with `start_new_session=True` (or `process_group=0` on 3.11+) and killed as a **group** via `os.killpg`. `subprocess.run(timeout=)` and `proc.kill()` reap the direct child only and leave any grandchild it spawned alive — proven with live PIDs, not inferred. Guard the signal on `proc.returncode is None`: a reaped child's pid, and the pgid that equals it, can be reissued to an unrelated process (bpo-38630, CWE-367). Never hand-roll this with `preexec_fn`, which is documented as unsafe in a threaded parent. Scoped to sites that spawn something they may need to kill, not to every call. | `rg -n --glob '*.py' --glob '!**/.venv/**' -e '\.terminate\(\)' -e '\.kill\(\)' .` lists every teardown site. For each file, `rg --files-without-match --glob '*.py' -e 'start_new_session' -e 'process_group' <file>` — the path printed is the finding, silence is the pass. Behavioural: cancel mid-run, then `pgrep -g <pgid>` — a pid printed is the orphan | SHOULD |
| PY-PROC-03 | A `Popen` holding `stdout=PIPE`/`stderr=PIPE` is reaped by `communicate()`, never by a bare `.wait()`/`.poll()`. The child blocks once it has written one pipe buffer — measured at 65536 bytes on Linux, per stream, so either stream alone is enough — and the parent then waits forever for a process waiting on the parent. `subprocess.run(capture_output=True)` is `communicate()` underneath and is safe by construction; N concurrent `Popen`s reaped by a list comprehension of `.wait()` is the textbook hang, and the harness has a live one. | `rg -l --glob '*.py' --glob '!**/.venv/**' 'Popen\(' .` names every file that spawns directly. For each, `rg --files-without-match --glob '*.py' 'communicate\(' <file>` — the path printed is the finding, silence is the pass. Discard a hit whose `Popen` passes no `stdout=`/`stderr=` pipe | MUST |
| PY-PROC-05 | Every group-kill path branches on the platform. `os.killpg`, `os.getpgid` and `start_new_session` are POSIX-only, and on Windows `os.killpg` raises `AttributeError` rather than no-opping — so an unguarded group kill converts a cleanup path into a hard failure on the `windows-latest` jobs this fleet really runs. The Windows branch is `creationflags=subprocess.CREATE_NEW_PROCESS_GROUP` at spawn plus `send_signal(CTRL_BREAK_EVENT)`, or a plain `proc.kill()` where reaping just the child is enough. | `rg -l --glob '*.py' --glob '!**/.venv/**' -e 'os\.killpg' -e 'os\.getpgid' -e 'start_new_session' .` names the POSIX-only callers. For each, `rg --files-without-match --glob '*.py' -e 'sys\.platform' -e 'os\.name' -e 'hasattr\(os' <file>` — the path printed is the finding, silence is the pass | MUST |
| PY-PROC-06 | Cancelling the task that awaits an `asyncio` subprocess does **not** kill the child (gh-88050): the coroutine unwinds and the process keeps running, unreaped, past the end of the test or request that owned it. Every `asyncio.create_subprocess_*` site catches `asyncio.CancelledError`, kills the child, awaits it, and re-raises. | `rg -l --glob '*.py' --glob '!**/.venv/**' 'create_subprocess_' .` names every async spawn. For each, `rg --files-without-match --glob '*.py' 'CancelledError' <file>` — the path printed is the finding. Behavioural: cancel the awaiting task, then assert the child's `returncode` is set | SHOULD |

The deadlock is not a style point and does not need a hostile child to
reproduce. `fcntl.F_GETPIPE_SZ` reports 65536 bytes here; a child that writes
200,000 bytes to an unread `stdout=PIPE` never returns from its own flush,
and the parent's `wait()` never returns either. Both sides are blocked on
each other, and a timeout only converts the hang into a failed test — the
output is still gone:

```python
# The hang. N concurrent children, N*2 pipes, nothing draining any of them.
procs = [subprocess.Popen(cmd, stdout=PIPE, stderr=PIPE) for cmd in cmds]
codes = [p.wait() for p in procs]  # fine until one child prints 64 KiB

# The fix, and it is the whole fix. Draining serialises the reap, which is
# fine: the children still run concurrently, they just get read in turn.
outs = [p.communicate() for p in procs]
```

Streaming a live tail *and* bounding it is a different problem, needing a
reader thread or `selectors` on both fds. No caller in this fleet wants a
live tail — every one wants the complete output at the end — so do not build
that until one does.

`ocx-sdk-python`'s `_process.py` is the in-fleet reference for all four: session
at spawn, SIGTERM-then-SIGKILL to the group, one `returncode is not None`
pid-reuse guard every group signal routes through, a `_POSIX` branch for
Windows, and a `CancelledError` handler that kills before re-raising.

```python
proc = subprocess.Popen(
    argv,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    encoding="utf-8",
    errors="replace",
    start_new_session=os.name == "posix",
)
try:
    out, err = proc.communicate(timeout=grace)
except subprocess.TimeoutExpired:
    if proc.returncode is None:  # pid-reuse guard, not politeness
        if sys.platform == "win32":
            proc.kill()  # no killpg here — it does not exist
        else:
            os.killpg(proc.pid, signal.SIGKILL)
    out, err = proc.communicate()  # drain, or the pipes hang the reap
    raise
```

## Reading the Exit Status

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PROC-07 | A `subprocess.run` either passes `check=True` or has its `.returncode` read in the same scope — `check=False` with nobody reading the result turns a CLI exit-code regression into a green test. A returncode **below zero is a signal number, not an error code**: `-15` is SIGTERM, `-9` is SIGKILL, so a bare `returncode != 0` cannot tell "the CLI failed correctly" from "we killed it," and a test that terminates a child asserts the specific negative value it expects. | `ruff check --isolated --select PLW1510 --output-format concise .` — any diagnostic is a finding, and `--isolated` keeps a project ignore from silencing it. `PLW1510` is `subprocess-run-without-check`: it fires on an **omitted** `check=`, never on a missing `timeout=`, and no ruff or bandit rule checks for a missing timeout at all — verified against 0.16.1, not assumed. Then `rg -n --glob '*.py' --glob '!**/.venv/**' 'check=False' .` — a hit with no `.returncode` read in the same function is a finding, and no lint covers it. Then `rg -n --glob '*.py' --glob '!**/.venv/**' 'returncode != 0' .` — a hit in a scope that also calls `.terminate()`/`.kill()` is a finding | SHOULD |

## What Agents Get Wrong Here

1. Reading `PLW1510` in the lint output as "timeouts are covered." It is
   `subprocess-run-without-check`. Nothing in ruff or bandit looks for a
   missing `timeout=`; a clean lint run says nothing about it either way.
2. Believing `subprocess.run(timeout=)` kills the tree. It kills the direct
   child and returns; the grandchild keeps the terminal, the port and the
   lock.
3. `proc.wait()` on a `Popen` holding pipes, because it reads like a simpler
   `communicate()`. It is the deadlock, and it only shows up once output
   crosses 64 KiB — which is a logging change away, not a rewrite away.
4. Reading `text=True` as "decode UTF-8." It is "decode with whatever the
   locale says," which is why the failure is CI-image-specific.
5. Reaching for `preexec_fn=os.setsid` to get a process group.
   `start_new_session=`/`process_group=` exist to replace exactly that, and
   `preexec_fn` carries a documented deadlock warning for threaded parents.
6. Adding a per-call `timeout=` to every `subprocess.run` as the "fix" for a
   hang. That bound belongs to the suite; a per-call value has to cover cold
   binary start plus a registry round-trip in one number, which is how flaky
   tests are made.
7. Porting a POSIX `killpg` teardown into a cross-platform runner unguarded.
   On Windows `os.killpg` is absent, not inert — the cleanup path raises.
8. Copying a `re` pattern into `pexpect.expect()`. pexpect reads one
   character at a time, so `$` matches after *every* character and `.+`
   returns a single character; a PTY ends lines with `\r\n`, never a bare
   `\n`. Give `expect()` a pattern list containing `pexpect.TIMEOUT` and
   `pexpect.EOF`, and put `child.before` in the failure message.

## Sources

- [`subprocess`](https://docs.python.org/3/library/subprocess.html) — the `wait()` deadlock note, `start_new_session`, `process_group`, the `preexec_fn` warning, `text=True`'s locale decoding, and negative `returncode`
- [`subprocess` security considerations](https://docs.python.org/3/library/subprocess.html#security-considerations) — why the argv form is the safe one
- [`os.killpg`](https://docs.python.org/3/library/os.html#os.killpg) — POSIX only
- [cpython#88050](https://github.com/python/cpython/issues/88050) — cancelling a task does not terminate its subprocess
- [bpo-38630](https://bugs.python.org/issue38630) — `send_signal` and the pid-reuse race
- [PEP 750](https://peps.python.org/pep-0750/) and [PEP 787](https://peps.python.org/pep-0787/) — t-strings landed in 3.14; safer subprocess usage did not
- [`PLW1510`](https://docs.astral.sh/ruff/rules/subprocess-run-without-check/), [`PLW1514`](https://docs.astral.sh/ruff/rules/unspecified-encoding/) and [`S602`](https://docs.astral.sh/ruff/rules/subprocess-popen-with-shell-equals-true/) — what the lints actually assert
- [pexpect overview](https://pexpect.readthedocs.io/en/stable/overview.html) — CR/LF line endings and non-greedy trailing quantifiers
