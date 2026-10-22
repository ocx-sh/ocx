---
paths:
  - .github/workflows/**
  - .github/actions/**
  - renovate.json
---

# CI Subsystem

GitHub Actions workflows for OCX build, test, lint, license, release.

## Verification tiers

`verify-basic.yml`, `verify-deep.yml` and `claude.yml` belong to the conventions template ([ocx-sh/conventions](https://github.com/ocx-sh/conventions); the first line of each says so, and `task copier:update` is how they change, never a hand edit). Their jobs run the `ci:*` task aliases (`taskfiles/ci.taskfile.yml` holds each tier's membership), and each has one `repo` job that calls a workflow this repository owns, passing only `BAZEL_CACHE_WRITE_AUTH` by name. Everything below the template line is ours:

| Tier | Workflows | Fires on | Runs |
|---|---|---|---|
| **basic** | `verify-basic.yml` (`lint`, `test`, `build`, `docs`, `basic-gate`) → `basic-repo.yml` | every PR (drafts included), every push to `main`, `merge_group`, `workflow_dispatch` | `task ci:lint`, `ci:test`, `ci:build`, `ci:docs`; then `conventional-commits` (PRs), `scripts-tests` (decides inside whether `scripts/**` changed, never skipped), `unit-live-shells` (the unit run again with nu, elvish and fish on PATH and `__OCX_TESTING_REQUIRE_LIVE_SHELLS` set, so the parity arms cannot skip), `binaries`, `acceptance` (calls `acceptance.yml`), `smoke-acceptance`. **Linux only** (D5). `basic-gate` is the one required check |
| **deep** | `verify-deep.yml` (`deep`, `nightly-red`) → `deep-repo.yml` | the nightly cron, `workflow_dispatch`, `workflow_call`, a pull request labelled `run-deep` | `task ci:deep`; then `cross` (calls `cross.yml`), `unit-native` and `acceptance-native` (macOS and Windows, `needs: cross`, download and execute only), `shell-activation-deep` (calls `shell-activation-deep.yml`), `satellite-verify`, `satellite-contract`, `index-conformance-drift` |
| **acceptance** | `acceptance.yml`, reusable | called by `basic-repo.yml` | `plan` → `binary` (rebuilds `//crates/ocx_cli:ocx` for `binary: build`; the only job holding the cache credential) → `shard` × `shard-count` (pytest on the prebuilt binaries) → `merge` (`task test:accept:floor` over every shard's report) |
| **cross** | `cross.yml`, reusable | called by `deep-repo.yml` (variant B of `plan_pr637_round2.md` D13; flipping to A is one job in `basic-repo.yml` plus `CROSS_VARIANT` in the tests) | one leg per `task cross:targets` triple |
| **readiness** | `release-readiness.yml` | `workflow_dispatch` on the ref about to be released | calls `verify-deep.yml` and rolls its result into a GO/NO-GO verdict |

- **Permission ceiling.** The template's `repo` job grants `contents: read`, and a nested job asking for more fails the whole run at startup — checked statically, so a job an `if:` would skip counts. Repo-owned workflows therefore publish test results as annotations and a job summary (`mikepenz/action-junit-report` with `annotate_only: true`), never as a check run or a pull request comment. `release-readiness.yml`'s `deep` job grants the union of what everything below it asks for (`contents: read`, plus `issues: write` for the template's `nightly-red`); it passes no secrets because `verify-deep.yml` declares none, so that run is uncached. Nesting is four deep (readiness → `verify-deep.yml` → `deep-repo.yml` → `cross.yml`); GitHub allows ten on github.com and four on GitHub Enterprise Server ([reusing workflow configurations](https://docs.github.com/en/actions/sharing-automations/reusing-workflows)), so `deep-repo.yml` must not call a workflow that calls another.
- **Opt-in deep, accepted.** A push to `main` runs the basic tier alone (D15), and a pull request runs deep only once someone adds `run-deep` — or dispatches `verify-deep.yml --ref <branch>`. Until then a PR compiles no `cfg(windows)` / `cfg(target_os = "macos")` arm, runs no `#[cfg(windows)]` unit test and exercises no multi-OS acceptance path; the nightly run and the readiness gate are the safety net, and a red nightly opens the `nightly-red` issue. Ask for the label on anything touching those surfaces. `.claude/tests/test_workflows.py` pins the trigger set, the `run-deep` guard on every deep job and `merge_group` on basic.
- **Observing it.** Open a PR: `gh run list --branch <branch>` shows `verify-basic` and no `verify-deep`. Add the label (or dispatch) and the run appears with event `pull_request` / `workflow_dispatch`.
- **Bazel gates.** `task verify` runs eight Bazel gates in two lanes. Seven sit in the basic tier — `bazel:pin:check` → `bazel:build:nobuild` → `bazel:build:drift` → `bazel:tag:guard` → `bazel:lint` → `bazel:mod:check` in the template's `lint` job (through `ci:lint`) and `bazel:test:unit`, doctests included, in `test` (through `ci:test`) — and the eighth, `bazel:test:accept`, is `acceptance.yml`'s sharded run (`task test:accept:native`), which replaces the cached `sh_test` fan-out in CI.
- **Cross is the one Linux compile.** `cross.yml` builds `ocx` for all eight triples on one Linux host; `unit-native` and `acceptance-native` only download what it built (`test_deep_non_linux_jobs_only_download_and_execute` rejects a cargo, Bazel or Rust-toolchain step there). `needs:` names a whole matrix, so a native leg waits for the slowest cross leg.
- **`satellite-verify`** (`task satellite:verify --force`, ocx-mirror built against this tree) is **non-blocking**: job-level `continue-on-error: true` until the mirror re-points at the split crates (DEC-4; the flip is deleting that line). **Never make it a required check while the line is set** — its own check run still concludes `failure`. It checks the mirror out at its default branch (the template's dispatch passes no ref input). **`satellite-contract`** (`task satellite:contract --force`, ocx-mirror's acceptance suite against the ocx this tree builds, floored on 6 distinct passed `ocx_contract_site` site ids) is **blocking**.
- **`announce/**` branches (D10)**: a PR into `announce/**` gets the basic tier like any other.
- **Claude responders.** The template's `claude.yml` reviews pull requests and answers `@claude` comments read-only (`contents: read`). `claude-write.yml` keeps the write-capable responder (`contents: write`) for what the template does not take: an issue labelled `claude` or opened with `@claude`, and a comment holding `/claude-write` and no `@claude`. Only an org member or owner starts it. `test_claude_write_answers_only_what_the_template_responder_does_not` holds the split.
- **Hand-written workflows add no zizmor finding.** `.github/zizmor.yml` ignores only the generated files; a finding in ours is fixed in the file or ignored inline with its reason (`notify-discord.yml`'s `workflow_run` trigger is the one).

### Release provenance scan (`adr_test_speed_tiers.md` C-PROV, D1)

A test build (`--features ocx/__testing` under cargo; `crates/ocx_cli/BUILD.bazel`'s `_TESTING_PROVENANCE` for the Bazel-built acceptance binary, since Bazel never runs `build.rs`) bakes fixed placeholder provenance into `ocx`; nothing that carries it may ship. `scripts/release_provenance_check.py` has two modes, both callers use: `--scan --min-files <n>` byte-scans every downloaded binary for the placeholder markers, floored on the binary count so an empty download cannot pass; `--exec` runs the one binary native to the runner and asserts its `version` report carries release provenance.

- **`scan-binaries.yml`** (reusable `workflow_call`) is the one implementation. `release.yml`'s `dist-workspace.toml` `global-artifacts-jobs = ["./scan-binaries"]` renders it as `custom-scan-binaries`, running after `build-local-artifacts`; the `host` job (GitHub Release, then OCI publish) `needs` it, and cargo-dist reads `result == 'skipped' || result == 'success'` as a pass — so the job carries no job-level `if:` and no `continue-on-error`, on purpose, or a skip would be indistinguishable from a scan.
- **`deploy-dev.yml`** calls the same reusable workflow over its own build jobs' raw `ocx-<target>` binaries before its `publish` job.
- **`cross.yml`** does not scan: its binaries are the Bazel cross-builds, which carry the `__testing` placeholder provenance, so a scan could only red, and a second release-profile compile to scan is the cost the restructure removed. `scan-binaries.yml` (release, deploy-dev) is the scan (ADR Amendment AM-2, superseded on this point).
- **`oci-publish.yml`** runs `--exec /usr/local/bin/ocx` as its own check, after `scan-binaries.yml`'s byte scan has already run over every target.
- **Schema artifact**: `acceptance.yml`'s `binary` job runs `task schema:generate` and uploads it beside the binaries; the `shard` jobs download it into `website/src/public/schemas/metadata`.

## Design Principles

### 1. Taskfile is the single source of truth

Every check dev run locally MUST be Taskfile task. CI call task, not raw command. Kill drift between local + CI.

```yaml
# CORRECT
- name: Clippy
  run: task rust:clippy:check

# WRONG — will drift from taskfile
- name: Clippy
  run: cargo clippy --workspace --locked -- -D warnings
```

Raw commands in CI OK only for CI-only glue (artifact paths, GitHub annotations, `${{ steps.*.outcome }}`).

**Merge rule**: CI + Taskfile diverge → adopt stricter flags in Taskfile, CI call task.

### 2. Minimize duplication via composite actions

| Scope | Mechanism | When |
|-------|-----------|------|
| Steps within a job | Taskfile task | Same steps used locally and in CI |
| Steps across jobs in one workflow | Composite action (`.github/actions/*/action.yml`) | Setup sequences (checkout + toolchain + cache) |
| Jobs across workflows | Reusable workflow (`workflow_call`) | Shared CI pipeline patterns |

### 3. Never let lint block test results

Test results > lint results. Pattern: `continue-on-error: true` on linters + final gate step.

```yaml
- name: Check formatting
  id: fmt
  run: task rust:format:check
  continue-on-error: true

