---
outline: deep
---
# Building with Bazel {#contributing-bazel}

OCX's Rust workspace builds twice: once through `cargo`, and once through a second, parallel graph that [Bazel][bazel] walks to check the first one honestly. Eight gates run in two lanes. Seven of them run on every change; the eighth, the acceptance suite, runs on the deep lane only. Four read the graph itself — a pin check, a loading-phase sanity build, a dependency-drift comparison against `Cargo.toml`, and a sandbox-tag audit. Two lint what declares it: `bazel:lint` runs buildifier over every `BUILD.bazel` and `.bzl` file, and `bazel:mod:check` refuses a stale `MODULE.bazel.lock`. The last two run tests: `bazel:test:unit` runs the workspace's own unit tests, floored per target off the build event stream rather than a bare pass/fail, and `bazel:test:accept` runs the acceptance suite as one target per test module. `bazel:test:accept` is the one that is not on every change: `verify-basic.yml` — the only workflow a pull request fires — runs the other seven, and the acceptance gate lives in `verify-deep.yml`, which has no `pull_request` trigger. Locally `task verify` runs all eight. Together they catch the class of defect a green `cargo build` cannot see on its own: a `BUILD.bazel` target that quietly stopped matching the crate it describes.

Cloning a repository with an unfamiliar second build system is exactly the moment a contributor installs the wrong version, hunts for a `WORKSPACE` file that does not exist, or wonders whether [bazelisk][bazelisk] belongs on `PATH`. None of that applies here. Nobody types `bazel` directly — [`task`][task-runner] is the one command anyone runs, and `bazel` itself is a pinned entry in `ocx.lock`, resolved the same way `git-cliff` or `shellcheck` are.

## First build {#contributing-bazel-first-build}

Before your first Bazel build, run `/init-bazel-config` — or `task bazel:doctor` for the checks alone. `.bazelrc` and `MODULE.bazel` are committed and already correct. What a first build gets wrong is host state no gate here can see: the output root, the RAM envelope, the `libstdc++` link prerequisite and the remote-cache reader credential. That state belongs in `~/.bazelrc`, never `.bazelrc.user`.

All eight live in `taskfiles/bazel.taskfile.yml` as `task` targets: `bazel:pin:check`, `bazel:build:nobuild`, `bazel:build:drift`, `bazel:tag:guard`, `bazel:lint`, `bazel:mod:check`, `bazel:test:unit`, and `bazel:test:accept`. `task verify` runs all of them; on CI the first seven are `verify-basic.yml`'s and `bazel:test:accept` is `verify-deep.yml`'s. None needs a `bazel` invocation typed by hand.

::: tip Bazel is a pinned tool, not a system dependency
`ocx exec bazel -- bazel --version` resolves the exact binary every one of those gates uses, straight from `ocx.lock` — the same mechanism that resolves `uv`, `shellcheck`, and every other tool in the [project toolchain][project-toolchain]. There is nothing to install with a system package manager and no [bazelisk][bazelisk] version file to maintain.
:::

The scoped `ocx exec bazel --` form is what every gate uses, and it is deliberate: it names the one binding it needs, so an unrelated tool with no leaf for this host cannot block the run.

```sh
$ ocx exec bazel -- bazel --version
bazel 9.2.0
```

**The one thing a fresh clone does need once.** `Cargo.bazel.lock.json` is the lockfile [rules_rust][rules-rust]'s `crate_universe` reads to resolve third-party crates, and it is gitignored on purpose. Three `[patch.crates-io]` entries point at path-dependency submodules, which makes `crate_universe` record the *generating machine's* output base into the file. A committed copy therefore fails to read back on any other tree — including the one that generated it, after a `bazel clean --expunge`. On a fresh clone every `bazel query` dies with `Unable to read lockfile` until it is regenerated once.

`task bazel:bootstrap` — and every gate that needs a live graph calls it first — notices the missing file and repins:

