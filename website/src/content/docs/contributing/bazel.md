---
title: "Building with Bazel"
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

This page covers how to build, test and read the Bazel graph behind OCX's Rust workspace. It runs from a fresh clone to a first build, the cached acceptance suite, the warm cache and the Grafana dashboard.

## Orientation {#contributing-bazel-orientation}

OCX's Rust workspace builds twice: once through `cargo`, and once through a second, parallel graph that [Bazel][bazel] walks to check the first one honestly. Eight gates run in two lanes. Seven of them run on every change. The eighth, the acceptance suite, runs on the deep lane only. Locally `task verify` runs all eight.

Four of the eight read the graph itself. They are a pin check, a loading-phase sanity build, a dependency-drift comparison against `Cargo.toml`, and a sandbox-tag audit. Two lint what declares it. `bazel:lint` runs buildifier over every `BUILD.bazel` and `.bzl` file. `bazel:mod:check` refuses a stale `MODULE.bazel.lock`.

The last two run tests. `bazel:test:unit` runs the workspace's own unit tests, floored per target off the build event stream rather than a bare pass/fail. `bazel:test:accept` runs the acceptance suite as one target per test module. The targets of `bazel:test:unit` include, one per library crate, a `rust_doc_test`, so the workspace's doctests run here too, never as a separate cargo step.

`bazel:test:accept` is the one that is not on every change. `verify-basic.yml` runs the other seven. It is the only workflow that runs the Bazel gates on a pull request. The acceptance gate lives in `verify-deep.yml`, which has no `pull_request` trigger.

Together the gates catch the class of defect a green `cargo build` cannot see on its own: a `BUILD.bazel` target that quietly stopped matching the crate it describes.

The same graph backs three more checks. `rust:clippy:check` runs clippy as a cached Bazel aspect. `rust:doc:ratchet` does the same for rustdoc. Both compare their diagnostics against a per-file baseline instead of failing on `-D warnings`.

`schema:generate` builds the JSON schemas from the `//crates/ocx_schema:schemas` genrule. So `task verify` runs none of `cargo build`, `cargo clippy`, `cargo doc` or `cargo test --doc`.

A second build system in an unfamiliar clone invites mistakes. A contributor installs the wrong version, hunts for a `WORKSPACE` file that does not exist, or wonders whether [bazelisk][bazelisk] belongs on `PATH`. None of that applies here.

Nobody types `bazel` directly. [`task`][task-runner] is the one command anyone runs. `bazel` itself is a pinned entry in `ocx.lock`, resolved the same way `git-cliff` or `shellcheck` are.

## First build {#contributing-bazel-first-build}

Before your first Bazel build, run `/init-bazel-config`, or `task bazel:doctor` for the checks alone. `.bazelrc` and `MODULE.bazel` are committed and already correct. A first build gets host state wrong, and no gate here can see it. That state is the output root, the RAM envelope and the remote-cache reader credential. It belongs in `~/.bazelrc`, never `.bazelrc.user`.

All eight live in `taskfiles/bazel.taskfile.yml` as `task` targets: `bazel:pin:check`, `bazel:build:nobuild`, `bazel:build:drift`, `bazel:tag:guard`, `bazel:lint`, `bazel:mod:check`, `bazel:test:unit`, and `bazel:test:accept`. `task verify` runs all of them. On CI the first seven are `verify-basic.yml`'s and `bazel:test:accept` is `verify-deep.yml`'s. None needs a `bazel` invocation typed by hand.

:::tip[Bazel is a pinned tool, not a system dependency]
`ocx exec bazel -- bazel --version` resolves the exact binary every one of those gates uses, straight from `ocx.lock`. It is the same mechanism that resolves `uv`, `shellcheck`, and every other tool in the [project toolchain][project-toolchain]. There is nothing to install with a system package manager and no [bazelisk][bazelisk] version file to maintain.
:::

Every gate uses the scoped `ocx exec bazel --` form, and that is deliberate. It names the one binding it needs. An unrelated tool with no leaf for this host therefore cannot block the run.

```sh
$ ocx exec bazel -- bazel --version
bazel 9.2.0
```

**Third-party crates need no setup step.** [rules_rs][rules-rs] resolves them straight from `Cargo.lock`, so a fresh clone builds without a generated lockfile, a repin or a bootstrap task. The three forks under `external/` are picked up from the `[patch.crates-io]` paths as they are.

