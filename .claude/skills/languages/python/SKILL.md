---
name: python
description: Write Python code following best practices. Use when developing Python applications. Covers design principles, type hints, async, and modern tooling.
---

# Python Development

Universal design principles (SOLID, DRY, YAGNI) are defined in `.claude/rules/code-quality.md`. This section covers **Python-specific applications**.

## Design Principles in Python

### SOLID
- **SRP**: One class per concern. Split large classes into focused modules — use composition over inheritance.
- **OCP**: Use `Protocol` (structural subtyping) or ABC to define extension points. Add new behavior via new implementations, not by modifying existing functions.
- **LSP**: Every `Protocol`/ABC implementation must honor the documented contract — no `NotImplementedError` in methods that callers expect to work.
- **ISP**: Define narrow `Protocol` classes. Accept `Iterable[T]` not `list[T]` when only iterating. Accept `Sequence[T]` not `MutableSequence[T]` when not mutating.
- **DIP**: Type-hint against protocols/ABCs, inject implementations via constructor parameters.

### DRY
- **Protocols**: zero-cost DRY via structural typing — same interface for multiple implementations
- **Decorators**: eliminate cross-cutting boilerplate (logging, retry, timing)
- **`dataclasses`/`attrs`**: reduce boilerplate for data types
- **Don't DRY test code** — explicit assertions are clearer than clever fixtures

### YAGNI
- Don't create ABCs/Protocols until a second implementation exists
- Don't add `**kwargs` "for future flexibility" — explicit parameters are safer
- Don't write custom metaclasses when a decorator suffices

## Anti-Patterns (Python-Specific)

### Block
- **Bare `except:`** — always catch specific exceptions. `except Exception:` at minimum.
- **Mutable default arguments** — `def f(items=[])` shares state across calls. Use `None` sentinel.
- **`assert` for validation** — stripped with `-O`. Use `if`/`raise` for runtime checks.

### Warn
- **`type: ignore` without code** — use `type: ignore[specific-error]` to avoid masking real issues
- **Wildcard imports** (`from module import *`) — pollutes namespace, breaks tooling
- **Stringly-typed dicts** where `TypedDict`, `dataclass`, or `NamedTuple` would provide type safety

## Project Setup

```bash
# Create project with uv
uv init my-project
cd my-project

# Add dependencies
uv add litestar
uv add --dev pytest ruff mypy
```

### pyproject.toml
```toml
[project]
name = "my-project"
version = "0.1.0"
requires-python = ">=3.13"
dependencies = ["litestar>=2.0"]

[tool.ruff]
line-length = 88
target-version = "py313"

[tool.mypy]
strict = true
python_version = "3.13"
```

## Type Hints

```python
from typing import TypeVar, Generic
from collections.abc import Sequence

T = TypeVar('T')

class Repository(Generic[T]):
    async def find_by_id(self, id: str) -> T | None:
        ...

    async def save(self, entity: T) -> T:
        ...

def process_items(items: Sequence[str]) -> list[str]:
    return [item.upper() for item in items]
```

## Async Patterns

```python
import asyncio
from collections.abc import AsyncIterator

async def fetch_all(urls: list[str]) -> list[Response]:
    async with aiohttp.ClientSession() as session:
        tasks = [fetch_one(session, url) for url in urls]
        return await asyncio.gather(*tasks)

async def stream_data() -> AsyncIterator[bytes]:
    async with aiofiles.open('large.csv', 'rb') as f:
        async for chunk in f:
            yield chunk
```

## Error Handling

```python
from dataclasses import dataclass
from typing import TypeVar, Generic

T = TypeVar('T')
E = TypeVar('E')

@dataclass
class Ok(Generic[T]):
    value: T

@dataclass
class Err(Generic[E]):
    error: E

Result = Ok[T] | Err[E]

def divide(a: int, b: int) -> Result[float, str]:
    if b == 0:
        return Err("Division by zero")
    return Ok(a / b)
```

## Testing with pytest

```python
import pytest
from unittest.mock import AsyncMock

@pytest.mark.asyncio
async def test_create_user():
    repo = AsyncMock()
    service = UserService(repo)

    user = await service.create("test@example.com")

    assert user.email == "test@example.com"
    repo.save.assert_called_once()

@pytest.fixture
def mock_database():
    with patch('app.database') as mock:
        yield mock
```

## Tooling

```bash
# Ruff (linting + formatting)
ruff check --fix .
ruff format .

# MyPy (type checking)
mypy --strict .

# pytest
pytest -v --cov=src
```
