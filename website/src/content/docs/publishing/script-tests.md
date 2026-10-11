---
title: "Write script tests for a package"
description: "Assert on a package's behavior with a Starlark script that runs the same on every platform, using ocx package test --script."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

Write a Starlark script that asserts on a package's behavior, then run it with `ocx package test --script`. You need a built archive and its identifier, as in [Test a package before you push it][test-locally]. The script runs the same on Linux, macOS and Windows, with no host shell.

## Why scripts instead of a command {#scripted-tests}

The `-- CMD` form works well when the package ships its own test runner. Packages of standalone executables such as `cmake`, `shellcheck` and `goreleaser` do not. They need `sh -c '...'` on the host, which breaks on Windows without WSL or Git Bash.

`--script PATH` solves this. Instead of exec'ing a command, OCX interprets a [Starlark][starlark-lang] script against the materialized package environment. The script has no access to the host shell or runtime. It runs identically on `linux/*`, `macos/*`, and `windows/*`.

:::note[What is Starlark?]
[Starlark][starlark-lang] is a deterministic, Python-like scripting language designed for embedding in build tools. [Bazel][bazel] and [Buck2][buck2] use it. It has no `while` loops, iterates in a deterministic order and starts with an empty sandbox. Those properties make it safe to run in a package manager.
:::

## Run a script {#scripted-tests-invocation}

```sh
# Read script from a file
ocx package test -p linux/amd64 -i shfmt/shfmt:3.8.0 shfmt.tar.xz --script smoke.star

# Read script source from stdin (the value `-` means stdin)
printf 'r = ocx.run("shfmt", "--version")\nexpect.ok(r)\n' \
  | ocx package test -p linux/amd64 -i shfmt/shfmt:3.8.0 shfmt.tar.xz --script -
```

`--script` and `-- CMD` are mutually exclusive. Supplying both exits with code 64, and so does supplying neither. With `--script -`, OCX reads the script source from stdin. A read failure such as a broken pipe or a closed stream exits with code 74.

A typical smoke test runs the binary, checks its output and checks the env the package exports. Save this as `smoke.star`, the file the run command above passes:

```python
r = ocx.run("shfmt", "--version")
expect.ok(r)
expect.contains(r.stdout, "v3")

# Verify the package exported the expected env var
expect.true(ocx.env("SHFMT_ROOT") != None, "SHFMT_ROOT should be set")

# Branch on the platform the package was built for
if ocx.target_platform.os == ocx.os.Linux:
    expect.eq(ocx.target_platform.arch, ocx.arch.Amd64)
```

Read paths are relative. They resolve against the scratch root first, then the bundle's `content_root`, so a file the package ships at `bin/javac` is read as `bin/javac`:

```python
if ocx.exists("bin/javac"):
    expect.ok(ocx.run("javac", "-version"))
```

Every `ocx.*` function and `expect.*` assertion, with its arguments and return types, is listed in the [Script Host API reference][ref-script-host-api].

## Sandbox model {#scripted-tests-sandbox}

The script runs inside a bounded environment:

- **Writable area**: a scratch directory created as a sibling of the package root. The `ocx.scratch_root` attribute holds its path. Files written here survive `--keep`.
- **Read area**: the bundle's own files (`ocx.content_root`) are readable but not writable. The package directory around them is storage structure, not a script contract, so a script never has to name it.
- **Symlink containment**: every path is validated for symlink escape after lexical normalization. A symlink inside `scratch_root` that points outside is refused on access, not just at creation.
- **Path portability**: use `/` as the separator on all platforms. Absolute paths and `..` escapes are rejected, including an absolute path built from `ocx.content_root`. Use the relative spelling.

:::caution[Spawned binaries are not sandboxed]
The sandbox applies to the `ocx.*` host API only: file reads, writes, and path resolution. Binaries launched via `ocx.run` run with normal host OS privileges, exactly as `-- CMD` does. A binary can write anywhere the OS allows. This matches the trailing-command form and is a documented v1 scope limit.
:::

Re-entrant `ocx` invocations are refused in v1. `ocx.run("ocx", ...)` exits with code 1 and a message explaining the limitation.

`ocx.run` resolves a bare program name the way the trailing-command form does, [against the package first][test-locally-resolution]. A name your package ships but cannot execute fails the script instead of running the host's copy. Names the package does not ship still resolve on the host `PATH`.

## Output format {#scripted-tests-output}

Pass `--format json` to get a structured envelope alongside the exit code:

```sh
ocx package test -p linux/amd64 -i shfmt/shfmt:3.8.0 shfmt.tar.xz \
  --script smoke.star --format json
```

The envelope has three top-level keys, all part of the stable v1 contract:

```json
{
  "status": "passed|failed|usage|script_error|io|timeout",
  "assertion": { "kind": "ok|eq|ne|true|false|contains|matches|fail|other|unknown", "message": "…",
                 "location": { "file": "smoke.star", "line": 12, "column": 5 } },
  "run":       { "exit_code": 0, "stdout": "…", "stderr": "…", "duration_ms": 12, "truncated": false }
}
```

