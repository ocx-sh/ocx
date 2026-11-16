# Research: replacing the CMake examples

Snapshot: 2026-11-16, worktree `uno`, other workers were editing docs pages while this ran (counts are line counts of that moment). Read-only; nothing in the repo was changed except this file.

## Summary

- CMake appears in 58 docs pages (about 290 lines), 41 of the 91 tested doc scripts (113 lines), 7 casts, 3 `setup:*` fixtures, the generated reference (from about 16 Rust help/doc-comment lines), `README.md`, `packaging/ocx/CATALOG.md`, the glossary, `product-context.md` and the docs skill.
- Doc scripts do not use the public registry. They run against a local zot test registry filled with **fake packages**: a shell-script stub that prints a hand-typed `--version` string (`test/recordings/setups.py` via `make_package`, `test/src/helpers.py`). The tag is a fixture label and the printed version is a separate literal. That is why `cmake:4.2.0` prints `4.2.3`, `uv:0.10.0` prints `0.10.10`, `bun:1.3.0` prints `1.3.10`.
- "Use the real packages" therefore has two readings: (1) real names, tags and version output in the existing fixtures (cheap, edits about 41 scripts, 3 setups, 1 state table, the pages), or (2) doc scripts that install real binaries from `ocx.sh` (new mechanism, needs network, 16-70 MB per package). No script does (2) today.
- Real packages exist on `ocx.sh` for everything needed: `astral-sh/python-build-standalone` (tags `3`, `3.14`, `3.14.7`, `3.14.7_20260814`, `latest`, plus the real `slim-` variant), `nodejs/node` (tags `22`, `24`, `24.21`, `24.21.0`, `latest`; no build-suffixed tags), `astral-sh/uv`, `pnpm/pnpm`, `oven-sh/bun`, `denoland/deno`, `pypa/pipx`.
- None of node, python, uv, pnpm, bun, deno, go or the real `kitware/cmake` declares any variable besides `PATH`. The `CMAKE_ROOT`, `NODE_HOME`, `BUN_HOME`, `PYTHON_HOME` variables in docs and fixtures are invented. Real non-PATH variables: `JAVA_HOME` (`amazon/corretto`, `adoptium/temurin`), `PYTHONPATH`/`PYTHONDONTWRITEBYTECODE` (`pypa/pipx`, private), `MANPATH` (`git-lfs/git-lfs`).
- Only `pypa/pipx` among python/node relatives has a dependency (private, on python-build-standalone) and real entry points (`pipx`, `python`).
- A real variant exists only for Python (`slim`). Node has none; the docs already contain a non-existent variant (`debug-3.12` on python-build-standalone).
- Not every CMake hit is an example: CMake is the subject in the "CMake vocabulary" notes (visibility analogy) and the link to the CMake integration site; those should stay.

## 1. Where CMake appears

Counted with `grep -i cmake`, excluding `.git`, `target`, `node_modules`, `external`.

| Kind | Files | Lines | Notes |
|---|---|---|---|
| (a) hand-written docs pages `website/src/content/docs/**` | 58 | about 290 | split below |
| (b) tested doc scripts `test/doc_scripts/*.sh` | 41 of 91 | 113 | see below |
| (c) casts `website/src/public/casts/**` (git-ignored, generated) | 7 | 23 | see below |
| (d) fixtures and tests: `test/recordings/setups.py` 37, `test/src/state_providers.py` 4+, `test/src/helpers.py` 3, `test/recordings/test_cast_realign.py` 6, `test_truncate_digests.py` 5, `test/lint/test_doc_scripts_parser.py` 7, `test/tests/*.py` 185 in 28 files, `test/bench` 3, `test/ocx.toml`+`ocx.lock` 3 | about 45 files | about 300 | acceptance tests use "cmake" as an arbitrary unique repo name (see section 5) |
| (e) Rust under `crates/**` | 150 files | 2275 | almost all unit-test fixtures; user-visible text is about 16 doc-comment lines (list below) |
| (e) generated reference + schemas | `reference/command-line/*` (commands, index-commands, package-consuming, package-publishing), `website/src/public/schemas/{project,project-lock/v4,config,execution-record}`, goldens in `crates/ocx_schema/tests/golden/*` (10 lines in 6 files), `crates/ocx_sdkgen/tests/golden/rust/commands.rs` (3), `crates/ocx_schema/contract/baseline/cli.json` (2) | | | generated from Rust doc comments by `task website:reference:generate` (`website/reference.taskfile.yml`) |
| (f) glossary `website/src/data/glossary.json:874`, `reference/glossary.md:265` (generated from json), `README.md` 4, `packaging/ocx/CATALOG.md` 4, `.claude/rules/product-context.md` 9, `.claude/rules/subsystem-tests.md` 3, `.claude/rules/quality-rust-errors.md` 2, `.claude/rules/docs-style.md` 1, `.claude/skills/docs/references/terminal-commands.md` 2, `.github/ISSUE_TEMPLATE/package_request.yml:9` (name example), `.opencode/**` mirrors | | about 45 | |
| (g) outside `website/`: README and `packaging/ocx/CATALOG.md` only (both show `cmake:4` install/exec); no installer script shows cmake | | | |
| planning records `.claude/artifacts/**` | 107 | 669 | historical; not docs |

