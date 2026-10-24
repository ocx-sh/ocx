# The Python, TypeScript and C++ branches

Read this when the pilot is Python, TypeScript or C++. Each branch names the
ruleset version, the toolchain pin, the dependency-lock mechanism, the generator
or its absence, and the first three rules to satisfy. Read only the branch you
need.

Contents: [Python](#python) · [TypeScript](#typescript) · [C++](#c) ·
[What all three share](#what-all-three-share)

## Python

| | |
|---|---|
| Ruleset | `rules_python` **2.3.3** (2026-09-04); Gazelle plugin **2.3.3** |
| Bazel floor | Real and CI-tested; the released module declares none, which reads as "undocumented" (BZL-FLAG-27) |
| Toolchain pin | `python.defaults(python_version = "X.Y")` **and** `python.toolchain(python_version = "X.Y")` |
| Lock mechanism | `pip.parse`, bound to an interpreter explicitly |
| Generator | Production, but **only** paired with a pytest wrapper — and not worth standing up below roughly 30-50 hand-maintained targets |

**Hermeticity is not opt-in here, and the received framing says it is.** Under
Bzlmod the ruleset registers a hermetic prebuilt toolchain as a *soft default* for
any root module that never calls `python.toolchain()`. The old host-PATH fallback
is not reached. So the failure is not non-hermeticity — it is a hermetic but
**unpinned rolling default** that drifts on every `bazel_dep` bump with no error
and no warning (BZL-PY-01, BZL-PY-04).

```sh
grep -n 'python.defaults(\|python.toolchain(' MODULE.bazel
```

**Empty output is the finding**: the repository is on a moving default. Pin with
both calls. Register one `python.toolchain()` per distinct exact version the
repository builds or tests against (BZL-PY-02).

Bind every `pip.parse()` to an interpreter explicitly — a `python_version`
matching a registered toolchain, or an explicit interpreter target (BZL-PY-03).
Never author WORKSPACE-era `python_register_toolchains()` or `pip_parse()` in a
repository that has a `MODULE.bazel` (BZL-PY-05).

Three lock traps:

- `pip.parse(uv_lock = …)` is **not** uv.lock parity. On any use, confirm the pin
  is at or past the release that added it and check the lock for workspace
  members.
- `pylock.toml` is not consumable by the ruleset. Never write guidance, an
  example, or a check that assumes it is (BZL-PY-09).
- A dependency with no matching wheel does not build hermetically by assumption:
  before adding one, either pin a pre-built wheel or prove the sdist path works
  (BZL-PY-33).

**The silent-green trap that decides the migration's credibility.** A native test
rule runs unittest discovery inside `srcs`. Pointed at a pytest-style file with no
`TestCase`, the target is permanently green and runs zero tests. Never point a
test target's `srcs` at a pytest-style file without a pytest entrypoint — a
wrapper's `main()` or a console-script-based macro (BZL-PY-21).

```sh
git grep -ln '^def test_' -- '*_test.py' 'test_*.py'
```

Every file listed needs a wrapper on its test target. **Empty output means no
pytest-style files**, and the native rule is sufficient.

Two more that bite during a migration:

- A separately-built binary under test is resolved through `data = [":the_binary"]`
  plus the runfiles library, replacing the common "environment variable with a
  fixed relative-path fallback" fixture. That fixture requires a CI step to have
  populated the path; runfiles does not (BZL-PY-25).
- A hand-maintained test target lists every sibling **and ancestor** `conftest.py`
  in its `srcs`/`data`/`deps`. The generator wires both automatically; a hand-written
  target gets neither (BZL-PY-24).

If the generator is adopted, wire **two** distinct gates, not one: BUILD-file
freshness and manifest freshness are separate targets with separate failure modes,
and conflating them is itself a finding (BZL-PY-37, BZL-PY-26). Create and commit
the manifest file before the first run — an empty `touch` is enough; it is not
auto-created.

**First three rules:** BZL-PY-01 (pin the version with both calls), BZL-PY-03
(bind `pip.parse` to an interpreter), BZL-PY-21 (no pytest file without an
entrypoint).

## TypeScript

| | |
|---|---|
| Rulesets | `rules_js` **3.4.1** (`bazel_compatibility = [">=7.6.0"]`), `rules_ts` **3.10.1** (no declared floor at the tag) |
| Toolchain pin | An explicit `transpiler` on every hand-written `ts_project` |
| Lock mechanism | **pnpm only.** `npm_translate_lock` has exactly three lockfile attributes: pnpm, npm and yarn |
| Generator | A vendor plugin; the language module is past 1.0, the prebuilt distribution it recommends is still `0.0.x` — usable with care |

**pnpm is not a preference, it is the ingestion surface.** The virtual store is
the only `node_modules` layout that decomposes into discrete cacheable actions.
There is no `bun_lock` attribute and nobody has filed for one, and `bun test` has
no ruleset either — a bun-locked package converts to pnpm first or it does not
adopt (BZL-JS-01, BZL-JS-03, BZL-JS-29).

```sh
ls pnpm-lock.yaml 2>/dev/null && head -1 pnpm-lock.yaml
```

**Empty output means there is no pnpm lock**, and conversion is step one, not a
detail. Convert with the package manager's own import command rather than
hand-writing one.

**Adopting for a repository not already on pnpm is a "no" as a starting move.**
The cost — conversion, `hoist: false`, phantom-dependency fixes — is paid before
any Bazel benefit arrives. The answer flips only when the JS package is a minority
slice of a polyglot migration already justified by other languages.

The ordering rule that keeps the tree green: set `hoist: false`, run the install,
and get the **existing** build and test suite passing outside Bazel before writing
a single BUILD target. Every phantom-dependency break surfaces there, where it is
cheap, and classifying each one correctly is BZL-JS-08.

Then three that decide whether the result means anything:

- Name a `transpiler` on every hand-written `ts_project`. Do not adopt the
  repo-wide default-transpiler flag as a way of never deciding; it is a legitimate
  escape hatch and a defect as a permanent state (BZL-JS-13).
- **`bazel build` does not typecheck.** Report a target as clean only from
  `bazel test //pkg:name_typecheck_test` or an explicit typecheck output group
  (BZL-JS-11), and make `bazel test` the CI verb of record over a target set that
  includes every typecheck test (BZL-JS-12).
- Never let two `ts_project` targets list the same `.ts` path in `srcs`. Read a
  permission error on an output path as that defect, never as a filesystem
  problem (BZL-JS-16).

Keep the package manager's `node_modules` out of Bazel's directory walk with an
ignore directive (BZL-JS-04). Route every custom rule or genrule that runs a Node
tool through the ruleset's own runner, never a raw action (BZL-JS-05).

If the generator is adopted, three known gaps bind. It never emits a `transpiler`
attribute, so every generated target needs a kept hand-patch or the repo-wide flag
adopted as a documented, reviewed exception (BZL-JS-33). Its lockfile parser
dispatches on major versions 5, 6 and 9 only — `head -1 pnpm-lock.yaml`, and a
version outside that set is a finding (BZL-JS-34). And it decides workspace
membership from the lockfile's `importers:` keys alone, never from the workspace
globs, so a package directory absent from `importers:` gets no linked packages and
every third-party import fails to resolve by name (BZL-JS-36).

**First three rules:** BZL-JS-01 (a real pnpm lock before any ingestion),
BZL-JS-13 (a named `transpiler` on every `ts_project`), BZL-JS-11 (typecheck is a
test, not a build).

## C++

**This branch had no consumer in the codebases this research was grounded in.**
Its rules are vacuously satisfied there, which is not the same as passing, and
every claim below rests on upstream sources rather than on a migration anyone
ran. Treat it as a starting map, not a worked path, and keep it short.

| | |
|---|---|
| Ruleset | `rules_cc` **0.2.22** (pre-1.0; judge staleness on the second component) |
| Toolchain pin | **There is no default hermetic C++ toolchain**, and this procedure will not invent one |
| Lock mechanism | None of its own; third-party C/C++ is either native `cc_*` or wrapped, never both |
| Generator | An early plugin at `0.1.0` — experimental |

Choose the toolchain from a three-branch constraint tree and **record which
constraint decided it** (BZL-CC-01): newest sanitizers and named modules point one
way, cheap cross-compilation with sanitizer defaults absorbed another, and exactly
one target platform with no appetite for an external cadence a third.

Two checks before anything else:

- A bare `CC=` or `--repo_env=CC=<path>` override is **not** a hermetic toolchain.
  It redirects the autodetected toolchain's compiler probe, registers nothing and
  resolves nothing ahead of the default (BZL-CC-02).
- A build whose resolved graph reaches the autodetected C++ repository **is** on
  the non-hermetic default, whatever the rc file says. Move its flags into a
  toolchain definition rather than accumulating them in `.bazelrc` (BZL-CC-03).

```sh
TARGET=//foo:bar
bazel cquery "deps($TARGET)" > /tmp/cc.deps || { echo 'cquery FAILED — not a pass'; exit 1; }
grep -c 'local_config_cc' /tmp/cc.deps
```

**Zero, on a successful cquery, = the graph does not reach the autodetected
toolchain = pass.** A non-zero count is the finding, regardless of what the
configuration claims. A failed cquery is not a pass — it is unknown, and must
fail loudly rather than read as zero.

Do not adopt header-layering enforcement as one repo-wide flip; roll it per
package behind two preconditions, and confirm it is actually enforcing on the real
spawned command line rather than inferring it from a green build (BZL-CC-11,
BZL-CC-12). Use the wrapped-foreign-build path only to vendor third-party code you
do not control and will not rewrite, never as the long-term strategy for
first-party CMake (BZL-CC-24).

One cross-cutting fact worth knowing before promising anything: from protobuf
**34.0** onward (**36.1.bcr.1** still carries it) the compiler arrives as a
**prebuilt download** by default, so a registered C++ toolchain does not build
every binary in the graph. Name the protobuf version alongside any claim about
it (BZL-FLAG-34, BZL-CC-32).

**First three rules:** BZL-CC-01 (choose from the constraint tree and record the
constraint), BZL-CC-02 (`CC=` is not a toolchain), BZL-CC-03 (a graph reaching the
autodetected repository is on the default).

## What all three share

Four rules bind on every branch and are cited, not restated:

- **BZL-ARCH-24** — the language's own manifest stays the sole hand-edited source
  of dependency truth, and the Bazel-side lock is one-way generated from it. The
  IDE's language server reads the manifest, never the Bazel graph, so a hand-edit
  on the Bazel side reintroduces a second source of truth invisibly.
- **BZL-ARCH-11** — granularity follows generator maturity. Coarse and
  hand-maintained wherever the generator is not production-grade.
- **Run both builds on the same trigger** as required checks for the whole
  duration of the migration. With two systems claiming to build the same code, a
  shared gate is the only thing that catches the day one stops representing what
  ships. No primary source names this pattern, so it is planning, not a rule.
- **BZL-ARCH-29** — every checked-in generated file names its generator and is
  backed by a regeneration gate before it is treated as source.

And one sequencing rule that costs a redraw if discovered late: decide **in
writing**, before drawing any package boundaries, between one root BUILD file and
a per-package output restructure, in any repository whose tooling writes to a
shared top-level output directory. Bazel requires a package's outputs to live
under that package's own output directory, and that constraint has exactly two
resolutions (BZL-ARCH-03).

```sh
find . -maxdepth 2 -type d \( -name dist -o -name build -o -name out \)
```

**Empty output = no shared output root = no decision forced.** Any hit above an
intended BUILD boundary means the decision is owed now, in writing.
