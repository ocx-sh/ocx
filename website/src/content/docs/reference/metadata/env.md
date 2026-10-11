---
title: "Metadata: Environment Variables"
---
<!-- doc_type: reference -->

The `env` array of [`metadata.json`](./index.md#format) declares the environment variables OCX exposes when it runs commands with the package.

## Environment Variables {#env}

The `env` array feeds [`ocx package exec`][cmd-exec] and [`ocx env`][cmd-env-root]. Each entry is an object with a `key`, a `type` (`path`, `constant` or `list`) and a `value` template that OCX resolves when it composes the environment.

### Interpolation Tokens {#env-interpolation}

`${…}` is a closed namespace: every occurrence in a `value`, or in an [entry-point `args`](./entry-points.md#entry-points-wire-shape) element, must parse as one of four recognised bodies.

- A `${…}` that matches none of them is refused, not passed through.
- A value that resolves to more than 64 KiB is refused too.

| Body | Resolves to |
|---|---|
| `${installPath}` | This package's own `content/` directory. |
| `${self.installPath}` | Exact alias of `${installPath}`; see [Aliases](#env-interpolation-self). |
| `${self.env.KEY}` | The resolved value of this package's own `KEY` var, declared earlier in the same `env` array. Env values only, not legal in entry-point `args`; see [`self.env`](#env-interpolation-self-env). |
| `${deps.NAME.installPath}` | A declared dependency's `content/` directory, where `NAME` is the dependency's repository basename or its explicit `name` field. |

```json
{
  "key": "PATH",
  "type": "path",
  "value": "${self.installPath}/bin:${deps.cmake.installPath}/bin"
}
```

- A token may appear more than once, and different bodies combine freely in one `value`.
- Three bodies take an optional `:native` / `:posix` suffix. See [Render Modifiers](#env-interpolation-render).

#### Render Modifiers {#env-interpolation-render}

The three install-path bodies accept a `:native` or `:posix` suffix, for example `${installPath:posix}`, that controls how path separators render. It is independent of `type`, which governs how the value as a whole composes.

- `:native` (default when omitted): Windows paths render with `\`, POSIX paths with `/`.
- `:posix`: always `/`. It suits values read by tools that expect forward slashes regardless of host. Examples are an MSYS2/Git Bash binary or a value piped into a shell script.
- `${self.env.KEY}`: takes no modifier, and one written there is refused at exit 65, because the suffix rewrites every `\` in a resolved value OCX cannot inspect.
- Render where the value is built and the reference inherits it, as in the example.
- Interface-visible `PATH` value: a modifier takes the token out of [binaries-scan scope](./entry-points.md#executables-scope). On Linux and `any` the `ocx package create` [libc lint][cmd-package-create-libc-check] refuses it with exit 65, naming the variable. On darwin and Windows the same exclusion is silent.

```json
[
  { "key": "SDK_ROOT", "type": "constant", "value": "${installPath:posix}/sdk" },
  { "key": "SDK_TOOL", "type": "constant", "value": "${self.env.SDK_ROOT}/bin/tool" }
]
```

#### Escaping {#env-interpolation-escape}

Write `$${` to emit a literal `${`: `$${workspaceFolder}` publishes as the literal text `${workspaceFolder}`. This is the only escape, and a bare `$` not immediately followed by `${` is ordinary text.

- A value that ships another tool's `${…}` syntax, such as a devcontainer snippet, needs `$${workspaceFolder}` so ocx leaves it for that tool to resolve

#### Aliases {#env-interpolation-self}

`${self.installPath}` is an exact alias of `${installPath}`: same referent, same resolution, interchangeable.

- Preferred in new values, because it reads unambiguously next to `${self.env.KEY}` and `${deps.NAME.installPath}`.
- `${installPath}` and `${self.installPath}` are interchangeable.

#### `self.env` {#env-interpolation-self-env}

`${self.env.KEY}` resolves to the resolved value of this package's own `KEY` env var. It is not its template text and not a value folded together with a consumer's or a dependency's contribution.

- Declaration order: `KEY` must be declared strictly earlier in the same package's `env` array
- A forward reference, to a var declared later or to itself, is refused at publish time.
- Declaring the same key twice and then referencing it is refused as ambiguous.
- Legal only inside `env` values, never inside entry-point `args`.
- Declaration order is part of the grammar once `${self.env.KEY}` is used. A generator must therefore emit `env` from an ordered structure (a slice, a `LinkedHashMap`, a sorted map), not an unordered map type.

#### Unrecognised Tokens {#env-interpolation-unrecognised}

OCX claims the entire `${…}` namespace in a `value`: there is no fifth body and no pass-through for a token this `ocx` does not recognise. A refusal names what is wrong.

- A near-miss (`${slef.installPath}`) suggests the root it is closest to.
- An unknown root (`${workspaceFolder}`) explains the `$${` escape.
- A recognised root with an illegal body (`${self.env.A B}`) lists the supported bodies for that root.
- A recognised namespace with an unknown leaf (`${deps.cmake.version}`) names the field and lists the leaves that exist for it (`installPath`).
- Refusing (exit 65, naming the token): [`ocx env`][cmd-env-root], [`ocx package exec`][cmd-exec], [`ocx exec`][cmd-run], any environment composition, [`ocx package create`][cmd-package-create], [`ocx package push`][cmd-package-push].
- Refusing at run time, after install: a generated entry-point launcher whose baked `args` element holds the token.
- Succeeding on such a document: [`ocx package pull`][cmd-package-pull], [`ocx package install`][cmd-package-install], [`ocx package inspect`][cmd-package-inspect], [`ocx package description pull`][cmd-package-info], [`ocx package which`][cmd-package-which] and [`ocx package deps`][cmd-package-deps], none of which resolve `env` values. `ocx package description pull` never reads `env` at all, only the package's `__ocx.desc` tag.
- [`ocx package inspect`][cmd-package-inspect] echoes the declared template text verbatim, never a resolved value. Add `--resolve` for a multi-platform image index.

### Path Variables {#env-path}

Path variables are prepended to any existing value of the environment variable, separated by the platform path delimiter. When `required` is `true` and the resolved path does not exist, the operation fails with an error.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"path"`. |
| `required` | boolean | No | If `true`, the resolved path must exist on disk. Defaults to `false`. |
| `value` | string | Yes | Value template; see [Interpolation Tokens](#env-interpolation). |
| `visibility` | string | No | Entry visibility; see [Entry Visibility](#env-entry-visibility). Default: `"private"`. |

```json
{
  "key": "PATH",
  "type": "path",
  "required": true,
  "value": "${self.installPath}/bin"
}
```

### Constant Variables {#env-constant}

Constant variables replace any existing value of the environment variable.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"constant"`. |
| `value` | string | Yes | Value template; see [Interpolation Tokens](#env-interpolation). |
| `visibility` | string | No | Entry visibility; see [Entry Visibility](#env-entry-visibility). Default: `"private"`. |

```json
{
  "key": "JAVA_HOME",
  "type": "constant",
  "value": "${self.installPath}"
}
```

### List Variables {#env-list}

List variables are appended to any existing value of the environment variable, joined by `separator`. Any earlier occurrence of the same contribution is removed first, so re-declaring a value moves it to the back instead of duplicating it.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"list"`. |
| `separator` | string | Yes | The string this contribution joins to the variable's existing value (for example `" "` for `JDK_JAVA_OPTIONS`, `","` for `GODEBUG`). Non-empty, and must not contain `=`, a newline, or a carriage return. See [Separator Is Required](#env-list-separator). |
| `value` | string | Yes | Value template; see [Interpolation Tokens](#env-interpolation). Must not start or end with `separator`. |
| `visibility` | string | No | Entry visibility; see [Entry Visibility](#env-entry-visibility). Default: `"private"`. |

```json
{
  "key": "GODEBUG",
  "type": "list",
  "separator": ",",
  "value": "gctrace=1"
}
```

- A contribution is opaque: OCX never splits it into elements. A value carrying its own separator (`"gctrace=1,madvdontneed=1"` joined with `","`) is therefore removed and re-appended as a unit, never partially matched.

#### Separator Is Required {#env-list-separator}

Unlike `path` and `constant`, `separator` has no default in package metadata: a publisher must spell it out explicitly. A wrong separator fails silently, as Go's `GODEBUG` ignores a setting it cannot parse, so the wire format refuses to guess one.

- The `[env]` table in [`ocx.toml`][config-project-env] and the [`--env`][cmd-run] flag may omit it. An omitted separator inherits whatever another contributor to the same key declared, or defaults to a single space if none did.
- Rejected with exit 65, naming the variable: a separator that is empty, contains `=`, a newline or a carriage return, or edges the `value`.
- Enforced wherever `metadata.json` is validated: [`ocx package create`][cmd-package-create] / [`push`][cmd-package-push], and every later read.

See [Env Composition][env-composition-list] for the per-key agreement rule across a composition.

### Entry Visibility {#env-entry-visibility}

Each `env` entry carries a `visibility` field that controls which surface the entry contributes to when composing the runtime environment. It is distinct from the [dependency-edge `visibility`](./dependencies.md#dependencies-visibility), which controls how a dependency's env propagates to its dependents.

| Value | Interface surface (`--self` off) | Private surface (`--self` on) | Use case |
|---|---|---|---|
| `private` (default) | No | Yes | Internal paths the package's own launchers need; not part of the public contract. |
| `public` | Yes | Yes | Variables consumers should see: `PATH`, `JAVA_HOME`, package-specific prefix paths. |
| `interface` | Yes | No | Values forwarded to consumers but not used by the package's own runtime: `PKG_CONFIG_PATH`, library include hints. |

- `"sealed"` is rejected at parse time on `env` entries, because an entry invisible on both surfaces is dead configuration.

See [Env Composition][env-composition] for how entry values interact with dependency-edge visibility during the composition walk.

<!-- commands -->
[cmd-exec]: ../command-line/package-consuming.md#package-exec
[cmd-env-root]: ../command-line/commands.md#env-root
[cmd-run]: ../command-line/commands.md#exec
[cmd-package-create]: ../command-line/package-publishing.md#package-create
[cmd-package-create-libc-check]: ../../publishing/build-and-push.mdx#create-libc-check
[cmd-package-push]: ../command-line/package-publishing.md#package-push
[cmd-package-install]: ../command-line/package-consuming.md#package-install
[cmd-package-pull]: ../command-line/package-consuming.md#package-pull
[cmd-package-inspect]: ../command-line/package-consuming.md#package-inspect
[cmd-package-info]: ../command-line/package-publishing.md#package-description-pull
[cmd-package-which]: ../command-line/package-consuming.md#which
[cmd-package-deps]: ../command-line/package-consuming.md#deps

<!-- reference -->
[config-project-env]: ../../guides/project-toolchain.mdx#project-env
[env-composition-list]: ../env-composition.md#composition-order-list

<!-- guides -->
[env-composition]: ../../explanation/environments.mdx
