# Typing and Annotations

When an annotation is evaluated, who evaluates it, and what a type checker is
configured to look at. Forward references, `cast()`, the typed shape of a JSON
boundary, and the pyright/ruff settings that decide whether any of it is
enforced. Loads with the Python quality rule on any diff that touches an
annotation, a `TypedDict`, or a checker config.

Contents: [Annotation Evaluation](#annotation-evaluation-pinned) ·
[Forward References](#forward-references) · [Typing a Boundary](#typing-a-boundary) ·
[Configuring the Checker](#configuring-the-checker) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Modern spellings — `X | None`, `list[X]`, `dict[K, V]` — are a lint, not a
rule: the gate carries `UP006,UP035,UP045` and nothing below restates them.

## Annotation Evaluation (pinned)

**`from __future__ import annotations` stays.** It is 90–96% adopted across
every shape, it is the only way to get deferred annotations below 3.14, and
PEP 749 keeps it working *on* 3.14 with no deprecation warning until after
3.13 reaches end of life. Removing it does not fix a single forward-reference
bug — those are broken with or without it. Reaching 3.14 is not the signal to
drop it; the deprecation warning actually firing is.

**`TC` (flake8-type-checking) adoption is ordered by `PY-CORE-03`, not by
this file.** What belongs here is why no configuration substitutes for that
ordering. `TC` relocates a type-only import into `if TYPE_CHECKING:`, which
by construction leaves that name unbound at runtime — the exact condition
`typing.get_type_hints()` raises `NameError` on, and the stdlib docs' own
worked example. No ruff setting closes the gap:
`runtime-evaluated-base-classes` protects class-*definition* time, not
"something calls `get_type_hints` three releases later". The policy: no
annotation-resolving dependency (pydantic, attrs + cattrs, typeguard,
beartype, a FastAPI-style framework) enters a repo without first sweeping
every `if TYPE_CHECKING:` block for what it would need to resolve.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn.

## Forward References

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TYP-01 | No name used in an annotation is undefined where that annotation is evaluated — a class imported inside the annotated function's own body does not count, and reads as correct to a human, to `pytest`, and to the interpreter alike while being invisible to every annotation consumer. A `# noqa: F821` names the consumer that is expected to leave it unresolved — "pytest matches fixtures by parameter name, this annotation is documentation" — never "resolved at runtime", which is true of no resolver that exists. | `ruff check --select F821 --no-cache .` — every printed line is a live bug; empty output is the pass. Then `rg -n --hidden --glob '**/workflows/*.yml' --glob '**/workflows/*.yaml' 'ruff check' .` — **no hit is the finding here**, the one inverted row in this file: a repo that never runs ruff in CI leaves this rule unenforced, which is the state of every repo in the fleet today. Swap `ruff check` for `runs-on` first to prove the glob reaches the workflow files at all. | MUST |
| PY-TYP-02 | Every module carries `from __future__ import annotations`. The sole exception is a module whose own annotations something resolves at runtime — and that exception is granted by naming the resolver, never by preference. | `rg --files-without-match --glob '*.py' 'from __future__ import annotations' src` — each listed file is a candidate; a module with no annotations at all is not a finding. Separately `rg -n --glob '*.py' -e 'get_type_hints' -e 'eval_str=True' .` — a deliberate union, either call forces resolution. Empty output means no resolver exists and the exception cannot be claimed. | SHOULD |
| PY-TYP-03 | Never quote an annotation in a module that already has the future import. The quotes are stored *inside* the stringized annotation, so `inspect.signature(..., eval_str=True)` returns the string `'Thing'` instead of the class — wrong data, no error, no traceback — while `get_type_hints()` still raises. | `rg -n --glob '*.py' -e '\) *-> *"' -e '^\s*(async )?def \w+\(.*: *"' .` — a union: both are the same violation in return and parameter position. Since PY-TYP-02 makes the future import universal, treat every hit as a violation unless that file demonstrably lacks it. A quoted *default value* (`sep: str = "\n"`) is not a hit. | MUST |

```python
# wrong — double-stringized, and the name never reaches module scope
def grim(binary: Path) -> "GrimRunner":
    from runner import GrimRunner
```

```python
# right — one module-scope import, one unquoted annotation
from runner import GrimRunner


def grim(binary: Path) -> GrimRunner: ...
```

## Typing a Boundary

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TYP-04 | Every `cast()` carries a comment naming the invariant the checker cannot see, or is deleted in favour of a validating parse. `cast()` over an `argparse.Namespace` is the class to *eliminate*: extract the arguments once per subcommand into a typed structure rather than re-asserting the type at each of dozens of reads. | `rg -n --glob '*.py' 'cast\(' src` — a hit with no adjacent rationale is the finding. Then `rg -n --glob '*.py' -e 'cast\([^)]*\bargs\.' -e 'cast\(.*getattr\(args,' src` — a union over the two argparse spellings; these are removed, not documented. | SHOULD |
| PY-TYP-05 | A JSON boundary is a `TypedDict` with `Required`/`NotRequired`, validated *at* the boundary. `total=False` marks every key optional and buys nothing; a `dict[str, object]` alias walked with `.get()` buys a `cast()` at every read. | `rg -n --glob '*.py' 'TypedDict.*total=False' .` — every hit is the finding. Then `rg -n --glob '*.py' -e 'dict\[str, Any\]' -e 'dict\[str, object\]' .` — a union; an alias used to walk an external payload is the finding, an internal scratch mapping is not. Go-red: delete a required key from a fixture payload and confirm the boundary raises — a consumer failing three frames later is the violation. | SHOULD |

## Configuring the Checker

A verification that cannot go red is worse than none, and two pyright defaults
manufacture exactly that. Both rules below exist because the silence is
indistinguishable from a clean run.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-TYP-06 | Any pyright scope running below `strict` sets `reportMatchNotExhaustive = "error"` explicitly. It is `"none"` at Basic *and* Standard, so a `match` over a `Literal` union that misses an arm produces no diagnostic, ever — and a repo whose baseline mode is `standard` with `strict = ["src"]` leaves its whole test tree in that state. | `rg -n --glob 'pyproject.toml' --glob 'pyrightconfig.json' -e 'typeCheckingMode.*basic' -e 'typeCheckingMode.*standard' .` — a union over the two below-strict modes; each hit must also set the rule, or keep every `match`-carrying path inside `strict`. Go-red: drop one arm from a `match` over a `Literal` union and confirm pyright errors — silence is the violation. | MUST |
| PY-TYP-07 | `reportUnhashable` and `reportPrivateImportUsage` are left at their Basic-mode default of `error`. Neither needs strict mode, so downgrading either in config removes a check that was already running for free. | `rg -n --glob 'pyproject.toml' --glob 'pyrightconfig.json' -e 'reportUnhashable.*"none"' -e 'reportUnhashable.*"warning"' -e 'reportPrivateImportUsage.*"none"' -e 'reportPrivateImportUsage.*"warning"' .` — a four-way union over the two rules and the two downgrades; any hit is the violation, empty output is the pass. Then `rg -n --glob '*.py' -e 'pyright: *ignore\[reportUnhashable' -e 'pyright: *ignore\[reportPrivateImportUsage' .` — a per-line suppression with no rationale beside it is the finding. | SHOULD |
| PY-TYP-08 | `ANN` is selected over `src/` and carved out over the test tree. Gating it across a pytest harness is ~2,500 findings, and the change that follows is always a blanket suppression that switches it off over shipped code too. Binds the two pytest harness shapes and the SDK; a stdlib-only single-file tool has no `src/` to scope. | `ruff check --select ANN src` — every printed line is a missing annotation in shipped code; empty output is the pass. Then `rg -n -A6 --glob 'pyproject.toml' --glob 'ruff.toml' 'per-file-ignores' .` — a config that selects `ANN` with no `ANN` entry for its test tree is the finding. | SHOULD |

## What Agents Get Wrong Here

1. Writing the annotation before the import. `-> "Runner":` with
   `from x import Runner` inside the body passes every signal a model trusts —
   it imports, `pytest` passes, the name is visibly right there.
2. Quoting a forward reference *and* keeping the future import, on the theory
   that quotes are the safe belt-and-braces spelling. They are the one
   combination that produces silently wrong data instead of an exception.
3. Applying the `TC` autofix because ruff offers it. The diff is correct by
   ruff's definition and plants a `get_type_hints` landmine per moved import.
4. "Fixing" a `NameError` from `get_type_hints` by moving the name into a
   `TYPE_CHECKING` block — which is exactly as unresolvable, now sourced from
   a more idiomatic-looking line. Re-run the thing that raised, not the linter.
5. Reaching for `cast()` the moment pyright complains. A `cast()` is an
   assertion the checker must believe; three casts over one `Namespace` mean
   the argument was never typed, not that the checker was wrong three times.
6. Treating a clean pyright run as coverage. `reportMatchNotExhaustive` is off
   below strict, and a scope excluded from `strict` reports nothing at all.
7. Deciding whether a *file* needs the future import, when the load-bearing
   question is whether every forward reference in it resolves from module
   scope — a question the import's presence does not answer either way.
