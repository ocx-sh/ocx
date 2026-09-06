# Async and Cancellation

asyncio rules for the one async codebase in this fleet: blocking calls inside
the loop, timeout scopes, structured spawning, cancellation handlers, and
event-loop ownership. Loads when editing any file containing `async def`,
`await`, or an `asyncio.` call.

Contents: [Scope](#scope-pinned) ·
[Blocking and Deadlines](#blocking-and-deadlines) ·
[Structure and Cancellation](#structure-and-cancellation) ·
[Entry Points and Yield Points](#entry-points-and-yield-points) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

## Scope (pinned)

- **`ocx-sdk-python` is the only asyncio codebase here** — 17 `async def`, 27
  `await`. `index/bot` has zero of either, confirmed by full-tree grep. None of
  this applies to it, and none of it gets adopted preemptively: the event that
  adopts this file is a first `async def`, not a refactor someone scheduled.
- **Most of it is already true, and stays true.** `TaskGroup` is the only spawn
  primitive in `src/`, `asyncio.gather(` has zero call sites, `RUF006` is clean,
  and no blocking primitive appears inside an `async def`. Those rules exist to
  stop a regression — the cheaper half of the job, and the half a green CI run
  will not do for you.
- **This surface is unguarded today.** `ASYNC` is selected in no ruff config in
  the fleet, so every blocking-call rule below currently fails open.
  PY-ASYNC-01 is the line that turns it on.

## Blocking and Deadlines

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-ASYNC-01 | Never call a blocking primitive from inside an `async def` — `time.sleep`, `subprocess.run`/`.wait()`/`.communicate()`, `open()`, `Path.read_*`/`write_*`, `input()`, a sync HTTP client. Each stalls every other task on the loop for its full duration, and the failure shows up as latency under load rather than as a test failure. Select `ASYNC` in ruff for any tree containing `async def`. Two blind spots need a read rather than a lint, because they have no lint signature at all: CPU-bound work (hashing, parsing, compression) and a slow synchronous logging handler block just as hard. | `ruff check --select ASYNC --no-fix src/` — every finding is the violation, except an `ASYNC109` site already resolved under PY-ASYNC-06. Six findings today, all `ASYNC109`; the blocking families `ASYNC210/212/220/221/222/230/240/250/251` are all clean and stay that way | MUST |
| PY-ASYNC-06 | A public async entry point may take `timeout:` as its contract, but exactly one `asyncio.timeout()` scope wrapping the whole operation enforces it. Never re-derive a shrinking budget at each nested layer: every layer resets its own clock, so a per-call timeout threaded downward bounds each call and no total — a server dribbling one byte per interval keeps a "10-second" call alive forever. Prefer the scope to `asyncio.wait_for()`, which since 3.12 is implemented on top of it and only ever wraps a single awaitable. | `ASYNC109` flags the public-parameter shape; the 6 hits in the SDK all resolve to one internal scope. Suppress per site with `# noqa: ASYNC109` naming that scope, never with a config-level ignore that would also hide the case where no scope exists: `rg -n --type py '# noqa: ASYNC109\s*$' src/` — a bare suppression with no rationale is the violation, and so is any `ASYNC109` in `pyproject.toml`. `rg -n --type py 'asyncio\.wait_for\(' src/` — each hit is a candidate for a scope | SHOULD |

PY-ASYNC-01 is a config change, not a review habit. Turning the family on costs
two edits — one in the manifest, one per pre-triaged `ASYNC109` site:

```toml
[tool.ruff.lint]
select = ["E", "W", "F", "I", "B", "UP", "ANN", "RUF", "D", "ASYNC"]
```

```python
async def run_command_async(
    argv: Sequence[str],
    *,
    timeout: float | None = None,  # noqa: ASYNC109 - one asyncio.timeout() scope enforces it
) -> CommandResult: ...
```

The suppression goes on the parameter line, names the scope, and is never
hoisted into `ignore` — a blanket entry would also hide the sites where the
parameter is threaded downward and no scope exists at all.

## Structure and Cancellation

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-ASYNC-02 | Every `asyncio.create_task()`/`ensure_future()` result is bound — to a variable, to a set held for the task's whole lifetime, or to the `TaskGroup` that owns it. The loop keeps only a **weak** reference, so an unbound task can be collected mid-execution and its work simply never finishes, with nothing raised anywhere. A bare set fixes the lifetime but not the error path: nothing ever retrieves the exception, which is why a `TaskGroup` is the form that does both. | `ruff check --select RUF006 --no-fix src/` — any finding is the violation; clean today, and enforced now since `RUF` is already selected. `TaskGroup.create_task` is correctly exempt, the group holds the reference. RUF006 is syntactic: a task bound and then dropped, or a set cleared before its tasks finish, needs a read | SHOULD |
| PY-ASYNC-03 | Per PY-CORE-01, only a clause catching `BaseException` sees a cancellation at all — `except asyncio.CancelledError`, `except BaseException`, bare `except:`. Each of those re-raises in its own body after its cleanup, or calls `.uncancel()` where absorbing the request is deliberate. Cancellation is a request, not an error to absorb: swallowing one turns "stop now" into a normal return that the caller reads as success. | The AST check below prints one line per offending handler; empty output is a pass. No lint covers this: `B036` fires on `except BaseException` without a re-raise, and never on `except asyncio.CancelledError` | MUST |
| PY-ASYNC-04 | Spawn concurrent children only inside an `asyncio.TaskGroup`. Never `asyncio.gather()`: at its default `return_exceptions=False` the first exception propagates and the siblings are **not** cancelled — they keep running with no scope watching them, past the `with` block that owned their resources. `return_exceptions=True` trades that for silence, returning failures as ordinary list elements nobody has to inspect. | `rg -n --type py 'asyncio\.gather\(' src/` — any line printed is the violation; zero call sites today, and the count staying at zero is the whole rule | SHOULD |
| PY-ASYNC-05 | A `CancelledError` handler contains no `await` beyond what releasing the resource strictly requires, and none at all when the release is synchronous — awaiting here delays the stop the caller already asked for. A task that spawned a child process must signal it (`terminate`/`kill`/`killpg`) inside that handler: CPython does **not** kill the child when the awaiting task is cancelled (gh-88050), so the child survives as an orphan. Cleanup that genuinely must be async belongs on the `TimeoutError`/`BaseException` path, inside its own bounded `asyncio.timeout(grace)`. | The AST check below prints every `await` inside a `CancelledError` handler, and every such handler in a module that spawns a child without signalling it. Zero lines against `ocx-sdk-python/src` today | MUST |

The check behind PY-ASYNC-03 and PY-ASYNC-05. Empty output is a pass; every
line printed is one handler to fix:

```python
import ast, pathlib, sys

for f in sorted(pathlib.Path(sys.argv[1]).rglob("*.py")):
    text = f.read_text()
    for h in (n for n in ast.walk(ast.parse(text)) if isinstance(n, ast.ExceptHandler)):
        caught = ast.unparse(h.type) if h.type else "bare except"
        if h.type and "CancelledError" not in caught and "BaseException" not in caught:
            continue
        body = ast.unparse(h)
        if not any(isinstance(n, ast.Raise) for n in ast.walk(h)) and "uncancel()" not in body:
            print(f"{f}:{h.lineno}: except {caught} neither re-raises nor uncancels")
        if "CancelledError" not in caught:
            continue
        for n in ast.walk(h):
            if isinstance(n, ast.Await):
                print(f"{f}:{n.lineno}: await inside except {caught}")
        if "create_subprocess_" in text and "terminate" not in body and "kill" not in body:
            print(f"{f}:{h.lineno}: cancellation handler in a subprocess module signals no child")
```

The handler shape all of this is aiming at — a synchronous signal, a note, a
re-raise, and not one `await` on the path out:

```python
err = bytearray()
try:
    async with asyncio.timeout(timeout):
        await _drain(proc, err)
except asyncio.CancelledError as cancelled:
    # gh-88050: CPython leaves the child running when the awaiting task is
    # cancelled. No grace wait and no awaits at all here — awaiting inside a
    # cancellation handler is how a caller that asked to stop ends up hanging.
    _terminate_group(proc)
    if err:
        cancelled.add_note(f"partial stderr before cancellation: {err!r}")
    raise
```

## Entry Points and Yield Points

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-ASYNC-07 | `asyncio.run()` appears once, at the process entry point. Library code never creates, sets, or closes an event loop on its caller's behalf — no `new_event_loop`, no `set_event_loop`, no `loop.close()` — and nothing calls `asyncio.get_event_loop()`; inside a coroutine or callback the answer is `get_running_loop()`. A second `asyncio.run()` reached from inside a running loop raises `RuntimeError`, and as of 3.14 `get_event_loop()` raises rather than quietly manufacturing a loop that nothing runs. | `rg -n --type py -e 'get_event_loop\(' -e 'new_event_loop\(' -e 'set_event_loop\(' -e 'loop\.close\(' src/` — any line printed is the violation; zero today. `rg -n --type py 'asyncio\.run\(' src/` — more than one hit, or one outside the entry point, is the violation | SHOULD |
| PY-ASYNC-08 | Do not treat `await asyncio.sleep(0)` as a guaranteed yield point. It is a scheduler implementation detail, not a language guarantee, and it does not generalise across loop implementations. Where the code must wait for something, wait on an `asyncio.Event`; keep `sleep(0)` to places where any scheduler tick will do, such as letting a task reach its first await in a test. | `rg -n --type py 'asyncio\.sleep\(0\)' src/` — each hit needs a comment naming why a scheduler tick suffices; zero in `src/` today. Nothing lints it: `ASYNC115` is trio/anyio-only and does not fire on the asyncio spelling, and `ASYNC110` catches only the `while …: await sleep(…)` busy-wait shape | SHOULD |

## What Agents Get Wrong Here

1. **`asyncio.get_event_loop()` inside a coroutine.** Pre-3.10 examples
   dominate the corpus, and the call still "works" outside a running loop, so a
   quick manual test does not catch it.
2. **`gather()` where a `TaskGroup` is meant.** `gather` predates it by a
   decade; the two read as interchangeable and differ exactly where it matters
   — what happens to the siblings of a task that failed.
3. **A defensive `except BaseException:` added for symmetry, with no `raise`**
   — the one edit that converts a working cancellation into a silent success.
4. **`wait_for()` reached for reflexively**, because every pre-2022 tutorial
   uses it, where the call site actually wants a scope over several statements.
5. **Porting a sync function by adding `async` to the signature** and leaving
   the blocking body in place. It compiles, the tests pass, and the loop
   starves under load. The single most likely defect here.
6. **`asyncio.run()` inside a helper already running under `asyncio.run()`** —
   a convenience wrapper written without tracking whether the call site is
   already inside a loop. `RuntimeError`, far from the wrapper that caused it.
7. **A coroutine constructed and never awaited** (`result = fetch()`). Nothing
   raises at the call site; CPython emits a `RuntimeWarning` at GC time.
   `filterwarnings = ["error::RuntimeWarning"]` turns that into a test failure.
8. **Awaiting inside a cancellation handler "to clean up properly"** — the one
   place in the codebase where more awaiting is strictly worse.
9. **Assuming a cancelled task takes its subprocess with it.** It does not, and
   has not since the bug was filed in 2021.
10. **`await asyncio.sleep(0)` written as a fairness guarantee** in a polling
    loop, where an `Event` is what the code actually wanted.