- name: Clippy
  id: clippy
  run: task rust:clippy:check
  continue-on-error: true

- name: Build
  run: task rust:build

- name: Test
  run: task rust:test:unit -- --profile ci

- name: Publish Test Results
  if: ${{ !cancelled() }}
  uses: mikepenz/action-junit-report@SHA
  with:
    report_paths: target/reports/junit/*.xml
    annotate_only: true

- name: Check lint results
  if: ${{ !cancelled() }}
  run: |
    if [[ "${{ steps.fmt.outcome }}" == "failure" || "${{ steps.clippy.outcome }}" == "failure" ]]; then
      echo "::error::Lint checks failed"
      exit 1
    fi
```

### 4. Share artifacts, don't rebuild

Build binary **once** in smoke job, upload with `compression-level: 0` and `retention-days: 1`. Downstream jobs `download-artifact` not rebuild.

### 5. Security

- **SHA-pin every action** — `uses: owner/action@<full-sha>  # vX.Y.Z`. Includes first-party actions (`ocx-sh/setup-ocx`): no floating-major carve-out. Exception: `release.yml` is cargo-dist-generated and floats by design (ignored by the bumper).
- **Keep pins fresh automatically** — dependency updates run via **Renovate** (`renovate.json`, replaces Dependabot). (The ocx-mirror pipeline templates and their customManager moved to the ocx-sh/ocx-mirror repo.)
- **Minimal permissions** — declare at workflow level, elevate per-job
- **No secrets in `run:` steps** — use `env:` intermediary to block script injection
- **OIDC for cloud auth** — not static credentials
- **No self-hosted runners for public repos**

### 6. Concurrency

Every workflow:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
```

A workflow with a `merge_group` trigger adds `!startsWith(github.ref, 'refs/heads/gh-readonly-queue/') &&` in front (see `verify-basic.yml`, whose `cancel-in-progress` is `github.event_name == 'pull_request'`, which reaches the same end): the queue treats a cancelled run as a failed check.

### 7. Lint workflows with actionlint

Workflow YAML is linted by [actionlint](https://github.com/rhysd/actionlint) (project toolchain via `ocx.toml`). Local + CI gate = `task ci:actionlint`; the template's `lint` job runs it on push + PR through `task workflows:lint` (actionlint and zizmor, pinned in `taskfiles/baseline.taskfile.yml`). actionlint catches invalid contexts (e.g. `matrix` on a step's `shell:` key), unpinned/typo'd expressions, and runs shellcheck over `run:` scripts. Conventions:

- Embedded shellcheck severity floor = `--severity=warning` (`SHELLCHECK_OPTS` in the task), matching `shell:shellcheck`. Info/style findings are not gated.
- The cargo-dist-generated `release.yml` is excluded via `.github/actionlint.yaml` (`paths:` ignore-all) — never hand-edited, drift policed by `verify-release-ci.yml`.

## Bazel remote cache

Decision: [`adr_bazel_build_adoption.md`](../artifacts/adr_bazel_build_adoption.md) § Cache staging rulings 5, 5a, 5b (amended 2026-10-22, [#638](https://github.com/ocx-sh/ocx/issues/638)).

- **Every lane that holds the secret `BAZEL_CACHE_WRITE_AUTH` writes** — same-repo PRs, merge queue, `main`, dispatch, schedule. A fork PR gets no secrets: it holds none, writes no rc file, and builds with `--disk_cache` only.
- **`.github/actions/bazel-cache-rc` has one mode.** With `write-auth` it writes `$RUNNER_TEMP/bazel-cache.rc` (0600: `--credential_helper` to the 0700 helper, plus `--remote_upload_local_results=true`); empty input writes no file. Callers pass `write-auth: ${{ secrets.BAZEL_CACHE_WRITE_AUTH }}` unconditionally — no event predicate. Cleanup stays under `if: always()`.
- **The upload grant lives in that rc, never on a step.** `--bazelrc` loads after `.bazelrc`, whose `=false` stays the default. Root `taskfile.yml` holds the one `CACHE_RC` var and every `ocx exec bazel -- bazel` line passes it.
- **No CI read credential** (`BAZEL_CACHE_READ_AUTH` and `read-auth` are retired). Reads are still 401 without a credential, so the developer-side read credential (`~/.bazelrc`, `task bazel:doctor`) stays.
- **A reusable workflow that holds a cache step declares the secret `required: false` and its caller passes it by name** (`basic-repo.yml`, `deep-repo.yml`, `acceptance.yml`, `cross.yml`), never `secrets: inherit` (`release.yml` is generated and exempt). The template's `bazel-cache: trusted` setup step is the same writer under the name `cache-write-auth`, and owns its credential's lifetime itself; the repo action's cleanup duty applies to `bazel-cache-rc` only. `.claude/tests/test_workflows.py` holds all of this.
- **Deviation from the vendored lore rule (plan_pr637_round2 D5).** `.claude/rules/bazel-quality/caching.md` BZL-CACHE-02 still states the old writer rule ("Keep write access to the Action Cache strictly narrower than read access — never symmetric, and never held by a developer machine's default configuration"), and BZL-CACHE-01 still keeps every same-repo PR lane read-only. ocx deliberately deviates from both until I1's `grim update` pulls the lore release carrying the amended BZL-CACHE-02. Never edit the vendored file here: `grim:check` reds.

## Test telemetry

Every unit and acceptance run — in CI and on a developer machine — pushes its JUnit report to `https://otel.ocx.sh` as OTLP traces, so Grafana can name the tests that accumulate the most wall clock. One converter, `junit2otlp` 0.1.2, on both routes: the composite action `.github/actions/test-telemetry` in CI, the `telemetry:push` task locally.

The Bazel build lane pushes to the same collector under its own service name and over a different transport; that half is § Build telemetry below, and everything from here to it is about the JUnit one.

**What is pushed.** Resource `service.name=ocx-tests`. Span attributes, on every span: `ocx.suite` (`unit` / `acceptance` / `smoke`), `ocx.source` (`ci` / `local`), `ocx.os`, `ocx.git.sha`, `ocx.git.branch`, `ocx.ci.run_id`, `ocx.ci.job` (CI only), `host.name` (the runner name, or the hostname locally). The tool adds `tests.case.duration` and `tests.suite.duration` in **milliseconds**, plus `code.function`, `tests.case.classname` and `tests.case.status`. `OTEL_RESOURCE_ATTRIBUTES` is not read, so none of these can be moved onto the resource without patching the tool.

**Three properties that decide how this is read and wired**, all measured against a local collector rather than assumed:

- **The span's own duration is not the test's.** Each test span is created and ended in the same instant (under a microsecond wide); the real figure is the `tests.case.duration` attribute. A dashboard aggregating span duration reports nothing.
- **The exit status says nothing.** Export errors go through the OTel error handler, so a push that lands nowhere still exits 0 — three `traces export: ... connection error` lines and `$? == 0`. Both routes therefore grep the tool's own output for `traces export:` and warn; neither trusts the status.
- **The endpoint must carry a scheme.** The tool builds `otlptracegrpc` and has no HTTP exporter, and the Go SDK reads the endpoint as a URL: `https://otel.ocx.sh:443` selects TLS, `http://` selects plaintext, and a bare `host:port` fails with `dns resolver: missing address`. It also always builds a metric exporter (`OTEL_METRICS_EXPORTER` is not read); with no metrics receiver that logs one `Unimplemented` per push, which is non-retryable and costs no measurable time.

**Two defaults would each have lost data quietly, so neither route feeds the tool a report as written.** Both are handled in the same few lines of both routes, and both were found by counting spans rather than by reading the tool's output, which reported success in both cases.

- **A 1 MB-per-line stdin scanner, against producers that emit one line.** The acceptance report is a single line of 545,175 bytes; past the ceiling junit2otlp exits 1 with `bufio.Scanner: token too long` and pushes nothing at all. Both routes therefore re-emit the document with `xml.dom.minidom.toprettyxml` first, which drops the longest line under a kilobyte whatever the suite grows to. Whitespace between elements is ignorable in XML, so what gets parsed is the same document: measured, the 3,819-case acceptance report yields exactly 3,821 spans (one run, one suite, one per `<testcase>`) and the 8,215-case unit report exactly 8,250.
- **A 2,048-span export queue, against reports an order of magnitude larger.** junit2otlp builds a `BatchSpanProcessor` and never sets a queue size, so the OTel SDK default applies — and a span enqueued above it is dropped with no log line, no error and exit 0. At the default the unit report landed **2,560 of its 8,250 spans**; with `OTEL_BSP_MAX_QUEUE_SIZE` set it landed all 8,250. Both routes size it from the report they are about to push (`cases + 1024`), so no suite can outgrow it. `--batch-size 512` is set beside it: the default of 10 makes the unit report 825 gRPC round trips instead of 17, and one run at the default lost a contiguous batch of exactly ten spans mid-report, unreproducibly and with nothing logged.

Nextest's report needs neither on its own — it is already 8,286 lines with a 242-byte maximum — but it takes both, because the routes do not branch on which producer wrote the file.

**There is no in-band way to notice the next such loss, and the obvious one does not exist.** `tests.suite.total` reads like the count a query could check a run against; in v0.1.2 it is a metric name passed once to `createIntCounter` and never placed on a span, and the ingested trace confirms it — the only `tests.suite.*` keys on any span are `duration`, `suitename`, `systemerr` and `systemout`. Nothing a span carries declares how many were meant to arrive, so a short run is a smaller believable number and not a gap. Checking a count means holding Tempo's span count against the `<testcase>` count in the report, from outside.

**Auth** is nginx basic auth. The header value (`Basic <base64 of ci:password>`) lives in the org secret `OTEL_OTLP_AUTH` and reaches the tool as `OTEL_EXPORTER_OTLP_HEADERS`. It is lifted into job `env:` rather than read from `secrets` in the step, for the reason the sccache credentials are: `secrets` is not a context a composite action can read, and the guard `env.OTEL_OTLP_AUTH != ''` is what makes a fork PR — which sees no secrets — skip the push instead of failing it. Every push step is also `continue-on-error: true` with `timeout-minutes: 5`: telemetry never decides a job. They run under `if: ${{ !cancelled() && … }}`, so a red suite pushes its timings too.

Wired after the JUnit-producing step of `deep-repo.yml`'s `unit-native` job (macOS, Windows), `acceptance.yml`'s `merge` job and `basic-repo.yml`'s `smoke-acceptance` job, and by the template's setup action (`report:`, a second run under `if: always()`) in its `test` and `deep` jobs.

**Which file each lane pushes, because the Bazel swap changed two of them.** macOS and Windows push `target/cross/unit-junit.xml` (the bundle runner's report). The Linux unit lane — the template's `test` job, through `ci:test` — pushes `target/reports/junit/unit.xml`, a copy of the `target/bazel/junit.xml` that `task bazel:test:unit` writes off the same `test.log` files its floor reads. The acceptance lane pushes the merged shard reports (`accept-junit/*.xml`). **Never `bazel-testlogs/**/test.xml` for the Rust half**: Bazel synthesises that file with one `<testcase>` per *target*, so it names 34 targets and cannot name a single `#[test]` — measured on a deliberately failed case, the failing fn appears only inside the testsuite-level `system-out`, which neither `publish-unit-test-result-action` nor `junit2otlp` reads. The acceptance targets are the opposite case and their `test.xml` is published as written: their runner passes `--junit-xml=$XML_OUTPUT_FILE`, so pytest writes a real per-case report. The same asymmetry decides the annotations `mikepenz/action-junit-report` renders. Between the swap and this, neither the annotations nor otel.ocx.sh saw a unit result at all.