### (a) Docs pages by kind (line classification by script)

Totals: prose or inline code 139, plain fences 78, fences inside `<ShellSession>` 42 (plus 1 ShellSession-line), table rows 15, link definitions 8, `<Tree>` node/description lines 8. (`reference/dependencies.md` is a false hit: a crate list.)

| Page | Lines | prose | fence | ShellSession output/commands | table | tree/link-def |
|---|---|---|---|---|---|---|
| explanation/versioning.mdx | 17 | 7 | 5 | | 5 | |
| explanation/entry-points.mdx | 15 | 11 | 4 | | | |
| getting-started/first-project.mdx | 15 | 4 | 1 | 10 | | |
| getting-started/first-package.mdx | 14 | 4 | | 10 | | |
| publishing/declare-dependencies.mdx | 13 | 5 | 6 | | | 2 |
| publishing/repackage-releases.mdx | 13 | 2 | 10 | | | 1 |
| publishing/entry-points.md | 12 | 6 | 5 | | | 1 |
| explanation/environments.mdx | 9 | 6 | | 1 | | 2 |
| explanation/project-toolchain.md | 9 | 5 | 4 | | | |
| explanation/local-index.mdx | 8 | 6 | 2 | | | |
| guides/choose-versions.mdx | 8 | 3 | | | 5 | |
| guides/project-toolchain.mdx | 8 | 4 | 3 | 1 | | |
| explanation/storage.mdx | 7 | 4 | | | | 3 tree |
| guides/find-packages.mdx | 7 | 4 | | 3 | | |
| explanation/indices.md | 6 | 5 | 1 | | | |
| publishing/env-surface.mdx | 6 | 3 | 1 | | 1 | 1 |
| integration/github-actions.mdx | 6 | 1 | 5 | | | |
| integration/gitlab-ci.md | 6 | 1 | 5 | | | |
| guides/work-offline.mdx | 6 | 3 | | 3 | | |
| explanation/public-index.mdx | 5 | 2 | 1 | | | 2 tree |
| reference/metadata/index.md | 5 | 1 | 3 | | 1 | |
| guides/global-toolchain.mdx | 5 | 4 | 1 | | | |
| guides/run-project-tools.mdx | 5 | 3 | 1 | | 1 | |
| guides/stable-paths.mdx | 5 | 2 | 2 | | 1 | |
| 4-line pages: explanation/toolchain-tree, explanation/execution-records, explanation/configuration, reference/execution-records, reference/metadata/entry-points | 20 | | | | | |
| 3-line pages: index.mdx, explanation/toolchain-render, explanation/shell-integration, explanation/launcher-forwarding, explanation/records-and-sbom, publishing/bundle-anatomy, reference/command-line/package-publishing, guides/corporate-mirror, guides/update-tools | 27 | | | | | |
| 2-line pages (9): deferred-packages, reference/env-composition, reference/configuration, reference/metadata/env, reference/metadata/dependencies, reference/command-line/{package-consuming,commands,index-commands}, integration/index, guides/lock-and-reproduce, guides/patch-packages | about 20 | | | | | |
| 1-line pages (12): explanation/{project-groups,toolchain-activation}, publishing/script-tests, reference/{machine-interface,glossary}, integration/{docker,devcontainers-and-editors,unattended-ci}, guides/{serve-an-index,clean-up,update-ocx} | 11 | | | | | |

