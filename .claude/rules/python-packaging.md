---
paths:
  - "**/pyproject.toml"
  - "**/uv.lock"
summary: Python manifests and distribution — the version floor that must actually run, dependency declaration, lockfiles, wheel contents, and publishing credentials
keywords: python,pyproject,packaging,uv,lockfile,requires-python,classifiers,py.typed,dependency-groups,trusted-publishing,hatchling,deptry
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Python Packaging

The manifest is the only place a Python project states a promise a machine
can check, and almost nothing checks it. Loads while editing
`pyproject.toml`, a lockfile, or a publish workflow.

Contents: [The Metadata That Lies](#the-metadata-that-lies) ·
[Dependencies](#dependencies) · [What Ships](#what-ships) ·
[Publishing](#publishing) · [Where Tool Config Lives](#where-tool-config-lives) ·
[Severity](#severity)

## The Metadata That Lies

Every key below is a claim, and each one was found false somewhere in this
family. A manifest claim nothing executes is documentation, not a contract.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PKG-01 | The declared `requires-python` floor is installed in CI and the suite collects against it — a floor nothing runs is a guess, and two harnesses here declared `>=3.10` while failing collection on it. | From the project directory: `uv python install 3.10` then `uv run --python 3.10 python -m pytest --collect-only -q .`, substituting the declared floor. A collection error is the violation; clean collection is the pass. | MUST |
| PY-PKG-02 | Every `Programming Language :: Python ::` classifier names a version the CI matrix actually runs — a classifier is a public compatibility claim, and advertising 3.13 while testing only 3.12 ships an untested promise. | Read the classifier list, read the workflow's `matrix.python-version`, and name every classifier absent from the matrix. Each one named is the violation. | SHOULD |
| PY-PKG-03 | An upper bound on `requires-python` appears only with an adjacent comment naming the incompatibility — an unexplained `<4.0` makes every future interpreter a resolver conflict for every consumer. | `rg -n 'requires-python' --glob 'pyproject.toml' .` then read each hit: a `<` with no adjacent reason is the violation. Empty output means no upper bound anywhere, which is the pass. | SHOULD |
| PY-PKG-04 | A single source defines the version, and reading it at runtime never lets a packaging exception reach a consumer — `importlib.metadata.PackageNotFoundError` escaping the package is an internal detail becoming API. | Uninstall the distribution while keeping the source importable, then import the package. It must raise its own error type; a bare `PackageNotFoundError` is the violation. | SHOULD |

## Dependencies

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PKG-05 | The lockfile is committed and verified in CI on every change — a lockfile that drifts from the manifest means CI and a contributor resolve different code. | `uv lock --check` in each project directory. Non-zero is the violation. | SHOULD |
| PY-PKG-06 | Declared dependencies match imported ones, checked with the project's own configured ignores — `deptry` invoked naively reported 67 phantom findings here against a real answer of zero, so the configuration is part of the rule and not a detail. | `uvx deptry . --optional-dependencies-dev-groups dev` from the project directory, plus whatever `--exclude` the project needs to cover `tests`. A finding that survives the configured run is the violation; a naive `deptry .` is not evidence either way. | SHOULD |
| PY-PKG-07 | Development-only tooling lives in PEP 735 `[dependency-groups]`, not in `[project.optional-dependencies]` — an extra is installable by a consumer, so a `dev` extra offers the world a way to pull your test stack. | `rg -n 'optional-dependencies' -A 12 --glob 'pyproject.toml' .` and name any group holding a linter, a type checker or a test runner. Each is the violation; a `docs` extra is defensible and a `dev` extra is not. | SHOULD |

## What Ships

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PKG-08 | A package that ships annotations ships `py.typed` **inside the built wheel** — present in the source tree and absent from the wheel means every downstream `--strict` consumer silently sees the whole package as untyped, with no error anywhere. | `uv build --wheel` then `unzip -l dist/*.whl` and look for `<package>/py.typed`. Absent from the archive is the violation; presence in `src/` alone is not a pass. | MUST |
| PY-PKG-09 | The layout is `src/`, with the build backend declared and version-constrained — a flat layout lets the test suite import the working tree while the built wheel is broken, and that failure appears first in a consumer's install. | Confirm a `src/` directory holds the package, then `rg -n 'build-backend' --glob 'pyproject.toml' .` and read each hit for a version constraint on the corresponding `requires`. An unconstrained backend or a flat layout is the violation. | SHOULD |

## Publishing

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-PKG-10 | Publishing authenticates through Trusted Publishing (OIDC), never a long-lived token in a repository secret — a stored token outlives the job, the contributor, and usually the memory of who minted it. | `rg -n 'PYPI_TOKEN' .github/workflows` and `rg -n 'password:' .github/workflows` as two separate commands. Any hit in a publish job is the violation; empty output from both is the pass. | SHOULD |

## Where Tool Config Lives

Not a rule, a settled decision, because a project with both files silently
resolves one and ignores the other.

`[tool.ruff]` belongs in `pyproject.toml` wherever a `pyproject.toml`
exists — one file, not two. A standalone `ruff.toml` is correct only where
there is no `[project]` table at all, which in this family means the
catalog repository itself and nothing else.

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not
in the commit body. CONSIDER = Suggest: never blocks, never re-raised
after a decline.

## Siblings

- **`python-quality`** — everything about the Python itself: typing,
  subprocess and process control, async, HTTP, the CLI contract, testing,
  security, logging, and the single-file stdlib tools. Loads on `**/*.py`.
  **Read its `ci-gate.md` when wiring a gate**, not this file — a workflow
  filename says nothing about its language, so nothing here globs
  `.github/workflows/`.
