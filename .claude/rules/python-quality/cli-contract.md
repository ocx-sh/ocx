# Python CLI Contract

The exit codes, streams, entrypoint shape and failure behaviour every Python
tool in this catalog honours — `index/bot`, the stdlib-only single-file
scripts and gates, and the Claude Code hook fleet. Read this before changing
anything that can end a process, pick a status, or write to stdout. The file
that does it is rarely named `main.py`, which is why this is routed to by
subject rather than matched by path.

Contents: [The Exit-Code Table](#the-exit-code-table-pinned) ·
[Entrypoint and Exit Codes](#entrypoint-and-exit-codes) ·
[Streams, Pipes, and Torn Writes](#streams-pipes-and-torn-writes) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — one `IntEnum`, `main(argv) -> int`, a single `sys.exit`
  at the module guard, stdout for the result and stderr for everything else —
  is general Python CLI practice.
- **The table** — the specific numbers below — is a *pinned decision*. Its
  value is that it is agreed, not that it is derivable. It is already
  shipped and scripted against, and it exists so a Rust sibling and a Python
  sibling in the same pipeline classify a failure the same way. Extend it;
  do not re-litigate it.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## The Exit-Code Table (pinned)

| Code | Name | Meaning |
|---|---|---|
| 0 | success | Nothing to report. **A Claude Code hook is the documented exception — see below** |
| 1 | failure | General failure, or a validation finding: the tool ran correctly and found a real problem |
| 2 | usage | Bad invocation. `argparse`'s own hard-coded default, kept deliberately |
| 65 | `EX_DATAERR` | Integrity or data anomaly: needs a human, never auto-healed |
| 75 | `EX_TEMPFAIL` | Transient failure: the caller may retry |
| 120 | *(not ours)* | Python's own signal that cleanup *after* `SystemExit` failed — in practice an unhandled `BrokenPipeError` at the interpreter's shutdown flush |
| 126, 127 | *(not ours)* | Shell-level: found-but-not-executable, and command-not-found |
| 128+N | *(not ours)* | Killed by signal N — `130` is SIGINT, `141` is SIGPIPE, `143` is SIGTERM |

`sysexits.h` is adopted **only** for the bespoke application-failure
categories a tool fully controls, because that is the layer where agreeing
with the Rust CLIs in the same pipeline actually pays. `argparse`'s `2`
stays: remapping it to `EX_USAGE` (64) is nonstandard inside Python, cannot
be intercepted at all without `exit_on_error=False` and a wrapper, and buys
nothing an orchestration script cannot get by accepting either code. Nothing
in `3`–`63`, `66`–`119` is claimed; a new category updates this table first.

**The one deliberate collision, and it must stay.** All nine `.claude/hooks/`
scripts call `sys.exit(0)` on every path, *including a deny*. The verdict
travels in stdout JSON — `hookSpecificOutput.permissionDecision` — not in the
status, because Claude Code treats a plain exit `1` as a non-blocking error
and proceeds with the tool call anyway; only a literal `2` blocks through the
code alone, and none of the nine uses it. So `0` means "the hook process did
not crash" there and "nothing wrong was found" everywhere else. A reader who
"fixes" a hook to exit non-zero on a deny breaks the hook.

## Entrypoint and Exit Codes

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-CLI-01 | Every status a tool chooses comes from the pinned table above. A new integer means the table is updated first, in the same change — a locally invented `3` or `42` is a private convention that breaks a caller already scripting against the shared set, and it breaks it silently, since an unrecognised code still looks like "some kind of failure." | `rg -n --glob '*.py' --glob '!**/.venv/**' -e 'sys\.exit\([0-9]+\)' -e 'raise SystemExit\([0-9]+\)' .` — every hit's integer must be `0`, `1`, `2`, `65` or `75`; any other number is the finding. Then `rg -n -A8 --glob '*.py' --glob '!**/.venv/**' 'class ExitCode' .` — read the discriminants; one outside the set is the finding | MUST |
| PY-CLI-03 | The entrypoint is `main(argv: Sequence[str] \| None = None) -> int`, returning a plain `int` or an `IntEnum` member. Never `-> bool`: `sys.exit(True)` exits **1**, so a `main` that returns `True` for success inverts pass and fail while still printing its success message. Never `-> None` where a caller consumes the value. Taking `argv` is also what lets a test call `main([...])` and assert on the code with no subprocess. | `rg -n --glob '*.py' --glob '!**/.venv/**' -e 'def main\(.*-> bool' -e 'def main\(.*-> None' -e 'def main\([^)]*\):' .` — any hit is a finding: the first two are the wrong annotation, the third is no annotation at all. Empty output is the pass; a correctly annotated `def main(argv: … ) -> int:` does not match | MUST |
| PY-CLI-04 | `sys.exit(main(...))` appears exactly once in a module, on the line under `if __name__ == "__main__":`, and `sys.exit` never appears inside `main()`'s own body — an exit buried in the dispatch path is unreachable to a test and invisible to a reader tracing the contract. | `rg -n --glob '*.py' 'sys\.exit\(' <file>` — the pass is exactly one hit, immediately under the `__main__` guard; every other hit is a finding. **Zero hits is also a finding**: a module with a `main()` nobody exits through has no exit-code contract at all, only whatever an exception happens to leave behind | SHOULD |
| PY-CLI-05 | `argparse`'s `2` for usage errors is kept as-is. Never subclass `ArgumentParser` to override `error()` and remap it, and never emit `64`/`EX_USAGE` from Python. A tool-specific failure category is raised *after* `parse_args()` has succeeded and mapped to its own code there, the way `index/bot` maps `IndexBotError` subclasses. | `rg -n --glob '*.py' --glob '!**/.venv/**' -e 'sys\.exit\(64\)' -e 'EX_USAGE' -e 'def error\(self' .` — any hit is a finding; empty output is the pass, and it is empty across the catalog today | SHOULD |
| PY-CLI-06 | `os._exit()`, bare `exit()`, `quit()` and `argparse`'s `type=bool` never appear in a shipped script. `os._exit` skips `finally` blocks and the stream flush; `exit`/`quit` are `site` conveniences that do not exist under `python -S`; and `type=bool` is the trap an agent introduces unprompted, because `bool("False")` is `True` — the flag reads as set whatever the user typed. Use `action="store_true"` or an explicit converter. | `rg -n --glob '*.py' --glob '!**/.venv/**' -e 'os\._exit\(' -e '^\s*exit\(' -e '^\s*quit\(' -e 'type=bool' .` — any hit is a finding, empty is the pass. The anchored `^\s*` spellings catch bare `exit(...)` in statement position without matching `sys.exit(`/`parser.exit(` | MUST |

```python
class ExitCode(IntEnum):
    OK = 0
    VALIDATION_FAILURE = 1
    ANOMALY = 65  # EX_DATAERR — a human decides
    TRANSIENT = 75  # EX_TEMPFAIL — the caller may retry


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)  # bad invocation exits 2 here
    try:
        return int(dispatch(args))
    except ToolError as exc:  # our own categories only
        print(f"error: {exc}", file=sys.stderr)
        return int(exc.exit_code)  # anything else crashes, on purpose


if __name__ == "__main__":
    sys.exit(main())
```

That shape is what makes the contract testable with no subprocess at all:
call `main([...])` and assert on the returned `int` for the dispatch paths,
and use `pytest.raises(SystemExit)` plus `exc_info.value.code` for the two
argparse raises from inside `parse_args()` — `--version` and a missing
subcommand. `capsys.readouterr()` covers the streams in the same test.

## Streams, Pipes, and Torn Writes

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-CLI-02 | stdout carries the tool's result — the thing a caller parses, greps or redirects to a file. Every message *about* the run — status, progress, warnings, prompts and errors — goes to stderr, unconditionally. Under a machine-output flag stdout is the payload and nothing else: no banner, no progress, no trailing "done" line, or the caller's `json.loads` fails on output that was technically correct. `sys.exit("message")` already writes to stderr, which is why it composes; a bare `print()` of an error does not. | `rg -l --glob '*.py' --glob '!**/.venv/**' 'print\(' .` names every file that writes. For each, `rg --files-without-match --glob '*.py' 'file=sys\.stderr' <file>` — a printing file that never names stderr is the finding, since no real tool is all payload. Then classify each remaining `print(` one line at a time: "this is the output" or "this is a message about the run". Per machine-output subcommand, an integration test parses the *whole* captured stdout | MUST |
| PY-CLI-07 | Where a harness genuinely requires exit `0` on every path — the hook fleet — a top-level `except Exception` is the licensed exception to the core no-catch-all rule, and the module docstring or an adjacent comment names the contract that requires it. `except Exception` is also the right *width* here, for the reason the core exception rule gives; `except BaseException` at that position would be the actual bug. | `rg -n -A3 --glob '*.py' 'except Exception' <file>` finds the swallowing handlers; then `sed -n '1,12p' <file>` — a handler whose module header does not state why a non-zero exit is forbidden is the finding. `post_tool_use_tracker.py`'s "It MUST never exit non-zero" docstring is the reference wording | MUST |
| PY-CLI-08 | A tool whose stdout can exceed one pipe buffer restores the default SIGPIPE disposition and keeps a `BrokenPipeError` guard. Python installs `SIG_IGN` for SIGPIPE, so a `… \| head` failure surfaces at the interpreter's *shutdown flush* — past every handler in `main()` — printing a traceback and exiting `120`. `signal.signal(signal.SIGPIPE, signal.SIG_DFL)` makes the tool die like a normal Unix filter instead; Windows has no SIGPIPE, so the `BrokenPipeError` guard is still needed there and the `hasattr` check is what keeps the module importable on both. | Run `python3 <tool> <args producing >64 KiB> 2>err.log \| head -1`, then read `err.log`. Any `BrokenPipeError` or `Exception ignored while flushing sys.stdout` text is the finding, as is a writer status of `120`. **Empty stderr is the pass — and the writer's status is then `141` (128+SIGPIPE), not `0`.** `check-artifacts.py`'s `__main__` block is the reference recipe | MUST |
| PY-CLI-09 | A file another process reads — a lock, a session file, a tracker log — is written atomically: `tempfile.mkstemp` in the *target's own directory*, write, `Path.replace()`, and `except BaseException: tmp.unlink(missing_ok=True); raise`. A direct `.write_text()` killed mid-write leaves a torn file, and a reader's `json.loads` guard papers over the state loss rather than preventing it. `BaseException` is deliberate here, unlike PY-CLI-07: cleanup must run for a Ctrl-C too. | `rg -n --glob '*.py' --glob '!**/.venv/**' -e '\.write_text\(' -e '\.write_bytes\(' .` — a hit writing a path another process reads is the finding; a write to a fresh, process-private path is not. `index/bot`'s `local_files.py:_write_atomic` is the reference; `hook_utils.py`'s six direct writes are the open findings | SHOULD |

The SIGPIPE recipe, verified against a planted violation: unguarded, the same
tool piped into `head -1` prints a traceback plus `Exception ignored while
flushing sys.stdout` and exits `120`; guarded, stderr is empty and it dies of
SIGPIPE like `grep` would.

```python
if __name__ == "__main__":
    # `… | head` closes the pipe early. Python installs SIG_IGN for SIGPIPE, so
    # the write fails with EPIPE — and because stdout to a pipe is block
    # buffered, it fails at the interpreter's *shutdown* flush, past any
    # handler here. Restoring the default disposition makes this die like a
    # normal Unix filter. Windows has no SIGPIPE; there BrokenPipeError does
    # surface, so keep both.
    if hasattr(signal, "SIGPIPE"):
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    try:
        sys.exit(main(sys.argv[1:]))
    except BrokenPipeError:
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        sys.exit(1)
```

## What Agents Get Wrong Here

1. `def main() -> bool` plus `sys.exit(main())`. `True` is `1`, so every
   successful run reports failure — and the success message still prints,
   which is what makes it survive review.
2. "Fixing" a hook to exit non-zero on a deny. Exit `1` is silently
   *non-blocking* there; the JSON verdict is the whole mechanism.
3. Reaching for `EX_USAGE` (64) because the Rust siblings use it. In Python
   the usage code is `2`, and `argparse` already emits it.
4. A progress line, a banner, or a "done" on stdout under a machine-output
   flag — output that is correct to read and unparseable to a caller.
5. `sys.exit("message")` assumed to print to stdout or exit `0`. It writes
   the string to **stderr** and exits **1**. A legitimate idiom, but not the
   one an agent asked to "print a note and stop" usually means.
6. No `BrokenPipeError` handling on a tool with a print loop, because the
   tool works in every invocation that is not piped into `head`.
7. Inventing an exit code for a new failure class instead of using `65` or
   `75`, or updating the table.
8. `argparse(type=bool)` for a `--flag false` interface. It is `store_true`
   or nothing.

## Sources

- [`sys.exit`](https://docs.python.org/3/library/sys.html#sys.exit) — `True` is `1`, a string goes to stderr, and cleanup failure becomes `120`
- [Note on SIGPIPE](https://docs.python.org/3/library/signal.html#note-on-sigpipe) — why the failure lands at the shutdown flush, and the official recipe
- [`argparse`](https://docs.python.org/3/library/argparse.html) — the hard-coded `2`, `ArgumentParser.error`, `exit_on_error`, and the `type=bool` warning
- [Built-in exceptions](https://docs.python.org/3/library/exceptions.html) — `SystemExit` and `KeyboardInterrupt` inherit `BaseException` on purpose
- [FreeBSD `sysexits.h`](https://man.freebsd.org/cgi/man.cgi?sysexits) — `EX_DATAERR` 65, `EX_TEMPFAIL` 75
- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks) — only exit `2` blocks; exit `1` is non-blocking
- [clig.dev](https://clig.dev/) — streams, machine output, and the stdout/stderr split
- [POSIX shell exit status](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html) — 126, 127, and 128+N