```sh
$ CARGO_BAZEL_REPIN=1 ocx exec bazel -- bazel fetch --repo=@crates
```

Measured in this checkout: 12 seconds. The repin's own exit code is not proof it ran — `bazel fetch --repo=@crates` exits 0 in about two seconds as a no-op once `@crates` is already in the repo contents cache, splicing nothing and writing no lockfile. `task bazel:bootstrap` checks that the file actually exists afterward rather than trusting the exit code.

**A build also writes three files you should not commit.** `crate_universe` splices each `[patch.crates-io]` path submodule and leaves a generated `BUILD.bazel` behind in `external/rust-oci-client`, `external/docker_credential` and `external/sigstore-rs`. They belong to the generator, not to this repository — CI regenerates them on every run — so `task bazel:bootstrap` adds `BUILD.bazel` to each submodule's `$GIT_DIR/modules/<path>/info/exclude` instead. That is the one ignore file a superproject may write without editing a fork's tree, and it is per worktree, which is why the task writes it rather than a setup step you run once. After a build, `git status --short --ignore-submodules=none` is clean; if it is not, run `task bazel:bootstrap`.

Two failure modes are worth knowing before they surprise you:

- **A `~/.cargo/config.toml` above the splice temp directory.** If one sits in a parent of wherever `crate_universe` splices its workspace, the repin refuses with exit code 8 and `A Cargo config file was found in a parent directory`. Point the splice temp outside `$HOME`: `CARGO_BAZEL_REPIN=1 bazel fetch --repo=@crates --repo_env=TMPDIR=/var/tmp/ocx-splice`.
- **A missing `libstdc++.so` development symlink.** Fedora-family distributions ship `libstdc++.so.6` but not the unversioned `libstdc++.so` symlink `rules_rust`'s own `process_wrapper` link step needs — that symlink comes from the `libstdc++-devel` package (pulled in transitively by installing the `gcc-c++` group, if you already have that). Without it every Rust link action fails inside the sandbox. The fix is either `sudo dnf install libstdc++-devel` or a manual symlink farm referenced from `~/.bazelrc` — and it needs **both** `--linkopt=-L<dir>` and `--host_linkopt=-L<dir>`, because the exec-config tool build (`crate_universe`'s own build scripts, run for the host rather than the target) does not read plain `--linkopt`.

**Host state lives in `~/.bazelrc`, never a per-checkout file.** This machine carries four fixed worktrees of this repository plus disposable agent checkouts under `.agents/worktrees/`, and nothing host-specific — the linker workaround above, the JVM heap cap, the local `--jobs` cap, the cache reader credential below — should need setting up once per checkout. `bazel` already reads `~/.bazelrc` on its own, after the workspace's committed `.bazelrc` and before any `--bazelrc=` flag, with no `import` line needed on either side:

```sh
# ~/.bazelrc -- host-wide, outside every checkout by construction, read from every worktree
startup --host_jvm_args=-Xmx2g
build --jobs=12
build --linkopt=-L/home/you/.cache/ocx/libdir
build --host_linkopt=-L/home/you/.cache/ocx/libdir
```

`.bazelrc.user` is still gitignored and `.bazelrc` still `try-import`s it as its last line, but it is now a comment-only stub: the file for a genuine per-checkout experiment — one flag you want on this worktree and nowhere else — never for anything that describes the machine.

With that in place, the loading-phase gate is what actually exercises the graph:

```sh
$ ocx exec bazel -- bazel build --nobuild //...
```

```text
Analyzing: 290 targets (28 packages loaded, 6 targets configured)
INFO: Analyzed 290 targets (620 packages loaded, 26021 targets configured).
INFO: Found 290 targets...
INFO: Build completed successfully, 0 total actions
```

