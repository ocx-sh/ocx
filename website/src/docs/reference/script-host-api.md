---
outline: deep
---

# Script Host API

`ocx package test --script` interprets a [Starlark][starlark-lang] script against the materialized package environment. The script has no access to the host shell. It calls into two host modules — `ocx.*` for the sandbox surface and `expect.*` for assertions — and receives back typed values whose shape this page documents.

The host API is a small contract: every host function and every typed value listed here is part of the v1 surface that a script written today must keep running on. Drift between this page and the code is a Block-tier review finding.

## At a glance {#overview}

```python
# Reach into the package environment.
r = ocx.run("shfmt", "--version")
expect.ok(r)
expect.contains(r.stdout, "v3")

# Compare typed enum constants — no string magic.
p = ocx.target_platform
if p.os == ocx.os.Linux:
    expect.eq(p.arch, ocx.arch.Amd64)
```

The Starlark surface is split into:

- **Host functions** on the `ocx` namespace — call into the sandbox.
- **Typed values** — return shapes the host functions hand back.
- **Type namespaces** — `ocx.os` and `ocx.arch` carry the typed enum constants.
- **Assertion functions** on the `expect` namespace.

## `ocx.*` host functions {#ocx-fns}

Every function below is a method (always written with parens). Returned values are typed (declared shape, attributes via `r.exit_code` etc.), never anonymous dicts.

### `ocx.run(prog, *args, *, env=None, cwd=None, stdin=None)` {#ocx-run}