Casts embedded in pages (`<Terminal src=...>`): lifecycle in `explanation/deferred-packages.mdx:27` (title says "Deferring cmake until first use"), toolchain-activation in `guides/project-toolchain.mdx:401`, pinned in `guides/run-project-tools.mdx:114`, patches-consumer in `guides/patch-packages.mdx:19`, toolchain-bin-mode in `guides/global-toolchain.mdx:136`, toolchain-state in `troubleshooting/shell.mdx:118`.

### (b) Doc scripts

41 scripts, by `# state:`: `setup:full-catalog` 27, `setup:cmake-versions` 12 (`first-package__{install,install-select,uninstall,exec,env,clean,select-back}`, `first-project__{init,add,exec,pull,offline}`), `setup:patches-consumer` 1, `setup:managed-config-ci` 1. They name the package only through `$PKG_KITWARE_CMAKE` / `$REPO_KITWARE_CMAKE` (derived from the display name by `helpers.env_key`), plus literal `cmake --version`, `command -v cmake`, `.ocx/toolchain/active/bin/cmake`, `./.tools/cmake`, `ocx exec cmake --`, `ocx --global remove "$REPO_KITWARE_CMAKE"`, `ocx install --select $REPO_KITWARE_CMAKE:4.0`. Scripts that assert on the string: `user-guide__toolchain-bin-mode.sh:18-30` and `user-guide__bin-mode-render.sh:14` (path `.../bin/cmake`), `lazy-loading__lifecycle.sh:22-25` (`cmake_kind`), `user-guide__toolchain-activation.sh:18-19`. The comment at `toolchain-bin-mode.sh:22-26` says the runner image ships a real cmake, which is why the script asserts the resolved path (the same hazard applies to a real `python3`/`node` on a runner PATH).

Binding page to script: pages include `<<< @/_scripts/<slug>.sh`; slug = `# doc:` header; `website/scripts/publish_doc_scripts.py` copies each displayed script (region `cast` only, `render_display`) to `website/src/_scripts/<slug>.sh` (git-ignored, manifest `.published.json`); `test/src/doc_binding.py` (NC1-NC4, RG3) checks every transclusion resolves and that no page has an inline `ocx` shell fence. `test/src/doc_scripts.py` substitutes `$PKG_*` to canonical short refs for display (lines about 480-512). Scripts bound to two pages: `user-guide/install-link` (first-package, stable-paths), `user-guide/ci-project` (first-project, github-actions), `first-package/exec` (first-package, index.mdx).

### (c) Casts

Cast files are generated, not tracked (`git ls-files website/src/public/casts` is empty); they are produced by `# cast:` scripts through `//test/doc_scripts:casts` (`test/doc_scripts/cast.bzl`, `website/recordings.taskfile.yml`, `test/recordings/cast_recorder.py`).

| Cast | Script | State | cmake lines |
|---|---|---|---|
| lazy-loading/lifecycle | `lazy-loading__lifecycle.sh` | full-catalog | 6 (cmake + uv, prints `cmake version 4.2.3`) |
| user-guide/toolchain-bin-mode | `user-guide__toolchain-bin-mode.sh` | full-catalog | 4 |
| user-guide/toolchain-activation | `user-guide__toolchain-activation.sh` | full-catalog | 4 |
| reference/command-line/pinned | `command-line__pinned.sh` | full-catalog | 4 |
| user-guide/patches-consumer | `patches__consumer.sh` | patches-consumer | 2 |
| in-depth/shell-integration/toolchain-state | `shell-integration__toolchain-state.sh` | full-catalog | 2 |
| getting-started/index | `index.sh` | full-catalog | 1 (catalog listing `kitware/cmake`) |

The `first-package__*` and `first-project__*` scripts carry `region cast` but no `# cast:` header, so they produce no cast. The other getting-started casts (`getting-started/{install,exec,env,...}.cast`) exist on disk from older scripts and do not mention cmake (not verified whether they are stale).

### (e) Rust text that reaches users

Doc comments / help with cmake: `crates/ocx_cli/src/command/add.rs:39,42`, `index.rs:14`, `remove.rs:18,19,29`, `upgrade.rs:38`, `update.rs:389`, `which.rs:28`; `crates/ocx_config/src/lib.rs:208` (-> `config.md`, `schemas/config/v1.json`); `ocx_package/src/metadata/visibility.rs:43` (CMake analogy, subject not example); plus identifiers in `project`/`project-lock`/`execution-record` schemas. Changing them changes generated `reference/**`, `schemas/**` and goldens (`task website:reference:generate`; golden update). `crates/ocx_schema/contract/baseline/cli.json` holds 2 lines; whether a help-text change needs a ledger entry is not verified (the contract gate is about machine documents, help text is part of `cli.json`).

