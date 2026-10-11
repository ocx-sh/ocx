---
title: "Machine Interface"
---
<!-- doc_type: reference -->

The machine interface is everything a program observes when it calls `ocx`: the `--format json` documents, the error document, the exit code, the `OCX_*` variables and the command grammar. Per-command fields live in the [command reference][cmd-json-documents] and variables in the [environment reference][env-reference].

## Roots {#machine-interface-roots}

Every document `--format json` writes has one shape at the top level.

| Rule | Meaning |
|---|---|
| A root is an object | No command writes a bare array, string or number |
| `schema_version` comes first | An integer, the version of that root's shape |
| A list root is `{"schema_version": …, "items": […]}` | Nested collections inside a root use a plural noun. The key `entries` is not used |
| Absent means unset | An optional field with no value is left out, and `null` never stands for "unset". Publisher-supplied JSON, such as package metadata, passes through unchanged and may contain `null` |
| Unions carry a `type` key | An object that takes one of several shapes names its shape in `type`, and the other fields belong to that shape |
| Keys and enum values are `snake_case` | No other casing appears in a root |
| Stdout holds one document | Exactly one pretty-printed JSON document per invocation, including after a usage error. [`--quiet`][cmd-quiet] suppresses success reports only |

```json
{
  "schema_version": 1,
  "items": []
}
```

## Versions {#machine-interface-versions}

Two kinds of version exist, and both are plain integers.

| Version | Where it appears | What it names |
|---|---|---|
| `schema_version` | First key of every report root and of the error document | The shape of that one document |
| `contract` | `ocx --format json version` | The current version of every document and command, in one object |

- `contract.errors`: the error document's `schema_version`.
- `contract.reports`: each report root's `schema_version`, keyed by root name.
- `contract.commands`: each command's grammar version, keyed by the command path with spaces, such as `package sign`.
- A version increases on any of these breaks:
  - a property, root, enum value or union variant is removed or renamed
  - a property changes type
  - a flag, argument, output mode or exit code is removed or renumbered
  - a value keeps its name but changes meaning
- A version does not change for additions: a new property, root, enum value, union variant, command or flag.
- A contract ledger in the repository records each break for tooling, and a gate refuses a break missing from the ledger or the version bump. What changed in a release is stated in the [changelog][changelog].

## Open Values {#machine-interface-open-values}

Enum values and union variants are open: a later release of `ocx` may emit a value the program has never seen. The published schemas encode this, so a validator accepts an unknown string or an unknown `type`.

| Case | Rule |
|---|---|
| An unknown value | Legal. It is not an error in the document |
| An unknown `status` | Not success. Success checks compare against the known success values and fail closed |
| An unknown union variant | It names itself in `type`. A program skips an object whose `type` it does not know and never guesses |
| `status` | A named enum on every report that has one. A dry run is a `status` value, not a separate flag in the document |

## Outcomes {#machine-interface-outcomes}

Every invocation ends in one of three outcomes. The exit code decides which one, and stdout carries the document that goes with it.