**The C and C++ toolchain is hermetic.** The `llvm` module ([hermetic-llvm][hermetic-llvm]) supplies the compiler, linker and sysroots, so the build reads no host `gcc`, `libstdc++` or system linker.

**One Linux host builds every platform.** `task cross:targets` lists the supported triples and `task cross:build TARGET=<triple>` cross-builds `ocx` for one of them into `target/cross/<triple>/`. The Windows targets use `gnullvm`. Cross builds pass `-c opt`, because an unoptimised Windows binary overflows its 1 MiB main-thread stack at startup. On CI this is how macOS and Windows get their binaries: they are built once on Linux, and the native runners only download and execute them.

**Host state lives in `~/.bazelrc`, never a per-checkout file.** This machine carries four fixed worktrees of this repository plus disposable agent checkouts under `.agents/worktrees/`. Nothing host-specific should need setting up once per checkout. Examples are the JVM heap cap, the local `--jobs` cap and the cache reader credential below.

`bazel` already reads `~/.bazelrc` on its own, with no `import` line needed on either side. It reads the file after the workspace's committed `.bazelrc` and before any `--bazelrc=` flag:

```sh
# ~/.bazelrc -- host-wide, outside every checkout by construction, read from every worktree
startup --host_jvm_args=-Xmx2g
build --jobs=12
```

`.bazelrc.user` is still gitignored, and `.bazelrc` still `try-import`s it as its last line. The file is a comment-only stub for a genuine per-checkout experiment, such as one flag you want on this worktree and nowhere else. It is never for anything that describes the machine.

With that in place, the loading-phase gate is what actually exercises the graph:

```sh
$ ocx exec bazel -- bazel build --nobuild //...
```

```text
Analyzing: 287 targets (0 packages loaded, 0 targets configured)
INFO: Analyzed 287 targets (708 packages loaded, 27090 targets configured).
INFO: Found 287 targets...
INFO: Build completed successfully, 0 total actions
```

Zero actions, on purpose: `--nobuild` only loads and analyzes. `bazel:build:drift` and `bazel:tag:guard` read the same graph next, each against a floor of its own. Drift reads `//crates/...` and refuses to report on fewer than 98 targets across 24 packages. The tag guard reads the widest scope of all: measured today (`bazel query 'kind(rule, //...)'`), 329 rule targets over `//...`, up from 143 over `//crates/...` plus `//test/doc_scripts/...`. It had to widen to reach the acceptance package, because that is where its one narrowed clause lives.

The blanket rule covers a target that runs outside the sandbox. Such a target carries the cache-excluding tag its kind was measured to need: `external` for a test action, `no-remote-cache` for a build action. The acceptance targets break the rule on purpose. There is one per acceptance test module. Their results **are** cached, so `external` is exactly the tag they must not carry.

One module, `test_windows_shim.py`, keeps `external` anyway, via `UNCACHED_MODULES` in `test/bazel.bzl`. Its Windows-only `_find_shim_binary` fallback reads `target/{release,debug}/` on two adjacent lines. The `--allow` of the test-diff guard works one line for one line. It cannot express dropping both at once without hiding the read from the tag guard instead of fixing it. The module is skipped off Windows, so the Linux acceptance lane pays one extra collection and hides no verdict.

What stands in for it is a declaration. An acceptance target is credited only while its declared inputs include `//test:docker-compose.yml`, the compose definition, and `//test:suite_anchor`, the group carrying the `ocx` binary under test. Those are the two inputs whose change a cached pass would otherwise hide. Losing either turns the exemption off and the gate red. A `no-sandbox` test target anywhere else in the graph is judged by the blanket rule however it declares its inputs.

287 is what a wildcard build or test reaches, not the size of the whole graph. `bazel query 'kind(rule, //...)'` still lists 329. The 42 more are the GIF-render targets below. They are tagged `manual`, so wildcard expansion skips them while `bazel query` keeps naming every one.

Those 42 are one `genrule` per cast recording, which renders the `.cast` through [agg][agg] to an animated `.gif`. Beside them sit the `:gifs` filegroup that collects the 39 renders, the `:gif_check` test that proves them, and `:gif_check_probe`, the generated script it runs. Nothing on the site consumes a GIF. The site plays the `.cast` file directly.

A GIF is for a README or a social-media embed, where a human picks one file rather than a build step. That is why `manual` sits on all 42. It takes them out of `//...` and `bazel test //...` while leaving them reachable by label.

