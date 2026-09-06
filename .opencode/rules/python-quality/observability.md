# Observability

What a run tells the person and the script reading it: which stream carries the
answer, what a failure looks like from outside the process, and what must never
appear in either. Read this before adding a logger, a `print()`, a `--json`
mode, or any output path that carries a value the process did not produce.

Contents: [The Library Logger](#the-library-logger) · [Handler Streams](#handler-streams) ·
[Log Calls, Where They Exist](#log-calls-where-they-exist) ·
[Failure Reporting](#failure-reporting) ·
[Untrusted Text and Secrets](#untrusted-text-and-secrets) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — lazy log arguments, a library that configures nothing, a
  handler that never writes to the payload stream, one sanitizer at the write
  boundary — is general Python practice.
- **The pinned decisions** are this fleet's, already shipped and not
  re-litigated: the batch CLI reports through typed exceptions and exit codes
  rather than the `logging` module, and that is a design, not a gap; redaction
  is call-site-explicit with captured stdout deliberately exempt; structured
  output is stdlib `extra=` plus a `Formatter`, with no logging dependency.

One measurement reshapes this whole file. Fleet-wide, exactly one package
imports `logging` at all — three `getLogger()` sites and fourteen calls, every
one of them `.debug()`. The bot has zero across 93 files; the hooks have zero;
the harnesses have zero. So nothing here is an urgent remediation. Three rules
bind the instant a module grows a logger and cost nothing until then, one
governs a program that never will, and the one real uncovered gap is that no
project strips control bytes from untrusted text before it reaches a terminal.

## The Library Logger

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-OBS-01 | A library configures nothing: `logging.getLogger(__name__)` at module scope, at most one `NullHandler` attached to the package logger in `__init__.py`, and no `basicConfig`, `addHandler`, `dictConfig` or `setLevel` anywhere else in the package. Handler, format and level belong to the application; a library that sets them silently overrides its consumer's setup and their tests. Naming a logger with a string literal instead of `__name__` is a legitimate decision when the intent is the package-root logger — but it is a decision, and it carries an adjacent comment saying so. | `rg -n --glob '**/*.py' -e 'basicConfig\(' -e 'addHandler\(' -e 'dictConfig\(' <libsrc>` — every hit outside a single `NullHandler` attach is the violation; zero output is expected and clean. Then `rg -n --glob '**/*.py' 'getLogger\(\s*[\x27"]' <libsrc>` — each hit is a hardcoded logger name. The grep is the only check available: measured, `LOG002` fires only on `__file__`/`__cached__` and never on a plain string, so selecting it here proves nothing | MUST |

```python
# Wrong — a library module reconfiguring whichever application imported it.
logging.basicConfig(level=logging.DEBUG)
_LOG = logging.getLogger("mypkg")
_LOG.addHandler(logging.StreamHandler())

# Right — the package's entire logging footprint, and nothing else.
_LOG = logging.getLogger(__name__)  # in every module
logging.getLogger("mypkg").addHandler(logging.NullHandler())  # once, in __init__.py
```

## Handler Streams

The tool's payload contract — which stream carries the result — is settled
elsewhere and is not restated here. What belongs to logging is the separate,
later decision of where a *handler* writes, made once in configuration rather
than at any call site, and therefore invisible to a review that reads the call
sites.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-OBS-02 | Every handler the application installs writes to stderr or to a file, never to stdout. `logging.StreamHandler()` already defaults to `sys.stderr`, so the bare constructor is correct and the violation is always an explicit `sys.stdout` — which is why it reads as deliberate to a reviewer and is never questioned. A handler pointed at stdout interleaves diagnostics into the payload byte stream something downstream parses, and no amount of discipline at the call sites prevents it: the call site says `_LOG.debug(...)`, the destination was chosen in a different file. The same holds for a hook whose protocol owns stdout — logging there needs its own stderr or file handler, or it corrupts the JSON the harness reads. | `rg -n --glob '**/*.py' -e 'StreamHandler\(sys\.stdout' -e 'stream=sys\.stdout' <src>` — each hit is a violation; zero output is expected and clean. The escapes are load-bearing: an unescaped `(` makes this a regex parse error, which exits 2 with no output and reads as a clean tree. Watched red on a planted `basicConfig(stream=sys.stdout)` and a planted `StreamHandler(sys.stdout)`, and silent on the bare `StreamHandler()` that is already correct | MUST |

## Log Calls, Where They Exist

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-OBS-03 | No f-string, `.format()`, `%`-operator or concatenation as a log message — pass `%`-style arguments, or `extra=` for fields a handler should key on. Eager formatting runs even when the level discards the record, and it destroys the stable message template that a JSON handler or a log aggregator groups by, turning "N packages verified" into N unique strings. This binds the moment a module imports `logging` and not before; the fleet-wide cost of committing to it today is zero, which is the cheapest moment there will ever be. | `ruff check --select G001,G002,G003,G004 --no-cache --isolated <src>` — **but only after watching it flag a planted `<your_logger>.info(f"x {value}")` on your own logger variable.** Measured, ruff's G family recognises `logger`, `log`, `LOG`, `LOGGER` and `_logger`, and does *not* recognise `_log` or `_LOG` — which is what the fleet's only logging package names both of its loggers, so the family reports a clean tree today while seeing none of its fourteen calls. A pass on the plant means the codes are inert and the green run is worthless | MUST |

**Two smells no lint reaches**, both of which need the frame read rather than
grepped. *Double-reporting*: a `logger.error(...)` or `.exception(...)`
immediately before a `raise` in the same function, where the caller's handler
will report the same failure again — log at the frame that stops the
propagation, not at every frame that observes it. *Mislabelled recovery*: a
`logger.warning(...)` inside an `except` where the code after the block
continues as though the operation succeeded, but a value it was supposed to
produce is now missing or wrong. The stdlib defines WARNING as "still working
as expected"; if the surrounding code can no longer deliver its contract, that
is an ERROR reported as a WARNING to keep the log looking calm.

## Failure Reporting

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-OBS-04 | A single-shot batch CLI's machine-readable contract is its exit code plus its structured stdout — never a log line a caller greps. Four parts, all required: one typed exception per failure class a caller would act on differently, each mapped to exactly one exit code; every raise carrying the values that made it fail, inline in the message, at the point of failure; exactly one top-level handler that maps modeled errors to stderr plus their exit code; and anything unmodeled left to propagate as a full traceback, which is strictly more diagnostic than a hand-written handler because nobody had to anticipate it. A program built this way is fully observable with no `logging` at all — adding it would add a channel and remove nothing. | `rg -c --glob '**/*.py' 'except \w*Error' <entrypoint>` — a count above one is the violation, and a top-level `except Exception` is the violation at any count because it swallows the bugs the traceback exists for. Then `rg -n --glob '**/*.py' 'raise \w*Error\(\s*"[^{"]*"\s*\)' <src>` — each hit raises a constant message carrying none of the values that failed | MUST |

## Untrusted Text and Secrets

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-OBS-05 | One function strips C0 control and escape bytes from text the process did not produce, and every terminal-facing write goes through it. A package name, a PR-supplied path, or an upstream error reflected into a message can carry ANSI that moves the cursor, clears the screen, rewrites the window title or renders a fake prompt (CWE-150) — and the same unstripped newlines forge log lines for whoever reads the log afterwards (CWE-117). No project in this fleet strips control characters anywhere today; a maintainer running the validator locally is a real TTY-attached path, and this is the one genuinely uncovered gap here. No grep decides it — whether a string is attacker-influenced is not syntactic. | A runtime assertion against the project's own sink: feed it `"\x1b]0;pwned\x07\x1b[2Jinnocent"`, capture the stream, and assert `"\x1b" not in captured`. Watched red against a bare `print(text)` sink — `VIOLATION: raw ESC reached stdout: '\x1b]0;pwned\x07\x1b[2Jinnocent\n'` — and green against a `"".join(c for c in text if c.isprintable() or c in "\n\t")` sink | SHOULD |
| PY-OBS-06 | Redaction sits between a record and its handlers — a `logging.Filter`, or the same explicit redactor already applied to argv and captured stderr — and captured stdout is exempt by design, because substituting inside the JSON payload a caller is about to parse corrupts the document. A third-party library's own logger is never turned up where its output is persisted: `httpx` has emitted `https://user:pass@host` at request level and `urllib3.connectionpool` has emitted presigned URLs carrying their auth parameters, and neither passes through this project's redactor (CWE-532). None of this needs a dependency — `extra=` plus a `Formatter` subclass plus a `Filter` is a complete answer, which is why a zero-runtime-dependency SDK already ships it; `structlog` and `rich` buy ergonomics, not capability. | `rg -n --glob '**/*.py' -e 'getLogger\("httpx' -e 'getLogger\("httpcore' -e 'getLogger\("urllib3' <src>` — each hit that lowers a third-party logger's level rather than raising it is the violation, and zero output is expected and clean. Then plant a token-shaped value through the log path and assert the captured sink shows the redaction marker rather than the value | MUST |

```python
class RedactingFilter(logging.Filter):
    """The one net between every record and every handler. Stdlib only."""

    def filter(self, record: logging.LogRecord) -> bool:
        record.msg = _redact(str(record.msg))
        if isinstance(record.args, tuple):
            record.args = tuple(_redact(str(a)) for a in record.args)
        return True
```

## What Agents Get Wrong Here

1. Adds `logging.basicConfig(...)` inside a library module, because every
   tutorial opens with it. It reconfigures the root logger of whatever
   application imported the package (PY-OBS-01).
2. Writes `logger.info(f"processed {n} packages")`. It is the natural way to
   build a string in modern Python and it is the one shape the logging module
   is designed around avoiding (PY-OBS-03).
3. Selects `G004`, sees a clean run, and reports the codebase compliant —
   without ever checking that the lint can see the project's logger variable
   at all (PY-OBS-03).
4. Writes `logging.basicConfig(stream=sys.stdout)` when setting up an entry
   point, because stdout is what every "getting started with logging" snippet
   shows and the parameter looks like a formatting choice (PY-OBS-02).
5. Adds a `StreamHandler(sys.stdout)` to make log output visible while
   debugging, then leaves it in — the bare `StreamHandler()` was already
   writing to stderr and already visible (PY-OBS-02).
6. Asked to "add observability" to a batch CLI that already has typed
   exceptions and exit codes, bolts the `logging` module on top — a second
   channel reporting the same failures, with no verbosity control and nobody
   reading it (PY-OBS-04).
7. Catches `Exception` at the top level to produce a tidy error message,
   converting every unanticipated bug into a one-line summary with the
   traceback discarded (PY-OBS-04).
8. Enables `httpx`'s or `urllib3`'s DEBUG logger to diagnose a connection
   problem, in a process that also holds registry credentials (PY-OBS-06).
9. Prints a registry-returned or PR-supplied string straight to the terminal.
   Nothing at the call site marks it as foreign text (PY-OBS-05).
10. Reaches for `structlog` when asked for structured logs, in a package whose
    entire selling point is zero runtime dependencies (PY-OBS-06).

## Sources

- [Logging HOWTO](https://docs.python.org/3/howto/logging.html) — library-versus-application configuration, the `__name__` convention, the level table, and the lazy-formatting rationale
- [`logging` reference](https://docs.python.org/3/library/logging.html) — the `extra=` contract and its reserved `LogRecord` attribute names, and `Filter.filter()` including record replacement from 3.12
- [ruff — `logging-f-string` (G004)](https://docs.astral.sh/ruff/rules/logging-f-string/) — the eager-formatting and `extra=` rationale, and the `lint.logger-objects` setting
- [ruff — `print` (T201)](https://docs.astral.sh/ruff/rules/print/) — why an unconfigurable write bypasses every logging control
- [clig.dev](https://clig.dev/#the-basics) — stdout for the answer, stderr for everything else, and the TTY heuristic
- [12factor.net/logs](https://12factor.net/logs) — the competing model, cited to bound where it applies: a service with no answer to return, not a CLI with one
- [CWE-532](https://cwe.mitre.org/data/definitions/532.html) and [CWE-150](https://cwe.mitre.org/data/definitions/150.html) — secrets in logs, and control-sequence neutralization
- [encode/httpx#2765](https://github.com/encode/httpx/discussions/2765) — a widely used HTTP client leaking credentials through its own request logging