| Outcome | Exit code | Stdout |
|---|---|---|
| Report | `0` | The report root |
| Report, then fail | Non-zero | The report root. No error document follows it |
| Error | Non-zero | The [error document](#machine-interface-error-document) |

- A command can finish its work, write its report and still exit non-zero when part of the work failed. A batch with one missing package writes the full report and exits with the code of the failure that decided the outcome.
- Stdout is never two concatenated JSON values.

## Error Document {#machine-interface-error-document}

A command that fails without writing a report prints one error document on stdout. A refused command line (exit 64) and an invalid `OCX_*` variable or unreadable config file (exit 78) each print one before any command runs.

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 80,
  "error": {
    "kind": "auth_error",
    "detail": "oidc_token_rejected",
    "message": "Fulcio rejected OIDC token: issuer not in trust root",
    "context": { "identifier": "ocx.sh/cmake:3.28" }
  }
}
```

| Field | Type | Description |
|---|---|---|
| `schema_version` | integer | Version of the error document's shape |
| `command` | string | The command's words, such as `package sign`. Empty when the command line names no command |
| `exit_code` | integer | The process exit code, the same value the process exits with |
| `error.kind` | string | The coarse category, such as `not_found`, `auth_error` or `usage_error` |
| `error.detail` | string | A stable slug naming the specific cause. Omitted when the failure has no slug |
| `error.message` | string | Human-readable text. Not stable: never match on it |
| `error.context` | object | The packages the failure is about, as `identifier`, `source` or `target`. Always present, and empty when the failure names no package |

- A `detail` slug is stable once a release emits it, and a command's reference entry lists the slugs that command produces.
- A failure with no slug, such as a plain I/O error, carries only `error.kind`.
- A `--format json` or `--json` after a bare `--` belongs to the child command and asks for nothing.
- Under `--quiet`, a failure still prints the error document.
- A child run by [`ocx exec`][cmd-exec] is the one case where the exit code is not ocx's own: the child's code is forwarded unchanged.

See [exit codes][cmd-exit-codes].

## Schemas {#machine-interface-schemas}

Each document is described by a JSON Schema published on this site, so a program can validate output or generate types from it.

| Document | Schema |
|---|---|
| Success reports, every root | [reports][schema-reports] |
| Error document | [errors][schema-errors] |
| `config.toml`, any tier | [config][schema-config] |
| `ocx.toml`, project file | [project][schema-project] |
| `ocx.lock`, machine-generated | [project-lock][schema-project-lock] |
| `metadata.json`, package | [metadata][schema-metadata] |
| Patch descriptor (`ocx patch publish --descriptor`) | [patch][schema-patch] |
| Execution record (`[records]` sink) | [execution-record][schema-execution-record] |

- Editor integration: set `$schema` to the metadata schema URL at the top of `metadata.json`. Editors that read JSON Schema ([taplo][taplo], [yaml-language-server][yaml-ls], VS Code, Zed) use these for completion and validation.
- `ocx init` writes the project schema as a `#:schema` directive on the first line of every `ocx.toml` it generates.
- The `project-lock` schema carries a top-level `$comment` marking `ocx.lock` machine-generated.
- The `reports` schema lists each root as a wrapper with `schema_version` pinned to a constant.
- The schemas allow unknown properties and encode [open values](#machine-interface-open-values) as strings that carry a documented list of known values.
- Generating locally: `task schema:generate` writes the schemas to `website/src/public/schemas/`.
- Validation: `uvx check-jsonschema --schemafile https://ocx.sh/schemas/metadata/v1.json metadata.json` ([check-jsonschema][check-jsonschema]).

See the [command reference][cmd-reference] for flags, arguments and output modes, and the [environment reference][env-reference] for the `OCX_*` variables.

<!-- external -->
[taplo]: https://taplo.tamasfe.dev/
[yaml-ls]: https://github.com/redhat-developer/yaml-language-server
[check-jsonschema]: https://github.com/python-jsonschema/check-jsonschema

<!-- schemas -->
[schema-reports]: https://ocx.sh/schemas/reports/v1.json
[schema-errors]: https://ocx.sh/schemas/errors/v1.json
[schema-config]: https://ocx.sh/schemas/config/v1.json
[schema-project]: https://ocx.sh/schemas/project/v1.json
[schema-project-lock]: https://ocx.sh/schemas/project-lock/v3.json
[schema-metadata]: https://ocx.sh/schemas/metadata/v1.json
[schema-patch]: https://ocx.sh/schemas/patch/v1.json
[schema-execution-record]: https://ocx.sh/schemas/execution-record/v1.json

<!-- commands -->
[cmd-reference]: ./command-line/index.md
[cmd-json-documents]: ./machine-interface.md#machine-interface-roots
[cmd-quiet]: ./command-line/index.md#arg-quiet
[cmd-exit-codes]: ./exit-codes.md
[cmd-exec]: ./command-line/commands.md#exec

<!-- environment -->
[env-reference]: ./environment.md

<!-- internal -->
[changelog]: ../changelog.md
