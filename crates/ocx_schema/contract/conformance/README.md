# Conformance corpus

Language-neutral cases every SDK backend decodes (SD § 3.14, contract C-030). A case is a directory `<case>/` with two files:

- `instance.json`: one document as `ocx --format json` wrote it to stdout. Captured cases are real output of the post-phase-1 binary; synthetic cases are a captured instance with one deliberate mutation.
- `expect.json`: what a conforming decoder must produce for that instance.

The files are static. A run decodes the committed bytes; nothing regenerates them. Captured instances were taken against an empty `OCX_HOME`, offline, with machine paths rewritten to `/home/conformance/...` and `/work/project`. Build provenance (`commit`, `timestamp`) is left as captured.

## `expect.json`

| Key | Meaning |
|---|---|
| `root` | The root the instance decodes as: a key of `reports` in `reports.json`, or `ErrorDocument` for the error document. |
| `exit_code` | The process exit status. Harness input: feed it to the decoder together with `instance.json` as stdout. |
| `decode` | `ok`, or `refused` when the decoder must reject the document. |
| `error` | Only with `refused`: `contract_mismatch` (the root's `schema_version` is not the one the SDK was generated for). |
| `outcome` | Only with `ok`. `success`: a report, exit 0. `failed_with_report`: a report on stdout and a non-zero exit (`Outcome::Failed`). `error_document`: an `ErrorDocument`, a typed failure. |
| `unknown` | Only with `ok`. Sorted JSON pointers to the values that must land in an `Unknown` arm: an enum value or union `type` the schema does not register. A union's pointer addresses the whole object. Empty when every value is registered. |
| `fields` | Optional. JSON pointer to the exact value the decoded model must carry there. Used to pin an unregistered enum value kept verbatim and an integer of 2^53−1 kept exact. |
| `absent` | Optional. JSON pointers of optional fields the decoded model must report as unset, never as `null` or a default. |

Unknown properties are ignored by decoding, so a case with one states `decode: ok` and nothing else about them.

## Cases

Captured: `captured_*`. Synthetic mutation cases:

| Case | Derived from | Mutation |
|---|---|---|
| `synthetic_unknown_enum_value` | `captured_config_update_not_configured` | `status` set to an unregistered value |
| `synthetic_unknown_union_tag` | `ocx self update --check` offline | `skipped_reason.type` set to an unregistered tag; the exit is set to 0 |
| `synthetic_unknown_property` | `captured_about` | extra properties at the root and in `commit` |
| `synthetic_omitted_optional` | `captured_about` | optional fields removed |
| `synthetic_size_max_safe_integer` | `captured_shell_state_verbose` | a `ByteSize` set to 9007199254740991 |
| `synthetic_schema_version_mismatch` | `captured_about` | `schema_version` set to 2 |
| `synthetic_report_then_fail` | `captured_update_unchanged` | a pin change added, exit 65 |
| `synthetic_unknown_error_detail` | `captured_error_lock_stale` | `error.kind` and `error.detail` set to unregistered values |