`assertion` and `run` are `null` when not applicable. For example, `assertion` is `null` on a passing run. `assertion.kind` names the `expect.*` function that triggered the failure and is the stable machine field for tooling. The `assertion.message` prose is not stable.

`assertion.location` points at the failing statement, 1-indexed. Its `file` is the `--script` argument verbatim, or `<stdin>`. It is omitted when the failure has no source position, such as a timeout. The exit code remains the primary machine signal.

## Exit codes {#scripted-tests-exit-codes}

| Code | Meaning |
|------|---------|
| 0 | All expectations passed |
| 1 | An expectation failed, `expect.fail` was called, or a host API returned a failure |
| 64 | Usage error: both `--script` and `-- CMD` supplied, neither supplied, or script file not found |
| 65 | Script syntax, type, or arity error |
| 74 | I/O error: stdin read failure (`--script -`) scratch directory creation failure, or a `--junit` report that cannot be written after an otherwise green run |

## JUnit reports for CI {#scripted-tests-junit}

`--junit PATH` writes a [JUnit XML][junit-xml-format] report beside the JSON envelope. A CI system can then surface the run as a test result instead of a bare exit code:

```sh
ocx package test -p linux/amd64 -i shfmt/shfmt:3.8.0 shfmt.tar.xz \
  --script smoke.star --junit build/junit/linux-amd64.xml
```

Parent directories are created, so the pipeline needs no `mkdir` step. `--junit` requires `--script` and is not available for the trailing `-- CMD` form.

The file holds one `<testsuite name="ocx package test <identifier>">` with one `<testcase classname="<identifier>" name="<platform>">`. One invocation tests one identifier on one platform.

A failing assertion becomes a `<failure>` element. Its `type=` holds the failing `expect.*` kind and its `message=` holds the first diagnostic line. The element body holds the full multi-line diagnostic.

The failure also carries `file` and `line` pointing at the failing statement in the script. A script read from `--script -` has no source file, so both attributes are omitted.

A script that never delivered a verdict becomes an `<error>` instead. That covers a script that is unreadable, syntactically broken or timed out. Captured stdout and stderr are attached as `system-out` and `system-err` on every run that produced output.

The report is written whenever the scripted run is reached, including a red run and an unreadable `--script` path. A failure resolving the package beforehand writes none. The JSON envelope on stdout and the exit code are unchanged either way.

### One path per platform {#scripted-tests-junit-per-platform}

An existing file is **truncated**, never merged: ocx writes the one testcase it just produced and nothing else. Give every matrix leg its own path. Pointing every leg at one path keeps only the last leg's result.

```yaml
test:
  parallel:
    matrix:
      - PLATFORM: [linux/amd64, linux/arm64, darwin/arm64]
  script:
    - ocx package test -p "$PLATFORM" -i shfmt/shfmt:3.8.0 shfmt.tar.xz
        --script smoke.star --junit "build/junit/${PLATFORM//\//-}.xml"
  artifacts:
    reports:
      junit: build/junit/*.xml
```

What the `artifacts:reports:junit` glob buys you differs by CI system. [GitLab][gitlab-unit-test-reports] reads the `<testsuite name=>` attribute for internal grouping only and never displays it. Its test report UI labels each suite by `testcase classname`. ocx sets that to the package identifier, the same value as `<testsuite name>`. A GitLab reader therefore still sees the identifier as the suite label.

Each `parallel: matrix` leg is its own job with its own report. The glob only collects the files as job artifacts and does not merge them. The three platforms above therefore show up as three separate entries in GitLab's test report UI, one per leg.

The shared suite name and per-platform case name serve the other consumer. A [GitHub test-reporter action][github-test-reporter] downloads the JUnit artifacts from every matrix leg and parses them together. It sees one suite (`ocx package test <identifier>`) with one `<testcase>` per platform. Nothing in the file distinguishes the leg except the platform-specific `name=`. Design for that shape on GitHub, and plan for one suite per leg on GitLab.

## Editor integration {#scripted-tests-ide}

For `.star` syntax highlighting in VS Code, add the [vscode-bazel][vscode-bazel] extension to your workspace.

## See also {#see-also}

- [Script Host API reference][ref-script-host-api]: every `ocx.*` function and `expect.*` assertion
- [Test a package before you push it][test-locally]: the `-- CMD` form and the temp directory flags
- [`ocx package test` reference][cmd-package-test]: full flag table

<!-- external -->
[starlark-lang]: https://github.com/bazelbuild/starlark
[bazel]: https://bazel.build/
[buck2]: https://buck2.build/
[vscode-bazel]: https://marketplace.visualstudio.com/items?itemName=BazelBuild.vscode-bazel
[junit-xml-format]: https://llg.cubic.org/docs/junit/
[gitlab-unit-test-reports]: https://docs.gitlab.com/ci/testing/unit_test_reports/
[github-test-reporter]: https://github.com/dorny/test-reporter

<!-- commands -->
[cmd-package-test]: ../reference/command-line/package-publishing.md#package-test

<!-- internal -->
[test-locally]: ./test-locally.mdx
[test-locally-resolution]: ./test-locally.mdx#basic-resolution
[ref-script-host-api]: ../reference/script-host-api.md
