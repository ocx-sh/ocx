---
title: "Read ocx output from a script"
description: "Call ocx from a script or another program: JSON documents, the error document, exit codes and what to retry."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to call `ocx` from a script or another program and act on the result. It covers the JSON output, the error document, the exit code and the version check that keeps a wrapper safe across releases.

## Read ocx output from a script {#scripting}

A script that wraps `ocx` needs two things from it: the result in a form it can parse, and a way to tell success from failure. The global `--format json` flag (shorthand `--json`) switches every command to one JSON document on stdout.

When both `--format` flags are given, the last one wins, in both directions. So `ocx --json --format plain` prints plain text. That keeps plain output reachable when `--json` comes from a shell alias or a wrapper.

```sh
ocx --format json shell state | jq -r .toolchain_bin
```

A `--format json` or `--json` after `--` belongs to the child command and asks `ocx` for nothing. Stdout holds exactly one JSON document per invocation, so never parse it as a stream.

## Read the exit code first {#scripts-exit-code}

Read the exit code first, then the document. The exit code decides which of three outcomes happened, and stdout carries the matching document.

| Outcome | Exit code | Stdout |
|---|---|---|
| Report | `0` | The report root |
| Report, then fail | Non-zero | The report root. No error document follows it |
| Error | Non-zero | The error document |

A command can finish its work, write its report and still exit non-zero because part of the work failed. A batch with one missing package writes the full report and exits with the code of the failure that decided the outcome. A command that already wrote its result keeps it and emits no error document, so stdout is never two concatenated JSON values.

Under `--quiet`, success prints nothing and a failure still prints the error document.

[`ocx exec`][cmd-exec] is the one case where the exit code is not ocx's own. It forwards its child's code unchanged.

```sh
set +e
doc="$(ocx --format json pull)"
code=$?
set -e

if [ "$code" -ne 0 ]; then
  # An error document names the failure. A report-then-fail run has no "error" key.
  printf '%s\n' "$doc" | jq -r '.error // empty | "\(.kind): \(.detail // "no detail")"' >&2
  exit "$code"
fi
```

## Read the error document {#scripts-error-document}

A command that fails without a report writes one error document instead. It holds `schema_version`, `command`, `exit_code` and an `error` object. `command` holds the command's words, such as `package sign`. It is empty when the command line names none, as in `ocx --json --bogus`.

The error document also covers failures before the command runs:

- A command line the parser refuses exits 64 with kind `usage_error`.
- An invalid `OCX_*` variable or an unreadable config file exits 78.

Branch on `error.kind` for the family of failure and on `error.detail` for the cause. A `detail` slug is stable once a release emits it. `error.message` is not stable, so never match on it. A failure with no slug carries only `kind`, so a script must be able to branch on `kind` alone.

The [machine interface reference][reference-machine-interface] lists every field, and the [exit code reference][reference-exit-codes] lists every slug.

## Decide what to retry {#scripts-retry}

Of the [exit codes][reference-exit-codes], a retrying wrapper cares most about four:

| Exit | Name | Action |
|---|---|---|
| `75` | TempFail | A rerun may succeed |
| `69` | Unavailable | A rerun will not help until the service returns |
| `81` | PolicyBlocked | A local policy or safeguard refused the run, such as `--frozen` or a trust policy |
| `82` | Unsupported | The registry, forge or build lacks the capability |

Retry `75` with a backoff and fail fast on the rest. Exit `80` means the credential is missing or refused, so a retry changes nothing until the secret does.

## Check versions before you run {#scripts-versions}

Every document carries a `schema_version`, the version of that report's shape. A breaking change to a shape increments it. The `contract` field of [`ocx --format json version`][cmd-version] lists the current version of every report, the error document and every command grammar. A program can check them before it runs a command:

```sh
ocx --format json version | jq '.contract.reports'
```

Treat an unknown value as one from a later `ocx` release. Handle a `status` you cannot classify as not successful, and skip an object whose `type` you do not know. The published JSON Schemas encode the same rule, so a validator accepts an unknown string where the schema lists open values.

## Read the execution record {#scripts-records}

A script that audits what ran reads execution records instead of parsing command output. They are JSON files that ocx writes for each launch when a `[records]` sink is configured. [Execution records][explanation-execution-records] shows a policy check over the raw files, and the [record reference][reference-execution-records] lists the fields.

## Validate against the schemas {#scripts-schemas}

Every document has a JSON Schema published on this site, so a program can validate output or generate types from it. The [machine interface reference][reference-machine-interface] has the schema table.

<!-- commands -->
[cmd-exec]: ../reference/command-line/commands.md#exec
[cmd-version]: ../reference/command-line/commands.md#version

<!-- reference -->
[reference-machine-interface]: ../reference/machine-interface.md
[reference-exit-codes]: ../reference/exit-codes.md
[reference-execution-records]: ../reference/execution-records.md

<!-- guides -->
[explanation-execution-records]: ../explanation/execution-records.md#consumer