The one consumer is `task recordings:gifs`. It is not `task website:recordings:gifs`, which `website/taskfile.yml` includes with `internal: true` and refuses to run. `recordings:gifs` runs `bazel test //test/doc_scripts:gifs //test/doc_scripts:gif_check`, so the render proof still runs every time the task does. A `manual` target that no task names is a proof that has stopped running.

agg rasterises text, so the GIF targets need a monospace font on the host. `ubuntu-latest` carries DejaVu Sans Mono and resolves on the default family list of agg. A host with none of the listed families fails the build with `no faces matching font family options` rather than rendering blanks.

:::caution[A development host can carry none of the fonts on that list]
Measured on this machine: 136 fonts installed. `fc-list :spacing=mono` names exactly two monospace families, `Adwaita Mono` and `Nimbus Mono PS`. Neither is DejaVu or Liberation. Running `agg` with its own built-in default family list here writes nothing and exits 1 with `no faces matching font family options`.

`//test/doc_scripts`'s `gif.bzl` widens the font list past the default of agg for exactly this reason. It appends both families this host actually has. A probe for the condition should resolve the binary of `fc-list` explicitly rather than trust `$PATH`. This host happens to expose it at both `/usr/bin/fc-list` and `/usr/sbin/fc-list`, because the merged-usr layout of Fedora symlinks the two. That mapping is not a property every distribution shares.
:::

That gate also writes a [Build Event Protocol][bazel-bep] stream. Four tasks write one: `bazel:build:nobuild`, `bazel:test:unit`, `rust:clippy:check` and `rust:doc:ratchet`. Each writes to its own 0700 `mktemp -d` directory outside the checkout and removes it on exit. Only the `bazel:build:nobuild` stream is exported to `otel.ocx.sh`. Bazel serialises the whole client environment and every rc-file flag value into that stream, credentials included.

The directory is therefore deleted whatever the build did.

## The acceptance suite, one target per module {#contributing-bazel-acceptance}

`task bazel:test:accept` runs `bazel test //test:all --local_test_jobs=<n>`. Here `<n>` is the smaller of 8 and the machine's core count, and `ACCEPT_JOBS=<n>` overrides it. The target set is one `sh_test` per acceptance test module, named `//test:<module stem>`.

Check the live `ACCEPTANCE_MODULE_TARGETS` in `scripts/bazel_gate_proofs.py`, because the count changes as modules are added, move to the uncached `test/lint/` tier or get ported to Rust. Each target shells to `uv run pytest` against the docker-compose services this repository already starts. The task replaces the direct `task test:parallel` call in `task verify` and in the acceptance job of `verify-deep.yml`. `task test:parallel` still exists and still runs the suite through pytest-xdist. Its pytest step never overlaps a bazel run on one machine (see below for what does).

What the lane buys instead is **selection and caching**. Edit one module and one target re-runs, while editing a doc re-runs none. The structural sweeps (`test_smoke_coverage`, `test_no_crate_path_assertions`, `test_patch_global_slot`, `test_doc_scripts_publish_structure`) live in the uncached lint tier (`test/lint/`, `task test:lint:structure`), outside this package. Run it twice on an unchanged tree and Bazel reports `Executed 0 out of <n> tests`. That is only sound because the inputs are declared.

Each target names these inputs:

- its own module
- `//test:suite_inputs`, the shared group carrying `conftest.py`, `pyproject.toml`, `uv.lock`, the fixture tree and `docker-compose.yml`
- the `ocx` binary under test, which is Bazel's own `//crates/ocx_cli:ocx`, run straight from its runfiles
- its `module_data`, the files only that module reads, such as a scenario, a spec or a recording

Edit a file in one module's `module_data` and only that module re-runs. Change Rust code so the binary's bytes change and every target re-runs.

The targets run concurrently, and each one starts its own `zot` registries on free ports, so two targets share no registry state. The toolchain's `@tools//:zot` is a `data` input of every target. The runner supplies what xdist would otherwise provide:

- the `zot` launcher as an absolute path, in `OCX_TEST_ZOT`
- a pytest temp directory per target

Only the `registry:2` mirror and the Sigstore stack stay in Docker, and a test starts them on first use.

A target that has to wait for the lock on its temp directory prints `acceptance sh_test: <module> is waiting for <lock>` to stderr first. Only two runs of the same target, such as `--runs_per_test`, share that directory.

The stale-run eviction and binary rebuild before the pytest step of `task test:parallel` take no lock, and neither does `task test:smoke`. Do not start either in a checkout whose bazel run is still going. The eviction stops every process running that checkout's `test/bin/ocx`.

