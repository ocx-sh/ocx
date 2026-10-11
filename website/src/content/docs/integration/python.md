---
title: "Use ocx with Python"
description: "Drive ocx from Python code with the ocx-sdk package, and run a PEP 751-locked Python application as an ocx package."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows the two ways Python and ocx meet. You can call ocx from Python code through the `ocx-sdk` package. You can also ship a Python application as an ocx package that runs with no pip, uv or virtual environment.

ocx has no `ocx python` command and does not read a `pylock.toml` itself. The lock translation lives in a library that [ocx-mirror][ocx-mirror] links, so the second route is a publisher task and not an everyday one.

## Drive ocx from Python {#python-sdk}

[`ocx-sdk`][ocx-sdk-pypi] is a typed Python wrapper over the `ocx` CLI. Its methods mirror the commands one for one, and it parses only what ocx already validated. It needs Python 3.12 or later and has no runtime dependencies.

```sh
uv add ocx-sdk
```

A script needs three calls. `bootstrap.ensure()` downloads, verifies and caches a pinned `ocx` binary. `Ocx(exe=...)` wraps that binary. `ocx.project(path)` opens a directory that holds an `ocx.toml`, and its `pull()` and `exec()` methods run [`ocx pull`][cmd-pull] and [`ocx exec`][cmd-exec] there.

Use it where a Python test suite, a build script or a notebook needs a pinned tool. The SDK documentation has the full guides:

- [Run your first pinned tool][sdk-quickstart] is a five-minute tutorial.
- [Reproduce a toolchain in CI][sdk-ci] pins ocx, pulls the lock and fails on drift.
- [Test against a pinned tool in pytest][sdk-pytest] shows a fixture.
- [Bootstrap][sdk-bootstrap] covers pinning, corporate mirrors and hardened environments.

The SDK shells out to the CLI, so every setting in this section applies to it as well. [Run ocx unattended][p-unattended-ci] lists the variables, and [Read ocx output from a script][p-scripts-and-json] describes the JSON it parses.

## Run a locked Python application as an ocx package {#python-app}

A Python application normally needs an interpreter, pip or uv and a virtual environment on every machine that runs it. A mirror can instead turn its lock into one ocx package that holds every wheel plus a private interpreter. The consumer then runs it like any other package:

```sh
ocx package exec dev.ocx.sh/ocx/black:26.5.1 -- black --version
```

Nothing on the consumer's machine needs Python, and [`ocx package exec`][cmd-package-exec] runs it in a clean environment. The package declares the interpreter as a private [dependency][guide-dependencies], and each console script of the application becomes a [package entrypoint][publishing-entry-points].

### What the translation does {#python-translation}

The translation starts from a [PEP 751][pep-751] `pylock.toml`. The `ocx_python` crate runs five steps, and every writer of the shared registry namespace uses the same code so that the artifacts stay byte-compatible.

1. It parses the lock.
2. It selects one wheel per package for each target platform, checked against the interpreter version and ABI.
3. It repacks each wheel into a layer.
4. It checks that no two wheels write the same file.
5. It composes the environment package: the layers, the interpreter dependency, the environment variables and the entrypoints.

Selection fails closed. A compiled wheel whose ABI tag does not match the target is an error. So is a lock that names a wheel the platform cannot use. A wheel layer is shared across the applications that use the same wheel.

### Mirror an application {#python-mirror}

The mirror tool is [ocx-mirror][ocx-mirror], which has its own repository and documentation. Install it with `ocx --global add ocx.sh/ocx/mirror`, which provides the `ocx-mirror` binary. Start from a [PEP 751][pep-751] lock. With a `uv` project, `uv export` writes one:

```sh
uv export --format pylock.toml --output-file black.pylock.toml
```

A `pylock` source in the mirror spec `black.yml` then names the lock, the interpreter package and the target ABI:

```yaml
name: black
target:
  registry: dev.ocx.sh
  repository: ocx/black
source:
  type: pylock
  path: black.pylock.toml
python:
  version: "3.14.6"
  abi: cp314
  interpreter_package: "ocx.sh/cpython:3.14.6"
wheels:
  "linux/amd64+libc.glibc":   # empty entry keeps the default wheel filter
platforms:
  linux/amd64:
    runner: ubuntu-latest
```

Every `wheels` key needs a `platforms` entry for its base `os/arch`, which names the runner that tests it. The `+libc.glibc` suffix on a `wheels` key is not repeated in `platforms`: `platforms` keys stay plain.

Run `ocx-mirror package check black.yml` to see what a sync would push. It is a dry run and prints a table of version and platform pairs with what a real run would do. A `total / pushed / skipped / failed` summary follows.

Run `ocx-mirror package sync black.yml` to publish. It prints the same table and summary. A first run that publishes every version ends with a line like `12 total, 12 pushed, 0 skipped, 0 failed`, with your own counts. A `failed` count of 0 means every version reached the registry.

Then run `ocx package exec dev.ocx.sh/ocx/black:latest -- black --version` to confirm the package runs. The spec may also list `tests`, as the [mirror configuration reference][mirror-yml-pylock] shows.

A `pypi` source discovers versions from a PyPI-compatible index and derives one lock per version. The [mirror configuration reference][mirror-yml-pylock] lists every field, including the wheel filters and the entrypoint selection.

This page does not cover publishing the result. A mirrored package is an ordinary package, so [Publish packages][p-publishing-build-and-pushx] and [Publisher CI][p-publisher-ci] apply to it unchanged.

<!-- external -->
[ocx-mirror]: https://github.com/ocx-sh/ocx-mirror
[ocx-sdk-pypi]: https://pypi.org/project/ocx-sdk/
[pep-751]: https://peps.python.org/pep-0751/
[sdk-quickstart]: /integrations/python/guide/quickstart/
[sdk-ci]: /integrations/python/guide/reproducible-ci/
[sdk-pytest]: /integrations/python/guide/pytest-fixture/
[sdk-bootstrap]: /integrations/python/guide/bootstrap/
[mirror-yml-pylock]: https://github.com/ocx-sh/ocx-mirror/blob/main/docs/reference/mirror-yml.md#pylock

<!-- commands -->
[cmd-exec]: ../reference/command-line/commands.md#exec
[cmd-pull]: ../reference/command-line/commands.md#pull
[cmd-package-exec]: ../reference/command-line/package-consuming.md#package-exec

<!-- guides -->
[guide-dependencies]: ../publishing/declare-dependencies.md
[publishing-entry-points]: ../publishing/entry-points.md

<!-- pages -->
[p-unattended-ci]: ./unattended-ci.md
[p-scripts-and-json]: ./scripts-and-json.md
[p-publishing-build-and-pushx]: ../publishing/build-and-push.mdx
[p-publisher-ci]: ./publisher-ci.md
