# Design spec: the test-diff guard

`scripts/test_diff_guard.py` refuses a diff under `test/` that changes what the
acceptance suite asserts. This record holds the permit rules, verbatim from the
module docstring they used to live in; the plan IDs in the text (`DEC-`, `C-`,
`S-`, `DX-`, `AM-`) are those of `plan_crate_split_workspace.md` and
`plan_test_speed_tiers.md`. The guard's proofs are
`scripts/tests/test_test_diff_guard.py`.

## Permit rules

The crate split (plan_crate_split_workspace.md DEC-10, C-077, S-012) moves
code, never behaviour, and the acceptance suite is the proof: it must pass
unmodified at every commit. So under `test/` a merge may only
  - add `@pytest.mark.smoke` lines (and the `import pytest` that makes one
    resolve in a module that lacked it),
  - add `@pytest.mark.xdist_group("<name>")` lines — a scheduling constraint,
    the one mark that cannot change what a test asserts: it moves the test to
    a worker, and the tests it must not run beside are the reason. A group
    argument must be a literal, so the line names the slot it serialises on
    and a reader can grep for the other members,
  - change docstring / comment lines (path spellings follow the moved code),
  - add wholly new test functions (or test classes) to an existing module,
    or new `test/tests/**/*.py` modules — never a `conftest.py` or
    `__init__.py`, and never a `pytest.ini` / `tox.ini` / `setup.cfg`
    (pytest prefers any of those over `pyproject.toml`, so a new one
    silently replaces `--strict-markers`, `testpaths` and `pythonpath`),
  - change a `test/` file that a taskfile invokes — tooling the repo drives,
    not part of the pytest proof this guard freezes. `test/scripts/*.sh` is
    the case: `task test:index-conformance-drift` runs
    `sync_index_conformance.sh` and `deep-repo.yml` (called by `verify-deep.yml`)
    runs that task, so the "neither collected, imported nor run" rule below was false for it and
    forbade ever maintaining it. A file the collected suite *names* stays
    frozen even when a task also runs it, so this cannot thaw
    `test/docker-compose.yml`,
  - edit `test/pyproject.toml`, `test/taskfile.yml` and the floor files
    (`test/SUITE_FLOOR`, `test/SKIP_CEILING`, `test/XFAIL_CEILING`) — except
    that an added non-comment line there may not carry a pytest selection
    token (`--deselect`, `--ignore`, `--ignore-glob`, `-k`/`--keyword`,
    `-m`/`--markexpr`): those narrow the run without touching a test file.
    The one permitted expression is the smoke tier's own bare `-m smoke`
    (C-014, `test:smoke`) — `-m 'not smoke'`, `-m "smoke and x"` red.
    In a `.toml` among them the key check is a permit list too: an added
    line may set only a key of `_PERMITTED_CONFIG_KEYS`. Every way pytest
    has of collecting less is an ini key — `testpaths`, `norecursedirs`,
    `collect_ignore`, `collect_ignore_glob`, `python_files`, whatever the
    next release adds — so a list of the dangerous ones is a list already
    out of date, and a new key is a decision rather than an edit. The
    deletion direction has its own permit rule: a *removed* line may not
    drop a command-line option that no added line carries again. Removing
    a selection token widens the run and stays green; removing
    `--strict-markers` does not, because without it a misspelled
    `@pytest.mark.smok` stops being a collection error and becomes a test
    that silently leaves the `-m smoke` selection. A re-spelling that keeps
    every option passes.

## Violations and the allow-list

Everything else is a violation and exits non-zero naming the line: a deleted
file, a removed code line, a changed line that carries `assert`,
`pytest.mark.skip`, `xfail`, `parametrize` or a fixture signature, an added
line of any other shape, an unchanged line that moved between code and a
docstring (a docstring opened one line and closed after the body turns every
assertion between into text while git reports the body as untouched), and any
edit to `test/src/**`, `test/conftest.py` or the other assets under `test/`.
One hunk may be allow-listed by `--allow
<path>:<line>` — the trampoline blob path constant at
`test/tests/test_trampoline_exec.py:852` (DEC-10 d) is the only intended use.
An allow-listed hunk must be exactly one line replacing one line: the
neighbours of that constant are asserts, and a `--unified=0` hunk spanning
them would let the allow-list re-point the constant and neutralise an assert
in one commit. Under `--tiered-shapes` an `--allow test/lint/<file>.py:<line>`
also names a reviewed rewrite of a module-level binding a moved test reads
(`_moved_global_problems`): every statement binding that name in the lint
home must be allow-listed at its first line, and what the rewrite reads is
held to the same rule in turn.

## New definitions

A wholly new test function or `Test*` class is exempt from the line checks
but not from every check — new code that rebinds what the existing tests
resolve makes their assertions vacuous with the diff reading as "added a
test". A new class must be `Test*` and hold only defs and a docstring; a new
def may carry only `@pytest.mark.*` decorators and no default argument
(evaluated at import); a new class may carry no decorator, base or
keyword.

## Body rule