## 2. How doc scripts get their packages

- States are registered in `test/src/state_providers.py` (`DECLARED_PACKAGES` literal table, lines 255-345; setups table at `test/recordings/setups.py:979+`). `provision()` pushes fixture packages to the session-local zot registry (`test/src/zot.py`, `test/conftest.py`), UUID-prefixed repo names (`t_<hex>_kitware/cmake`, SP7), then display rewriting maps them back to `kitware/cmake` / `ocx.sh/...`.
- Both mechanisms in one answer: scripts never reach `ocx.sh` / `index.ocx.sh`. Every package is a fake: `make_package` (`test/src/helpers.py:672`) builds a one-file archive whose binaries are shell scripts echoing a marker, or, with `outputs={...}`, the exact text written in `setups.py` (`cmake_versions` 3.31 -> `cmake version 3.31.12`, 4.0 -> `4.0.7`; `full_catalog` cmake `4.2.0` -> `4.2.3`, uv `0.10.0` -> `uv 0.10.10`, node `24.0.0` -> `v24.14.0`, bun `1.3.0` -> `1.3.10`, corretto `21.0.0` -> `21.0.10`). `size_mb` defaults to 0, so downloads are bytes.
- The tag you see in prose and the version the stub prints are independent literals in `setups.py`; nothing checks them against each other or against the real packages. Hand-written `<ShellSession>` output in `first-package.mdx`, `first-project.mdx`, `index.mdx` repeats the stub strings (`cmake version 3.31.12`, `4.0.7`) as typed copies; no test compares them to the script output (`.claude/artifacts/research_docs_getting_started.md:114,214` records this as an accepted honesty gap).
- Environment of the fakes is invented: cmake gets `PATH` (+ `MANPATH` in `full_catalog`), node `PATH` (+ `NODE_HOME` in `dependencies`/`deps_visibility`), bun `BUN_HOME`, python `PYTHON_HOME` in `variants`, corretto `JAVA_HOME`.
- Existing fixtures that already carry the target names: `nodejs/node` (`24.0.0`, in `full_catalog`, `dependencies`, `deps_visibility`), `astral-sh/python-build-standalone` (`3.13.14`, `slim-3.13.14` in `setups.py:variants`, state `setup:variants`), `astral-sh/uv` (`0.10.0`, state `setup:basic`), `oven-sh/bun`, `amazon/corretto` (21 and 25, `setup:multi-version`, `JAVA_HOME`). The fixture comment at `setups.py` (`variants`) already says the real package publishes exactly one variant prefix `slim`.
- Cost of (2) real packages in doc scripts: not implemented; would need network in the doc-script lane and a way to select the public index; sizes below. Not verified: whether `task test:doc-scripts` lanes have network.

## 3. Real Python and Node.js packages

Commands run (all read-only, `OCX_HOME` set to scratch dirs `.../scratchpad/ocxhome-research{,2,3}/`, binary `target/release/ocx`): `ocx --remote index catalog`, `ocx --remote index list <repo>`, `ocx --remote index list --platforms <id>`, `ocx --remote --format json package inspect --resolve -p linux/amd64+libc.glibc <id>` (nothing installed), plus the already-installed repo toolchain (`python3 --version`, `node --version`, `npm --version`, `pnpm --version`, `uv --version`). Quirk observed: a second `ocx --remote index catalog` against an existing but empty `OCX_HOME` failed with an `ocx.toml: I/O error`; against a not-yet-existing dir it worked. Not investigated.

Repo toolchain (`ocx.toml` `[tools]`, pinned to `ocx.lock`):

