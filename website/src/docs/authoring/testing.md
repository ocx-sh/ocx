---
outline: deep
---
# Testing locally

## The problem with push-debug-push {#motivation}

The fastest way to find out whether a package works is to push it and install it. That loop has a cost: every push bakes a new digest into the registry, forces a re-download on every consumer, and leaves a trail of half-finished tags in the cascade. A typo in `metadata.json` can mean three push-install cycles before the env surface looks right.

`ocx package test` cuts that loop. It runs the same install pipeline that consumers see — dep resolution, layer extraction, env composition — but writes the result to a temp directory instead of the registry. No network round-trip. No new digest. The temp directory disappears when the command exits.

:::info Analogy
[`npm pack`][npm-pack] + `npm install ./pkg.tgz` is the closest analogue: materialize locally, run the thing, throw away the scratch directory. [`cargo publish --dry-run`][cargo-publish] does a rebuild-from-scratch instead of reusing the archive — `ocx package test` uses the archive you already have.
:::

## Basic usage {#basic}

The argument shape mirrors [`ocx package push`][cmd-package-push]: identifier as `-i/--identifier`, then layers, then `--platform`.

```sh
ocx package test -p linux/amd64 -i mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool --version
```

The `--` separator marks the end of package arguments and the start of the command to run. Everything after `--` is passed verbatim to the child process. If you need to run scripted assertions instead of a single command, use `--script` instead of `--` — the two forms are mutually exclusive.

OCX will:

1. Read `metadata.json` (inferred from the layer filename or supplied via `-m`).
2. Auto-install any declared dependencies into the regular package store.
3. Assemble the package in a temp directory under `$OCX_HOME/temp/test/`.
4. Compose the env exactly as [`ocx exec`][cmd-exec] would.
5. Exec the trailing command in that env.
6. Delete the temp directory on exit — whether the command succeeds or fails.

The child's exit code is forwarded unchanged. A failing test command (`exit 7`) gives you exit code 7.

## Identifier constraints {#identifier}

The identifier must be in tag form — `repo:tag` or `registry/repo:tag`. An explicit `@digest` suffix is rejected with a usage error (exit 64), because the digest is computed locally from the layers you supply and would conflict with any pre-committed value.

```sh
# good
ocx package test -p linux/amd64 -i mytool:1.0.0 mytool.tar.xz -- true

# bad — digest rejected
ocx package test -p linux/amd64 -i mytool:1.0.0@sha256:abc… mytool.tar.xz -- true
```

## Keeping the build dir for inspection {#keep}

When a command fails you often want to inspect the materialized layout — check which files landed where, whether entrypoints were generated, whether `resolve.json` is correct. Pass `--keep` to preserve the temp directory. OCX prints its path to stderr just before executing the command:

```sh
ocx package test -p linux/amd64 --keep -i mytool:1.0.0 mytool.tar.xz -- mytool --version
# stderr: kept at /home/user/.ocx/temp/test/test-a1b2c3d4
```

The directory persists whether the command succeeds or fails. Without `--keep`, the temp directory is deleted in both cases.

## Writing to a specific directory {#output}

`--output DIR` materializes the package to a directory you control instead of an auto-managed temp dir. The directory must not exist or must be empty — OCX creates it.

```sh
ocx package test -p linux/amd64 --output ./build -i mytool:1.0.0 mytool.tar.xz -- mytool --version
```

The directory is never deleted by OCX. `--output` implies keep — omitting `--keep` is fine, combining them is an error.

:::warning Same filesystem required
`--output DIR` must reside on the same filesystem as `$OCX_HOME/layers/`. OCX assembles packages via hardlinks from the layer store; copying across filesystem boundaries is not supported. Passing a path on a different filesystem (for example `/tmp/…` when `$OCX_HOME` is on a separate partition) exits with code 74 (`IoError`) and a clear message.
:::

:::warning Windows: `--output` must be under `$OCX_HOME`
On Windows, `--output` must point to a directory under `$OCX_HOME/`. Placing the output on a different volume — for example a separate drive letter — is not currently supported. Cross-volume hardlink support is planned for a future release.
:::

## Testing the private env surface {#self}

By default, `ocx package test` composes the interface surface — the env vars marked `public` or `interface` that consumers see. To compose the private surface (what the package sees when its own launchers run), pass `--self`:

```sh
ocx package test -p linux/amd64 --self -i mytool:1.0.0 mytool.tar.xz \
  -- sh -c 'echo $MY_PRIVATE_VAR'
```

The `--self` flag mirrors the same flag on [`ocx exec`][cmd-exec] and [`ocx env`][cmd-env].

## Stripping the parent env {#clean}

By default the composed env inherits the parent shell's variables. Pass `--clean` to strip everything except the `OCX_*` config keys and the package-declared vars:

```sh
ocx package test -p linux/amd64 --clean -i mytool:1.0.0 mytool.tar.xz \
  -- sh -c 'env | sort'
```

Useful when you want to verify the package supplies all required env on its own, without relying on ambient values from the developer's shell.

## Digest layer references {#digest-layers}

Layer arguments can be file paths or digest references, exactly like [`ocx package push`][cmd-package-push]:

```sh
# base layer already in registry; only the top layer is local
ocx package test -p linux/amd64 -i mytool:1.0.1 \
  sha256:<hex>.tar.xz ./newtool.tar.xz -- mytool --version
```

Digest layers are fetched from the registry on demand when not already cached locally. In `--offline` mode, a missing digest layer exits with code 81 (`OfflineBlocked`).

## The inner pre-push loop {#workflow}

A typical authoring session looks like this:

```sh
# 1. Build the archive.
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz

# 2. Test it locally — no registry involved.
ocx package test -p linux/amd64 -m metadata.json -i mytool:1.0.0 \
  mytool-1.0.0.tar.xz -- mytool --version

# 3. Something wrong? Keep the dir and inspect.
ocx package test -p linux/amd64 --keep -m metadata.json -i mytool:1.0.0 \
  mytool-1.0.0.tar.xz -- mytool --version
ls "$HOME/.ocx/temp/test/"*/

# 4. Happy with it? Push.
ocx package push -n -p linux/amd64 -m metadata.json \
  -i mytool:1.0.0 mytool-1.0.0.tar.xz
```

<Terminal src="/casts/package-test.cast" title="Testing a package locally before pushing" collapsed />

## Scripted tests {#scripted-tests}

The `-- CMD` form works well when the package ships its own test runner. Tool packages — `cmake`, `shellcheck`, `goreleaser` — do not. They need `sh -c '...'` on the host, which breaks on Windows without WSL or Git Bash.

`--script PATH` solves this. Instead of exec'ing a command, OCX interprets a [Starlark][starlark-lang] script against the materialized package environment. The script has no access to the host shell or runtime. It runs identically on `linux/*`, `macos/*`, and `windows/*`.

:::info What is Starlark?
[Starlark][starlark-lang] is a deterministic, Python-like scripting language designed as an embedded configuration and scripting language for build tools. It is used by [Bazel][bazel] and [Buck2][buck2]. No `while` loops, deterministic iteration order, empty sandbox by default — properties that make it safe to run in a package manager context.
:::

### Invocation {#scripted-tests-invocation}

```sh
# Read script from a file
ocx package test -p linux/amd64 -i shfmt:3.8.0 shfmt.tar.xz --script smoke.star

# Read script source from stdin (the value `-` means stdin)
printf 'r = ocx.run("shfmt", "--version")\nexpect.ok(r)\n' \
  | ocx package test -p linux/amd64 -i shfmt:3.8.0 shfmt.tar.xz --script -
```

`--script` and `-- CMD` are mutually exclusive. Supplying both exits with code 64. Supplying neither exits with code 64.

When `--script -` is used, OCX reads the script source from stdin. A read failure (broken pipe, closed stream) exits with code 74.

### Host API — `ocx.*` {#scripted-tests-ocx-api}

The `ocx.*` module gives the script access to the materialized package environment.

| Function | Returns | Purpose |
|----------|---------|---------|
| `ocx.run(prog, *args, *, env=None, cwd=None, stdin=None)` | `RunResult` | Spawn a binary from the composed package env. `env` is a dict overlaid on top of the composed env for this call only. `cwd` defaults to the scratch root. `stdin` is a string written to the child's stdin. |
| `ocx.env(name)` | `str \| None` | Read one variable from the composed package env. Returns `None` if the variable is not set. |
| `ocx.platform()` | `{"os": str, "arch": str}` | Reflects the `-p` flag passed to the command — not the host platform. |
| `ocx.package_root()` | `str` | Path to the materialized package (read-only). |
| `ocx.scratch_root()` | `str` | Path to the writable scratch directory. |
| `ocx.read_file(path, *, max_bytes=1048576)` | `str` | Read a file within `{scratch_root, package_root}`. |
| `ocx.write_file(path, content)` | — | Write a file within `scratch_root` only. Parent directories must exist. |
| `ocx.exists(path)` | `bool` | Check whether a path exists within `{scratch_root, package_root}`. |
| `ocx.mkdir(path)` | — | Create a directory and its parents within `scratch_root` (idempotent). |

`ocx.run` returns a `RunResult` with fields: `exit_code: int`, `stdout: str`, `stderr: str`, `duration_ms: int`, `truncated: bool`. A non-zero exit code does not raise — the script decides whether to fail.

`ocx.env(name)` reads only the composed package env. Host credentials such as `OCX_AUTH_*` are not readable from scripts.

Path arguments use `/` as separator on all platforms. Absolute paths and `..` escapes are rejected.

### Assertion API — `expect.*` {#scripted-tests-expect-api}

| Function | Purpose |
|----------|---------|
| `expect.ok(result, msg=None)` | Assert `result.exit_code == 0`. On failure, the message automatically includes the captured stderr — no boilerplate needed. |
| `expect.eq(actual, expected, msg=None)` | Assert equality. |
| `expect.ne(actual, expected, msg=None)` | Assert inequality. |
| `expect.true(cond, msg=None)` | Assert truthiness. |
| `expect.false(cond, msg=None)` | Assert falsiness. |
| `expect.contains(haystack, needle, msg=None)` | Substring check for strings; membership check for lists. |
| `expect.matches(text, pattern, msg=None)` | Regex match using [Rust `regex` syntax][rust-regex]. An invalid pattern exits with code 65. |
| `expect.fail(msg)` | Unconditional failure. |

