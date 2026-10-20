---
title: "Metadata: Entry Points and Executables"
---
<!-- doc_type: reference -->

The `entrypoints` and `binaries` fields of [`metadata.json`](./index.md#format) declare the commands a package puts on `PATH`.

## Entry Points {#entry-points}

The `entrypoints` object declares named launchers that [`ocx package install`][cmd-package-install] generates into an `entrypoints/` directory inside the package directory. Each launcher is a `.sh` script on Unix, or a native `<name>.exe` shim plus a one-line `<name>.shim` sidecar on Windows.

- Each launcher re-enters through [`ocx launcher exec`][cmd-launcher-exec] with the package root baked in, and resolves its dispatch command against the composed `PATH` of the package's [`env`](./env.md#env) block.
- The dispatch command is the entry point's own name unless the optional `command` field sets another.
- A package with no entrypoints produces no `entrypoints/` directory.

See [Entry points in depth][in-depth-entry-points] for how launchers are generated, reached and reported.

### Wire Shape {#entry-points-wire-shape}

`entrypoints` is a JSON object keyed by the invocable name, and each value is an object of per-entry fields.

| Position | Type | Required | Description |
|---|---|---|---|
| Key | string | Yes | The invocable name: the launcher script filename and the command users invoke. Must match `^[a-z0-9][a-z0-9_-]*$` and be at most 64 bytes. |
| Value | object | Yes | Per-entry fields. `{}` is the common case: the invocable name is the dispatched command. |
| `command` | string | No | Dispatch target resolved on the composed `PATH` when it differs from the invocable name. Same slug rule and 64-byte limit as the key. Not interpolated, so it must be a plain slug and not a path. |
| `args` | array of strings | No | Fixed leading arguments prepended before user-supplied arguments at dispatch. Each element is one argv token with no shell word-splitting. `${installPath}` and `${self.installPath}` (with the `:native` / `:posix` modifier) are interpolated per element; `${deps.*}` and `${self.env.*}` are rejected at publish time with `DisallowedToken`. Omitted and `[]` are wire-identical. |

```json
{
  "entrypoints": {
    "cmake": {},
    "ctest": {},
    "hello": { "command": "hello-bin" },
    "mytool": { "command": "python", "args": ["${installPath}/app/main.py"] }
  }
}
```

Invoking `mytool a b` runs `python <content>/app/main.py a b`, with baked arguments first and user arguments after. A `command` reaches a dependency's binary only when that dependency exposes it through `interface` or `public` env entries.

### Uniqueness {#entry-points-uniqueness}

Duplicate keys in the on-wire JSON are rejected at deserialization with a descriptive error, instead of the `serde_json` default of last wins. Name collisions across different selected packages are detected at select time.

## Executables {#executables}

The optional `binaries` array lists bare executable names the package exposes on its interface `PATH` surface. A consumer can look a name up without installing the package.

- Claim: the array is a publisher-declared claim that OCX does not verify against the package content
- Launchers: a generated entry-point launcher name never appears in it
- Same name in both: `binaries` and `entrypoints` may both name `cmake`, because `entrypoints/` is prepended ahead of `bin/` on the composed `PATH` and so the launcher, not the raw binary, resolves

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public" }
  ],
  "binaries": ["cmake", "cpack", "ctest"]
}
```

- [`ocx package create`][cmd-package-create] `--bin-scan` / `--no-bin-scan` can fill or verify the claim against the local content tree. A foreign or reused layer added later at `push` time is never part of that scan.
- On Windows the claim reflects the default executable-resolution set (`.exe`, `.com`, `.bat`, `.cmd`), the fixed allowlist the create-time scan uses. A hardened child environment with a customized `PATHEXT` may resolve fewer of these. A name backed only by a `.bat`, `.cmd` or `.com` file may then fail to resolve.

See [Declaring executables][authoring-binaries] for why the claim is unverified and how it differs from entry points.

### Interface Surface, Own Package Only {#executables-scope}

A name is eligible only if it is reachable through a `${installPath}`- or `${self.installPath}`-rooted [`path`](./env.md#env-path) variable named `PATH` with [interface visibility](./env.md#env-entry-visibility).

- Private, `libexec`-style directories are never candidates.
- Only the `PATH` variable is scanned (`Path` also counts for a Windows target). `LD_LIBRARY_PATH` and every other path variable never contribute a name.
- Shared libraries in a scanned directory are never claimed: ELF shared objects and Mach-O files that are not executables are skipped even when their exec bit is set. Scripts and unrecognized files are claimed.
- The claim is own-package only and never transitive: a package never lists a dependency's executables. What is reachable through a dependency chain is reported by [`ocx env`][cmd-env-root] and [`ocx package env`][cmd-package-env] in their `binaries` array, attributed per admitted package.
- A [render modifier](./env.md#env-interpolation-render) on the `PATH` value takes it out of scan scope, so the segment is silently excluded from the auto-filled claim. On Linux and `any` the [libc lint][cmd-package-create-libc-check] of `ocx package create` refuses such a value at publish time. On darwin and Windows nothing diagnoses it.

### `None` vs an Empty Array {#executables-none-vs-empty}

The two wire states carry different meaning, so a consumer reading `metadata.json` directly can tell "nobody declared this yet" from "the publisher asserts zero executables".

```json
{ "type": "bundle", "version": 1, "binaries": [] }
```

- Omitted: undeclared. The package predates the field, or the publisher never set it.
- `[]`: declared, with zero executables on `PATH` (for example an env-only package).

### Writing the Field {#executables-writing}

`binaries` is a plain array of bare strings, sorted and de-duplicated, with no per-entry object form. Each name is validated against a grammar looser than an [entry point key](./entry-points.md#entry-points-wire-shape), admitting names such as `python3.13`, `c++` and `MSBuild`.

- ASCII printable characters only, no whitespace anywhere.
- Forbidden characters: `/ \ < > : " | ? *`, the Windows-reserved filename characters.
- No leading `-`, and no leading or trailing `.`.
- Non-empty, at most 64 bytes.
- Reserved Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0` to `COM9`, `LPT0` to `LPT9`) are rejected case-insensitively, checked against the basename before the first `.`, so `CON.txt` is rejected too.
- Two names that differ only by case (`Cmake` and `cmake`) are rejected.
- Bare names only, never `.exe` or any other extension.

<!-- commands -->
[cmd-package-install]: ../command-line/package-consuming.md#package-install
[cmd-launcher-exec]: ../../explanation/launcher-forwarding.md#launcher-embedding
[cmd-package-create]: ../command-line/package-publishing.md#package-create
[cmd-package-create-libc-check]: ../../publishing/build-and-push.mdx#create-libc-check
[cmd-env-root]: ../command-line/commands.md#env-root
[cmd-package-env]: ../command-line/package-consuming.md#package-env

<!-- guides -->
[in-depth-entry-points]: ../../explanation/entry-points.md
[authoring-binaries]: ../../publishing/entry-points.md#binaries