The body rule is DEC-10(b) as ruled 2026-09-16, a permit rule: a new def
(and every def in a new module, and a new module's own import-time
statements — module level and class bodies alike, where `monkeypatch`
does not exist and nothing is undone at teardown) stores only into names
bound in that same scope, and `monkeypatch` — the fixture parameter — is
the only sanctioned mutation route. Concretely (`_store_problems`): every
store target (assignment, augmented or annotated assignment, walrus,
`del`, `for` and `with` targets) must root in a parameter or a name the
scope bound earlier to a fresh value — a name bound to a reference into shared state
(`c = helpers.CACHE`, `x = REGISTRY`) stays shared, and a container built
here is the scope's own while what it holds is not, so reaching back
through it (`box = [os.environ]`, `box[0]["X"] = …`) reaches what was put
in. An attribute call is a permit rule too (`_PURE_READS`): on a receiver
the scope does not own — a module, a module-level object, a container
reached back through — only a listed pure read may be called, so
`helpers.CACHE.sort()`, `os.putenv(…)`, `shutil.rmtree(…)` and
`Path("test/pytest.ini").write_text(…)` red beside the mutators that were
once enumerated, while `re.search(…)`, `ast.parse(…)`, `p.read_text()` and
a call into the suite's own frozen `test/src/**` (`helpers.make_package()`)
pass. A binding is followed through an alias, so `change =
os.environ.update` then `change({…})` is the same refusal at the bare-name
call. On a *fixture parameter* the rule stays the mutator enumeration
(`_MUTATORS`), because calling a fixture's methods is what a test does:
`ocx.run(…)` and `tmp_path.joinpath(…)` pass, `ocx.env.update(…)` does
not. `monkeypatch` — the fixture parameter — is exempt from both
(`monkeypatch.setattr(helpers, …)`, `monkeypatch.setitem(REGISTRY, …)`,
`with monkeypatch.context() as m` pass), and `os.environ["X"] = …`,
`REGISTRY["k"] = …`, `sys.path.append(…)` and `COUNT += 1` do not;
`global` and `nonlocal` are refused outright. On
top of that (`_rebinding`, every walk): the name `getattr`, `setattr`,
`delattr`, `globals`, `vars`, `exec`, `eval`, `__import__`,
`__builtins__`, `MonkeyPatch` or `pytest_plugins` in any position (bare,
aliased, subscripted); an attribute that is a dunder other than
`__name__`; a `.MonkeyPatch(`, `.patch(`, `.start(`, `.import_module(`,
`.reload(`, `.exec(` or `.eval(` call (each persists past the test or
executes text); an assignment or walrus aliasing an imported module
(`h = helpers`); a `sys.modules` reference; a class keyword; and, at
module level of a new module, a store into an imported module's attribute
or item — imports bind wherever they appear, def-local ones included.

## New modules and plan-owned tests

A new `test/tests/**/*.py` module is not accepted by name alone: the same
walk runs over its whole body, module level and every def (it may hold
helpers, constants and module-local fixtures — those change nothing outside
it — but not a store into, or a mutating call on, anything it imports). The plan's own structural tests
under `test/tests/` (PLAN_OWNED_STRUCTURAL_TESTS) are exempt from the line
checks — they are the plan's oracles (C-014, C-048), edited by later WPs,
and never an acceptance assertion DEC-10 protects — and get that whole-module
walk instead. Every other module, in-series acceptance tests included, gets
the full checks — so the guard is run over both the per-WP range and the
merge-base range at each merge (DX-17), and neither range hides an edit the
other would show.

## Tiered shapes

`--tiered-shapes` (plan_test_speed_tiers.md C-007, P-2) admits five more
shapes, and only when passed — without it each is refused exactly as above
(DEC-10's default, S-023): (a) a `test*` def removed under `test/tests/`
whose AST-identical twin is added under `test/lint/` (a whole module when
every def is matched; statements the move orphans may go with it); (b) a new
`test/lint/conftest.py`, `test/lint/test_*.py`, `test/LINT_FLOOR` or a
`test/LINT_*_CEILING`, whose
own code is walked as new code except what it carries verbatim from a source
module of its moved tests — module-level statements and undecorated helper
defs; (c) one
`pytest.mark.command("<key>", ...)` statement per module, in the form its
existing `pytestmark` binding dictates; (d) `test/scoped_rows.toml` and
`test/LINT_FLOOR` and the `test/LINT_*_CEILING` pair as config; (e) a removed def named by an added
`// ported-from:` marker whose Rust test executed and passed in a fresh
`target/bazel/junit.xml` — the guard runs `task bazel:test:unit` itself at
HEAD (AM-7). A `test/SUITE_FLOOR` decrease may then not exceed the collected
count of the (a)/(e) removals, and `test/LINT_FLOOR` may not decrease at all. In a config file, a `cargo` line's `--release`
counts as the `--profile` it abbreviates (C-021's `--profile test-bin`).

## Proofs

Stdlib only; driven by `git diff`. Its proofs (`scripts/tests/test_test_diff_guard.py`)
build one throwaway git repository per shape under pytest's own `tmp_path` and
show the guard red on every forbidden shape and green on every allowed one —
a guard that was never seen red is a habit, not a check.

