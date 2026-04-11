---
paths:
  - "**/*.py"
  - "**/pyproject.toml"
  - "**/requirements*.txt"
---

# Python Code Quality

Python-specific quality guide (Python 3.13+). Universal design principles
(SOLID, DRY, YAGNI, severity tiers, review checklist) live in `quality-core.md` —
this file covers **Python-specific applications** plus modern type system and
tooling guidance.

Project-independent and shareable.

---

## Anti-Patterns (Python-Specific)

### Block (must fix before merge)

- **Bare `except:` or `except Exception:`** — swallows `KeyboardInterrupt`, `SystemExit`, and hides bugs. Always name the exception(s). Ruff rule: `E722`.
- **`assert` for input validation** in production code — asserts are stripped with `python -O`. Use explicit `if`/`raise` for invariants that must hold at runtime.
- **Mutable default arguments** — `def f(x=[])` creates one shared object across all calls. Use `None` sentinel and set inside the body. Ruff rule: `B006`.
- **Wildcard imports (`from module import *`)** — pollutes namespace, defeats type checkers, breaks refactoring. Ruff rule: `F403`.
- **`dict[str, Any]` or untyped `TypedDict` at public API boundaries** — stringly-typed dictionaries prevent type narrowing. Use `dataclass`, fully-typed `TypedDict`, or `NamedTuple`.
- **Exception chaining dropped** — `raise NewError(...)` inside an `except` block without `from e` loses the original traceback. Always `raise NewError(...) from e` (or `from None` when deliberately hiding). Ruff rule: `B904`.
- **Catching then re-raising without context** — `except Foo: raise Bar()` is the same as above; chain explicitly.
- **`asyncio.gather(*tasks)` for new async code** — use `asyncio.TaskGroup` (3.11+) for structured concurrency with automatic sibling cancellation. `gather()` is legacy.
- **`yield` inside `asyncio.TaskGroup` or `asyncio.timeout` context managers** — PEP 789: suspending execution inside these contexts transfers cancellation to the wrong task.
- **Missing type annotations on public functions** — without annotations, pyright/ty cannot check callers. Ruff rule: `ANN` group (enable selectively).
- **`eval()` / `exec()` on user input** — injection risk.
- **Shadowing built-ins** (`list`, `dict`, `id`, `type`, `input`, `map`, `filter`) — causes subtle bugs and confuses readers.
- **Comparing with `is` for value equality** (except `None`, `True`, `False`) — `is` checks identity, not equality.

### Warn (should fix)

- **`type: ignore` without an error code specifier** — use `type: ignore[specific-error]` to avoid masking real issues.
- **`@runtime_checkable` Protocol used for `isinstance` in hot paths** — runtime-checkable Protocols use `__dict__` introspection per call; O(n) in method count. Reserve for plugin loading.
- **`ABC` where `Protocol` would do** — prefer `Protocol` for interfaces at module boundaries and DI, especially with third-party types. Use `ABC` only when sharing implementation between subclasses.
- **`dataclass` without `slots=True`** (Python 3.10+) — `__dict__` overhead. Add `@dataclass(slots=True)` unless you need dynamic attribute assignment or multiple non-slotted inheritance.
- **`Self` type not used for builder/fluent methods** — `def copy(self) -> "MyClass"` breaks for subclasses. Use `from typing import Self` (3.11+).
- **`match` not used for exhaustive enum dispatch** — `if/elif` chains over `Enum` members should be `match` statements so the type checker can warn on non-exhaustive patterns.
- **`contextvars.ContextVar` not used for request-scoped state in async code** — passing request/trace IDs through every function signature is fragile; `ContextVar` propagates automatically through `asyncio.Task` copies.
- **Legacy generic syntax**: `List[int]` instead of `list[int]` (3.9+); `Optional[X]` instead of `X | None` (3.10+).
- **`**kwargs` "for future flexibility"** — explicit parameters are safer and self-documenting.

---

## Type System (Python 3.13+)

- **`Protocol`** over `ABC` for structural subtyping at module boundaries and DI
- **`Iterable[T]` / `Sequence[T]`** over `list[T]` in function params when only iterating or indexing without mutation
- **`TypedDict` with `Required`/`NotRequired`** (3.11+) instead of `total=False` — mark individual fields, not the whole dict
- **`Self`** (3.11+) for builder/fluent return types
- **`Final`**, **`Literal`**, **`Never`** where they capture intent
- **Modern generics**: `list[int]` not `List[int]` (3.9+); `X | None` not `Optional[X]` (3.10+)
- **`match` statements** (3.10+) with class patterns for exhaustive dispatch

---

## Async Patterns (Python 3.11+)

- **`asyncio.TaskGroup`** over manual `gather` — structured concurrency with automatic sibling cancellation on failure, analogous to Rust's `JoinSet`
- **Cancel safety**: never swallow `asyncio.CancelledError` — always re-raise after cleanup
- **`contextvars`** for per-task state; auto-propagates through `asyncio.Task` copies
- **`async with asyncio.timeout(…)`** (3.11+) over `asyncio.wait_for`
- **Avoid `yield` inside `TaskGroup` / `timeout`** (PEP 789): transfers cancellation to the wrong task

---

## Modern Tooling (2026)

| Tool | Role | Replaces |
|------|------|----------|
| **uv** | Package manager, venv, script runner | pip, virtualenv, poetry, pipx, pyenv |
| **ruff** | Linter + formatter | flake8, black, isort, pylint |
| **pyright** | Type checker (production default) | mypy |
| **ty** | Type checker (Astral, Beta 2026) | mypy/pyright in the long run — 10-60x faster, lacks plugin system |
| **pytest** + **pytest-asyncio** | Testing | unittest |

2026 recommendation: `uv` + `ruff` + `pyright` is the default stack. Evaluate `ty` for editor speed but keep `pyright` in CI until it gains plugin parity (Django, Pydantic, SQLAlchemy stubs). `ruff` replaces `black` for formatting — Black-compatible output, no reason to run Black separately.

---

## Code Review Checklist (Python-Specific)

See `quality-core.md` for the universal review checklist. Python-specific additions:

- [ ] No bare `except:`, no unnamed `except Exception:`
- [ ] No mutable default arguments
- [ ] No `assert` for runtime validation
- [ ] Exception chaining preserved (`from e` or `from None`)
- [ ] Public functions fully type-annotated
- [ ] `TaskGroup` over `gather` for new async code
- [ ] Modern generics (`list[T]`, `X | None`) not legacy (`List[T]`, `Optional[X]`)
- [ ] `Protocol` at module boundaries; `ABC` only for shared implementation
- [ ] `dataclass(slots=True)` where applicable
- [ ] Type checker passes (`pyright --strict` or `ty check`)
- [ ] Linter passes (`ruff check`); formatter applied (`ruff format`)

---

## Sources

Authoritative references used in this rule:

- [PEP 789 — Preventing task-cancellation bugs in async generators](https://peps.python.org/pep-0789/)
- [Python asyncio docs — TaskGroup](https://docs.python.org/3/library/asyncio-task.html)
- [Python typing spec — Protocols](https://typing.python.org/en/latest/spec/protocol.html)
- [Python dataclasses docs](https://docs.python.org/3/library/dataclasses.html)
- [ruff GitHub](https://github.com/astral-sh/ruff)
- [Astral ty announcement](https://astral.sh/blog/ty)
- [Real Python — Python Protocols](https://realpython.com/python-protocol/)