| | |
|---|---|
| **Returns** | [`RunResult`](#run-result-value) |
| **Purpose** | Spawn a binary from the composed package env. Capture stdout / stderr (with truncation cap), exit code, and wall-clock duration. |

The first positional argument is the program; the rest are argv. Splat a list with `ocx.run(*cmd)`. Calling with zero positional args fails the script.

The keyword-only parameters:

- `env` — dict overlaid on top of the composed env for this call only. Reserved keys (`PATH`, `OCX_HOME`, the `OCX_*` loader vars, and the `OCX_AUTH_*` credential family) are rejected.
- `cwd` — defaults to the scratch root. Validated against the sandbox guard (no symlink escapes).
- `stdin` — string written to the child's stdin.

A non-zero exit code does **not** raise. The script decides whether to fail.

:::warning Spawned binaries are not sandboxed
The sandbox applies to the `ocx.*` host API only — file reads, writes, and path resolution. Binaries launched via `ocx.run` run with normal host OS privileges, exactly as `-- CMD` does.
:::

### `ocx.env(name)` {#ocx-env}

| | |
|---|---|
| **Returns** | `str \| None` |
| **Purpose** | Read one variable from the composed package env. |

`OCX_AUTH_*` credentials and `OCX_*` resolution-affecting keys are never readable — the function returns `None` for those names regardless of whether they are set in the host env.

### `ocx.target_platform` {#ocx-target-platform}

| | |
|---|---|
| **Kind** | Attribute (no parens) — per-run constant materialized at script-engine init. |
| **Type** | [`Platform`](#platform-value) |
| **Purpose** | Reflect the `-p` flag passed to `ocx package test` — the platform the package was built/tested for. |

Named `target_platform` (not `host_platform`) because Bazel-style terminology distinguishes host from target, and what a script sees here is the target. The host platform may differ. For platform-agnostic packages (`-p any` or omitted), `p.is_any` is `True` and `p.os` / `p.arch` are `None`.

Exposed as an attribute rather than a method because the value never changes during a single script run — calling it would be ceremony for a constant.

### `ocx.package_root` / `ocx.scratch_root` {#ocx-roots}

| | |
|---|---|
| **Kind** | Attributes (no parens) — per-run path constants materialized at script-engine init. |
| **Type** | `str` |
| **Purpose** | `package_root` is the materialized package directory (read-only). `scratch_root` is the writable scratch directory. Both `/`-normalized on every platform. |

Exposed as attributes rather than methods for the same reason as [`ocx.target_platform`](#ocx-target-platform): a path that never changes during a single run is a constant, so parens would be ceremony.

### `ocx.read_file(path, *, max_bytes=1048576)` {#ocx-read-file}

| | |
|---|---|
| **Returns** | `str` |
| **Purpose** | Read a UTF-8 file within `{scratch_root, package_root}`. Truncated at `max_bytes` (default 1 MiB). Symlink-escape attempts and non-UTF-8 content are rejected. |

### `ocx.write_file(path, content)` {#ocx-write-file}

Write a file within `scratch_root` only. Parent directories must exist. Symlink-escape attempts are rejected.

### `ocx.exists(path)` {#ocx-exists}

| | |
|---|---|
| **Returns** | `bool` |
| **Purpose** | Existence check within `{scratch_root, package_root}`. Same guard as `read_file`. |

### `ocx.mkdir(path)` {#ocx-mkdir}

Recursive, idempotent `mkdir -p` inside `scratch_root`.

## Typed values {#typed-values}

### `Platform` {#platform-value}

Returned by [`ocx.target_platform`](#ocx-target-platform).

| Attribute | Type | Description |
|---|---|---|
| `is_any` | `bool` | `True` for platform-agnostic packages (`Platform::Any`), `False` for a specific OS/arch target. |
| `os` | [`os`](#os-value) \| `None` | The target OS, or `None` when `is_any` is `True`. |
| `arch` | [`arch`](#arch-value) \| `None` | The target CPU architecture, or `None` when `is_any` is `True`. |

`str(p)` returns `"any"` for the sentinel form or `"os/arch"` for the populated form — same shape as the `-p` flag.

### `os` (operating system) {#os-value}

Typed enum value. `type(ocx.os.Linux) == "os"`. Instances live in the [`ocx.os`](#ocx-os) namespace. Compares equal only to another `os` value of the same variant — never to a string. `str(value)` returns the lowercase OCI string (e.g. `"linux"`).

### `arch` (CPU architecture) {#arch-value}

Typed enum value. `type(ocx.arch.Amd64) == "arch"`. Instances live in the [`ocx.arch`](#ocx-arch) namespace. Same equality and `str()` semantics as `os`.

### `RunResult` {#run-result-value}

Returned by [`ocx.run`](#ocx-run).

| Attribute | Type | Description |
|---|---|---|
| `exit_code` | `int` | Child exit code. Signal-killed children report `128 + signal` (Unix convention). |
| `stdout` | `str` | Captured stdout (UTF-8 lossy), truncated at 10 MiB. |
| `stderr` | `str` | Captured stderr (UTF-8 lossy), truncated at 10 MiB. |
| `duration_ms` | `int` | Wall-clock duration of the spawn, in milliseconds. |
| `truncated` | `bool` | `True` iff stdout or stderr hit the 10 MiB capture cap. |

## Type namespaces {#type-namespaces}

### `ocx.os` {#ocx-os}

| Constant | `str(...)` |
|---|---|
| `ocx.os.Linux` | `"linux"` |
| `ocx.os.Darwin` | `"darwin"` |
| `ocx.os.Windows` | `"windows"` |

The set is closed — every variant supported by OCX. Adding a new variant Rust-side without also extending this namespace is caught by a structural parity test.

Prior art for the lowercase namespace shape: Bazel [`@platforms//os:linux`][bazel-platforms] and Buck2 [`host_info().os`][buck2-host-info].

### `ocx.arch` {#ocx-arch}

| Constant | `str(...)` |
|---|---|
| `ocx.arch.Amd64` | `"amd64"` |
| `ocx.arch.Arm64` | `"arm64"` |

Same closed-set rule and structural parity gate as `ocx.os`.

### Cross-type wall {#cross-type-wall}

The typed constants are not interchangeable with strings, and an `OperatingSystem` is not interchangeable with an `Architecture`. Comparisons across types return `False`, never an error:

```python
expect.ne(ocx.os.Linux, "linux")        # typed != string
expect.ne(ocx.os.Linux, ocx.arch.Amd64) # typed != typed-of-other-kind
```

Same-variant comparisons inside a single typed namespace are equal:

```python
expect.eq(ocx.os.Linux, ocx.os.Linux)
expect.eq(ocx.arch.Amd64, ocx.arch.Amd64)
```

## `expect.*` assertions {#expect-fns}

Each assertion exits the script with the documented exit code on failure. The captured failure message reads back in the `--format json` report under the `kind` field (a stable wire token: `ok`, `eq`, `ne`, `true`, `false`, `contains`, `matches`, `fail`).

| Function | Purpose |
|---|---|
| `expect.ok(result, msg=None)` | Assert `result.exit_code == 0`. On failure, the captured stderr is included in the message automatically. |
| `expect.eq(actual, expected, msg=None)` | Assert equality. |
| `expect.ne(actual, expected, msg=None)` | Assert inequality. |
| `expect.true(cond, msg=None)` | Assert truthiness. |
| `expect.false(cond, msg=None)` | Assert falsiness. |
| `expect.contains(haystack, needle, msg=None)` | Substring check for strings; membership check for lists. |
| `expect.matches(text, pattern, msg=None)` | Regex match using [Rust `regex` syntax][rust-regex]. An invalid pattern exits with code 65. |
| `expect.fail(msg)` | Unconditional failure. |

The builtin Starlark `fail(msg)` is also available — same exit code, separate `kind` attribution (`fail` for the assertion form, `fail` for the builtin).

## Exit codes {#exit-codes}

| Code | Meaning |
|---|---|
| `0` | All expectations passed. |
| `1` | An expectation failed, `expect.fail` / `fail()` was called, or a host API returned a failure. |
| `64` | Usage error — bad invocation. |
| `65` | Script syntax / arity / type error, or invalid regex in `expect.matches`. |
| `74` | I/O error — stdin read failure on `--script -`, scratch I/O failure. |

## See also {#see-also}

- [Authoring → Testing locally][authoring-testing] — narrative introduction with smoke-test patterns.
- [`ocx package test` reference][cmd-package-test] — full flag table.

<!-- external -->
[starlark-lang]: https://github.com/bazelbuild/starlark
[rust-regex]: https://docs.rs/regex/latest/regex/#syntax
[bazel-platforms]: https://github.com/bazelbuild/platforms
[buck2-host-info]: https://buck2.build/docs/api/build/native/#host_info

<!-- commands -->
[cmd-package-test]: ./command-line.md#package-test

<!-- authoring -->
[authoring-testing]: ../authoring/testing.md
