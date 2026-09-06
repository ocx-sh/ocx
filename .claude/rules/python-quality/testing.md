# Testing

Isolation, hangs, assertion shape, and what a green run actually proves, for
every Python suite in the family. Loads while writing or changing a test, a
fixture, a `conftest.py`, or the CI job that runs one.

Contents: [Isolation and Determinism](#isolation-and-determinism) ·
[Assertions and Selection](#assertions-and-selection) ·
[Hangs, Timeouts and Log Signal](#hangs-timeouts-and-log-signal) ·
[Testing the CLI](#testing-the-cli) ·
[Coverage and Executed Docs](#coverage-and-executed-docs) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Four suite shapes adopt these rules, and a rule that binds only one says so in
its cell: the **acceptance harness** (thousands of black-box tests driving a
compiled binary through a subprocess runner), the **typed library** (a
published package, strict type checking, real 100% coverage), the **service**
(an internal bot or job, same rigor, no public API), and **single-file stdlib
tools** (hooks and scripts with no package around them). A rule written against
the harness does not transfer to a library suite unmodified, and the two places
that trip people are `os.environ` and `time.sleep` — both settled below.

## Isolation and Determinism

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TEST-01 | Direct `os.environ` mutation is legitimate in exactly one place: the body of a `pytest_configure`, `pytest_sessionstart` or `pytest_load_initial_conftests` hook, carrying a comment naming why no fixture can substitute — a hook runs before `monkeypatch` exists to be requested, and the value has to survive into xdist workers that fork from it. Everywhere else — any fixture, any test body — it is `monkeypatch.setenv`/`delenv`, or the mutation leaks into every test that worker runs afterwards. | `rg -n --glob '*.py' --glob '!conftest.py' -e 'os\.environ\[[^]]*\] *=' -e 'os\.environ\.pop\(' -e 'os\.environ\.update\(' -e 'del os\.environ\[' <test-root>` — every hit is the violation, empty is the pass; the assignment anchor keeps `os.environ.get` reads out. Then `rg -n -B6 --glob 'conftest.py' -e 'os\.environ\[[^]]*\] *=' -e 'del os\.environ\[' <test-root>` — a hit whose enclosing `def` is not `pytest_*` is the violation. | MUST |
| PY-TEST-02 | A `time.sleep` names, in a comment, what it waits past. A sleep waiting on something observable — a file appearing, a process exiting, a port opening, a request count rising — polls that proxy against a `time.monotonic()` deadline instead. A blanket ban is wrong: two of the three real categories below are not bugs. | `rg -n --glob '*.py' '^[^#]*time\.sleep\([^#]*$' <test-root>` — prints only sleeps with no same-line comment; each hit is either annotated on the line immediately above (re-read it) or is the violation. Empty output is the pass. | SHOULD |
| PY-TEST-03 | A fixture scoped wider than `function` that shares an external resource states, in its docstring, which of the four safety arguments below applies. "It is faster" is not one of them, and a shared resource with none of the four belongs behind `@pytest.mark.xdist_group(name=...)` affinity rather than the default distribution. | `pytest -p no:cacheprovider -n 0 <test-root>`, then `pytest -p no:cacheprovider -n 4 --dist load <test-root>` — a test that passes in the first and fails in the second is the violation, and the parallel run names it. Watched red: a session fixture returning a mutable dict passed sequentially and failed under `-n 2`. | SHOULD |

The three `time.sleep` categories, and which one is the bug:

1. **The inner step of an already-bounded poll.** Not a bug. Prefer a
   `time.monotonic()` deadline over a fixed iteration count where the timeout
   value itself is what the test asserts on.
2. **A wait on filesystem or clock resolution** — a distinct `mtime` on a
   coarse filesystem, a throttle window elapsing. **Irreducible.** Spinning on
   `stat()` converges no faster; the value cannot advance until real time has.
3. **"Give the process a moment"** with no observed proxy. **The actual
   anti-pattern**, and usually convertible — poll for the lockfile, the exit
   code, the log line. Where genuinely no proxy exists (another process's
   advisory lock), a generous commented sleep is the honest answer.

The four xdist-safety arguments, one of which a wide-scoped fixture must claim:
the resource is started once in a session hook **before workers fork**; its
name is derived from a **dynamically allocated port** so concurrent copies
cannot collide; the fixture is **pure and read-only**; or the state is
genuinely unpartitionable and the module is pinned to one worker by
**`xdist_group`**.

## Assertions and Selection

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TEST-04 | Acceptance-harness assertions on CLI output are substring checks on the fact under test, not whole-blob equality — the harness does not own the exact bytes of a binary it did not build, so a whole-blob assertion also pins a colour code and a trailing newline nobody meant to freeze. The opposite holds for a library or service whose serialized bytes **are** the product: that gets a committed golden fixture plus a documented command that regenerates it from the real code path, never a hand-typed expected string. | `rg -n --glob '*.py' -e '\.stdout == ' -e '\.stderr == ' <test-root>` — every hit that is not comparing against a committed golden file is the violation; empty is the pass. For the golden half: run the documented regeneration command, then `git status --porcelain <golden-dir>` — non-empty output is the violation, the fixture no longer matches what the code produces. | SHOULD |
| PY-TEST-05 | `pytest.raises` always carries `match=`. One line of config, not prose: select `PT011` and list the project's own exception bases under `[tool.ruff.lint.flake8-pytest-style] raises-extend-require-match-for`. Those names must be **fully qualified** — a bare class name is accepted and silently matches nothing, so the check reads green while every project-specific bare `raises` sails through. `["*"]` requires `match=` everywhere. | `ruff check --select PT011 <test-root>` — each reported line is the violation. Watched red on a bare `pytest.raises(ValueError)`; watched *silently green* on a bare `pytest.raises(GrimError)` until the config named `module.GrimError` rather than `GrimError`. | SHOULD |
| PY-TEST-13 | `--strict-markers` is in `addopts`, and every marker used in the tree is registered in `markers`. Without it a mistyped marker emits a warning and nothing else: the test stays in whatever tier the typo put it in, the selection the developer wrote silently does not apply, and the run exits 0. One line of config; there is no prose substitute. | `rg -n --glob 'pyproject.toml' --glob 'pytest.ini' --glob 'tox.ini' 'strict-markers' <project-dir>` — empty output is the finding. Watched red: with a marker misspelled by two letters, `pytest -m "not requires_docker"` reported `2 passed, 1 warning` and exit 0 — the excluded test ran. The same invocation with `--strict-markers` hard-failed at collection with `'requires_dokcer' not found in markers configuration option`. | MUST |

## Hangs, Timeouts and Log Signal

The four rules below are one finding, split by where it is fixed. Measured on
a real CI configuration, a single hung test produced: six hours of runner time
burned, because no job set `timeout-minutes`; a log with no test name in it,
because the verbosity flag the task file declared was silently dropped the
moment CI passed an argument; possibly no output at all for the last several
dozen passing tests, because the log pipe is not a TTY and CPython
block-buffers; and no JUnit artifact, because the upload was guarded on
`!cancelled()` and a timed-out job reports as cancelled. Each is individually
one line. Together they make a hang unattributable.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TEST-06 | Every suite sets a session-level `pytest-timeout` bound and chooses its method deliberately. `signal` fails the one hung test and lets the run finish, so every other test still lands in the JUnit report — but the alarm only fires when the interpreter regains control, so a GIL-holding C call overshoots the bound. `thread` kills the whole process at the bound after dumping every thread's stack, and takes the JUnit report with it. `signal` is POSIX-only; a Windows job gets `thread` whether it chose it or not. Do not add a per-call `subprocess` timeout to compensate — a value guessed to cover a cold binary start plus a registry round-trip creates flakes faster than it catches hangs. | `rg -n --glob 'pyproject.toml' -e '--timeout' <project-dir>` — empty output is the finding, no bound is configured. Then add a `while True: pass` test and run the suite: watched red at 3.01s under `--timeout=3 --timeout-method=signal` (run completed, one named failure) and at 3.17s under `--timeout-method=thread` (process terminated, stack dump, no summary). Same probe against a GIL-holding C call landed at 3.95s under `signal`. | MUST |
| PY-TEST-07 | Every CI job that runs a Python suite sets `timeout-minutes`. With none, the platform default is six hours, and a hung test burns all of it before anything turns red. | `rg --files-without-match --glob '*.yml' --glob '*.yaml' 'timeout-minutes' .github/workflows` — every listed file is the violation, empty is the pass. This is file-level: a workflow that sets it on one job and not another still needs a per-job read. The explicit path operand is load-bearing — with none, `rg` reads stdin, finds nothing, prints nothing and reads as clean. | MUST |
| PY-TEST-08 | A CI step running a suite sets `PYTHONUNBUFFERED=1`, and the test-report upload is guarded `if: always()`, never `if: !cancelled()` — a job killed by its own `timeout-minutes` reports as *cancelled*, so a `!cancelled()` guard skips the upload and the one artifact that would say which test hung never leaves the runner. Unbuffered matters for the same reason: the log pipe is not a TTY, so CPython block-buffers, and the last several dozen passing tests can sit unflushed when the process is hard-killed. | `rg -n -B3 --glob '*.yml' --glob '*.yaml' '!cancelled\(\)' .github/workflows` — every hit guarding a report or artifact upload is the violation. Then `rg --files-without-match --glob '*.yml' --glob '*.yaml' 'PYTHONUNBUFFERED' .github/workflows` — a listed file that runs a Python suite is the violation. | SHOULD |
| PY-TEST-09 | Flags a task file declares for pytest actually reach pytest. A templating default that only substitutes when the argument list is *empty* silently evaporates the moment CI passes anything at all — which CI always does, to name the JUnit path. The suite then runs without the verbosity the task file claims, and a hang prints no test name. | `rg -n --glob 'taskfile.yml' --glob 'Taskfile.yml' 'CLI_ARGS.*default' <project-dir>` — every hit is a flag CI loses; empty is the pass. Confirm live with `task -d <dir> test -- <the exact arguments CI passes>`, which prints the resolved command. Watched red: the same task echoed `pytest -v` bare and `pytest --junit-xml=results/junit.xml` with the argument — `-v` gone. | MUST |

A timeout is never a retry. A test that did not finish in its bound is either
a real product hang or an infrastructure symptom; retrying it silently turns
both into "it eventually passed", which is the one result that teaches nobody
anything. Where a retry genuinely belongs — a known-flaky network fixture — it
lives in that fixture, not folded into the timeout.

## Testing the CLI

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TEST-10 | Where an entry point is `main(argv) -> int` with `sys.exit` confined to the `__main__` guard, a test calls `main([...])` directly and reads `capsys` — a subprocess is reserved for the case where the process boundary itself is the contract (signal handling, a real TTY, the shipped console script). Applies to the library, service and single-file-tool shapes; the acceptance harness drives a compiled binary and has no in-process option. Spawning a fresh interpreter to exercise Python you could have called buys a slower test that reports a returncode instead of a traceback. | `rg -n --glob '*.py' -e 'subprocess\.run\(\[sys\.executable' -e 'subprocess\.Popen\(\[sys\.executable' <tests-dir>` — in those three shapes every hit is the violation unless the process boundary is what is under test; empty is the pass. | SHOULD |

The rule is not "never spawn a subprocess" — it is that the subprocess must be
buying something. It buys the exit code the shell actually observes, the stream
a user actually reads, signal handling, a real TTY, and the behaviour of the
installed console script rather than the module. It buys nothing when the test
could have called the function and read the return value, and it costs a
traceback: a failed in-process call names the line, a failed subprocess names
a number.

## Coverage and Executed Docs

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TEST-11 | Coverage runs with branch coverage on, and every `# pragma: no cover` states a reason a reviewer can independently check — a platform gate, a typing-only block. A bare pragma is indistinguishable from someone reaching a `fail_under` number, and a whole module excluded by config is that at scale. Structural exclusions (`if TYPE_CHECKING:`, the `__main__` guard, a bare `...`) belong in `exclude_also`, not repeated as pragmas at every site. | `rg -n --glob '*.py' 'pragma: no cover *$' <src-dir>` — every hit is a bare pragma and the violation; a reasoned one has text after `cover` and is correctly not matched. Then `rg -n --glob 'pyproject.toml' 'branch = true' <project-dir>` — empty output is the finding, coverage is statement-only. | SHOULD |
| PY-TEST-12 | A `>>>` example in shipped documentation is collected and executed by the suite, or it is a claim nobody checks. An unexecuted example drifts from the code silently, and it drifts in the one direction that matters: the version a reader copies. | `rg -l --glob '*.py' '^\s*>>> ' <src-dir>` lists every module carrying an example. Then `rg -n --glob 'pyproject.toml' --glob 'conftest.py' -e '--doctest-modules' -e 'sybil' -e 'Sybil' <project-dir>` — empty output while the first command listed anything is the violation. Confirm by deleting one character from an expected value and watching the run fail. | SHOULD |

Coverage is a floor, not a claim. A test that calls a function expected to
raise, wrapped in a bare `try`/`except`, executes every line of the raising
branch and asserts nothing about which exception, its message, or whether it
was the expected failure at all — 100% statement coverage over a test that
cannot fail. That is why PY-TEST-05 is separately enforced, and why a coverage
number is worth reporting but never worth defending.

## What Agents Get Wrong Here

1. **Copying a library-suite rule onto an acceptance harness.** The blanket
   "never `time.sleep`, never touch `os.environ`" is correct where the test
   controls all of its dependencies and wrong where it drives a real binary
   against a real filesystem. Both carve-outs are above; neither is a loophole.
2. **`scope="session"` reached for to make a slow fixture fast**, with no
   safety argument, then debugged as "flaky under xdist" months later.
3. **Reporting a check green without ever seeing it red.** A search with no
   path operand reads stdin, matches nothing, exits quietly and looks identical to
   a clean tree. Every check here carries its path for that reason.
4. **Adding a per-call `subprocess` timeout "to be safe."** It converts a slow
   pass into a new flake and does not kill the grandchildren anyway — a
   timeout reaps the direct child only.
5. **Widening a `pytest.raises` to the base class** to make a test pass, when
   `match=` on the specific one was the assertion.
6. **A `# pragma: no cover` added to reach the coverage number**, on a line
   that is reachable.
7. **Asserting on a whole captured stdout blob** for a claim that needed one
   substring — it passes today and breaks on the next colour-code change.
8. **`mock.patch` aimed at the definition site instead of the lookup site.**
   Patches nothing, test still passes, because the real path never ran.