Zero actions, on purpose — `--nobuild` only loads and analyzes. `bazel:build:drift` and `bazel:tag:guard` read the same graph next, each against a floor of its own. Drift reads `//crates/...` and refuses to report on fewer than 56 targets across 20 packages. The tag guard reads the widest scope of all: measured today, 332 rule targets over `//...`, up from 143 over `//crates/...` plus `//test/doc_scripts/...`. It had to widen to reach the acceptance package, because that is where its one narrowed clause lives.

The blanket rule is that a target running outside the sandbox carries the cache-excluding tag its kind was measured to need — `external` for a test action, `no-remote-cache` for a build action. The 181 acceptance targets break it on purpose: their results **are** cached, so `external` is exactly the tag they must not carry. What stands in for it is a declaration. An acceptance target is credited only while `//test:docker-compose.yml` and `//test:suite_anchor` — the compose definition, and the group carrying the `ocx` binary under test — are in its declared inputs. Those are the two inputs whose change a cached pass would otherwise hide, so losing either turns the exemption off and the gate red. A `no-sandbox` test target anywhere else in the graph is judged by the blanket rule however it declares its inputs.

290 is what a wildcard build or test reaches, not the size of the whole graph. `bazel query 'kind(rule, //...)'` still lists 332 — the 42 more are the GIF-render targets below, tagged `manual` so wildcard expansion skips them while `bazel query` keeps naming every one.

Those 42 are one `genrule` per cast recording rendering the `.cast` through [agg][agg] to an animated `.gif`, plus the `:gifs` filegroup that collects the 39 renders, the `:gif_check` test that proves them, and `:gif_check_probe`, the generated script it runs. Nothing on the site consumes a GIF — the site plays the `.cast` file directly, and a GIF is for a README or a social-media embed, a human picking one file rather than a build step. That is why `manual` sits on all 42: it takes them out of `//...` and `bazel test //...` while leaving them reachable by label.

The one consumer is `task recordings:gifs` — not `task website:recordings:gifs`, which `website/taskfile.yml` includes with `internal: true` and refuses to run. `recordings:gifs` runs `bazel test //test/doc_scripts:gifs //test/doc_scripts:gif_check`, so the render proof still runs every time the task does. A `manual` target that no task names is a proof that has stopped running.

agg rasterises text, so the GIF targets need a monospace font on the host. `ubuntu-latest` carries DejaVu Sans Mono and resolves on agg's default family list; a host with none of the listed families fails the build with `no faces matching font family options` rather than rendering blanks.

::: warning A development host can carry none of the fonts on that list
Measured on this machine: 136 fonts installed, and `fc-list :spacing=mono` names exactly two monospace families — `Adwaita Mono` and `Nimbus Mono PS` — neither DejaVu nor Liberation. Running `agg` with its own built-in default family list here writes nothing and exits 1 with `no faces matching font family options`. `//test/doc_scripts`'s `gif.bzl` widens the font list past agg's default for exactly this reason, appending both families this host actually has. A probe for the condition should resolve `fc-list`'s binary explicitly rather than trust `$PATH`: this host happens to expose it at both `/usr/bin/fc-list` and `/usr/sbin/fc-list` (Fedora's merged-usr layout symlinks the two), but that mapping is not a property every distribution shares.
:::

That gate also writes the only [Build Event Protocol][bazel-bep] stream this repository produces, and it writes it to a scratch directory outside the checkout. Bazel serialises the whole client environment and every rc-file flag value into that stream, credentials included, so the task deletes the directory on its way out whatever the build did.

## The acceptance suite, one target per module {#contributing-bazel-acceptance}

`task bazel:test:accept` runs `bazel test //test:all --local_test_jobs=1`: one `sh_test` per acceptance test module — 181 of them, named `//test:<module stem>` — each shelling to `uv run pytest` against the docker-compose services this repository already starts. It replaces the direct `task test:parallel` call in `task verify` and in `verify-deep.yml`'s acceptance job. `task test:parallel` still exists and is still the fastest way to run the whole suite by hand — it keeps pytest-xdist, which this lane gives up.

