---
title: Python Under Bazel
summary: The BZL-PY family — hermetic toolchain pinning, pip.parse and the requirements lock, py_test shape, imports, entry points, Gazelle and coverage
---

# Python Under Bazel

Owns `BZL-PY`: what `rules_python` registers when you say nothing, how PyPI
resolution actually reaches an interpreter, the shape of a `py_binary`/`py_test`
that runs what it claims to run, `imports` and the `sys.path` model, typing and
entry points, Gazelle for Python, and the coverage toolchain. It does not own
what siblings own, and never restates their rows: `BZL-FLAG` owns the Bazel 9
autoload flip in general, `BZL-HERM` owns action nondeterminism and
repository-rule hygiene (including the `PYTHONHASHSEED` pinning a Python-backed
build tool needs), `BZL-TEST` owns test sizing, sharding and coverage mechanics,
`BZL-MOD` owns `MODULE.bazel` structure and the lockfile, and `BZL-ARCH-12` owns
Gazelle freshness across languages. The contents of `pyproject.toml`,
`requirements.txt` and `uv.lock` belong to the python-packaging rule set; a rule
here names the file it has you edit, and that file is `MODULE.bazel`, `.bazelrc`
or a `BUILD` file unless it says otherwise.

Contents: [Python Targets in BUILD Files](#python-targets-in-build-files) ·
[The Root MODULE.bazel](#the-root-modulebazel) ·
[Strings Copied Out of Upstream Docs](#strings-copied-out-of-upstream-docs) ·
[Locks, uv and PyPI Resolution](#locks-uv-and-pypi-resolution) ·
[Gazelle for Python](#gazelle-for-python) ·
[Typing, Entry Points and Coverage](#typing-entry-points-and-coverage) ·
[The Bootstrap and sys.path](#the-bootstrap-and-syspath) ·
[Precompiling](#precompiling) · [Gaps](#gaps) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn, CONSIDER =
Suggest. Version-bound claims were measured 2026-09-06 against Bazel 8.7.0,
8.8.0 and 9.2.0 on a Linux host, and bind `rules_python` 2.3.3 and
`rules_python_gazelle_plugin` 2.3.3 (plus `rules_mypy` v0.41.0 and
`aspect_rules_py` 1.12.1 / 2.0.0-alpha.6 where a row names them); Bzlmod is
assumed throughout. Behaviour is identical on both majors except where a row
says otherwise. Before citing any Bazel flag, read it at your pinned version on
**both** help surfaces — `bazel help <command> --long` **and**
`bazel help startup_options` — because a flag can live in either and checking
one misses the other. Every `grep` below reads checked-in source only: the
`BUILD` and `.bzl` text a repository rule or module extension writes into an
external repo — every `py_library` a `pip.parse` hub emits, every wheel's
`entry_points.txt` — is not on disk when the grep runs, so reaching it needs
`bazel query`/`cquery` or a read of the fetched external repo, and a report says
which of the two it did.

## Python Targets in BUILD Files

The files this section governs:
`grep -rl -e 'py_test(' -e 'py_binary(' -e 'py_library(' --include=BUILD.bazel --include=BUILD .`
Every row is settled by reading them, plus one `bazel test` run for the second.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-20 | Give every `py_binary`, `py_library` and `py_test` its own explicit `load("@rules_python//python:defs.bzl", ...)`, or the per-rule `python:py_test.bzl` form. | Bazel 9.0 (2026-01-20) defaults `--incompatible_autoload_externally` to empty, deleting the allowlist that let 8.x resolve a bare `py_*` symbol. Pre-2026 training data emits the bare form, which builds on 8 and dies at load time on 9. The measured 9.2.0 text is `name 'py_library' is not defined (did you mean 'cc_library'?)` — the suggestion is wrong, and unlike `cc_library`, which gets a purpose-built removed-rule message naming `buildifier --lint=fix`, the Python failure hands back no fix at all. | `grep -rl -e 'py_test(' -e 'py_binary(' -e 'py_library(' --include=BUILD.bazel --include=BUILD . \| xargs -r grep -L -e 'load(.*py_test' -e 'load(.*py_binary' -e 'load(.*py_library'` — empty output, including empty because `xargs -r` had no file to scan, is the pass; any listed file is the finding regardless of the major targeted. Measured: a bare `py_library` exits 0 on 8.7.0 and fails at load on 9.2.0; adding the `bazel_dep` plus the `load()` passes on both with no other change. | MUST |
| BZL-PY-21 | Never point a `py_test`'s `srcs` at a pytest-style file without a pytest entrypoint — a wrapper's `main()`, or a `py_console_script_binary`-based macro. | Native `py_test` runs `unittest` discovery inside its `srcs`; on a file of `def test_` functions with no `TestCase`, the target reports green forever, having executed nothing. A maintainer states it in those words on the still-open request to have Gazelle generate the shim. This is the single highest-risk step in migrating an existing pytest suite. | For every `.py` file containing `def test_`, read the owning target's `deps` for a pytest wrapper; a `def test_` file whose owning `py_test` names none is the finding. Confirm with `bazel test --test_output=all //<target>` and read a nonzero collected-test count. **An empty collected-test count under a `PASSED` line is the FINDING, never the pass** — that is the whole failure, and it reads identically on both majors. | MUST |
| BZL-PY-24 | List every sibling **and ancestor** `conftest.py` in a hand-maintained `py_test`'s `srcs`, `data` or `deps`. | The sandbox contains only what Bazel staged, and the pytest wrapper does no conftest plumbing — it relies on pytest's own discovery. A missing conftest fails at collection with a fixture error that reads like a test bug. Gazelle auto-wires siblings since `rules_python` 0.14.0 and ancestors only since 1.9.0, so a two-tier layout below that pin is unwired whatever the directives say. | For each hand-written `py_test`, confirm every `conftest.py` between the test file and the Python root appears in `srcs`/`data`/`deps`. Where Gazelle generates the target, `grep -n 'rules_python_gazelle_plugin' MODULE.bazel` must show ≥1.9.0. **A missing ancestor conftest, or a <1.9.0 pin under a two-tier layout, is the FINDING.** Empty output from the plugin grep means the target is hand-maintained, so the full list is required; a tree with no `conftest.py` is not applicable. | MUST |
| BZL-PY-25 | Resolve a separately-built binary under test through `data = [":the_binary"]` plus `@rules_python//python/runfiles` and `Runfiles.CreateOrRaise()` — never a fixed relative-path fallback, and never `$(location)`/`$(rootpath)` as the path handed to a test. | The env-var-plus-fixed-path shape needs an out-of-band build step to populate the path and silently runs a stale binary whenever that step is skipped. `Runfiles.Create()` returns `None` outside Bazel, so a bare `Create().Rlocation(...)` chain becomes an `AttributeError` the moment someone runs the file under plain pytest. Bazel's own Make Variables reference calls `$(location)` legacy and ambiguous and names `$(rlocationpath)` the preferred form. An env-var **override with no path fallback** stays a legitimate escape hatch while two build systems coexist. | `grep -rn 'os.environ.get(".*_COMMAND"' <test-root>` and `grep -rn 'Runfiles.Create()' <test-root>` — empty on both is the pass; an env-var read paired with a fixed relative path, or a `Create()` with no `None` check, is the finding. Then `grep -rn -e '\$[(]location' -e '\$[(]rootpath' --include=BUILD.bazel --include=*.bzl .` — any hit converts to `$(rlocationpath ...)`. Identical on 8.7.0 and 9.2.0. | MUST |

```starlark
# wrong — unittest discovery inside srcs finds no TestCase: green forever, zero tests
py_test(name = "unit", srcs = ["test_api.py"])

# right — main is a wrapper whose main() invokes pytest and returns its exit status
py_test(name = "unit", srcs = ["pytest_main.py", "test_api.py"], main = "pytest_main.py")
```

```python
# wrong — runs a stale binary whenever the out-of-band build step was skipped
tool = os.environ.get("TOOL_COMMAND") or PROJECT_ROOT / "test" / "bin" / "tool"

# right — resolves inside the sandbox, raises outside Bazel instead of guessing
tool = Runfiles.CreateOrRaise().Rlocation("_main/cli/tool")
```

## The Root MODULE.bazel

One read of the root `MODULE.bazel` settles this whole block. `BZL-MOD` owns the
file's structure and its lockfile; these rows own only the `python` and `pip`
extension calls inside it.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-01 | Pin the root module's Python version explicitly, with both `python.defaults(python_version = "X.Y")` and `python.toolchain(python_version = "X.Y")`. | Under Bzlmod, `rules_python` registers a hermetic prebuilt-standalone toolchain as a **soft default** for any root module that never calls `python.toolchain()`. That default is hermetic but unpinned — 3.11 at tag 2.3.3, already 3.14 on `main` — so the interpreter moves on any `bazel_dep` bump, with no error and no warning. | `grep -c 'python\.toolchain(' MODULE.bazel` in the root module. **0 matches is the FINDING** — the repo rides the rolling default; one or more calls carrying a literal `python_version` is the pass. Same on 8.7.0 and 9.2.0. | MUST |
| BZL-PY-02 | Register one `python.toolchain()` per distinct exact Python version the repo builds or tests against, and wire `pip.parse(pyproject_toml = ...)` only where that project's `requires-python` is an exact `==X.Y.Z` pin. | `rules_python` neither selects nor interpolates a toolchain from a `>=` range, and `pyproject_toml=` (≥2.3.0) reads `requires-python` only when it is an exact pin — against a range it resolves nothing, silently, and the hand-copied `python_version` beside it drifts from the project it claims to follow. | `grep -rh --include=pyproject.toml requires-python . \| sort -u` for the distinct floors, `grep -c 'python\.toolchain(' MODULE.bazel` for the registrations, `grep -n 'pyproject_toml\s*=' MODULE.bazel` for the wiring. Two or more distinct floors with fewer registrations is the finding; a `pyproject_toml=` wiring against a `>=` floor is the finding. Empty on all three is a pass only in a repo with no Python. The edit is to `MODULE.bazel`; `pyproject.toml` is read, never rewritten to fit Bazel. | MUST |
| BZL-PY-03 | Bind every `pip.parse()` to an interpreter explicitly — a `python_version` matching a `python.toolchain()` already in the module graph, or a `python_interpreter_target`. | `pip.parse` is a repository rule: it runs in the loading phase, before analysis-phase toolchain resolution, so there is no toolchain to inherit. An unbound call resolves against the host and "works" only because that host happened to carry build tooling. | `grep -A5 'pip\.parse(' MODULE.bazel \| grep -e python_version -e python_interpreter` — **empty with a `pip.parse(` present is the FINDING**; no `pip.parse(` at all is not applicable. Identical on both majors. | MUST |
| BZL-PY-04 | Read a missing `python.toolchain()` under Bzlmod as hermetic-but-unpinned, never as a host-PATH fallback, and never register `@bazel_tools//tools/python:autodetecting_toolchain` to "fix" one. | The WORKSPACE-era host-PATH fallback is the lowest-priority path and is not reached under Bzlmod short of a toolchain misconfiguration. The autodetecting toolchain autodetects nothing: it takes `python3` from the runtime environment, which is the opposite of hermetic. The misdiagnosis leads straight to the wrong fix, and the wrong fix looks like it worked. | Confirm a `MODULE.bazel` exists and that no `WORKSPACE`/`WORKSPACE.bazel` registers a Python toolchain ahead of it, then `grep -rn 'autodetecting_toolchain' MODULE.bazel WORKSPACE* 2>/dev/null`. **Empty is the pass**; any match is the finding. The correct reading of a missing `python.toolchain()` is BZL-PY-01's, never "non-hermetic". | MUST |
| BZL-PY-05 | Never author WORKSPACE-era `python_register_toolchains()` or `pip_parse()` in a repository that has a `MODULE.bazel`. | The macros still exist for genuine mixed-mode migrations, so the snippet looks valid; in a Bzlmod-only repo it is dead weight or actively wrong. On Bazel 9 there is no way back — measured, `--enable_workspace` and `--enable_bzlmod` are present (`false`/`true`) at 8.7.0 and 8.8.0 and **absent from every help surface** at 9.2.0, so there is not even a flag to no-op. | `grep -l -e 'python_register_toolchains' -e 'pip_parse(' WORKSPACE WORKSPACE.bazel 2>/dev/null` — **empty, or no such file, is the pass**; a match alongside a `MODULE.bazel` is the finding (dead code, or a mid-migration repo that must be labelled as one). Confirm the flags' fate at your pin on both surfaces: `bazel help build --long` and `bazel help startup_options`. | MUST |

## Strings Copied Out of Upstream Docs

Three literals that upstream's own documentation spells wrong, has deprecated,
or documents ahead of the release that carries it. Each compiles, each does
nothing, and none of them errors. One grep apiece; **empty output is the pass in
all three rows**, and all three behave identically on 8.7.0 and 9.2.0.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-12 | Write `musl` — never `muslc` — as a `py_linux_libc` flag value. | The enum is `LibcFlag.MUSL = "musl"`. The ruleset's own multi-platform how-to and its 1.0.0 CHANGELOG entry both write `muslc`, while the same page's CLI example writes it correctly — the docs disagree with themselves. A `config_setting` copied verbatim from either compiles and silently never matches, so the platform it exists to select is never selected. | `grep -rn 'py_linux_libc.*muslc' --include=*.bazel --include=*.bzl .` — empty is the pass, any match is the finding. Check literal enum values against [`python/private/flags.bzl` at the pinned tag](https://github.com/bazel-contrib/rules_python/blob/2.3.3/python/private/flags.bzl), never against prose. | MUST |
| BZL-PY-34 | Configure a custom package index through `pip.default(index_url = ...)`, never through `pip.parse(experimental_index_url = ...)`, and never describe the `experimental_` spelling as the modern or opt-in mechanism. | `experimental_index_url` and `_overrides` carry a `versionchanged 2.0.0` deprecation notice in the extension's own attribute docs; the replacement `pip.default.index_url` defaults to `https://pypi.org/simple` unconditionally, so the Simple-API path is the **default**, not an experiment to enable. The `experimental_` prefix reads to an agent as "the new thing", which is backwards. | `grep -n 'experimental_index_url\b' MODULE.bazel` — word-boundary, so `_overrides` is its own hit. Empty is the pass or not applicable; any hit at `rules_python` ≥2.0.0 is the finding, migrate it. Confirm the attribute name at the pinned tag rather than from a doc page: `curl -sL https://raw.githubusercontent.com/bazel-contrib/rules_python/<pinned-tag>/python/private/pypi/extension.bzl \| grep -n 'index_url'`. | MUST |
| BZL-PY-28 | Use `gazelle_python_manifest`'s `requirements =` attribute against `rules_python` ≤2.3.3; `lockfiles =` does not exist yet. | The rename — on the very page that documents `uv.lock` acceptance — is unreleased, present only in `main`-branch docs behind a `VERSION_NEXT_FEATURE` marker, so copying the current doc page against a 2.3.3 pin fails with an unknown-parameter error. The attribute is format-agnostic either way (integrity hash only), so `requirements = "//:uv.lock"` already works under the current name — and is not evidence that `pip.parse` resolves anything from that lock. | `grep -rn --include=BUILD.bazel --include=MODULE.bazel 'lockfiles' .` — empty is the pass; a `lockfiles =` use at ≤2.3.3 is the finding. Confirm the parameter at the pinned tag: `curl -sL https://raw.githubusercontent.com/bazel-contrib/rules_python/<pinned-tag>/gazelle/manifest/defs.bzl \| grep -n 'def gazelle_python_manifest' -A5`. | MUST |

## Locks, uv and PyPI Resolution

The check for this block: read every `pip.parse()` and `lock()` call in
`MODULE.bazel`, and — when a wheel build dies — the error log before the fix.
The python-packaging rule set owns what goes inside `pyproject.toml` and
`uv.lock`; these rows own only what Bazel is told to read.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-07 | **pinned** Treat every `uv.lock` as a migration cost: feed `pip.parse()` a generated `requirements.txt` (from the `lock()` rule, or an out-of-band `uv export`), and treat full workspace-aware `uv.lock` consumption as a ruleset switch to `aspect_rules_py`, never as a `rules_python` configuration. | The `lock()` rule runs `uv pip compile` as a build action and never reads an existing `uv.lock`; no combination of `rules_python` flags reaches parity, and the maintainer said so on the record (2026-02-23). `pip.parse(uv_lock = ...)` exists from 2.2.0, silently dropped workspace/root members until 2.3.0, and is undocumented in every prose page at 2.3.3. A reviewer who sees "uv" in a Bazel file assumes a compatibility that is not there. `lock()` does reach `[tool.uv]` settings — including `no-build-isolation` — because it passes `--project <dir>`, but that covers the **locking** step only; the wheel build is a separate seam (BZL-PY-33). | `grep -rn 'load("@rules_python//python/uv:lock.bzl"' --include=*.bazel --include=*.bzl .`, then read the matched `lock()` target's `srcs`: `uv.lock` absent from them is by design, not a bug. **Empty means no `lock()` usage — not applicable, not a pass.** Any design note, PR or diff proposing "configure rules_python to read our uv.lock" is the finding. Same on both majors. | MUST |
| BZL-PY-09 | Never write guidance, an example, or a check that assumes `pylock.toml` (PEP 751) is consumable by `rules_python`. | PEP 751 is final upstream and widely known, so an agent assumes ruleset support followed. It has not: the tracking issue has been open since 2025-04-18 with nothing shipped as of 2.3.3. | `gh issue view 2787 --repo bazel-contrib/rules_python --json state --jq .state`. **`"OPEN"` means the assumption is still false, and any text asserting support is the FINDING**; `"CLOSED"` means re-date this rule before trusting either answer. Version-independent. | MUST |
| BZL-PY-33 | Never assume a `pip.parse` hub can build an sdist-only dependency hermetically: pin a pre-built wheel, set `download_only = True` and pick a package that has one, or accept an out-of-band build — and never diagnose the failure as a Bazel sandboxing bug. | `pip.parse`'s repository rule shells out to `pip wheel --no-deps` under the bare hermetic interpreter, in the loading phase, with no C/C++ toolchain on `PATH` and no declared exec-toolchain dependency to supply one, so an sdist whose build needs a compiler dies with `error: command 'clang' failed: No such file or directory`. There is no PEP 517 build-as-a-Bazel-action; that request has been open since 2024-11-14. `--no-build-isolation` is two seams, not one: `[tool.uv]` reaches only `lock()`'s `uv pip compile`, while `pip.parse(extra_pip_args = [...])` reaches this wheel build and changes nothing, because that interpreter carries no `setuptools`/`wheel`/`cython`. | On a failure: `grep -n -e 'Failed to build' -e 'did not run successfully' -e "command '.*' failed" <the bazel error log>` — a hit beside a wheel-build step confirms this root cause. **Empty on that grep means a different failure mode: do not apply this rule.** Preventively, `grep -n 'download_only' MODULE.bazel` and check PyPI for a wheel matching each target platform. A repo with no `pip.parse` is not applicable. | MUST |

## Gazelle for Python

Python is the only language with **two** distinct freshness gates, and the
check for this block runs both plus a read of the CI workflow:
`bazel test //:gazelle_test` and `bazel test //:gazelle_python_manifest.test`.
`BZL-ARCH-12` owns `gazelle_test` across languages; these rows are the Python
instance and the manifest half that only Python has.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-37 | Wire both Gazelle gates into CI — `bazel test //:gazelle_test` for BUILD-file freshness and `bazel test //:gazelle_python_manifest.test` for manifest freshness — and never report one as covering the other. | Measured across four ecosystems' own canonical examples — bazel-gazelle's root BUILD file at v0.54.0, `rules_python` 2.3.3's install doc, gazelle_rust's example, and Aspect's `aspect_gazelle()` macro — **none** wires `gazelle_test`; every one ships only the `gazelle()`/`sh_binary` half. Following the install doc therefore leaves BUILD drift uncaught by `bazel test //...`, and the two failures are different: a stale manifest resolves third-party imports wrongly, stale BUILD files list targets that no longer match sources. | `bazel query 'kind("sh_test", //:*)'` must show a `gazelle_test`-produced target, and `bazel query 'attr(name, ".*gazelle_python_manifest.test", //...)'` the manifest test; then grep the CI workflow for **both** labels. **Empty on either query, or either label absent from CI, is the FINDING** — and empty on the first specifically reads "no BUILD-freshness gate", never "the manifest test covers it". No Gazelle in the repo is not applicable. | MUST |
| BZL-PY-26 | Create and commit `gazelle_python.yaml` (an empty `touch` is enough) before the first manifest run, and wire `gazelle_python_manifest.test` into CI — not only `.update` into a local workflow. | The update target declares the file as an **input**, so its absence is a `missing input file` build failure rather than a first-run bootstrap, and that has been tracked and unfixed for two majors. Manifest drift against the requirements file is caught only by the `.test` target, so skipping it in CI keeps drift silent until a `bazel run //:gazelle` produces surprising BUILD diffs. | `test -f gazelle_python.yaml` (or the `python_manifest_file_name` value) in every directory a `gazelle_python_manifest` targets, then `bazel query 'attr(name, ".*gazelle_python_manifest.test", //...)'` and grep the CI workflow for that label. **A missing file is the FINDING; empty query output, or the target absent from CI, is the FINDING.** No Gazelle in the repo is not applicable. | MUST |
| BZL-PY-27 | Pin `rules_python_gazelle_plugin` ≥2.3.0 whenever a registered toolchain includes Python 3.13 or 3.14, and bump `rules_python` ≥1.5.0 in the same change. | Below plugin 2.3.0 the extension falls back to the Python **3.11** stdlib list for those versions, so `telnetlib` is still treated as stdlib and `compression.zstd` as third-party: wrong `deps`, generated with no error and no warning. Plugin 2.3.0 is a BREAKING release that branches on an `is_python_3.14` config setting only `rules_python` ≥1.5.0 defines, so a half-bump fails at analysis time instead. | `grep -n -e 'rules_python_gazelle_plugin' -e 'bazel_dep(name = "rules_python"' MODULE.bazel`, cross-checked against every `python.toolchain(python_version = ...)`. **A plugin pin <2.3.0 with a 3.13/3.14 toolchain is the FINDING; a plugin ≥2.3.0 with `rules_python` <1.5.0 is the FINDING.** Empty output means no plugin dependency at all: not applicable, as is a repo whose toolchains stop at 3.12. | MUST |
| BZL-PY-29 | Set `# gazelle:python_root` in the package that is the real import root whenever Python lives under a subdirectory rather than at the workspace root. | Without it, Gazelle treats the repo root as the import root and generates wrong `imports` attributes, or none, so absolute imports break. It is also the only sanctioned source of a `../` `imports` entry (BZL-PY-16). | `grep -rn 'gazelle:python_root' <python-source-root>/BUILD.bazel` when the Python tree does not start at the workspace root. **Empty is the FINDING.** Python at the workspace root is not applicable. | MUST |
| BZL-PY-30 | **pinned** Stand the Gazelle Python plugin up only above roughly 30-50 hand-maintained Python targets in one project; below that, hand-write `py_library`/`py_test`. Count the targets the declared `# gazelle:python_generation_mode` would produce, not source files. | The fixed cost is three `bazel_dep`s, a `modules_mapping`/`gazelle_python_manifest` pair, a committed manifest, a `gazelle_binary` and **two** CI-wired test targets, plus the three failure surfaces above, before one BUILD file is generated. No upstream source states a threshold — this number is a decision an adopter may override once, which is why it never blocks. The mode matters to the count: `file` emits one target per source file, `project` collapses a whole subtree. | `bazel query 'kind("py_.* rule", //...)' \| wc -l` once targets exist, or a source-file count adjusted for the mode (`grep -rn 'gazelle:python_generation_mode'`; absent means the `package` default) before they do. **Empty query output means no `py_*` targets exist yet, so the count is a projection — use the source-file proxy, never a zero.** Below the threshold with a Gazelle pipeline already present is a finding to reconsider, never a gate to enforce. | CONSIDER |

## Typing, Entry Points and Coverage

The check for this block: `grep` `MODULE.bazel` and `.bazelrc*` for the wiring,
then run the thing once and read its output. Three of these four fail by
producing *nothing* — no diagnostics, no coverage records, no error — so the
wiring grep alone is never the whole check.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-31 | **pinned** Gate Python types in CI with `rules_mypy`'s aspect, wired through `.bazelrc` as `build --aspects=//tools:aspects.bzl%mypy_aspect` plus `build --output_groups=+mypy` — never a hand-rolled `py_test` wrapper, and never `bazel-mypy-integration`. | `rules_python` ships no first-party type-check story and has scoped it out (tracking issue open since 2023-09-04, maintainer comment 2025-07-10). The aspect *is* the gate: `bazel build //...` with those two lines fails on any `py_binary`/`py_library`/`py_test` mypy rejects, and it needs no materialised venv because it resolves imports from providers already in the dependency graph. `bazel-mypy-integration` is the obvious hit for "bazel mypy" and its README still reads as working software — but the repository is archived (`archived: true`, last push 2025-05-08), a fact no README states and no training corpus can carry. | `grep -rn -e 'bazel-mypy-integration' -e 'mypy_integration' MODULE.bazel WORKSPACE*` — **any hit is the FINDING**, migrate it. Then `grep -n 'rules_mypy' MODULE.bazel` and `grep -rn -e 'mypy_aspect' -e 'output_groups=+mypy' .bazelrc*`: a `bazel_dep` plus both lines is the pass. **Empty on all of them means no type gate is configured — report it as a gap, not a violation.** Confirm the archive state live: `gh api repos/bazel-contrib/bazel-mypy-integration --jq .archived`. | MUST |
| BZL-PY-32 | Never present a `.venv`/`.venv_link` target, or any IDE venv materialisation, as a `rules_python` mechanism — it belongs to `aspect_rules_py`, and adopting it is a ruleset switch, not a flag. | The literal phrase `bazel run //target.venv` sits inside a `rules_python` issue thread, written by another ruleset's author about his own ruleset; an agent skimming that thread repeats it as `rules_python` behaviour, where it does nothing. `rules_python` has no `.venv`-materialising target and no such doc page at 2.3.3. Both real paths cost something worth stating up front: `aspect_rules_py`'s IDE default differs by track (auto-emitted `.venv` on stable 1.12.1, opt-in `expose_venv_link = True` on the 2.0.0-alpha line), and `rules_pyvenv` has no `MODULE.bazel` and no BCR entry, so adding it to a Bzlmod-only repo means a WORKSPACE-era `http_archive` — the exact shape BZL-PY-05 calls a finding. No maintained Pyright integration exists in either direction. | `grep -n -e 'rules_pyvenv' -e 'expose_venv_link' -e 'aspect_rules_py' MODULE.bazel` before writing any venv guidance, and name the ruleset the answer belongs to. **Empty means no IDE-venv tooling is wired: a gap to flag, never a violation.** Guidance that attributes a `.venv` target to `rules_python`, or that cites "the aspect_rules_py IDE mechanism" without naming which major, is the finding. | MUST |
| BZL-PY-35 | Before wiring `py_console_script_binary`, read the target wheel's own `entry_points.txt` and confirm the console script is a plain `module:attr` — no dotted attribute chain, no extras marker. | The macro never reads `pyproject.toml`. It parses the wheel's `dist-info/entry_points.txt` with `configparser`, then does `attr, _, _ = entry_point.partition(".")`, so only the first dotted segment of the right-hand side survives: `pkg.cli:App.run` silently generates a call to `App`. Extras are explicitly unhandled per the generator's own TODO at 2.3.3. The debugging cost is worse than the wrong sentence — editing `[project.scripts]` and rebuilding changes nothing until the wheel is rebuilt and the hub re-resolved, which reads as "the macro is broken". | Read the **fetched wheel**, not the workspace, because this is generated-repo content: `bazel build @pypi//<pkg>:dist_info`, then grep the `[console_scripts]` section of the produced `entry_points.txt` for a second `.` after the `:` or a `[` before it. **Either is the FINDING** (the macro will mis-wire silently); all single-segment with no extras is the pass. An empty `[console_scripts]` section is also the FINDING — the macro has nothing to generate from. A workspace-only grep of `pyproject.toml` proves nothing here and is itself the mistake this rule catches. | MUST |
| BZL-PY-36 | On any Python coverage leg set `python.toolchain(configure_coverage_tool = True)`. | The bundled-wheel condition and its `DA:` check are `BZL-TEST-26`'s, cited never restated. | `grep -n 'configure_coverage_tool' MODULE.bazel` — **empty on a repo that has a coverage leg is the FINDING**; no coverage leg is not applicable. | MUST |

## The Bootstrap and sys.path

The check for this block: read `.bazelrc*` for `--bootstrap_impl`, and read
every `imports =` attribute in the tree. Both rows fail by loading the wrong
thing successfully, so neither produces an error to grep for.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-14 | Treat `system_python` as the bootstrap default on every platform, and treat any Windows-targeted config setting `--bootstrap_impl=script` as inert. | `script` was announced in 0.33.0 as becoming the default "in a subsequent release" and never did — a roadmap sentence agents repeat as settled fact. The flag's own `select()` force-overrides to `system_python` under `:_is_windows` unconditionally, so a Windows `.bazelrc` line setting `script` is dead configuration, not a bug to fix toward working. | Read `build_setting_default` for `//python/config_settings:bootstrap_impl` in the **installed** version's `python/config_settings/BUILD.bazel`, or run `bazel config` against a `py_binary`. **Empty output from `grep -rn 'bootstrap_impl' .bazelrc*` reads "default = `system_python`", never "undefined".** Same on both majors. | MUST |
| BZL-PY-16 | Audit every `imports =` entry for a basename collision before merging it, and allow a `../` escape only where Gazelle generated it from a declared `# gazelle:python_root`. | `imports` paths are transitive to every consumer and land in the "user" band of `sys.path` — ahead of runtime site-packages under both bootstraps since 1.7.0 — so a directory sharing a name with a PyPI package, or with another `imports` entry, wins silently: a wrong module, never an `ImportError`. A hand-written `../..` adds an ancestor up to the repo root and multiplies that surface; Gazelle emits the same shape deliberately and narrowly, and that one is not a finding. | `grep -rn 'imports\s*=\s*\[' --include=BUILD.bazel --include=BUILD .`, and for each hit resolve the contributed basename against every PyPI top-level import reachable by the same binary and every other `imports` basename in the same transitive closure. Then `grep -rn 'imports\s*=\s*\[.*"\.\./' --include=BUILD.bazel --include=BUILD .`. **Empty on both is the pass.** A collision, or a hand-written `../` entry with no `python_root` and no justification comment, is the finding. | SHOULD, and MUST once a first real collision is found in the repo |

## Precompiling

One grep settles both rows: `grep -rn 'precompile' .bazelrc*` — **empty is the
pass, because the default holds.**

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-PY-17 | **pinned** Leave precompiling off unless a cold-start problem has been measured, and never recommend it without naming the `exec_properties` caveat and the same-`srcs` audit. | `--precompile=auto` resolves to `disabled` in the flag's own effective-value function — "auto" is not a maybe. Runfiles count and size roughly double, one of the three caveats the docs name has been tracked open since 2024-11-26 across two majors, and the ruleset itself treats precompiling as an advanced per-target opt-in (`pyc_collection`), not a broad recommendation. | `grep -rn 'precompile' .bazelrc*` — **empty is the pass**. In any shipped guidance or skill text, a precompiling recommendation with no `exec_properties` mention inside it is the finding. | MUST |
| BZL-PY-18 | Before enabling precompiling anywhere in a build, prove that no two Python targets share `srcs` while differing in `exec_properties`. | Two targets sharing source files with different `exec_properties` produce an `ActionConflictException` on the generated `.pyc` — filed 2024-11-26, last comment 2025-09-17, still open, and reported broader than first scoped (any `args`- or env-driven variant pair). *Argued-tier mitigation, not a rule*: one maintainer comment plus one corroborating report suggest scoping each `exec_properties` key to its owning exec group where the two targets genuinely need different properties, because the docs' own advice — make them the same — is often impossible. | For each `.py` file, `bazel query "kind('py_binary\|py_library\|py_test', same_pkg_direct_rdeps(//path:file.py))"`, then compare `exec_properties` across the owning targets. **Empty — no file owned by more than one target — is the pass.** Any shared-`srcs` pair with differing `exec_properties`, with precompiling enabled, is the finding; a `bazel build //...` over both targets reading no `ActionConflictException` is the fallback confirmation. | MUST, once precompiling is enabled anywhere |

## Gaps

- Every measurement here ran on Linux; darwin and Windows behaviour is
  source-confirmed at best, never run.
- Windows specifically: `rules_python` forces `system_python` (≥1.5.0) and
  forces `--enable_runfiles=true` for `py_binary`/`py_test` by rule-level
  transition (≥1.9.0, changelog says the override "will soon become required").
  Neither was exercised.
- `bazel coverage` on a Windows runner is unmeasured — `docs/coverage.md` does
  not mention Windows at all — and the bundled wheel set's missing
  platform×version cells inside CPython 3.9-3.14 are undocumented.
- `pylock.toml` (PEP 751) support and the `gazelle_python_manifest`
  `requirements=`→`lockfiles=` rename are both unshipped at 2.3.3 with no dated
  target; re-check both before repeating either status.
- BZL-PY-30's 30-50 threshold and BZL-PY-18's exec-group mitigation are
  judgment and argued evidence respectively, not measured facts.

## What Agents Get Wrong Here

1. **Emitting a bare `py_binary`/`py_library`/`py_test` with no `load()`** — the
   default from years of WORKSPACE-era examples, a hard load failure on 9.2.0,
   and its error suggests `cc_library`, which is not the fix (BZL-PY-20).
2. **Pointing a `py_test` at a pytest file and reporting the green target as
   evidence** — it ran nothing, and it will report green forever (BZL-PY-21).
3. **Reading a missing `python.toolchain()` as a host-PATH fallback** and
   "fixing" it by registering the deprecated autodetecting toolchain, which
   really is non-hermetic (BZL-PY-04).
4. **Claiming `uv.lock` parity, or inventing the mechanism** — a
   `pip.parse(pylock = ...)` attribute or an `--experimental_uv_lock` flag, both
   of which have never existed (BZL-PY-07, BZL-PY-09).
5. **Copying a literal out of upstream's docs that compiles and never fires**:
   `muslc`, `experimental_index_url`, `lockfiles =` (BZL-PY-12, BZL-PY-34,
   BZL-PY-28).
6. **Recommending precompiling as a free win, or asserting `script` is the
   current bootstrap default** — both are single sentences from a changelog read
   out of context (BZL-PY-17, BZL-PY-14).
7. **Recommending `bazel-mypy-integration`, or attributing a `.venv` target to
   `rules_python`** — the archive flag is repository metadata no README states,
   and the `.venv` phrase was written about a different ruleset (BZL-PY-31,
   BZL-PY-32).
8. **Describing `py_console_script_binary` as reading `pyproject.toml`, and
   treating `--no-build-isolation` as one switch** — the macro reads the built
   wheel, and the flag has two unrelated seams (BZL-PY-35, BZL-PY-33).