| Binding | Identifier | Lock platforms |
|---|---|---|
| `python` | `ocx.sh/astral-sh/python-build-standalone:3.14.7_20260814` (comment: plain `3.14.7` rolls to the newest build) | darwin/amd64, darwin/arm64, linux/amd64+glibc, linux/amd64+musl, (arm64 variants), windows |
| `node` | `ocx.sh/nodejs/node:24.21.0` | darwin/amd64, darwin/arm64, linux/amd64+glibc, linux/arm64+glibc |
| `uv` | `ocx.sh/astral-sh/uv:0.12.5` | darwin x2, linux/amd64 (+glibc), linux/arm64, windows |
| `pnpm` | `ocx.sh/pnpm/pnpm:11.22.0` | darwin/arm64, linux glibc+musl, ... |
| others | `bazel`, `cosign`, `git-cliff`, `task`, `lychee`, `prek`, `shellcheck`, `shfmt`, `zot`, `actionlint`, `agg` | |
| `test/ocx.toml` | `cmake`, `uv` pointing at `localhost:5000/t_da5a15bc_...` (a stale local-fixture lock) | |

Public index (catalog of 144 repositories): all of the below are on `ocx.sh`.

| Candidate | Example tags | Platforms (for the tag shown) | Binaries (metadata `binaries`) | Env besides PATH | Dependencies | Layer size (linux/amd64 glibc) | Real `--version` |
|---|---|---|---|---|---|---|---|
| `astral-sh/python-build-standalone` | `3`, `3.13`, `3.14`, `3.14.7`, `3.14.7_20260814` (build tag), `latest`; variant `slim-3`, `slim-3.14`, `slim-3.14.7` (only variant prefix) | darwin amd64+arm64, linux amd64/arm64 x glibc/musl, windows/amd64 | `python`, `python3`, `python3.14`, `pip`, `pip3`, `pydoc3`, `idle3`, `python3-config`, ... | none (PATH only) | none | `3.14.7_20260814` 69.5 MB; `3.13` 54.3 MB; `slim-3.14.7` 29.2 MB | `Python 3.14.7` |
| `nodejs/node` | `22`, `22.23`, `24`, `24.21`, `24.21.0`, `26`, `latest`; no build tags, no variants | darwin amd64+arm64, linux amd64+arm64 glibc only (no musl), windows amd64+arm64 | `node`, `npm`, `npx`, `corepack` | none (PATH only) | none | `24.21.0` 39.1 MB; `22` 38.1 MB | `v24.21.0`; `npm --version` 11.19.0 |
| `astral-sh/uv` | `0.12`, `0.12.5`, `latest` | darwin x2, linux amd64/arm64 (plain and +glibc), windows x2 | `uv`, `uvx` | none | none | 16.7 MB | `uv 0.12.5` |
| `pnpm/pnpm` | `11`, `11.22`, `11.22.0`, `12`, `latest` | darwin, linux glibc+musl, ... | `pnpm` | none | none | 37.3 MB | `11.22.0` |
| `oven-sh/bun` | `1`, `1.3`, `1.4.4`, `latest` | not queried | `bun` | none | none | 42.8 MB | not run |
| `denoland/deno` | `2`, `2.9`, `2.9.8`, `latest` | not queried | `deno` | none | none | 34.1 MB | not run |
| `pypa/pipx` | `1`, `1.18`, `1.18.16`, `latest` | darwin x2, linux x4, windows/amd64 | none (`binaries: []`) | `PYTHONPATH` (path, private), `PYTHONDONTWRITEBYTECODE=1` (constant, private) | `ghcr.io/ocx-contrib/astral-sh/python-build-standalone:3.13.14@sha256:...` (private) | not queried | not run |
| `pdm-project/pdm` | | | `pdm` | none | none | 22 MB | not run |
| `kitware/cmake` (for comparison) | `3.31` | | `ccmake`, `cmake`, `cmake-gui`, `cpack`, `ctest` | **none** (PATH only; no `CMAKE_ROOT`, no `MANPATH`) | none | 44.7 MB | |

Scan of the latest tag of 138 of 144 catalog repositories (host platform linux/amd64 glibc; 6 not scanned: `docker/docker-credential-{osxkeychain,wincred}`, `extra-code/{container,parbake}`, `meta/zstd` and one other, no platform match or parse failure): the only packages with env other than PATH are `amazon/corretto` and `adoptium/temurin` (`JAVA_HOME`, constant, public), `anomalyco/opencode` (`OPENCODE_DISABLE_AUTOUPDATE`), `git-lfs/git-lfs` (`MANPATH`), `pypa/pipx`. The only package with dependencies or entry points is `pypa/pipx` (`entrypoints`: `pipx` with args, `python`). `golang/go`, `ruff`, `just`, `pdm` are PATH-only.

