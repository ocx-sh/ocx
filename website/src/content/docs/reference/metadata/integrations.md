---
title: "Metadata: Integrations"
---
<!-- doc_type: reference -->

The `integrations` field of [`metadata.json`](./index.md#format) carries vendor-namespaced configuration for tools OCX does not model.

## Integrations {#integrations}

`integrations` is an optional object mapping a namespace key to an arbitrary JSON payload (an object, array, string, number or `null`). OCX stores the payload and resolves its own `${...}` tokens inside string values.

- Propagation: the whole map is carried through the dependency graph without interpreting, validating or merging it

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "integrations": {
    "com.microsoft.vscode": {
      "extensions": ["rust-lang.rust-analyzer"],
      "settings": { "rust-analyzer.server.path": "${installPath}/bin/rust-analyzer" }
    },
    "com.jetbrains": { "plugins": ["com.jetbrains.rust"] }
  }
}
```

- An absent `integrations` field and an explicit `{}` are the same state.
- Any package a composition admits may declare it, including a patch companion.

See [Integrations][explanation-integrations] for the design, including why OCX never merges two packages' blocks.

### Namespace Keys {#integrations-namespace}

A namespace key follows the reverse-DNS convention (`com.microsoft.vscode`), which OCX does not enforce. An invalid key is rejected with exit 65, naming the key, wherever `metadata.json` is validated: [`ocx package create`][cmd-package-create] / [`push`][cmd-package-push], and every later read.

| Rule | Rejects |
|---|---|
| Non-empty | `""` |
| At most 128 bytes | a 129-byte key |
| No control characters (C0, DEL and C1: `U+0000`-`U+001F`, `U+007F`, `U+0080`-`U+009F`) | `"a\nb"`, a key containing a raw newline or tab |
| No invisible characters: general category `Cf` and the `Default_Ignorable_Code_Point` property, since neither contains the other | a key containing `U+202E` (right-to-left override), `U+200B` (zero-width space), `U+FEFF` (byte-order mark) or `U+3164` (Hangul filler) |
| No Unicode whitespace, not just ASCII spaces | `"com.foo bar"`, a key containing `U+00A0` (no-break space) |

- Everything else is legal: `vscode`, `VSCode`, `com.微软`, `a`, `x/y` and `123` all pass. Case is preserved, so `Foo` and `foo` are two distinct namespaces.
- The `sh.ocx.` prefix is reserved for first-party namespaces by convention only. No rule refuses a third-party key under it.

### Size Caps {#integrations-caps}

| Cap | Limit |
|---|---|
| One namespace's payload | 8 KiB |
| The whole `integrations` map | 32 KiB |

- Measured as compact (whitespace-free) JSON, the per-package cap over the whole map including every key and its punctuation.
- A namespace over its own cap is rejected first, then the per-package cap.
- Enforced everywhere `metadata.json` is validated, with exit 65.
- Both caps are raise-only.

### Interpolation {#integrations-interpolation}

A payload gets the same interpolation engine as [`env`](./env.md#env) values, resolved inside every string in the payload at any depth. It never applies to object keys, numbers, booleans or `null`, and a `${…}` outside the closed vocabulary is refused (see [Interpolation Tokens](./env.md#env-interpolation)).

- A payload that is not an object is legal too. A bare string is interpolated the same way, so `"integrations": { "com.example": "${installPath}/notes.txt" }` is valid metadata.
- `${installPath}` and `${self.installPath}` resolve to the declaring package's own content directory, never the consuming root's, even when the payload propagates through the dependency graph. See [Aliases](./env.md#env-interpolation-self).
- `${deps.NAME.installPath}` resolves to a direct dependency's content directory. A `${deps.NAME}` naming an undeclared dependency is invalid metadata, rejected at publish time.
- All three install-path bodies accept the `:native` / `:posix` modifier. See [Render Modifiers](./env.md#env-interpolation-render).
- `$${installPath}` escapes to the literal text `${installPath}`.
- Every other `${...}` token (`${workspaceFolder}`, VS Code's [`${env:VAR}`][vscode-variables], devcontainer's [`${localEnv:VAR}` / `${containerEnv:VAR}`][devcontainer-vars]) is refused with exit 65, naming the token, by [`ocx package create`][cmd-package-create] / [`push`][cmd-package-push] and again at composition ([`ocx env`][cmd-env-root], [`ocx package exec`][cmd-exec], [`ocx exec`][cmd-run]).
- Read-only paths ([`pull`][cmd-package-pull], [`install`][cmd-package-install], [`inspect`][cmd-package-inspect]) echo an unrecognised token verbatim instead of refusing it.
- Resolved for a package at `/home/u/.ocx/packages/.../content`: `toolPath` becomes `/home/u/.ocx/packages/.../content/bin/tool`, `literalToken` becomes `${installPath}`, and `editorVariable` becomes `${workspaceFolder}/config`. A bare `"${workspaceFolder}/config"` is refused at exit 65.

```json
{
  "integrations": {
    "com.example": {
      "toolPath": "${installPath}/bin/tool",
      "literalToken": "$${installPath}",
      "editorVariable": "$${workspaceFolder}/config"
    }
  }
}
```

<!-- external -->
[vscode-variables]: https://code.visualstudio.com/docs/editor/variables-reference
[devcontainer-vars]: https://containers.dev/implementors/json_reference/

<!-- commands -->
[cmd-env-root]: ../command-line/commands.md#env-root
[cmd-exec]: ../command-line/package-consuming.md#package-exec
[cmd-run]: ../command-line/commands.md#exec
[cmd-package-create]: ../command-line/package-publishing.md#package-create
[cmd-package-push]: ../command-line/package-publishing.md#package-push
[cmd-package-pull]: ../command-line/package-consuming.md#package-pull
[cmd-package-install]: ../command-line/package-consuming.md#package-install
[cmd-package-inspect]: ../command-line/package-consuming.md#package-inspect

<!-- guides -->
[explanation-integrations]: ../../explanation/integrations.md