A host without `flock(1)` refuses to run the targets unless you pass `--test_env=OCX_ACCEPTANCE_UNSERIALISED=1`. That flag runs them with no temp directory lock, so pair it with `ACCEPT_JOBS=1`. `--local_test_jobs` stays on the command line of the task and in no rc file. An rc-file line applies per command rather than per target pattern. It would also throttle the 35 Rust test targets.

Pass `NOCACHE=1` to add `--nocache_test_results` when you want the run regardless of what the cache says. It is deliberately not `--force`. This repository tells everyone to type `task verify --force`. Binding the escape hatch to it would have made every full gate re-run every target.

The caching would then be unreachable from the one command that runs the lane. `release:prepare` always passes `NOCACHE=1`, through its `release:verify` hook, because a release re-executes the suite rather than trusting a cached verdict.

The lane also carries the shape gates of the suite, which live in the Bazel acceptance lane. `test/SUITE_FLOOR` means the suite never shrinks. `test/SKIP_CEILING` and `test/XFAIL_CEILING` catch a skip added to dodge a red.

The gates read the per-case JUnit reports Bazel writes, copied out of `bazel-testlogs` into `target/bazel/accept/<module>/junit.xml`. This is real per-test data, because the acceptance runner passes `--junit-xml=$XML_OUTPUT_FILE`. The `test.xml` files of the unit lane are Bazel's synthesised one-testcase-per-target files and are not the same thing.

:::caution[What a cached acceptance pass does not cover]
The running compose stack is not an action input. `docker-compose.yml` is, so a bumped service image or a moved port re-keys every target. A stack that is *up with different content* does not. Neither does the handful of modules that read `crates/**`, `website/src/content/docs/**` or the `//test/doc_scripts` package across a package boundary. The `ocx_schema` binary read is declared today: `//crates/ocx_schema:ocx_schema_bin` on the two modules that need it.

The module docstring of `test/bazel.bzl` lists every remaining gap with its risk. The gaps are declared rather than closed. `//crates/...` and `//website/...` have gates of their own that red on their own content.
:::

## Warm cache {#contributing-bazel-warm-cache}

The committed `.bazelrc` sets `build --disk_cache=~/.cache/ocx/bazel-disk` unconditionally. Never anywhere under `/tmp`: on a typical development host that path is a tmpfs an hourly reaper clears mid-build. Never inside the workspace either, because a relative `--disk_cache` resolves against the *client's* working directory rather than the workspace root. That silently forks a second, empty cache the moment `bazel` runs from a subdirectory. The `~`-rooted absolute path means one cache, shared by every worktree on the machine, with no setup step.

:::note[The same idea `sccache` already applies to `cargo`]
This repository's own CI already runs Rust compiles through [sccache][sccache] (`SCCACHE_AWS_ACCESS_KEY_ID` in the workflow). A second `cargo build` therefore skips work the first one already paid for. Bazel's `--disk_cache` is the same trade for the Bazel graph. It is an action cache keyed by the exact inputs that produced it. A rebuild with nothing relevant changed reads results back from disk instead of recompiling.
:::

Measured here on a single crate, `//crates/ocx_exit:ocx_exit` with 336 actions, and with `bazel clean` between runs. That way the disk cache does the work, not the in-memory analysis cache of the Bazel server:

```text
# first build, empty disk cache
INFO: 336 processes: 233 internal, 103 linux-sandbox.
INFO: Build completed successfully, 336 total actions
INFO: Elapsed time: 29.553s

# same target, same disk cache, after `bazel clean`
INFO: 336 processes: 103 disk cache hit, 233 internal.
INFO: Build completed successfully, 336 total actions
INFO: Elapsed time: 1.913s
```

**The remote cache is a second layer above that, and the reader realm is live.** The committed `.bazelrc` points at it with `build --remote_cache=https://bazel-cache.ocx.sh/v1`, beside `build --remote_upload_local_results=false`. The file says of that line: "a contributor and a fork PR never write"; only CI lanes holding the write secret do, through an rc they pass on the command line. Reading it needs a `--remote_header` credential line in your own `~/.bazelrc`, never a repo file. Ask a maintainer for a `dev-read` account:

```sh
# ~/.bazelrc, appended to the block above
build --remote_header="authorization=Basic <base64 of user:password>"
```

