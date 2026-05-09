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

The `--` separator marks the end of package arguments and the start of the command to run. Everything after `--` is passed verbatim to the child process.

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

## Exit codes {#exit-codes}

| Code | Meaning |
|------|---------|
| Child's exit code | The command ran; forwarded unchanged |
| 64 | Usage error — bad identifier, conflicting flags |
| 65 | Data error — malformed metadata |
| 74 | I/O error — `--output` on wrong filesystem, filesystem failure |
| 81 | Offline blocked — digest layer missing and `--offline` set |

## See also {#see-also}

- [`ocx package test` reference][cmd-package-test] — full flag table
- [Building and pushing][authoring-building-pushing] — push workflow
- [`ocx exec` reference][cmd-exec] — same env composition, different trigger
- [Env surface][authoring-env-surface] — visibility levels: `private`, `public`, `interface`

<!-- external -->
[npm-pack]: https://docs.npmjs.com/cli/v10/commands/npm-pack
[cargo-publish]: https://doc.rust-lang.org/cargo/commands/cargo-publish.html

<!-- commands -->
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-test]: ../reference/command-line.md#package-test
[cmd-exec]: ../reference/command-line.md#exec
[cmd-env]: ../reference/command-line.md#env

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-env-surface]: ./env-surface.md
