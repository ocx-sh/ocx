# Research: OCX interface coherence — inventory

## Metadata

- Date: 2026-10-03
- Expires: when the coherence pass lands (re-measure after phase 1)
- Method: read-only codebase recon at commit 34337a6a2; grep/regex passes plus parsing `crates/ocx_schema/tests/golden/reports.json` (206 `$defs`, 585 properties, 56 report roots) and 284 clap `#[arg]` fields
- Author: research subagent (sonnet), for `.agents/discussions/ocx-interface-contract.md`

## Ten largest divergences, ranked by number of commands affected

1. **Success wrapper:** 51 of 56 roots emit a bare payload. Five use the `{schema_version, command, exit_code, data}` envelope through a custom `print_json`: attestation, sbom, signature, sweep, verification (`crates/ocx_cli/src/error_envelope.rs:69`). The error path always uses the envelope (`crates/ocx_cli/src/app.rs:181`).
2. **Root JSON shape:** 42 objects, 7 bare arrays, 4 maps, 2 untagged unions (`Tags`, `PackageDescription`). Only 5 roots use `entries`; there is no shared list convention.
3. **Package identifier:** 7 field names (`identifier`, `package`, `pinned_identifier`, `reference`, `base`, `source`/`target`, `name`) and 4 Rust types plus `String` (`PackageRef`, `PinnedPackageRef`, `OciIdentifier`). Inputs take 4 forms: positional `packages` (15 commands), a single positional (7), `--identifier/-i` (5, optional in 3 and required in 2), and other flags (`--package`, `--from`, `--to`, `--why`).
4. **Digest:** 10 field names. The validated `ocx_oci::Digest` type is on 12 properties; bare `String` is on 15 (`lock.rs:23`, `package_copy.rs:106`, `push.rs:33`, `sbom.rs:96`). One `PackageRef` is used as a digest (`update.rs:56`).
5. **Platform:** 5 JSON shapes in output: a scalar string, `Vec<String>`, `Vec<CopiedPlatformRow>`, a map keyed by platform, and a structured `Platform` object. The `--platform` flag takes 4 value types across 11 commands.
6. **Enum value spelling:** of 16 multi-word string enums, 11 use snake_case and 5 kebab-case (`PruneAction.would_delete` vs `PullStatus.would-fetch`, `DescriptionOutcome.skipped-dry-run`, `CredentialKind.job-token`).
7. **Status as string or enum:** 3 stringly `status` fields (`announce.rs:39`, `push.rs:31`, `sbom.rs:49`) against about 19 typed status/outcome enums. `dry_run: bool` coexists with `CopyStatus::planned` and `PullStatus::would-fetch`.
8. **Optional vs nullable:** 114 properties are omitted when absent and 86 are required-nullable, mixed within single roots.
9. **Flag type drift:**
   - `--group`: `Option<String>` (add, remove) vs `Vec<String>` (pull, update).
   - `--tags` and `--description`: a bool in one command, a value in another.
   - Output paths have 7 spellings (`--output/-o`, `--out`, `--export-file`, `--tags-file`, `--records-dir`, `--save-readme`, `--save-logo`).
   - 12 `--X`/`--no-X` pairs map to env vars of either polarity (`--verify`/`--no-verify` → `OCX_NO_VERIFY`).
10. **Env-name registry and short flags:**
    - `crates/ocx_config/src/env.rs` declares 37 names, but `website/src/docs/reference/environment.md` documents 54. Sixteen documented names have no constant (`OCX_JOBS`, `OCX_LOG`, `OCX_QUIET`, `OCX_NO_HOOK`, …).
    - Inline literals sit next to constants (`context_options.rs:88` vs `:42`).
    - Short flags `-g` (`--global`/`--group`), `-c` (`--config`/`--cascade`) and `-l` (`--log-level`/`--compression-level`) mean different things at root and subcommand level.

## Other measurements

- **Consistent today:**
  - All JSON keys are snake_case, enforced by `json_keys_are_snake_case.rs`. Exceptions: the `execution-record` envelope, the TOML kinds, and OCI `os.version`/`os.features`.
  - The env truthy-value syntax is documented once (`environment.md:9-30`).
  - The 17 exit codes are documented 1:1 (`ocx_exit/src/exit_code.rs:12-62`).
- **Timestamps:** 3 shapes (optional string, required string, epoch `u64`), no shared type. Sizes use 3 integer formats.
- **List wrappers:** `entries`, `tags`, `packages`, `reports`, `repositories`, `roots`, `rows`, `outcomes`, `changes`, plus bare `Vec` roots.
- **`type` vs `kind`:** 4 fields are renamed to `type`; 9 are named `kind`.
- **Errors:**
  - 70 `ClassifyExitCode` impls vs 4 `ClassifyErrorKind` impls, so only 4 error families produce `error.detail`.
  - `error.context` has 2 shapes and is `{}` elsewhere.
  - `remediation` is reserved and never emitted.
  - The 15 error categories map many-to-one from exit codes.
- **Env polarity:** 10 `OCX_NO_*` switches vs positive switches (`OCX_OFFLINE`, `OCX_FROZEN`, `OCX_GLOBAL`, …). The flag-to-env stem is sometimes renamed (`--key` → `OCX_SIGNING_KEY`, `--log-level` → `OCX_LOG`).

## Sampling and gaps

- Private api/data fields are only partly grepped and were filled in from the schema; `$defs` counts are exact.
- The flag parser misses positionals that have no `#[arg]` attribute.
- The env-to-flag mapping comes from scanning doc comments (heuristic).
- `crates/ocx_cli/src/exit.rs` was not read.
- No live binary run.
