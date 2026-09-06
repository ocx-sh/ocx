# Data Modelling

Which container holds a value and what that choice commits you to: dataclass
flags, hashability and equality, closed sets of strings, timezone-aware time,
and byte-stable serialization. Loads with the Python quality rule on any diff
that adds a `@dataclass`, an `Enum`, an `__eq__`, or a `json.dump`.

Contents: [Choosing the Container](#choosing-the-container-pinned) ·
[Dataclass Shape](#dataclass-shape) · [Closed Sets and Time](#closed-sets-and-time) ·
[Serialization Determinism](#serialization-determinism) ·
[The Lint Gate](#the-lint-gate) · [What Agents Get Wrong](#what-agents-get-wrong-here)

## Choosing the Container (pinned)

`@dataclass(frozen=True, slots=True)` is the default for a value object.
`NamedTuple` only when the value is genuinely tuple-shaped — positional,
iterable, unpacked at the call site. `TypedDict` only when the shape is
externally imposed and must round-trip through JSON, never for a value you
construct and control end to end. `Protocol` for a seam with more than one
implementation. A plain class for mutable state with real behaviour.

**`attrs` stays out.** The argument for it — that stdlib `dataclasses` collects
inherited fields wrongly through the MRO — rests on a real, open CPython bug,
and it is inert here: nothing in the fleet has a dataclass inheriting from
another dataclass, so there is no diamond for it to bite. Record the trigger
rather than the debate: **the day a dataclass gains a second dataclass parent,
this decision is reopened.** Until then a runtime dependency in a package that
declares `dependencies = []` is a design change, not a default.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn.

## Dataclass Shape

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-MODEL-01 | Every dataclass is `frozen=True, slots=True`. Anything else carries a one-line comment naming why — "stateful adapter, holds a live client" is a reason; omission is not. Unfrozen means unhashable, and unslotted means a misspelled attribute assignment silently creates a new one instead of raising. | Two commands, because frozen and slots are a conjunction and one `-e` list would pass a class carrying only one of them. `rg --pcre2 -n --glob '*.py' -e '@(dataclasses\.)?dataclass(\(\))?\s*$' -e '@(dataclasses\.)?dataclass\((?![^)]*frozen=True)' src` then the same with `slots=True`. Every printed line is a violation; empty output is the pass. | SHOULD |
| PY-MODEL-02 | `slots=True` and `functools.cached_property` never appear on the same class — the decorator needs a per-instance `__dict__` and slots removes it. The class still *defines* cleanly; the `TypeError` waits for the first property access, so a smoke test that never reaches that attribute passes. A class needing weak references adds `weakref_slot=True`, which is itself an error without `slots=True`. | `rg -lU --pcre2 --glob '*.py' '@(dataclasses\.)?dataclass\([^)]*slots=True[\s\S]{0,600}?@cached_property' src` — every listed file pairs the two within one class-sized window; empty output is the pass. This rule exists because PY-MODEL-01 makes `slots=True` the default and therefore sets the trap. | MUST |
| PY-MODEL-03 | Hashability is decided, not inherited. At the `eq=True, frozen=False` default `__hash__` becomes `None` and the class blows up at its first `set()` or dict-key use, not at definition. A hand-written `__eq__` returns `NotImplemented` — never `False` — for a type it does not recognise, and comes with a matching `__hash__`. | `ruff check --select PLW1641 .` — a class defining `__eq__` with no `__hash__`; empty output is the pass, and the rule is stable, not preview. Then `rg -nU --pcre2 --glob '*.py' 'def __eq__[^\n]*\n(?:[^\n]*\n){0,3}?[^\n]*return False' .` — every hit is the violation. Do not re-verify the unhashable *use* site by hand: pyright's `reportUnhashable` is `error` from Basic mode up and already catches it wherever pyright runs. | MUST |

```python
# wrong — a definitive answer, so Python never tries the other operand
    if not isinstance(other, PackageRef):
        return False
```

```python
# right — declines, and the reflected __eq__ still gets its turn
    if not isinstance(other, PackageRef):
        return NotImplemented
```

## Closed Sets and Time

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-MODEL-04 | A closed set of string values is a `StrEnum`, not the same literal retyped in three modules. A typo in a bare literal is a silently-never-matching branch; a typo in a member name is an `AttributeError` at import. Legacy `class X(str, Enum)` becomes `StrEnum`. | `ruff check --select UP042 .` — but **`UP042` is inert below `target-version = "py311"`, and ruff infers that from `requires-python`**: a package declaring `>=3.10` while actually running 3.12 gets an empty, meaningless pass. Pin `target-version` to the interpreter in use or this check cannot go red. For the repeated-literal half, take each string that names a state, kind or outcome and run `rg -l --glob '*.py' '"human-review-required"' src` against it — three or more modules is the finding, one or two is not. | SHOULD |
| PY-MODEL-05 | Every `datetime` is timezone-aware. A naive one compares and subtracts against an aware one by raising `TypeError`, and serializes to a timestamp nothing downstream can place. | `ruff check --select DTZ .` — empty output is the pass. `DTZ` is not in ruff's default selection, so a gate that omits the family reports nothing and looks identical to a clean run. | SHOULD |

## Serialization Determinism

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-MODEL-06 | Anything a digest is taken over, or that lands in version control, serializes byte-identically: `sort_keys=True` passed explicitly, no set iteration, no reliance on dict insertion order, explicit `separators`. Ordering that happens to hold because the input was pre-sorted is an undeclared invariant — the next added key loses it silently, and the diff shows up as churn in a committed artifact. Binds writers only; a payload that never reaches a file or a digest is out of scope. | `rg -n --pcre2 --glob '*.py' 'json\.dumps?\((?![^\n]*sort_keys)' src` — every call whose opening line does not declare an ordering; empty output is the pass. One false-positive shape, and only one: a multi-line call that passes `sort_keys` further down, which three lines of reading settles. Then the cross-run check, the only one that sees set iteration: `PYTHONHASHSEED=1 python -m <writer> > a.json`, `PYTHONHASHSEED=2 python -m <writer> > b.json`, `diff a.json b.json` — a printed diff is the violation, empty is the pass. | MUST |

## The Lint Gate

One row, not five: these are already mechanized, and the only thing a rule adds
is making sure the families are actually selected.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-MODEL-07 | The ruff selection includes `B`, `F` and `RUF`, and the gate runs with `--preview`. Each code here is a behaviour bug that reads as correct: `B006` a mutable default shared across every call, `RUF012` a mutable class attribute shared across every instance, `B019` an `lru_cache` on a method pinning `self` forever, `B909` a container mutated while iterated, `F632` `is` against a literal, `F403` a star-import that blinds `F821`. | `ruff check --select B006,F632,F403,RUF012,B019 .` — empty output is the pass. Then `ruff check --preview --select B909 .` as a separate command: `B909` is preview-only and is silently absent, with no warning, from a run without `--preview`. | MUST |

## What Agents Get Wrong Here

1. `@dataclass` bare, because it is the shortest spelling that compiles. The
   flags are the whole decision; the decorator without them is a default nobody
   chose.
2. `return False` from `__eq__` for an unrecognised type — it reads as the
   obvious answer and quietly prevents the other operand from ever answering.
3. `if exit_code:` against an enum whose success member is `0`. The falsy
   member makes the terse spelling test the wrong thing; compare the member.
4. `datetime.now()` and `datetime.utcnow()` written reflexively. Both are naive,
   and the second is deprecated on top of it.
5. Repeating a status string across modules rather than reaching for an enum,
   because each individual occurrence looks like the smallest possible change.
6. `json.dumps(data)` for a file that gets committed, then treating the stable
   diff on the next three runs as proof it is deterministic.
7. Serializing a `set` into an artifact. Iteration order varies per process,
   and only a cross-run comparison with a changed `PYTHONHASHSEED` shows it.
8. Assuming a clean `ruff check` covers this file. `DTZ` and `B909` are opt-in;
   with neither selected the command exits 0 over code that violates both.