The builtin Starlark `fail(msg)` is also available.

A typical smoke test looks like this:

```python
r = ocx.run("shfmt", "--version")
expect.ok(r)
expect.contains(r.stdout, "v3")

# Verify the package exported the expected env var
expect.true(ocx.env("SHFMT_ROOT") != None, "SHFMT_ROOT should be set")
```

### Sandbox model {#scripted-tests-sandbox}

The script runs inside a bounded environment:

- **Writable area**: a scratch directory created as a sibling of the package root. `ocx.scratch_root()` returns its path. Files written here survive `--keep`.
- **Read area**: the package root is readable but not writable.
- **Symlink containment**: every path is validated for symlink escape after lexical normalization. A symlink inside `scratch_root` that points outside is refused on access, not just at creation.
- **Path portability**: use `/` as the separator in all paths — it works on all platforms.

:::warning Spawned binaries are not sandboxed
The sandbox applies to the `ocx.*` host API only — file reads, writes, and path resolution. Binaries launched via `ocx.run` run with normal host OS privileges, exactly as `-- CMD` does. A binary can write anywhere the OS allows. This matches the existing trailing-command form and is a documented v1 scope limit.
:::

Re-entrant `ocx` invocations are refused in v1. `ocx.run("ocx", ...)` exits with code 1 and a message explaining the limitation.

### Output format {#scripted-tests-output}

Pass `--format json` to get a structured envelope alongside the exit code:

```sh
ocx package test -p linux/amd64 -i shfmt:3.8.0 shfmt.tar.xz \
  --script smoke.star --format json
```

The envelope has three top-level keys — all stable v1 contract:

```json
{
  "status": "passed|failed|usage|script_error|io|timeout",
  "assertion": { "kind": "ok|eq|ne|true|false|contains|matches|fail|other|unknown", "message": "…" },
  "run":       { "exit_code": 0, "stdout": "…", "stderr": "…", "duration_ms": 12, "truncated": false }
}
```

`assertion` and `run` are `null` when not applicable (for example, `assertion` is `null` on a passing run). `assertion.kind` reflects which `expect.*` function triggered the failure and is the stable machine field for tooling. `assertion.message` prose is not stable. Exit code remains the primary machine signal.

### Exit codes {#scripted-tests-exit-codes}

| Code | Meaning |
|------|---------|
| 0 | All expectations passed |
| 1 | An expectation failed, `expect.fail` was called, or a host API returned a failure |
| 64 | Usage error — both `--script` and `-- CMD` supplied; neither supplied; script file not found |
| 65 | Script syntax, type, or arity error |
| 74 | I/O error — stdin read failure (`--script -`), scratch directory creation failure |

### Editor integration {#scripted-tests-ide}

`ocx lsp` provides completion and hover for the `ocx.*` and `expect.*` APIs in editors that support the [Language Server Protocol][lsp]. It is an **internal, unstable** subcommand — it does not appear in `ocx --help` and its name and wire format carry no stability promise. Point your editor's `starlark.lspPath` setting at the `ocx` binary and add `lsp` as the subcommand argument.

For basic syntax highlighting without the LSP, add the [vscode-bazel][vscode-bazel] extension to your VS Code workspace — it provides `.star` file syntax highlighting.

## Exit codes {#exit-codes}

| Code | Meaning |
|------|---------|
| Child's exit code | The command ran; forwarded unchanged |
| 64 | Usage error — bad identifier, conflicting flags, or (for `--script`) missing/extra arguments |
| 65 | Data error — malformed metadata or script syntax error |
| 74 | I/O error — `--output` on wrong filesystem, filesystem failure, or stdin read failure |
| 81 | Offline blocked — digest layer missing and `--offline` set |

## See also {#see-also}

- [`ocx package test` reference][cmd-package-test] — full flag table
- [Building and pushing][authoring-building-pushing] — push workflow
- [`ocx exec` reference][cmd-exec] — same env composition, different trigger
- [Env surface][authoring-env-surface] — visibility levels: `private`, `public`, `interface`

<!-- external -->
[npm-pack]: https://docs.npmjs.com/cli/v10/commands/npm-pack
[cargo-publish]: https://doc.rust-lang.org/cargo/commands/cargo-publish.html
[starlark-lang]: https://github.com/bazelbuild/starlark
[bazel]: https://bazel.build/
[buck2]: https://buck2.build/
[rust-regex]: https://docs.rs/regex/latest/regex/#syntax
[lsp]: https://microsoft.github.io/language-server-protocol/
[vscode-bazel]: https://marketplace.visualstudio.com/items?itemName=BazelBuild.vscode-bazel

<!-- commands -->
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-test]: ../reference/command-line.md#package-test
[cmd-exec]: ../reference/command-line.md#exec
[cmd-env]: ../reference/command-line.md#env

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-env-surface]: ./env-surface.md