What the lane buys instead is **selection and caching**. Edit one module and one target re-runs; edit a doc and none does; run it twice on an unchanged tree and Bazel reports `Executed 0 out of 181 tests`. That is only sound because the inputs are declared: each target names its own module plus `//test:suite_inputs`, the shared group carrying `conftest.py`, `pyproject.toml`, `uv.lock`, the fixture tree, `docker-compose.yml` and the `ocx` binary under test. Rebuild `test/bin/ocx` with different bytes and all 181 re-run.

`--local_test_jobs=1` is on that command line and in no rc file. There is one compose stack on one set of ports, so the targets have to run one at a time — the `exclusive` tag already guarantees that, and the runner takes a host-wide `flock` so a `task test:parallel` in a sibling worktree queues behind it too. The flag only stops Bazel from scheduling a dozen tests it will then run serially. An rc-file line would apply per command rather than per target pattern, so it would also serialise the 34 Rust test targets, which is the unit lane's whole win.

Pass `NOCACHE=1` to add `--nocache_test_results` when you want the run regardless of what the cache says. Not `--force`, deliberately: `task verify --force` is what this repository tells everyone to type, so binding the escape hatch to it would have made every full gate run all 181 targets serially and left the caching unreachable from the one command that runs the lane.

The lane also carries the suite's own shape gates, which used to live in the pytest entry point: `test/SUITE_FLOOR` (the suite never shrinks) and `test/SKIP_CEILING` / `test/XFAIL_CEILING` (a skip added to dodge a red). They are read off the 181 per-case JUnit reports Bazel writes, copied out of `bazel-testlogs` into `target/bazel/accept/<module>/junit.xml` — real per-test data, because the acceptance runner passes `--junit-xml=$XML_OUTPUT_FILE`. The unit lane's `test.xml` files are Bazel's synthesised one-testcase-per-target files and are not the same thing.

::: warning What a cached acceptance pass does not cover
The running compose stack is not an action input. `docker-compose.yml` is, so a bumped service image or a moved port re-keys every target — but a stack that is *up with different content* does not. Neither do the handful of modules that read `target/release/ocx_schema`, `crates/**`, `website/src/docs/**` or the `//test/doc_scripts` package across a package boundary. `test/bazel.bzl`'s module docstring lists every one of those gaps with its risk; they are declared rather than closed, and `//crates/...` and `//website/...` have gates of their own that red on their own content.
:::

## Warm cache {#contributing-bazel-warm-cache}

The committed `.bazelrc` sets `build --disk_cache=~/.cache/ocx/bazel-disk` unconditionally. Never anywhere under `/tmp`: on a typical development host that path is a tmpfs an hourly reaper clears mid-build. Never inside the workspace either, because a relative `--disk_cache` resolves against the *client's* working directory rather than the workspace root — which silently forks a second, empty cache the moment `bazel` runs from a subdirectory. The `~`-rooted absolute path means one cache, shared by every worktree on the machine, with no setup step.

::: info The same idea `sccache` already applies to `cargo`
This repository's own CI already runs Rust compiles through [sccache][sccache] (`SCCACHE_AWS_ACCESS_KEY_ID` in the workflow) so a second `cargo build` skips work the first one already paid for. Bazel's `--disk_cache` is the same trade for the Bazel graph: an action cache keyed by the exact inputs that produced it, so a rebuild with nothing relevant changed reads results back from disk instead of recompiling.
:::

Measured here, on a single crate (`//crates/ocx_exit:ocx_exit`, 336 actions) after `bazel clean` between runs so the disk cache — not the Bazel server's in-memory analysis cache — is doing the work:

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

**The remote cache is a second layer above that, and the reader realm is live.** The committed `.bazelrc` points at it: `build --remote_cache=https://bazel-cache.ocx.sh/v1`, beside `build --remote_upload_local_results=false`. The file's own words for that second line are "a contributor, a fork PR and a non-main CI lane all read and none of them write." Reading it needs a `--remote_header` credential line in your own `~/.bazelrc` — never a repo file — ask a maintainer for a `dev-read` account:

