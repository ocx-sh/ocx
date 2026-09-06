# Single-File Stdlib-Only Tools

The shape where one `.py` file is the whole deliverable: an editor hook fired
on every tool event, a repository gate run by CI, a one-shot generator. No
install step, no third-party imports, a sibling helper module at most. Loads
with the Python quality rule on any diff under `.claude/hooks/`, `scripts/`,
or any standalone `*.py` a task runner or harness invokes directly.

Contents: [Declaring the Interpreter](#declaring-the-interpreter) ·
[Proving the Tool Still Works](#proving-the-tool-still-works) ·
[Surviving the Harness](#surviving-the-harness) ·
[Import and Module Discipline](#import-and-module-discipline) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Two invocation contracts share this shape and must not be conflated. A
**harness-invoked hook** is called on every editor event, communicates through
stdout, and must never fail its caller. A **standalone gate** is called by CI
or a human, communicates through its exit code, and must fail loudly. Exit
codes, stream discipline and the crash-versus-finding split for both live in
`cli-contract.md`; this file is everything else.

**The defect class this file exists to prevent** is a check that cannot go
red. A self-test stripped by an interpreter flag, a fallback parser that
returns empty instead of raising, a lint rule inert on the platform you run
on — each reports success while the thing it guards is broken.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Declaring the Interpreter

The whole header, for a script with no dependencies:

```python
# /// script
# requires-python = ">=3.10"
# ///
```

Both `dependencies` and `requires-python` are optional per the specification,
and omitting `dependencies` is the complete, valid way to say "this script has
none" — a secondary claim that `uv run` requires the key even when empty does
not survive testing. Note what does and does not consume the block: `uv run`,
`pipx run` and `hatch run` resolve the floor from it and select an interpreter;
ruff and mypy read it for a per-file target version; `pip` and a bare
`python3 tool.py` ignore it entirely. Under a plain-`python3` invocation the
header documents the floor without enforcing it, which is still worth having
and is not the same thing as a gate.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SOLO-04 | Every entry point declares its Python floor in a PEP 723 `# /// script` header with `requires-python`, not only in a lint config's `target-version` or a CI setup step. A floor that lives three files away is one nobody reads and nothing enforces at the point of use; under `uv run` the header is what actually selects the interpreter. Omitting the `dependencies` key entirely is the correct, spec-valid declaration for a zero-dependency script — do not write `dependencies = []` to satisfy a linter that does not exist. A shared module imported by the entry points needs no header of its own. | `rg --files-without-match --glob '*.py' '^# /// script' <dir>` — the files it **lists** are the finding; written the other way round as a `grep -q` the check prints nothing on failure and reads as a pass. Then, separately, `rg --files-without-match --glob '*.py' 'requires-python' <dir>`. Watched red against this catalog's own `scripts/` and skill-scripts directory, both of which carry no header; the audited hook fleet lists only its shared helper module, correctly | SHOULD |
| PY-SOLO-05 | A shebang and the executable bit are present together or absent together. A script always invoked through an explicit runner (`uv run "$PATH"`, `python3 path/to/tool.py`) needs neither; a script meant to be run as `./tool.py` carries both, and its shebang is `#!/usr/bin/env -S uv run --script`, the only form that enforces the declared floor rather than merely documenting it. A shebang with no exec bit is a lie a reader acts on. | `python3 -c 'import os,pathlib,sys; v=[(p,p.read_text(encoding="utf-8").startswith("#!"),os.access(p,os.X_OK)) for p in sorted(pathlib.Path(sys.argv[1]).rglob("*.py"))]; b=[t for t in v if t[1]!=t[2]]; [print(f"VIOLATION: {p}: shebang={s}, exec bit={x} - a script is both or neither") for p,s,x in b]; sys.exit(1 if b else 0)' <dir>` — output is the finding. **Do not use `ruff --select EXE` for this half**: EXE001 and EXE002 are documented as not enforced on Windows *or WSL*, so on a WSL workstation they are silently inert and the check reads clean. Confirmed: a planted shebang-without-exec-bit file passed `ruff --select EXE` and was caught by the command above. EXE003/EXE004 are textual and do fire | CONSIDER |

## Proving the Tool Still Works

An embedded `--self-test` is the right shape here: it ships with the tool,
needs nothing installed, and runs in CI on every change. The pattern is sound;
one implementation detail defeats it entirely.

```python
def self_test() -> None:
    assert classify("temp_file") == "bad"  # -O deletes this line...
    print("self-test: ok")  # ...and prints this one, exit 0


def expect(condition: bool, what: object) -> None:
    # `assert` is stripped by `python -O`; a self-test must not be.
    if not condition:  # an ordinary call, nothing strips it
        raise SystemExit(f"self-test: {what}")
```

Reach for `unittest.TestCase` instead once the hand-rolled version outgrows
one screen or starts hand-rolling `setUp`-shaped repetition — its assertion
methods are ordinary calls too, and its runner supplies per-check reporting
and exit codes at zero new dependencies. `doctest` covers the pure-function
slice only, never an entry point or anything with side effects.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SOLO-01 | A `--self-test` never signals pass or fail with a bare `assert`. `python -O` and `PYTHONOPTIMIZE` remove every `assert` statement, so the checks vanish and the function falls straight through to its success message: the self-test reports green on a genuinely broken tool. Use an `expect()` helper that raises `SystemExit`, or `unittest.TestCase` assertions, both of which are ordinary calls `-O` cannot strip. A `noqa`/`per-file-ignores` exemption for `S101` on the grounds that "those asserts are the proof" makes this worse, not better — it silences the one lint that would have flagged it. | Two halves. `rg -n --glob '*.py' '^\s+assert ' <dir>` — any hit inside a self-test path is the finding. Decisive: copy the tool, neuter one check it should catch, then `python3 -O <copy> --self-test; echo $?` — a zero exit is the violation. Watched red: the bare-`assert` copy printed `self-test: ok` and exited 0 under `-O` while plain `python3` raised `AssertionError` and exited 1; the `expect()`/`SystemExit` copy of the same regression exited 1 under `-O` | MUST |
| PY-SOLO-02 | An optional dependency is probed with `try: import X` and a hand-rolled fallback sized to the subset actually needed — and the fallback path is exercised on real inputs, distinguishably from a fallback *bug*. A fallback that returns an empty value instead of raising turns the check it feeds into a permanent silent pass, on exactly the machines that lack the dependency. Never let a bug inside the fallback hide in the same broad `except` that catches the missing import. | Hide the dependency and run the whole check suite against a known-bad input; a green result is the violation. Concretely: write a shim module that raises `ImportError` into a scratch directory, then `PYTHONPATH=<shim dir> python3 <tool> --self-test`. Watched red: sabotaging the fallback parser to return `{}` left the self-test green with the dependency installed and made it exit 1 with the dependency hidden — the sabotage is invisible except along the fallback path, which is why the hidden run is the only check that finds it | MUST |
| PY-SOLO-09 | An append-only file written by a per-event hook is trimmed to a declared bound on every run. Nothing else bounds it: the hook fires on every edit for the whole life of a session, and a log growing without limit is read back in full by the same hook on the next event. | `rg -n --pcre2 --glob '*.py' 'open\([^)]*,\s*.a.' <dir>` lists every append-mode sink — these are candidates, not findings. For each, drive the tool past twice its declared bound and count the lines; a count above the bound is the violation. Watched both ways: the audited fleet's tracker held 110 lines after 330 appends (bound 110); the same sink with the trim call removed held 330 | CONSIDER |

## Surviving the Harness

Four properties decide whether this shape survives contact with a real
runner: it never blocks on stdin, it bounds every wait, it tolerates a
consumer that closes early, and it survives being run twice at once. Reading
stdin is a single `read()` that degrades an empty or malformed payload to an
empty result rather than raising — with the honest limit that an unbounded
`read()` still blocks forever on a pipe nothing ever closes, which is
acceptable only because this harness always sends EOF.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SOLO-03 | Every text `open()`, `Path.read_text()` and `Path.write_text()` states `encoding="utf-8"`. The platform default is locale-dependent, so the same file round-trips correctly on the developer's machine and corrupts on a runner whose locale is not UTF-8 — a failure that only appears once a non-ASCII commit message, filename or heading passes through. | `rg -n --pcre2 --glob '*.py' -e '\b(?<!os\.)open\((?![^\n]*encoding=)(?![^\n]*"[rwax]b)' -e '\.read_text\((?![^\n]*encoding=)' -e '\.write_text\((?![^\n]*encoding=)' <dir>` — every hit is a finding. Two known false-positive classes to discard by eye: a call whose arguments wrap onto the next line (the lookahead is single-line), and a binary-mode open the mode-string guard misses. The `os.` lookbehind keeps `os.open`, which takes no encoding, out. Watched red against the audited fleet, isolating both bare-`open` append sites exactly | SHOULD |
| PY-SOLO-07 | A tool of this shape makes no network call. A hook firing on every tool event that reaches the network puts a remote host's availability on the critical path of every edit. Where one is genuinely unavoidable, it is `urllib.request` with an explicit `timeout=` — the default is to wait forever, which is indistinguishable from a hung editor. | `rg -n --glob '*.py' -e urlopen -e 'urllib\.request' -e 'http\.client' -e 'socket\.create_connection' -e 'requests\.' <dir>` — any hit is a finding to justify, not a lint to satisfy. Then `rg -n --pcre2 --glob '*.py' 'urlopen\((?![^\n]*timeout=)' <dir>` — every hit is a violation outright. Both watched red on a planted `urlopen` with no timeout; both silent on the audited fleet | SHOULD |
| PY-SOLO-08 | Concurrent invocations coordinate through one atomic primitive — `os.mkdir`, which raises `FileExistsError` atomically — never a check-then-create pair. Two invocations of a per-event hook overlap routinely, and `if not lock.exists(): lock.mkdir()` has a window between the two calls wide enough to lose. | `rg -nU --pcre2 --glob '*.py' 'exists\(\)[^\n]*\n(?:[^\n]*\n){0,3}?[^\n]*mkdir\(' <dir>` — an existence check within three lines of a `mkdir` is the finding. Decisive: release N workers on one barrier and count acquisitions plus crashes; anything other than exactly one acquisition and zero crashes is the violation. Watched red at N=40 — the check-then-create shape produced 1 acquisition and 2 `FileExistsError` crashes, the atomic `mkdir` 1 and 0 | SHOULD |

## Import and Module Discipline

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SOLO-06 | Module-level work is stdlib imports, constant construction and compiled regexes — no file read, subprocess or network call outside a function body. This code runs on every tool event, so import-time cost is paid on every edit, and an exception at module scope fails before any handler exists. The one sanctioned exception is `sys.path.insert(0, str(Path(__file__).parent))` for N sibling scripts sharing one un-installed module — genuinely correct here, since a relative import cannot resolve under `__main__`, `PYTHONPATH` is invisible at every call site, and installing a package fights the copy-this-directory deployment model. It is bounded, not banned: one unconditional line at module scope, before the import it exists for, and nowhere else. | `python3 -c 'import ast,pathlib,sys; A={"re.compile","str","Path","frozenset","tuple","len"}; v=[(p,n) for p in sorted(pathlib.Path(sys.argv[1]).glob("*.py")) for s in ast.parse(p.read_text(encoding="utf-8")).body if not isinstance(s,(ast.FunctionDef,ast.AsyncFunctionDef,ast.ClassDef,ast.If)) and not ast.unparse(s).startswith("sys.path.insert(") for n in ast.walk(s) if isinstance(n,ast.Call) and ast.unparse(n.func) not in A]; [print(f"VIOLATION: {p}:{n.lineno}: module-level {ast.unparse(n.func)}() runs on every invocation") for p,n in v]; sys.exit(1 if v else 0)' <dir>` — output is the finding; watched red on a planted module-level `json.loads` and `subprocess.run`, silent across the audited fleet. Then `rg -n --glob '*.py' '^\s+sys\.path\.insert' <dir>` — an *indented* insert is one buried in a function or a conditional, and every hit is a finding | CONSIDER |

When the shared module accumulates two concerns used by disjoint subsets of
the scripts that import it, split it into a second sibling file reached by the
same `sys.path` line — not into an installed package. Packaging would buy a
build step and a resolution stage for a deployment model that is "copy this
directory", which is the constraint the whole shape is organised around. The
trigger is the call-site sets barely overlapping, not the line count.

## What Agents Get Wrong Here

1. **`assert` as the pass/fail signal in a `--self-test`**, because it is the
   shortest thing that reads like a test — and it is the one construct an
   interpreter flag deletes.
2. **Adding `# noqa: S101` or a `per-file-ignores` entry** the moment the
   assert lint fires, silencing the exact warning that describes the bug.
3. **A fallback that returns an empty dict or list** where the real parser
   would raise, converting a missing optional dependency into a silent pass.
4. **Testing only the happy path of an optional import** — running with the
   dependency installed, never once without it.
5. **`import requests` / `import yaml` at the top of the file**, breaking the
   zero-dependency property this shape is defined by.
6. **A `#!/usr/bin/env python3` shebang on a file the harness always launches
   with its own interpreter**, and no exec bit to go with it.
7. **Reading a config file or shelling out at module scope**, so the cost and
   the failure land on every editor event rather than inside a function.
8. **`if not lock.exists(): lock.mkdir()`** as the concurrency guard, which
   passes every single-threaded test that will ever be written for it.
9. **`open(path, "a")` with no `encoding=`**, which is correct on the author's
   machine and corrupts on a runner with a different locale.
10. **An append-only `.jsonl` or `.log` with no trim**, invisible until a long
    session makes the hook slow.
