# Public API Surface

What a Python package promises to the outside and cannot quietly take back:
which names are importable, which signatures can grow, which exceptions a
caller may catch. Loads with the Python quality rule on any diff touching an
`__init__.py`, an `__all__` list, a non-underscore `def`/`class`, an exception
hierarchy, or a `warnings.warn` call.

Contents: [Declaring What Is Public](#declaring-what-is-public) ·
[Evolving It Without Breaking Callers](#evolving-it-without-breaking-callers) ·
[Shaping a Public Signature](#shaping-a-public-signature) ·
[Errors and Docstrings as Contract](#errors-and-docstrings-as-contract) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

**Library or application — this distinction decides half the rules below.** A
published library's public surface is its importable symbols, and every
consumer holds it to that. An application's public surface is its argv, its
exit codes and its files; nobody imports it, so an `__all__` audit or a
`griffe check` against it proves nothing. Each rule cell opens by naming which
shape it binds. Where the right answer genuinely differs between the two —
PY-SURF-07 — the rule says so rather than picking one and calling it style.

**The mechanism** is portable: the typing spec's re-export rule, one
deprecation gate, keyword-only growth room, a single exception root.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Declaring What Is Public

Three definitions of "public" coexist in every typed package — the
no-underscore convention, `__all__`, and what is actually bound in the module
namespace. They are only equal by construction, never by accident, and a type
checker follows the third one. The import form alone decides it:

```python
from .core import Ocx  # NOT re-exported — imported names are private by default
from .core import Ocx as Ocx  # re-exported — the redundant alias is the signal
from .core import Ocx as _ocx  # private, and says so at a glance

__all__ = ["Ocx"]  # re-exports, and overrides every rule above
```

This is the typing spec's rule, identical for `.py` and `.pyi` — not a
stub-only convention, as it is commonly mistaken for. Pyright enforces the
private-by-default half through `reportPrivateImportUsage`, which defaults to
**error** in basic, standard and strict alike: a leaked name is a diagnostic
for every consumer, not just the careful ones.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SURF-01 | **Library.** Every public module declares `__all__`, and the three definitions of public agree: a bound non-underscore name is either listed in `__all__` or imported with a redundant `as` alias, and every `__all__` entry is actually bound. An implementation-detail import that skips this becomes a name consumers can import and you can never remove — `from importlib.metadata import PackageNotFoundError` in an `__init__.py` re-exports the stdlib's exception as part of your API. Under pyright, a plain `from .x import Y` re-exports **only** via `__all__`; `from .x import Y as Y` (the redundant alias) is the other accepted form, and `from .x import Y as _y` is the correct spelling for "private". | Per public module: `python3 -c 'import importlib,sys; m=importlib.import_module(sys.argv[1]); d=set(getattr(m,"__all__",())); v=sorted({n for n in vars(m) if not n.startswith("_")}-d-{"annotations"}); u=sorted(d-set(vars(m))); [print("VIOLATION: public but absent from __all__:",n) for n in v]; [print("VIOLATION: in __all__ but not bound:",n) for n in u]; sys.exit(1 if v or u else 0)' <pkg>` — output is the finding, silence is the pass, exit 1 gates CI. `annotations` is excluded by name because `from __future__ import annotations` binds it in every module that uses it | MUST |
| PY-SURF-02 | **Library.** `griffe check <pkg> -s src` runs in CI **beside** PY-SURF-01, never instead of it. The two cover disjoint failure modes: griffe reads actual module reachability, so it reports a symbol that was deleted outright and stays silent when a name is dropped from `__all__` while remaining importable — which is the exact regression PY-SURF-01 exists to catch. Treating either as sufficient leaves a real break shipping green. | `griffe check <pkg> -s src -f verbose` — any output, and a non-zero exit, is the finding; silence is the pass. Watched both ways on a two-symbol package: deleting the symbol printed `Public object was removed` and exited 1; dropping only its `__all__` entry printed nothing and exited 0 while PY-SURF-01 named it | MUST |

## Evolving It Without Breaking Callers

SemVer 0.y.z promises nothing — "anything MAY change at any time" — which is
precisely why the gate has to be mechanical rather than stated. The failure
mode has real precedent: a widely used HTTP library shipped a documented
"deprecate in one release, remove in the next" policy, broke callers in a
point release anyway, and its maintainer's own post-mortem was that the policy
"wasn't cautious or clearly communicated enough". The intent was there; no job
blocked on it. Write the gate before there is anything to deprecate — the
first release is the cheapest moment, because there are no removals yet to
grandfather in.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SURF-03 | **Library.** A public symbol leaves the surface only after shipping deprecated in at least one released version. The gate is a job, not a paragraph in CONTRIBUTING — an intended "deprecate in 0.y, remove in 0.z" window that nothing blocks on is the documented way real projects break callers in a point release. | `griffe check <pkg> -s src --against <last tag> -f verbose` — every `Public object was removed` line names a symbol. For each, `git grep -n -e deprecated <last tag> -- src` ; an empty result for a removed name is the finding (the removal shipped with no prior deprecation). Watched red on a tagged package: griffe named the removed symbol, `git grep` returned nothing | SHOULD |
| PY-SURF-04 | **Library.** A deprecated symbol carries PEP 702 `@deprecated`, and every `warnings.warn(..., DeprecationWarning)` passes an explicit `stacklevel=`. The default `stacklevel=1` blames the `warn()` line inside your own library, which tells the caller nothing about their code; the correct number is a property of the call chain, not of the warning — one helper frame between the public entry point and `warn()` makes it 3, not the textbook 2. `@deprecated` is the typed half and pyright understands it, but `reportDeprecated` is `none` in basic and standard mode and only `error` in strict, so the marker alone is invisible to most consumers: ship both. | `python3 -c 'import pathlib,sys; v=[(p,i+1) for p in pathlib.Path(sys.argv[1]).rglob("*.py") for t in [p.read_text(encoding="utf-8").splitlines()] for i,l in enumerate(t) if "warnings.warn(" in l and "stacklevel=" not in "".join(t[i:i+6])]; [print(f"VIOLATION: {p}:{n}: warnings.warn(...) with no explicit stacklevel=") for p,n in v]; sys.exit(1 if v else 0)' src` — output is the finding. Then `rg --files-without-match -e typeCheckingMode -e strict pyproject.toml` — the file being **listed** is the finding: no type-checking mode is configured at all, so `reportDeprecated` can never fire | SHOULD |

## Shaping a Public Signature

Every optional parameter is a promise about insertion order. Keyword-only is
how a signature grows for ten releases without a single breaking change, and
the bare `*` costs one character to add on the day the function is written
against zero call sites — and is a breaking change to add later.

The other half is the sentinel, where the reflex idiom cannot be typed at all:

```python
class _Unset(Enum):  # private, single member
    TOKEN = "unset"


UNSET: Final = _Unset.TOKEN  # public, so wrappers can pass it on
type MaybeTimeout = float | Literal[_Unset.TOKEN] | None  # exactly three states

SENTINEL = object()  # the reflex: annotates to `T | object`
```

`Literal[]` accepts enum members, so `Literal[_Unset.TOKEN]` denotes exactly
one value. A bare `object()` denotes every object, so the checker cannot
narrow `is UNSET` and the third state exists only in the author's head.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SURF-05 | **Both.** At most one optional parameter is positional; everything after it sits behind a bare `*`. With two or more optional positionals, inserting a parameter next release silently reorders every call site that passed them positionally, and no type checker sees the caller. Booleans are never positional at all — `deploy(target, True)` reads as nothing at the call site. Selecting `FBT001`/`FBT002` costs zero today on both audited codebases (5 hits, all on underscore-prefixed private helpers); this is a do-not-regress rule, not a cleanup. | `python3 -c 'import ast,pathlib,sys; v=[(p,f) for p in pathlib.Path(sys.argv[1]).rglob("*.py") for f in ast.walk(ast.parse(p.read_text(encoding="utf-8"))) if isinstance(f,(ast.FunctionDef,ast.AsyncFunctionDef)) and not f.name.startswith("_") and len(f.args.defaults)>1]; [print(f"VIOLATION: {p}:{f.lineno}: {f.name}() takes {len(f.args.defaults)} optional parameters positionally") for p,f in v]; sys.exit(1 if v else 0)' src` — output is the finding; two live hits in the audited SDK, clean on the audited application. Separately `ruff check --select FBT001,FBT002 src` — every hit on a non-underscore `def` is a finding | SHOULD |
| PY-SURF-06 | **Both.** A "not given" marker distinct from `None` is a private single-member `Enum`, a `Final` alias to its member, and `Literal[_Unset.TOKEN]` in the public type alias — never a bare `object()`. `SENTINEL = object()` cannot be spelled in a type expression: the parameter degrades to `T \| object`, which narrows to nothing, so every caller and the checker lose the third state the sentinel was introduced to carry. | `rg -n --glob '*.py' '=\s*object\(\)' src` — any hit is the finding; nothing but a sentinel binds a bare `object()` to a name. Watched red on a planted `SENTINEL = object()`, silent on the audited SDK, whose `_Unset`/`UNSET`/`Literal[_Unset.TOKEN]` trio is the reference form | SHOULD |

## Errors and Docstrings as Contract

The exit-code-to-exception mapping is the place two correct answers look
identical on the page and are not interchangeable. Which one is right is
decided by the direction the code has to travel, and nothing else:

```python
# Library: an external process hands back a bare int. The reverse lookup has to live
# outside the classes, because a class cannot introspect which code it answers to.
_EXIT_CODE_ERRORS: dict[ExitCode, type[PkgProcessError]] = {ExitCode.USAGE: UsageError, ...}

# Application: the code always knows which exception it is about to raise.
# The forward direction is all it needs, and a class attribute cannot drift from a table.
class AnomalyError(AppError):
    _exit_code = ExitCode.ANOMALY
```

Port the dict onto the application and it buys an unused indirection that can
fall out of sync with the class list. Port the class attribute onto the
library and the code that classifies a raw exit status has nowhere to look it
up, so it grows an `if`/`elif` chain that is the same table under a worse name.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SURF-07 | **Both, in two different shapes.** Exactly one class in the package subclasses `Exception`/`BaseException` directly; every other exception routes through it, so `except <PkgError>` is a complete catch-all. A stray `class NetworkError(Exception)` bypasses it silently and escapes every caller's handler. The exit-code mapping then follows the direction the code actually needs, and the two are not interchangeable: a **library** that receives a raw status from an external process needs a reverse lookup (code → class) living outside the classes, because a class cannot introspect which code it answers to — an `ExitCode` enum plus an explicit mapping dict, kept complete. An **application** always knows which exception it is about to raise, so the forward direction is all it needs — a `_exit_code` class attribute, no external table, no drift. Porting either shape onto the other adds an unused indirection or forces an `if`/`elif` chain wearing a different name. | `python3 -c 'import ast,pathlib,sys; v=[(p,c,b.id) for p in pathlib.Path(sys.argv[1]).rglob("*.py") for c in ast.walk(ast.parse(p.read_text(encoding="utf-8"))) if isinstance(c,ast.ClassDef) and c.name!=sys.argv[2] for b in c.bases if isinstance(b,ast.Name) and b.id in ("Exception","BaseException")]; [print(f"VIOLATION: {p}:{c.lineno}: {c.name} subclasses {n} directly, bypassing {sys.argv[2]}") for p,c,n in v]; sys.exit(1 if v else 0)' src <RootError>` — output is the finding; watched red on a planted second root, silent on both audited codebases. Where a reverse mapping exists, a second check walks the exit-code enum and prints every member with no mapped subclass; watched red by removing one entry | MUST |
| PY-SURF-08 | **Library.** A public callable documents every exception it can propagate, including ones raised by a helper it calls. Select `DOC501` **alone**, not the `DOC` family: `DOC502` misreads a correct generic re-raise out of a broad `except` as extraneous (40 of 122 hits on the audited SDK), and `DOC201` fights Google convention's permitted `Returns:` omission (75 more) — selecting the family fails CI on accurate documentation, which teaches the next author to suppress the whole thing. | `ruff check --preview --select DOC501 src` — every hit is the finding; the `--preview` flag is required or the selector is silently inert ("Selection `DOC` has no effect"), which reads as a pass. Watched red on a planted undocumented `raise ValueError`; 7 live hits on the audited SDK, one hand-confirmed as a genuine undocumented propagation | SHOULD |

## What Agents Get Wrong Here

1. **Adding an import to `__init__.py` to fix a name error**, with no `as`
   alias and no `__all__` entry — the shortest edit that makes the traceback
   go away, and it publishes a stdlib symbol as your API forever.
2. **Appending a new optional parameter to an existing public function**
   because it is a one-line diff, instead of putting it behind the `*`.
3. **A positional boolean flag** rather than a second function or an enum,
   when the two behaviours share almost no body.
4. **`SENTINEL = object()`** for "not given" — the idiom every corpus is full
   of, and untypeable, so the annotation quietly widens to `object`.
5. **A new exception subclassing `Exception` directly** because that is what
   the tutorial shows, escaping the package's own catch-all.
6. **`warnings.warn(msg, DeprecationWarning)` with no `stacklevel=`**, which
   points the caller at a line inside your library.
7. **Deleting a public symbol in the same PR that stops using it**, with no
   deprecation release in between — the diff looks like tidying.
8. **Treating `griffe check` as the whole surface gate**, so an `__all__`-only
   regression ships green.
9. **Copying the exit-code mapping dict from a library into an application**
   (or the class attribute the other way) because the two look alike on the
   page, buying an indirection nothing reads.
10. **Turning on the whole `DOC` family after one useful hit**, then adding a
    blanket suppression when 122 findings land on correct docstrings.
