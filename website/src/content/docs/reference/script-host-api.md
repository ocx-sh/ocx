---
title: "Script Host API"
---
<!-- doc_type: reference -->

`ocx package test --script` interprets a [Starlark][starlark-lang] script against the materialized package environment, with no access to the host shell. The script calls the `ocx.*` and `expect.*` modules and receives typed values, and every function and value listed here is part of the v1 contract.

## At a glance {#overview}

A script runs a binary from the package environment, asserts on the result and branches on the typed target platform.

```python
r = ocx.run("shfmt", "--version")
expect.ok(r)
expect.contains(r.stdout, "v3")

p = ocx.target_platform
if p.os == ocx.os.Linux:
    expect.eq(p.arch, ocx.arch.Amd64)
```

## `ocx.*` host functions {#ocx-fns}

Every function is a method written with parens, while `ocx.target_platform` and the root paths are attributes. Returned values are typed, never anonymous dicts.

### `ocx.run(prog, *args, *, env=None, cwd=None, stdin=None)` {#ocx-run}

Spawns a binary from the composed package env and captures stdout, stderr, exit code and wall-clock duration. A non-zero exit code does not raise, and the script decides whether to fail.

| | |
|---|---|
| **Returns** | [`RunResult`](#run-result-value) |
| **`prog`, `*args`** | The first positional argument is the program, the rest are argv; splat a list with `ocx.run(*cmd)`. Zero positional args fails the script. |
| **`env`** | Dict overlaid on the composed env for this call only. Reserved keys (`PATH`, `OCX_HOME`, the `OCX_*` loader vars, the `OCX_AUTH_*` credential family) are rejected. |
| **`cwd`** | Defaults to the scratch root. Validated against the sandbox guard (no symlink escapes). |
| **`stdin`** | String written to the child's stdin. |
| **Resolution** | A bare program name resolves against the composed package directories before the host `PATH`. A package-shipped copy that cannot be executed fails the script instead of falling through to the host; a name the package does not ship resolves on the host. |
| **Unspawnable** | A name that resolves nowhere, or a file that is not executable, fails the script outright and never yields a `RunResult`. Probe with [`ocx.exists`](#ocx-exists) to ask whether a bundle ships a binary. |
| **Sandbox** | Spawned binaries are not sandboxed: they run with normal host OS privileges, exactly as `-- CMD` does. The sandbox covers the `ocx.*` host API only. |

### `ocx.env(name)` {#ocx-env}

Reads one variable from the composed package env, the same surface `ocx package exec` exposes. It does not read the host process env.

| | |
|---|---|
| **Returns** | `str \| None`; `None` when the variable is not set |
| **Never readable** | `OCX_AUTH_*` credentials and `OCX_*` resolution-affecting keys (`OCX_BINARY_PIN`, `OCX_HOME`, `OCX_CONFIG`, `OCX_PROJECT`, `OCX_INDEX` and the rest) return `None` whether or not the host sets them. |
| **Overlay** | The same keys cannot be set through the `ocx.run(env=...)` overlay; an attempt yields a `Failed` outcome. |

### `ocx.target_platform` {#ocx-target-platform}

Reflects the `-p` flag passed to `ocx package test`, the platform the package was built and tested for. The host platform may differ.

| | |
|---|---|
| **Kind** | Attribute (no parens), a per-run constant materialized at script-engine init |
| **Type** | [`Platform`](#platform-value) |
| **Platform-agnostic** | With `-p any` or no `-p`, `p.is_any` is `True` and `p.os` / `p.arch` are `None`. |

### `ocx.content_root` / `ocx.package_root` / `ocx.scratch_root` {#ocx-roots}

Path constants for the bundle's files, its package directory and the writable scratch directory.

| | |
|---|---|
| **Kind** | Attributes (no parens), per-run constants materialized at script-engine init |
| **Type** | `str`, `/`-normalized on every platform |
| **`content_root`** | The bundle's own file tree, read-only. [`ocx.read_file`](#ocx-read-file) and [`ocx.exists`](#ocx-exists) fall back to it, so an archive whose root holds `bin/` and `release` is read as `bin/javac` and `release`, never with a prefix naming the directory the package sits in. |
| **`package_root`** | The package directory holding the bundle, read-only. Its layout is storage structure, not a script contract; use it only to build an absolute path for [`ocx.run`](#ocx-run). |
| **`scratch_root`** | The writable scratch directory. |

```python
ocx.run(ocx.content_root + "/bin/java", "-version")
```

### `ocx.read_file(path, *, max_bytes=1048576)` {#ocx-read-file}

Reads a UTF-8 file within `{scratch_root, content_root}`.

| | |
|---|---|
| **Returns** | `str`, truncated at `max_bytes` (default 1 MiB) |
| **`path`** | Relative; resolved against the scratch root first, then [`content_root`](#ocx-roots). An absolute path is rejected before anything else, including one built from `ocx.content_root`. |
| **Rejected** | Symlink escapes and non-UTF-8 content |

```python
ocx.read_file("release")            # the bundle's own `release` file
ocx.read_file(ocx.content_root + "/release")   # rejected: path escapes the sandbox
```

### `ocx.write_file(path, content)` {#ocx-write-file}

Writes a file within `scratch_root` only. Parent directories must exist, and symlink escapes are rejected.

### `ocx.exists(path)` {#ocx-exists}

Checks existence within `{scratch_root, content_root}`, with the same guard and relative-path rule as [`read_file`](#ocx-read-file). It is the supported way to ask what a bundle contains, such as a JDK against a JRE in a `variants:` repository.

| | |
|---|---|
| **Returns** | `bool` |
| **Absent path** | Returns `False` rather than raising. |
| **Escaping path** | Fails the script. |

### `ocx.mkdir(path)` {#ocx-mkdir}

Recursive, idempotent `mkdir -p` inside `scratch_root`.

## Typed values {#typed-values}

Shapes the host functions hand back and the `ocx.os` / `ocx.arch` namespaces populate.

### `Platform` {#platform-value}

Returned by [`ocx.target_platform`](#ocx-target-platform). `str(p)` returns `"any"` for the sentinel form or `"os/arch"` for the populated form, the same shape as the `-p` flag.

| Attribute | Type | Description |
|---|---|---|
| `is_any` | `bool` | `True` for platform-agnostic packages (`Platform::Any`), `False` for a specific OS/arch target. |
| `os` | [`os`](#os-value) \| `None` | The target OS, or `None` when `is_any` is `True`. |
| `arch` | [`arch`](#arch-value) \| `None` | The target CPU architecture, or `None` when `is_any` is `True`. |

### `os` (operating system) {#os-value}

Typed enum value whose instances live in the [`ocx.os`](#ocx-os) namespace.

- Type: `type(ocx.os.Linux) == "os"`
- Equality: equal only to another `os` value of the same variant, never to a string or an `arch` value
- Cross-type comparison: returns `False` and never raises, so `expect.ne(ocx.os.Linux, "linux")` and `expect.ne(ocx.os.Linux, ocx.arch.Amd64)` both pass
- `str(value)`: the lowercase OCI string, such as `"linux"`

### `arch` (CPU architecture) {#arch-value}

Typed enum value whose instances live in the [`ocx.arch`](#ocx-arch) namespace.

- Type: `type(ocx.arch.Amd64) == "arch"`
- Equality and `str()`: same semantics as [`os`](#os-value)

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

Closed sets of typed enum constants, one per platform variant. A structural parity test catches a Rust-side variant missing from a namespace.

### `ocx.os` {#ocx-os}

| Constant | `str(...)` |
|---|---|
| `ocx.os.Linux` | `"linux"` |
| `ocx.os.Darwin` | `"darwin"` |
| `ocx.os.Windows` | `"windows"` |
| `ocx.os.Wasip1` | `"wasip1"` |
| `ocx.os.Wasip2` | `"wasip2"` |

### `ocx.arch` {#ocx-arch}

| Constant | `str(...)` |
|---|---|
| `ocx.arch.Amd64` | `"amd64"` |
| `ocx.arch.Arm64` | `"arm64"` |
| `ocx.arch.Wasm` | `"wasm"` |

## `expect.*` assertions {#expect-fns}

Each assertion exits the script with the documented exit code on failure. The failure reads back in the `--format json` report under `kind`, a stable wire token: `ok`, `eq`, `ne`, `true`, `false`, `contains`, `matches`, `fail`.

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
| `fail(msg)` | The builtin Starlark form: same exit code, `kind` `fail`. |

## Exit codes {#exit-codes}

| Code | Meaning |
|---|---|
| `0` | All expectations passed. |
| `1` | An expectation failed, `expect.fail` / `fail()` was called, or a host API returned a failure. |
| `64` | Usage error — bad invocation. |
| `65` | Script syntax / arity / type error, or invalid regex in `expect.matches`. |
| `74` | I/O error — stdin read failure on `--script -`, scratch I/O failure. |

## See also {#see-also}

- [Authoring → Testing locally][authoring-testing]
- [`ocx package test` reference][cmd-package-test]

<!-- external -->
[starlark-lang]: https://github.com/bazelbuild/starlark
[rust-regex]: https://docs.rs/regex/latest/regex/#syntax

<!-- commands -->
[cmd-package-test]: ./command-line/package-publishing.md#package-test

<!-- authoring -->
[authoring-testing]: ../publishing/script-tests.md#scripted-tests
