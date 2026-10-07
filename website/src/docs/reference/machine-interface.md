---
layout: doc
outline: deep
---
<!-- doc_type: reference -->

# Machine Interface

The machine interface is everything a program can observe when it calls `ocx`. That is the `--format json` documents, the error document, the exit code, the `OCX_*` variables and the command grammar.

This page states the conventions the documents and exit codes follow. Per-command fields live in the [command reference][cmd-json-documents]. Variables live in the [environment reference][env-reference]. The [published schemas](#machine-interface-schemas) describe every document.

## Roots {#machine-interface-roots}

Every document `--format json` writes has one shape at the top level.

- **A root is an object.** No command writes a bare array, string or number.
- **`schema_version` comes first.** It is an integer, the version of that root's shape.
- **A list root is `{"schema_version": …, "items": […]}`.** Nested collections inside a root use a plural noun. The key `entries` is not used.
- **Absent means unset.** An optional field with no value is left out, and `null` never stands for "unset". Publisher-supplied JSON, such as package metadata, passes through unchanged and may contain `null`.
- **Unions carry a `type` key.** An object that takes one of several shapes names its shape in `type`, and the other fields belong to that shape.
- **Keys and enum values are `snake_case`.**

Under `--format json`, stdout holds exactly one pretty-printed JSON document per invocation, including after a usage error. [`--quiet`][cmd-quiet] suppresses success reports only.

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

`contract` has three members. `errors` is the error document's `schema_version`. `reports` holds each report root's `schema_version`, keyed by root name. `commands` holds each command's grammar version, keyed by the command path with spaces, such as `package sign`. A program can read `contract` once and refuse to run a command whose version it does not support.

A version increases when a document or command breaks:

- a property, root, enum value or union variant is removed or renamed
- a property changes type
- a flag, argument, output mode or exit code is removed or renumbered
- a value keeps its name but changes meaning

Additions do not change a version. That covers a new property, root, enum value, union variant, command or flag. A program that ignores unknown keys keeps working across additions.

A contract ledger in the repository records each break for tooling. A gate refuses a break that is missing from the ledger or from the version bump. The ledger is not release notes. What changed in a release is stated in the [changelog][changelog].

## Open Values {#machine-interface-open-values}

Enum values and union variants are open: a later release of `ocx` may emit a value the program has never seen. The published schemas encode this, so a validator accepts an unknown string or an unknown `type`.

- **An unknown value is legal.** It is not an error in the document. Handle it as a value that cannot be classified.
- **An unknown `status` is not success.** Checks for success compare against the known success values and fail closed. A check written as "not `failed`" treats a future failure value as success.
- **An unknown union variant is skipped, not guessed.** Read the `type`, and ignore the object when it names a shape the program does not know.

`status` is a named enum on every report that has one, and a dry run is a `status` value rather than a separate flag in the document.

## Outcomes {#machine-interface-outcomes}

Every invocation ends in one of three outcomes. The exit code decides which one, and stdout carries the document that goes with it.

| Outcome | Exit code | Stdout |
|---|---|---|
| Report | `0` | The report root |
| Report, then fail | Non-zero | The report root. No error document follows it |
| Error | Non-zero | The [error document](#machine-interface-error-document) |

A command can finish its work, write its report and still exit non-zero when part of the work failed. For example, a batch with one missing package writes the full report and exits with the code of the failure that decided the outcome. Stdout is never two concatenated JSON values.

Read the exit code first, then parse stdout. A zero exit code with a report whose `status` is not a known success value is also a failure for the program's purposes. See [open values](#machine-interface-open-values).

## Error Document {#machine-interface-error-document}

A command that fails without writing a report prints one error document on stdout. This holds for every command. It also holds before a command runs: a refused command line (exit 64) and an invalid `OCX_*` variable or unreadable config file (exit 78) each print one.

```json
{
  "schema_version": 2,
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

Branch on `error.kind` for the family and on `error.detail` for the cause. A `detail` slug is stable once a release emits it. A command's reference entry lists the slugs that command produces. A failure with no slug, such as a plain I/O error, carries only `error.kind`, so a script must be able to branch on `kind` alone.

A `--format json` or `--json` after a bare `--` belongs to the child command and asks for nothing. Under `--quiet`, a failure still prints the error document.

## Exit Codes {#machine-interface-exit-codes}

Exit codes follow BSD [sysexits.h][sysexits-manpage] for the standard categories (64 to 78) and use 79 to 82 for ocx-specific cases. The values are stable across releases, so `case $?` is a supported way to branch. The error document's `exit_code` repeats the process exit code, and `error.kind` names its category.

The full table, with the conditions that produce each code and the recovery for it, is in the [command reference][cmd-exit-codes]. Two distinctions matter most to a program that retries:

- **`75` (TempFail) means the same command may succeed on a rerun. `69` (Unavailable) means it will not.** A wrapper can loop on 75 and stop on 69.
- **`81` (PolicyBlocked) is a deliberate local refusal, not a fault.** `--offline`, `--frozen` and a shell profile with user edits produce it.
- **`82` (Unsupported) means this registry, forge or build lacks the capability.** A rerun never helps; use another registry, forge or build.

A child process run by [`ocx exec`][cmd-exec] is the one case where the exit code is not ocx's own: the child's exit code is forwarded unchanged.

## Schemas {#machine-interface-schemas}

Each document is described by a JSON Schema published on this site, so a program can validate output or generate types from it.

| Document | Schema |
|---|---|
| Success reports, every root | [reports][schema-reports] |
| Error document | [errors][schema-errors] |

The `reports` schema lists each root as a wrapper with `schema_version` pinned to a constant. The schemas allow unknown properties, because a consumer is expected to ignore them. They encode [open values](#machine-interface-open-values) as strings that carry a documented list of known values.

The command grammar, meaning flags, arguments and output modes, is described in the [command reference][cmd-reference]. The `OCX_*` variables are described in the [environment reference][env-reference].

<!-- external -->
[sysexits-manpage]: https://man.freebsd.org/cgi/man.cgi?sysexits

<!-- schemas -->
[schema-reports]: https://ocx.sh/schemas/reports/v2.json
[schema-errors]: https://ocx.sh/schemas/errors/v2.json

<!-- commands -->
[cmd-reference]: ./command-line.md
[cmd-json-documents]: ./command-line.md#json-documents
[cmd-quiet]: ./command-line.md#arg-quiet
[cmd-exit-codes]: ./command-line.md#exit-codes
[cmd-exec]: ./command-line.md#exec

<!-- environment -->
[env-reference]: ./environment.md

<!-- internal -->
[changelog]: ../changelog.md