Note for docs that say a bare-binary package "declares `PATH`, the `*_HOME` variable and sometimes `MANPATH`" (`publishing/env-surface.mdx:94`, `:102`): that does not hold for real cmake, node or uv.

## 4. What each CMake example shows, by role

Fit statement uses only facts from section 3.

| Role | Pages and scripts | Python | Node.js | Verdict |
|---|---|---|---|---|
| Run once (`package exec`) | `index.mdx:20-29`, `first-package.mdx` (exec), scripts `first-package__exec`, `user-guide__exec-once` | `python3 --version` | `node --version` | either; both single-command |
| Install two versions and switch (`candidates/3.31` -> `4.0`, `current`) | `first-package.mdx:40,69,79-103`, `first-package__{install,install-select,select-back,clean,uninstall}`, `choose-versions.mdx`, `stable-paths.mdx`, `versions-select`, `storage.mdx:131-294`, `toolchain-tree`/`toolchain-render` trees | `3.13` and `3.14` both real, rolling-minor tags | `22` and `24` both real (two LTS lines, `v22.x`/`v24.x`) | either; node tags are simpler (no build suffix) |
| Rolling tags and version selection (`3.28.1_2026...` build tag, `3.28`, `3`, `latest`) | `versioning.mdx:11-165`, `choose-versions.mdx:36-44`, `local-index.mdx`, `indices.md`, `public-index.mdx` | Python fits: real `3.14.7_20260814` build tag, `3.14.7`, `3.14`, `3`, `latest` | Node lacks build-qualified tags | Python |
| Variants | `choose-versions.mdx:42,44`, `update-tools.mdx:68`, `variants` cast | Python fits: real `slim-` prefix | Node has no variants | Python (the pages' `debug-3.12` / `debug-3.28` examples are not real variants) |
| Variable besides PATH | `entry-points.mdx:24` (`CMAKE_ROOT`), `env-surface.mdx:16,60-102`, `environments.mdx`, scripts with `ocx package env` | none declares one | none declares one | neither; real ones: `JAVA_HOME` (corretto/temurin, already used elsewhere in docs), `PYTHONPATH` (pipx, private) |
| Dependencies | `declare-dependencies.mdx:20-100` (`${deps.cmake.installPath}`, name collision `myorg_cmake`), `entry-points.md:65-68`, `reference/metadata/{dependencies,env,entry-points}.md`, `package-consuming.md:16` | `pypa/pipx` -> python is a real dependency; python as the runtime a wrapper depends on | `node` as runtime a wrapper depends on (a wrapper "runs the dependency at run time" fits an interpreter better than a build tool); current fixtures already use `nodejs/node` + `oven-sh/bun` for dep demos | Python or Node as the dependency; `${deps.node.installPath}`/`${deps.python.installPath}` |
| Entry points (launchers `cmake`, `ctest`, `cpack`) | `entry-points.mdx` (15 hits, Windows launcher prose `cmake.exe`, `cmake.shim`), `publishing/entry-points.md`, `launcher-forwarding.md`, `reference/metadata/entry-points.md` | `pipx` is a real entry-point package (entrypoints `pipx`, `python`); python binaries `python3`, `pip` | node binaries `node`, `npm`, `npx`, `corepack`; `npm`/`npx` are scripts needing node | Python (real `pipx`) or Node; the sentence "CMake, ripgrep and mold have no runtime to conflict over" (`publishing/entry-points.md:27`) needs a self-contained binary such as `uv`, `ripgrep` or `go`, not python/node |
| Publish your own package | `bundle-anatomy.mdx:22-29` (archive `cmake-3.28.1-linux-x86_64`, strip components), `repackage-releases.mdx:65-120` (mirror yaml with GitHub release asset regexes, 13 lines), `build-and-push`/`test-locally` use `mytool` (no cmake) | upstream archive names not verified; a mirror yaml for python-build-standalone or uv would use `github_release`, real assets not verified | node distribution is nodejs.org, not GitHub releases: the `github_release` mirror source in `repackage-releases.mdx` does not fit; upstream archive name `node-v24.21.0-linux-x64` is from memory, not verified | `bundle-anatomy` either; `repackage-releases` needs an upstream that releases on GitHub (uv or python-build-standalone), not verified |
| Project toolchain (`ocx.toml`) | `first-project.mdx`, `project-toolchain.md`, `project-groups.mdx`, `deferred-packages.mdx:63`, `toolchain-activation.mdx:165`, `global-toolchain.mdx`, `run-project-tools.mdx`, `lock-and-reproduce.mdx`, `configuration.mdx` | `python = "ocx.sh/astral-sh/python-build-standalone:3.14"` (repo does exactly this) | `node = "ocx.sh/nodejs/node:24"` (repo does this) | either |
| CI snippets | `github-actions.mdx`, `gitlab-ci.md`, `unattended-ci.mdx`, `docker.mdx`, `devcontainers-and-editors.mdx:58` | either | either | either; Node is the common CI runtime |
| Lock and offline | `first-project.mdx` offline step, `work-offline.mdx`, `lock-and-reproduce.mdx`, `first-project__offline` | either | either | either |
| Index and registry identity (`ocx.sh/kitware/cmake`, `ocx index list kitware/cmake`, `cmake.json`) | `find-packages.mdx`, `public-index.mdx:15-109`, `local-index.mdx:17-193`, `corporate-mirror.mdx`, `index-commands.md` | real `astral-sh/python-build-standalone` (repository path `ghcr.io/ocx-contrib/astral-sh/python-build-standalone` seen in pipx dependency) | real `nodejs/node` | either; the example `root.repository` `oci://ghcr.io/ocx-contrib/cmake` and `$OCX_HOME/index/ghcr.io/p/ocx-contrib/cmake.json` shown in `local-index.mdx:101-103`, `public-index.mdx:36,106-109` is not verified against what the real tree holds for the replacement |
| Lazy loading (cmake + uv, `deferred-packages`, `lifecycle`) | `lazy-loading__lifecycle.sh`, `deferred-packages.mdx` | python | node | either; already pairs with `uv` |
| Patches (corp CA overlay on cmake) | `patches__consumer.sh`, `patch-packages.mdx`, state `setup:patches-consumer` | python (`SSL_CERT_FILE`-style overlays fit Python/Node, not verified) | node | not verified |
| Records and SBOM, execution records | `execution-records.mdx`, `records-and-sbom.mdx` (`cmake --build build`) | `python -m build` style | `node`/`npm run build` | either; `cmake --build build` has no direct equivalent in the stub fixtures |
| CMake is the subject, not an example | `environments.mdx:28-29,287-288,333-334` (CMake vocabulary / dedup contrast), `declare-dependencies.mdx:104-105,125-126`, `visibility.rs:43`, `integration/index.md:32,48` (link to the CMake integration site), `indices.md:94` (`ocx-actions/setup-cmake`) | not applicable | not applicable | stay as written, they explain the CMake analogy or point at the CMake integration |

## 5. Risks and couplings

- **Fixture rename fan-out.** `PKG_KITWARE_CMAKE`/`REPO_KITWARE_CMAKE` exist because `helpers.env_key("kitware/cmake")` -> `KITWARE_CMAKE` (`test/src/helpers.py:36`, asserted by `test/tests/test_state_providers.py:80-90`). Renaming the display name renames the variable in all 41 scripts (113 lines), `DECLARED_PACKAGES` (`state_providers.py:262-307`, cross-checked against the provisioned truth by DE3/DE6 in `test_state_providers.py`), `setup:cmake-versions` (12 scripts, plus the state name itself), `setup:patches-consumer` (`setups.py:473`), `setup:managed-config-ci`. The 12-script `cmake-versions` setup is also the template for a node/python two-version fixture.
- **Existing fixtures that already match.** `setup:variants` (python-build-standalone, `3.13.14` and `slim-3.13.14`), `setup:basic` (uv), `setup:multi-version` (corretto), and node/bun in `full_catalog`/`dependencies` can serve the replacements without new packages; new two-version node (`22`/`24`) or python (`3.13`/`3.14`) setups would be new code.
- **Stub versions diverge from real ones** (4.2.0 -> 4.2.3 style). With real packages the typed `<ShellSession>` outputs (`first-package.mdx`, `first-project.mdx`, `index.mdx`) can be made correct (`Python 3.14.7`, `v24.21.0`) but remain hand copies; the fixture `outputs` strings must be updated to match or the cast and page disagree.
- **Real runners ship node and python.** `toolchain-bin-mode.sh:22-26` already guards against a runner-provided cmake with a resolved-path assert; scripts using `command -v python3`/`node` need the same assert, and any `python3 --version` hit on host PATH would hide a broken toolchain. Moving the fake to node/python also means `ubuntu-latest` has both.
- **Tests asserting on the string `cmake` in output.** Doc-script-related: `toolchain-bin-mode.sh:27-30`, `bin-mode-render.sh:14`, `lifecycle.sh:22-25`; unit tests of the recording helpers (`test_cast_realign.py`, `test_truncate_digests.py`, `test/lint/test_doc_scripts_parser.py:789-960`) use `cmake` only as sample text and do not depend on the docs. The 28 files under `test/tests/` use `cmake` as an arbitrary unique repo name for `make_package` (and `test_entrypoints.py` 52, `test_upgrade.py` 25, `test_exec_modes.py` 14, ...); they are independent of the docs and need no change.
- **Casts.** All 7 are generated from scripts against the local fixture registry (no network required at record time); re-recording goes through `task website:recordings:*` / `//test/doc_scripts:casts` (`website/recordings.taskfile.yml`) and needs Bazel, a zot binary and Docker-free zot (`src/zot.py`); `lifecycle` and `pinned` embed digests and the `no trust policy covers 'kitware/cmake'` line, `index.cast` embeds the catalog listing; recorded casts are git-ignored so a rename changes nothing tracked but the published site until re-recorded.
- **Shared scripts.** `install-link` (first-package + stable-paths), `ci-project` (first-project + github-actions), `first-package/exec` (first-package + index.mdx): one edit changes two pages.
- **Generated outputs.** The Rust help lines (`add.rs`, `remove.rs`, `upgrade.rs`, `which.rs`, `index.rs`, `update.rs`, `ocx_config/src/lib.rs`) feed `reference/command-line/*`, `reference/configuration.md`, the JSON schemas and goldens; changing them needs `task website:reference:generate` and golden updates and a commit subject for the changelog.
- **Pages where cmake is also the subject:** see the last row of section 4. A blanket search-and-replace would turn "CMake's `target_link_libraries`" analogies and the CMake integration link into nonsense.
- **Hand-built example package.** The publishing pages' hand-built package is `acme/mytool` (`setup:publisher`, `patches-maintainer`, `signing`, `promotion`, 18 pages mention `mytool`), not cmake; the cmake parts of the publishing pages are `bundle-anatomy`, `declare-dependencies`, `entry-points`, `env-surface`, `repackage-releases`. Renaming `mytool` is not required by this request and would touch other states and scripts.
- **Package size if scripts ever download real ones:** python 54-70 MB (slim 29 MB), node 38-39 MB, uv 16.7 MB, pnpm 37 MB, versus bytes for the fixtures; a two-version switch downloads twice (node about 77 MB, python about 124 MB). The existing bench note (`test/bench/scenarios.py:528`) already calls real tool sizes "cmake ~30 MB, node ~70 MB" (the real cmake layer is 44.7 MB, node 39 MB; the note is out of date).
- **Windows/musl.** Node has no linux musl build and Python has `libc.glibc`/`libc.musl` builds; a doc that runs on Alpine shows a different platform list.

## Open questions for the planner

1. Reading (1) real names and outputs in the existing fake fixtures, or reading (2) doc scripts that fetch real packages from `ocx.sh`? (2) needs a new executor mode and network in the doc-script lane; the repo has no such lane today.
2. One replacement tool or several? Python covers rolling tags, build tag, variant and (via pipx) dependency and entry points; Node is simpler for CI and two-version switching but has no build tags, variants, env or dependencies.
3. For the "variable besides PATH" example, no real python/node package declares one. Keep `JAVA_HOME` (corretto), use pipx `PYTHONPATH`, or drop `CMAKE_ROOT` and rewrite the `*_HOME` claim in `env-surface.mdx`?
4. `repackage-releases.mdx` mirror yaml: replacement upstream must release on GitHub (uv, python-build-standalone) for `github_release`; real asset names of those releases are not verified here (the `ocx-mirror` repo is not on this machine).
5. Fix the non-real variant examples (`debug-3.12`, `debug-3.28`) together with this change?
6. Keep CMake-as-subject notes and the CMake integration link, and the `ocx.sh/cmake` flat-name examples in Rust help (those are identifiers, not CMake-the-tool)?
7. Whether `test/ocx.toml`/`ocx.lock` (cmake, uv at `localhost:5000/t_da5a15bc_...`) are a live fixture or leftover (not verified).
