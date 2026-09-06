---
paths:
  - "**/*.py"
summary: The Python quality index — the gate, the non-negotiables, the pinned exit-code contract, and where the depth lives
keywords: python,quality,standards,review,typing,async,subprocess,pytest,exit-codes,cli,security,logging,packaging,ruff,pyright
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Python Quality

Traps, not maps. Everything here names a mistake that gets made without
it; the architecture of any particular codebase is discoverable by reading
the code, so it is not in this file.

Contents: [The Gate](#the-gate) · [Non-Negotiables](#non-negotiables) ·
[Rules This File Owns](#rules-this-file-owns) ·
[Where the Depth Is](#where-the-depth-is) · [Severity](#severity) ·
[Siblings](#siblings)

**Ending a process, choosing a status, or writing to stdout? Read
[python-quality/cli-contract.md](python-quality/cli-contract.md) first.**
The exit-code table is pinned and already scripted against, and one shape
here deliberately exits `0` on failure because its harness reads the
verdict from stdout — a number changed locally is a shipped contract break.

## The Gate

Run it after every change, narrowest scope first — each stage costs more
than the last, so the common case never reaches the slow ones. Lint and
type-check are effectively free here (measured: ruff 0.06–0.16s, pyright
1.3–2.7s), so there is no version of "too slow to run".

```bash
uv run ruff format --check .
uv run ruff check .
uv run pyright <package-or-src-dir>
uv run pytest -q
uv run coverage report      # where a coverage gate is configured
```

Run them through the project's pin, never through `$PATH`: a globally
installed tool shadowing the pinned one is the most common way two people
get different answers from the same command.

A task is done when a command, its exit code, and the tree it ran against
are all named. Narration is not evidence.

## Non-Negotiables

Every line below blocks a merge. IDs resolve to the depth files in
[Where the Depth Is](#where-the-depth-is), where each rule carries its
rationale and verification.

| # | Rule | ID |
|---|---|---|
| 1 | `except Exception` does not catch `KeyboardInterrupt`, `SystemExit`, or `asyncio.CancelledError` — all three inherit `BaseException`. Never write, cite, or rely on the claim that it does. | PY-CORE-01 |
| 2 | No catch-all encloses the body of a tool whose exit code is consumed. A gate that crashes must not exit `0`. | PY-CORE-02 |
| 3 | Exit values come from the pinned table; a status invented locally breaks a caller that already scripts against it. | PY-CLI-01 |
| 4 | stdout carries the result; diagnostics, progress and errors go to stderr. Under a machine-output flag, stdout is the payload and nothing else. | PY-CLI-02 |
| 5 | Every text `open()` and every captured subprocess names its `encoding=` — `text=True` alone rides the locale. | PY-PROC-01 |
| 6 | Never read both `stdout` and `stderr` from a `Popen` without `communicate()` — the pipe buffer fills at 64 KiB and the wait never returns. | PY-PROC-03 |
| 7 | No blocking primitive inside `async def`, and `ASYNC` is selected wherever `async def` appears. | PY-ASYNC-05 |
| 8 | Every HTTP client states an explicit timeout, and no server-supplied URL is followed by an authenticated client before its host is checked. | PY-HTTP-02, PY-HTTP-05 |
| 9 | Untrusted input is bounded before use, not after: one archive member resolved and streamed with an explicit `filter="data"`, byte and entry caps applied inside the read loop, never `extractall()`. | PY-SEC-01, PY-SEC-02 |
| 10 | No secret reaches a log line, an error message, a `repr`, `argv`, or a subprocess environment. | PY-SEC-03 |
| 11 | A library configures logging never — `getLogger(__name__)`, at most a `NullHandler`, and nothing else. | PY-OBS-01 |
| 12 | No name used in an annotation is undefined when that annotation is evaluated, and `F821` is in the gate before any `TC`-family rule is enabled. | PY-TYP-01, PY-CORE-03 |
| 13 | Never weaken an assertion, skip a test, widen an ignore list, or run `ruff check --fix --unsafe-fixes` to reach green. | PY-CORE-04, PY-CORE-05 |
| 14 | Every Python tree is covered by a ruff config and a type-checker config. An uncovered tree is a hole nothing reports. | PY-CORE-06 |
| 15 | A verification ships only after someone has watched it go red against a deliberately broken copy. | PY-CORE-07 |

## Rules This File Owns

Cross-cutting rules that belong to no single depth file.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-CORE-01 | Never write or rely on the claim that `except Exception` catches `KeyboardInterrupt` or `SystemExit` — both inherit `BaseException`, so the claim is false and the rule built on it protects nothing. | Put a `raise KeyboardInterrupt` inside a `try`/`except Exception` block and confirm it propagates. Then `rg -n 'KeyboardInterrupt' --glob '*.md' .` across your own rule and doc corpus: any text asserting the opposite is the violation. | MUST |
| PY-CORE-02 | No catch-all wraps the dispatch of a tool whose exit code is consumed, so a crash can never be reported as a clean run. | `rg -n 'except Exception' <entrypoint>` and read each hit: one enclosing `main()`'s body without an adjacent comment naming a harness contract is the violation. Empty output is a pass. | MUST |
| PY-CORE-03 | `F821` is enforced before any `TC`-family rule is adopted — `TC` moves imports into `if TYPE_CHECKING:` blocks, manufacturing unresolvable forward references at scale, and no ruff setting closes that gap. | `uv run ruff check --select F821 .` must be clean, and `rg -n 'TC0' pyproject.toml ruff.toml` must return nothing until it is. Output from either is the violation. | MUST |
| PY-CORE-04 | Never reach green by weakening the check: no widened ignore list, no skipped test, no hand-edited expectation, no edit to the gate's own config as part of a functional change. | Read the diff for changes to lint config, `per-file-ignores`, skip markers and expected values. Any of them in a change that is not itself a gate change is the violation. | MUST |
| PY-CORE-05 | Never run `ruff check --fix --unsafe-fixes` under an agent — unsafe fixes change behaviour, and `assert False` becoming `raise AssertionError` changes what `python -O` does. | `rg -n 'unsafe-fixes' .` across task files, workflows and agent instructions. Any hit outside a human-confirmed one-off is the violation. | MUST |
| PY-CORE-06 | Every Python tree is covered by both a ruff config and a type-checker config — an uncovered tree is a hole nothing reports, and 60% of this family's Python sat in one. | For each directory containing `*.py`, resolve the nearest `pyproject.toml` `[tool.ruff]` or `ruff.toml`, and the nearest pyright configuration. A tree resolving to neither is the violation. | MUST |
| PY-CORE-07 | A verification enters a rule table only after it has been watched go red against a planted violation — a check that cannot fail launders an unchecked change as a checked one. | Copy the subject, break the thing the rule forbids, run the verification. A pass on the broken copy is the violation. | MUST |
| PY-CORE-08 | State whether empty output means a pass or means the finding, in every verification that is not self-evidently one or the other. | Read each verification cell: one whose empty output is ambiguous is the violation. | SHOULD |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is
filed under. One level deep; these files do not point at each other.

| Doing… | Read |
|---|---|
| Anything that ends a process, picks an exit status, parses argv, or writes to stdout | [python-quality/cli-contract.md](python-quality/cli-contract.md) |
| Spawning a process, capturing its output, driving a CLI, or handling a PTY | [python-quality/processes.md](python-quality/processes.md) |
| Writing or changing a test, a fixture, a marker, or a coverage setting | [python-quality/testing.md](python-quality/testing.md) |
| Adding or changing an annotation, a generic, a `cast`, or a type-checker setting | [python-quality/typing.md](python-quality/typing.md) |
| Anything `async`, awaited, spawned, cancelled, or timed out | [python-quality/async.md](python-quality/async.md) |
| Making a network request, handling a response, retrying, or paginating | [python-quality/http.md](python-quality/http.md) |
| Unpacking an archive, reading untrusted input, or touching a credential | [python-quality/security.md](python-quality/security.md) |
| Writing a log line, a warning, or user-facing diagnostic output | [python-quality/observability.md](python-quality/observability.md) |
| Designing a public function, class, or exception that something outside the package imports | [python-quality/api-surface.md](python-quality/api-surface.md) |
| Choosing a dataclass, enum, `TypedDict`, or sentinel; serializing anything another tool reads | [python-quality/data-modelling.md](python-quality/data-modelling.md) |
| Writing a script that must run with no third-party dependencies | [python-quality/single-file-tools.md](python-quality/single-file-tools.md) |
| Turning on a lint, wiring a CI job, or adopting a gate on a tree that had none | [python-quality/ci-gate.md](python-quality/ci-gate.md) |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not
in the commit body. CONSIDER = Suggest: never blocks, never re-raised
after a decline.

Keep the Block list short enough that a blocked change is unusual. A rule
set where everything blocks teaches the reader to negotiate with all of it.

## Siblings

- **`python-packaging`** — the manifest and distribution surface: the
  version floor that must actually run, dependency declaration, lockfiles,
  wheel contents, and publishing credentials. Loads on `pyproject.toml`
  and `uv.lock`.