:::caution[The line is whitespace-tokenized, so quote the header value]
A `.bazelrc` line splits on whitespace the same way a shell command line does. Written without quotes,

```
build --remote_header=authorization=Basic <base64>
```

the space before the base64 payload splits the flag from an unquoted second word. Bazel reads that word as a target: `no such target '//:<base64-prefix>'`. The quoted form above is the one that survives parsing. This is not a hypothetical. It broke a real setup while this page was being written.
:::

Without the header the `auth_basic` realm of the server answers 401 to every request. Your builds then run on `--disk_cache` alone, which is a slower first build and nothing worse.

**CI writes the cache from every lane that holds the secret.** Each Bazel job passes `write-auth` the secret `BAZEL_CACHE_WRITE_AUTH` on every trigger. The `.github/actions/bazel-cache-rc` action turns it into a CI-only rc under `$RUNNER_TEMP`. That rc names a credential helper for the cache host and grants the upload. The credential's user is in both realms, so one header reads and writes. A structural test in `.claude/tests/test_workflows.py` holds every Bazel lane to that step.

A fork PR still sees none of it. [GitHub Actions withholds a repository's secrets from a run triggered by a fork's `pull_request` event][github-actions-secrets-forks]. So that lane gets no rc and compiles cold over `--disk_cache`, by design and not by omission.

Verify precedence once, from wherever `bazel` runs on this machine. `--announce_rc`
echoes every rc line it read **including the header value**, so send it through a
filter rather than to your terminal or a CI log:

```sh
$ ocx exec bazel -- bazel info --announce_rc 2>&1 | grep -v remote_header | head
```

```text
INFO: Reading 'startup' options from <repo>/.bazelrc: --host_jvm_args=-Xmx2g
INFO: Reading 'startup' options from ~/.bazelrc: --output_user_root=~/.cache/ocx/bazel-root, --host_jvm_args=-Xmx2g
INFO: Reading rc options for 'info' from <repo>/.bazelrc:
  Inherited 'build' options: --jobs=12 --disk_cache=~/.cache/ocx/bazel-disk --remote_cache=https://bazel-cache.ocx.sh/v1 ...
INFO: Reading rc options for 'info' from ~/.bazelrc:
  Inherited 'build' options: --jobs=12 --remote_header=authorization=Basic <redacted>
```

The workspace `.bazelrc` is read first. `~/.bazelrc` is read after it, so a host-wide value can override a workspace one where the two conflict.

## Reading Grafana {#contributing-bazel-grafana}

A [Grafana][grafana] dashboard titled "Bazel build" on the project's monitoring stack has four panels. They show cache writes in the last hour, builds observed, cache write rate, and a divergence check. The divergence check compares the targets a build declares with the ones that land as trace spans.

The check exists because this telemetry pipeline has already shipped a dropped-span defect once. A build whose "landed" line sits below its "declared" line is the failure it was built to catch. That failure was discovered the hard way rather than designed in.

All four panels read zero today. It is worth knowing why before the dashboard reads as broken instead of quiet.

`scripts/bep_to_otlp.py` turns a [Build Event Protocol][bazel-bep] stream into the spans the "builds observed" and "declared vs landed" panels query. It runs at the tail of `bazel:build:nobuild`, in CI and locally alike. It pushes one span per analyzed target plus one summary span. That wiring is verified.

**What has not happened yet is a CI run that exercises it.** The step carrying the credential has never run on `main`, so no Bazel span has reached the collector from any lane.

Those two panels will fill from the first `smoke` job that runs the gate with the org secret present. The two cache panels read zero for the separate reason above.

Locally the push is a deliberate silent no-op unless an OTLP endpoint is configured on your machine. Reading the dashboard today mostly means confirming it is still quiet in the way described here. It does not mean chasing a number that should be moving.

<!-- external -->
[bazel]: https://bazel.build/
[bazelisk]: https://github.com/bazelbuild/bazelisk
[rules-rs]: https://github.com/hermeticbuild/rules_rs
[hermetic-llvm]: https://github.com/hermeticbuild/hermetic-llvm
[sccache]: https://github.com/mozilla/sccache
[grafana]: https://grafana.com/
[bazel-bep]: https://bazel.build/remote/bep
[github-actions-secrets-forks]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/using-secrets-in-github-actions
[task-runner]: https://taskfile.dev/
[agg]: https://github.com/asciinema/agg

<!-- pages -->
[project-toolchain]: ../getting-started/first-project.mdx#project-toolchain