```sh
# ~/.bazelrc, appended to the block above
build --remote_header="authorization=Basic <base64 of user:password>"
```

::: warning The line is whitespace-tokenized — quote the header value
A `.bazelrc` line splits on whitespace the same way a shell command line does. Written without quotes,

```
build --remote_header=authorization=Basic <base64>
```

the space before the base64 payload splits the flag from an unquoted second word, and Bazel reads that word as a target: `no such target '//:<base64-prefix>'`. The quoted form above is the one that survives parsing. Not a hypothetical — it broke a real setup while this page was being written.
:::

Without the header the server's `auth_basic` realm answers 401 to every request, and your builds run on `--disk_cache` alone, which is a slower first build and nothing worse.

**CI carries the reader credential too, and never beside the writer.** `verify-deep.yml`'s Linux build leg binds `read-auth` to the org secret `BAZEL_CACHE_READ_AUTH` unconditionally. `verify-basic.yml`'s `smoke` job serves every trigger from one job, so it binds `read-auth` to the *negation* of the trusted-event gate that releases `write-auth`: the `main` push presents the write credential alone — whose user is in both realms, so it reads and writes with one header — and every other trigger presents the read credential alone. Bazel sends one `Authorization` header per configured source rather than the last one, so two bindings live at once would leave the origin to choose between them; a structural test counts the sources per trigger and refuses any number but one. A fork PR still sees none of it: [GitHub Actions withholds a repository's secrets from a run triggered by a fork's `pull_request` event][github-actions-secrets-forks], so that lane alone reads anonymously over `--disk_cache` and compiles cold — by design, not by omission.

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
  Inherited 'build' options: --jobs=12 --linkopt=-L~/.cache/ocx/libdir --host_linkopt=-L~/.cache/ocx/libdir --remote_header=authorization=Basic <redacted>
```

The workspace `.bazelrc` is read first; `~/.bazelrc` is read after it, so a host-wide value can override a workspace one where the two conflict.

## Reading Grafana {#contributing-bazel-grafana}

A [Grafana][grafana] dashboard titled "Bazel build" exists on the project's monitoring stack, with four panels. Cache writes in the last hour, builds observed, cache write rate, and a divergence check between the targets a build declares and the ones that land as trace spans. That last panel exists because this telemetry pipeline has already shipped a dropped-span defect once. A build whose "landed" line sits below its "declared" line is the failure it was built to catch, discovered the hard way rather than designed in.

All four panels read zero today, and it is worth knowing why before the dashboard reads as broken instead of quiet. `scripts/bep_to_otlp.py` turns a [Build Event Protocol][bazel-bep] stream into the spans the "builds observed" and "declared vs landed" panels query. It runs at the tail of `bazel:build:nobuild`, in CI and locally alike, pushing one span per analyzed target plus one summary span. That wiring is verified. **What has not happened yet is a CI run that exercises it.** The step carrying the credential has never run on `main`, so no Bazel span has reached the collector from any lane. Those two panels will fill from the first `smoke` job that runs the gate with the org secret present. The two cache panels read zero for the separate reason above.

Locally the push is a deliberate silent no-op unless an OTLP endpoint is configured on your machine. Reading the dashboard today mostly means confirming it is still quiet in the way described here, not chasing a number that should be moving.

<!-- external -->
[bazel]: https://bazel.build/
[bazelisk]: https://github.com/bazelbuild/bazelisk
[rules-rust]: https://github.com/bazelbuild/rules_rust
[sccache]: https://github.com/mozilla/sccache
[grafana]: https://grafana.com/
[bazel-bep]: https://bazel.build/remote/bep
[github-actions-secrets-forks]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/using-secrets-in-github-actions
[task-runner]: https://taskfile.dev/
[agg]: https://github.com/asciinema/agg

<!-- pages -->
[project-toolchain]: ../getting-started.md#project-toolchain