**Enabling it locally** (off by default, and silently — no endpoint and no binary means no output and no failure):

1. Install `junit2otlp` 0.1.2 onto `PATH` from the [release tarball](https://github.com/mdelapenya/junit2otlp/releases/tag/v0.1.2) for your platform.
2. Write `~/.config/ocx-telemetry/env`, **mode 600** — it holds a password — with `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_HEADERS`. `taskfiles/telemetry.taskfile.yml` sources it itself, so it works from a shell that has not loaded it.

`task verify` then pushes: `rust:test:unit` reads `target/nextest/default/junit.xml` (the `[profile.default.junit]` entry in `.config/nextest.toml`), and the pytest legs read `test/results/junit.xml` / `junit-smoke.xml`, which they write on every run rather than only when CI passes the flag — a report produced only under a CI-supplied argument is one no local run has.

### Build telemetry (the Bazel half)

Same collector, **different credential scope, different service and different transport** (plan_bazel_build_adoption.md C-019, S-013). The reader is `scripts/bep_to_otlp.py`; its tests (`scripts/tests/test_bep_to_otlp.py`) run under `task scripts:verify` and is where its red states are proved. No check count is quoted here: it moves with every proof added, and a number this file cannot re-measure is a number that goes stale silently.

- **What is pushed.** Resource `service.name=bazel-build` — not `ocx-tests`; `monitoring/grafana/dashboards/bazel-build.json` in `server-hetzner1` (WP-27) filters on that string and blanks to *no data* if it changes. Per Bazel invocation: one `summary` span carrying the target count the reader actually read, plus one `target` span per `TargetComplete`, discriminated by `span.bazel.kind`. S-013 is the divergence panel: **Tempo span count == BEP target count** (+1 for the summary).
- **OTLP over HTTP/JSON, not gRPC.** nginx on otel.ocx.sh proxies `location /v1/` to `tempo:4318` and its own comment calls that the primary path; the gRPC location exists only because junit2otlp has no other transport. So the serialiser is stdlib `json`, the transport is stdlib `urllib`, and **the queue-drop budget that dominates the JUnit half does not exist here** — the POST is synchronous and the response is read. The one surviving drop vector is the server's `partialSuccess.rejectedSpans`, checked on every request. One endpoint value serves both halves: the reader appends `/v1/traces` to the same base.
- **Redirects are refused, never followed.** `urlopen`'s default handler rebuilds a redirected request and strips only *content* headers, so `Authorization` would survive across origins (CWE-200). OTLP/HTTP defines no redirect, so refusing one loses nothing.
- **The read surface is a field allowlist, not an event allowlist.** `BuildStarted` is the only carrier of the invocation id, and `BuildStarted.optionsDescription` holds the fully expanded option string, `--remote_cache=…` and all. `READ_FIELDS` is therefore per field; `prove_no_secret` asserts it against a credential planted in every committed fixture.
- **Where it runs, and where the stream lives.** Inside `bazel:build:nobuild` — **the only one of the four BEP producers (with `bazel:test:unit`, `rust:clippy:check`, `rust:doc:ratchet`) whose stream is exported** — as the `task telemetry:bazel` item at its tail, in CI and locally alike. No workflow has a push step for it; the task exports when `OTEL_EXPORTER_OTLP_ENDPOINT` and the headers are in its environment. The template's `lint` job, which runs it through `ci:lint`, passes the setup action no `otel-auth`, so CI exports nothing from this half until it does — a conventions-side change. `--nobuild` stops after analysis and still emits one `targetCompleted` per target: 322 for this `//...`, 293 KB. **The stream never touches the checkout.** Bazel serialises the whole client environment and every rc-file flag value into it: measured, 276 `--client_env` entries including `AWS_SECRET_ACCESS_KEY` with its value, plus nine `--remote_header=authorization=Basic …`. It was `target/bep.json`, mode 0644, inside the tree `Swatinem/rust-cache` saves on a push to `main`; it is now a `mktemp -d` removed by a `defer:` that runs whether the build, the floor or the export failed. A scanner keyed on `--client_env=` finds none of it — the BEP is proto3 JSON and the literal on disk is `--client_env=`; `bep_persistence_findings` and `prove_bep_not_persisted` are the reader and its red/green.
- **No composite action and no workflow step, deliberately.** The reader is python3 and stdlib, so there is no binary to download and no digest to pin — which is the whole reason the sibling `test-telemetry` is a composite action. And a step of its own would need the BEP to outlive the build step that wrote it, which is the thing this half must not allow; the taskfile item runs the same two lines in both lanes instead.
- **The credential is process-scoped on this half.** The task reads `OTEL_EXPORTER_OTLP_HEADERS` from its own environment. The template's setup action exports it job-wide through `$GITHUB_ENV` when `otel-auth` is passed, so every process of such a job inherits it — which is how a credential first reached a BEP, and why the stream never touches the checkout.
- **The floor is `CRATES_RULE_TARGETS` (78), the script's default, and no lane raises it.** It is a *reader* floor — it separates "the reader parsed nothing" from "the build was clean" — and 78 is the only number for this with a home (`scripts/bazel_gate_proofs.py`, re-asserted by `prove_counts`). Spelling the live `//...` count here would be a second copy of `STAGE_FLOORS["stage-4"]`, in a place nothing re-measures. The tighter reads of the same graph block two steps away: `bazel:build:drift` floors at 82 (`DRIFT_TARGET_FLOOR`), `bazel:tag:guard` at 357 (`STAGE_FLOORS["stage-4"]`, its default stage).
- **What blocks and what does not.** `bazel:build:nobuild` floors on the BEP it writes and **blocks** — a dropped `--build_event_json_file` is a defect in this tree, and the failure mode it prevents is an export that reports success while delivering nothing. The push itself never blocks (`continue-on-error: true`, `timeout-minutes: 5`, and the local task exits 0 after printing the failure): a collector outage is not a defect in this tree. **The only sanctioned silent exit is an unset endpoint** — every other path is one line on stderr and, in CI, a red step.

## Cost Factors

| Factor | Impact | Guidance |
|--------|--------|----------|
| **Job count** | 1 min minimum per job + ~8s startup | Combine short steps; split only when parallelism saves wall-clock |
| **Runner OS** | Linux $0.006, Windows $0.010, macOS $0.062 per min | Linux default; gate macOS/Windows behind Linux passing |
| **Artifact retention** | $0.008/GB/day; default 90 days | `retention-days: 1` inter-job, `7` debugging |
| **Cache** | 10 GB free, $0.07/GiB/month overage | `Swatinem/rust-cache` + `save-if: github.ref == 'refs/heads/main'` |
| **Duplicate builds** | A full Rust release build = 5-15 min | Share binaries via upload/download-artifact |
| **Matrix breadth** | N entries = N x job cost | `fail-fast: true` default |

Rust env to cut build time:

```yaml
env:
  CARGO_INCREMENTAL: "0"
  CARGO_PROFILE_TEST_DEBUG: "0"
```

## Recommended Actions (2026)

### Core

| Action | Version | Purpose |
|--------|---------|---------|
| `actions/checkout` | v6 | Repository checkout |
| `actions/upload-artifact` | v7 | Share files between jobs |
| `actions/download-artifact` | v8 | Receive shared files |
| `actions/cache` | v4 | Dependency caching |

### Rust

| Action | Version | Purpose |
|--------|---------|---------|
| `actions-rust-lang/setup-rust-toolchain` | v1 | Toolchain + cache + matchers |
| `Swatinem/rust-cache` | v2 | Cargo/target caching (standalone) |
| `taiki-e/install-action` | v2 | Install cargo tools (nextest, llvm-cov) |

### Quality + Infra + Security

| Action | Purpose |
|--------|---------|
| `mikepenz/action-junit-report@v6` | JUnit → annotations and job summary (`annotate_only: true` needs no `checks: write`) |
| `github/codeql-action/upload-sarif@v3` | SARIF → Security tab |
| `codecov/codecov-action@v5` | Coverage reporting |
| `go-task/setup-task@v2` | Install Taskfile runner |
| `astral-sh/setup-uv@v8` | Install uv (Python) |
| `softprops/action-gh-release@v2` | Create GitHub Releases |
| `aquasecurity/trivy-action@v0.33` | Vulnerability scanning |
| `actions/attest-build-provenance@v2` | SLSA Build Level 2 attestation |

No deprecated v3 artifact/cache actions. No `actions-rs/*`. Require Node 24+ runtime.

## Review Checklist

- [ ] Taskfile wrapping — every check callable via `task <name>`
- [ ] No command duplication — CI calls tasks, not raw commands
- [ ] Composite actions — repeated setup sequences extracted
- [ ] SHA pinning — every `uses:` has full commit SHA + version comment
- [ ] Permissions — explicit, minimal, at workflow and/or job level
- [ ] Concurrency — group + `cancel-in-progress` on non-main
- [ ] Cost check — no unnecessary `--release`; short artifact retention
- [ ] Annotations — problem matchers on; test results with `if: !cancelled()`
- [ ] Lint doesn't block tests — `continue-on-error` + final gate
- [ ] Artifact sharing — binary built once; downstream jobs skip rebuild
- [ ] Caching — `Swatinem/rust-cache` or equivalent; `CARGO_INCREMENTAL=0`
- [ ] No deprecated actions

## Authoring Workflow

Creating, modifying, auditing GitHub Actions workflow → follow sequence:

1. **Research** — Glob `.github/workflows/*.yml`, read existing structure. Read Taskfile to find which checks already have task definitions. Check action versions for deprecation; verify Node 24+ runtime.
2. **Cost analysis** — Estimate per-run cost via Cost Factors table above. Document estimate in PR description.
3. **Design** — Apply Design Principles (Taskfile wrapping, composite actions, lint doesn't block tests, artifact sharing, SHA pinning, minimal permissions, concurrency).
4. **Review** — Walk Review Checklist before merge.

### Tool Preferences

- **GitHub MCP** — `mcp__github__list_workflow_runs`, `get_workflow_run`, `get_workflow_run_logs` for structured run inspection. Fallback: `gh run list`, `gh run view`, `gh run view --log`.
- **`gh workflow run <name>`** — manual dispatch (no MCP parity yet).
- **WebFetch** — GitHub Actions changelog and deprecation notices.

### Handoffs

- To Builder — Taskfile changes needed by CI (Taskfile = source of truth)
- To Security Auditor — supply chain or permission review on new workflows

## Windows Shim Build Workflow (`build-windows-shims.yml`)

Cross-builds the `ocx_shim` binary for `x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm` on Linux runners via `cargo-zigbuild` — Zig bundles its own clang/lld/libc, so no Microsoft SDK manifest is fetched. Design authority: `.claude/artifacts/adr_shim_hermetic_zigbuild.md`, addendum 2.

### Structure

| Job | Runner | Purpose |
|-----|--------|---------|
| `gate` | `ubuntu-latest` | Host-runnable shim + launcher spec tests (`cargo nextest run -p ocx_shim -p ocx_store -p ocx_package_manager --locked` — `ocx_store` owns the committed blob, `ocx_package_manager` owns the launcher spec; nextest comes from `taiki-e/install-action`, the toolchain action does not ship it). Both other jobs carry `needs: gate`. |
| `build` (matrix) | `ubuntu-latest` | Per-arch `task rust:shim:build TARGET=${{ matrix.target }}`, then blob validation, SLSA attestation, artifact upload. `fail-fast: false` so one arch's status never masks the other's. |
| `acceptance-windows` | `windows-latest` | Native `cargo build -p ocx_shim --locked`, then `uv run pytest tests/test_windows_shim.py` with `OCX_SHIM_BINARY` set. Registry-independent (`OCX_TESTS_NO_REGISTRY=1`) — the `registry:2` fixture is Linux-only. Without this job the module is green-by-skip. |

### Key design points

- **The gate is provenance, not byte-equality.** The "Validate committed shim blob (provenance model)" step asserts three things about the committed blob under `crates/ocx_store/src/shims/`: MZ magic (valid PE), size within the `SHIM_SIZE_BUDGET` it greps out of `crates/ocx_store/src/shim.rs`, and `sha256` equal to that file's per-arch `SHIM_SHA256`. Nothing compares bytes against the fresh build — the gnullvm PE link embeds a per-link build id, so two runs of one pinned toolchain differ (ADR addendum 2). Drift control comes from pairing the SHA canary with the job's `paths:` filter (`crates/ocx_shim/**`, `Cargo.lock`, `rust-toolchain.toml`, `taskfiles/rust.taskfile.yml`, this workflow): a source change without a blob refresh reds the canary.
- **SLSA attestation** via `actions/attest-build-provenance` signs the *committed* blob (Sigstore/Rekor). The freshly built binary uploads as `ocx-shim-fresh-<target>` **before** the validation step and `if: !cancelled()`, because that artifact is the refresh source.
- **The whole toolchain is pinned.** `rust-toolchain.toml` pins rustc 1.95.0 and both gnullvm targets; `install:cargo-zigbuild` pins cargo-zigbuild 0.22.3; the workflow pins `ZIG_VERSION: 0.16.0` with a `ZIG_SHA256` the download is checked against. Zig's version changes the emitted bytes, so bump it deliberately in a blob-refresh PR, never incidentally. The `shim` profile (`opt-level="z"`, `lto`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`) is a size control, not a reproducibility one.
- **`RUSTFLAGS` goes on the command line, never in task `env:`.** go-task's task-level `env:` loses to an inherited variable and `setup-rust-toolchain` exports `RUSTFLAGS`, which left the former `-Clink-arg=/Brepro` inert in CI for every blob ever shipped while breaking clean dev shells (`zig cc` rejects the MSVC flag). `shim:build` prefixes `RUSTFLAGS=` on the `cargo zigbuild` line, carrying the `--remap-path-prefix` that keeps a dev box's rust-src `$HOME` paths out of the blob.
- **Gate-before-matrix** is the Cost Factors rule above ("gate macOS/Windows behind Linux passing"), not §"Never let lint block test results".
- **Signing is Phase 2.** The workflow explicitly does NOT sign. Authenticode signing via SignPath Foundation is a documented follow-on (ADR §Out of Implementation Scope plus the workflow's header and trailing comments). Do not add a signing step without the Phase-2 prerequisites.

### Refresh flow (updating the committed blob)

Canonical procedure: the `crates/ocx_store/src/shim.rs` module docs. In a dedicated PR:

1. `task rust:shim:build TARGET=x86_64-pc-windows-gnullvm` (and `aarch64-pc-windows-gnullvm`) — or download the `ocx-shim-fresh-<target>` artifact from a CI run, which exists because a dev box need not have a usable Zig.
2. Copy each `target/<triple>/shim/ocx-shim.exe` to `crates/ocx_store/src/shims/ocx-shim-{x86_64,aarch64}.exe`.
3. Record each blob's `sha256sum` in the matching per-arch `SHIM_SHA256` in `crates/ocx_store/src/shim.rs` — blob and constant must move together, or the canary reds.

## Windows & macOS Coverage — what each gate compiles

`cfg(windows)` and `cfg(target_os = "macos")` code is invisible to a host (Linux) build, so the coverage boundary is easy to overstate. What holds today:

| Gate | Platform scope |
|---|---|
| `task verify` (root) | **No Windows or macOS leg.** `.verify:lint` runs `rust:format:check` directly, not `rust:verify`, and `.verify:build-test`'s `rust:clippy:check` is a Linux-only Bazel pass (`platforms: [linux]`), so a macOS or Windows `task verify` runs no clippy. |
| `task rust:verify` → `check:windows-cfg` | `cargo check -p ocx_shim --locked --all-targets --target {x86_64,aarch64}-pc-windows-msvc` — **`ocx_shim` only**, deliberately scoped in `taskfiles/rust.taskfile.yml`. It type-checks none of `ocx_lib`'s `cfg(windows)` arms (`env.rs::pathext`, `setup/session_path/windows.rs`, `package_manager/launcher/generate.rs`, `package_manager/tasks/render_toolchain.rs`). No equivalent check exists for `cfg(target_os = "macos")`. |
| `task rust:check:windows` | Whole-workspace `cargo xwin check` for both MSVC targets in Docker — covers those Windows arms, but is opt-in: no task chain and no workflow invokes it. No macOS counterpart. |
| `verify-basic.yml` and `basic-repo.yml` | **No Windows or macOS leg** (D5, pinned by `test_basic_runs_on_linux_only`): a pull request compiles none of the `cfg(windows)` / `cfg(target_os = "macos")` arms. The only tier a pull request fires without a label, so that holds for every unlabelled PR — draft or ready, into `main` or into `announce/**`. |
| `deep-repo.yml` → `unit-native` Windows leg | The cross-built (Linux host, Bazel) `x86_64-pc-windows-gnullvm` unit-test bundle, executed on `windows-latest` by `task cross:test:run`; no compile on the runner. Nightly, `workflow_dispatch` / `workflow_call`, and a PR labelled `run-deep` — **never an unlabelled pull request**. The only CI gate that runs `ocx_lib`'s `cfg(windows)` code, `#[cfg(windows)]` unit tests included. |
| `deep-repo.yml` → `unit-native` macOS leg | The cross-built `aarch64-apple-darwin` unit-test bundle, executed on `macos-latest`; same triggers as the Windows leg. The only CI gate that runs the LaunchAgent session-PATH writer (`setup/session_path/macos.rs`). |
| `deep-repo.yml` → `acceptance-native` (macOS, Windows) | The smoke tier (`task test:smoke`, `OCX_TEST_NATIVE_SERVICES=1`) against the `test-bin-<target>` artifact `cross.yml` cross-built, on native `zot` (`task test:services:native:start`); no Bazel, cargo or Docker on the runner. The full suite does not run there yet (follow-up); the full suite runs on Linux, in `acceptance.yml`. |
| Windows target identity | Every Windows binary CI builds or tests is `*-pc-windows-gnullvm` (hermetic `@llvm`, cross-built from Linux). **Release ships MSVC** (cargo-dist), so no CI gate executes the shipped artifact's linker output or CRT; this gap is recorded in [`adr_bazel_rules_rs_migration.md`](../artifacts/adr_bazel_rules_rs_migration.md), and the MSVC `cargo check` arms above (`check:windows-cfg`, `rust:check:windows`) remain the only MSVC type-check. |

## Anti-Patterns

| Anti-Pattern | Fix |
|--------------|-----|
| Raw `cargo` in CI duplicating Taskfile | Call `task <name>` |
| Repeated setup across jobs | Composite action in `.github/actions/` |
| `@v4` tag without SHA | Pin by full commit SHA |
| Lint failure blocks test execution | `continue-on-error` + final gate |
| Binary rebuilt in every job | `upload-artifact` + `download-artifact` |
| Default 90-day artifact retention | `retention-days: 1` inter-job |
| No concurrency control | Add `concurrency:` block |
| Invalid context in a workflow (e.g. `matrix` on a step's `shell:` key) | Run `task ci:actionlint`; for matrixed shells use job-level `defaults.run.shell` |
| `fetch-depth: 0` when not needed | Default `fetch-depth: 1` |
| `pull_request_target` + checkout of PR head | Use `pull_request` trigger for untrusted code |
| `cargo test` instead of `cargo nextest` | nextest is ~40% faster in CI |
| `--release` for unit tests | Debug; reserve release for published binaries |
| Saving cache on failed builds | `cache-on-failure: false` |
| Editing generated workflows directly | Edit the config (`dist-workspace.toml`) and re-run `dist generate-ci`. `release.yml` is generated by cargo-dist — manual edits get overwritten. Hand-written workflows (`verify-version.yml`) can be edited directly. |