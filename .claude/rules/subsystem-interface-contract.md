---
paths:
  - crates/ocx_env/**
  - crates/ocx_cli/src/api/data/**
  - crates/ocx_cli/src/command/**
  - crates/ocx_cli/src/options/**
  - crates/ocx_cli/src/app/context_options.rs
  - crates/ocx_cli/src/error_document.rs
  - crates/ocx_cli/src/exit/**
  - crates/ocx_exit/**
  - crates/ocx_exit_derive/**
  - crates/ocx_schema/**
  - crates/ocx_sdkgen/**
  - website/src/docs/reference/environment.md
  - website/src/docs/reference/command-line.md
---

# Machine-Interface Contract: Sync Rule

Authority: [`adr_ocx_interface_contract.md`](../artifacts/adr_ocx_interface_contract.md) and [`system_design_ocx_interface_contract.md`](../artifacts/system_design_ocx_interface_contract.md) (SD § 3.13 and § 5); IC-22 is authorised by [`adr_exit_code_taxonomy.md`](../artifacts/adr_exit_code_taxonomy.md). Loads on every surface a program can observe: `--format json` reports, the error document, exit codes, the `OCX_*` environment and the clap grammar. A change to one of them is a contract change — the counterpart in the table below moves in the same commit.

## Style guide

Each rule names the lint that enforces it. A rule with no lint is held by the semantic-break checklist.

| ID | Rule | Lint |
|---|---|---|
| IC-01 | One pretty JSON document per invocation under `--format json`, usage and init errors included | conformance hook |
| IC-02 | Every report root is an object led by `schema_version`, published as a top-level wrapper | L09, L14 |
| IC-03 | A list root is `{schema_version, items}`; `entries` is retired; nested collections use a plural noun | L13 |
| IC-04 | Unset optionals are omitted; `null` is never emitted outside opaque payloads | L03, conformance hook |
| IC-05 | Unions are internally tagged with `type`, with an unknown arm; no payload field named `type` | L05 |
| IC-06 | Keys and enum values are snake_case on every surface (JSON, flag choices, env values) | L06, L07, C03 |
| IC-07 | Shared concepts use the vocabulary types and their `$def` | L11, L12 |
| IC-08 | Timestamps end `_at`, sizes end `size`, paths end `_path` / `_dir`, a home directory is `home` / `_home` | L11 |
| IC-09 | `status` is a named open enum; a dry run is a status value | L18 |
| IC-10 | Everything carries a description or help text | L08, C01 |
| IC-11 | One meaning per short letter | C02 |
| IC-12 | One value type per flag name; output destination is `--output` / `-o` | C04, C06 |
| IC-13 | Env name = `OCX_` + SCREAMING(long flag that departs from the default); positive `OCX_X`, negative `OCX_NO_X` | C05 |
| IC-14 | Internal variables are `__OCX_TESTING_*` (tests) or `__OCX_*` (plumbing), never `OCX_TEST_*` | registry test |
| IC-15 | Enum values and union variants are open in the schema; adding one is additive; success checks fail closed | L04, L05, compat classification |
| IC-16 | A value's meaning never changes under the same name; if it must, record a `semantic` break | checklist |
| IC-17 | Publisher-supplied JSON is `OpaqueJson`, passed through unchanged | L03 exemption |
| IC-18 | Never `additionalProperties: false` in an output schema | L15 |
| IC-19 | `error.context` holds vocabulary types only; URLs are `RedactedUrl` | L16 |
| IC-20 | Defaults are literals; env applies in `ContextOptions`, not in clap | C07 |
| IC-21 | Every command declares its output modes and version | C09 |
| IC-22 | Exit code and `error.kind` name a caller's next action, never a feature; feature identity is an `error.detail` slug; ≥3 distinct slugs per code; 83–87 retired | `every_exit_code_carries_at_least_three_distinct_detail_slugs`, `retired_numbers_are_never_reused` |

## Counterparts — change both in one commit

| You change | Also change | Held by |
|---|---|---|
| An env declaration in `ocx_env` | its heading in `environment.md` | `test/lint/test_env_reference_coverage.py` |
| A flag, argument or command | its entry in `command-line.md` | `test/lint/test_doc_command_reference.py` |
| A report field or root | the schema golden under `crates/ocx_schema/tests/golden/` | golden byte test |
| An error slug or exit code | the `error.detail` docs and the registry | registry tests, `ocx_exit` tests |
| A command's stdout | its `CONTRACT` entry (`command/contract.rs`) | the `CONTRACT` consistency test |
| A deliberate exception | `crates/ocx_schema/contract/exemptions.toml` (cites a decision record) or a `waivers/<area>.toml` entry | the ratchet: a stale entry reds |

## Authorities

`contract_lint` (the lint walker over the exported documents, with waivers and exemptions), the compat gate (baseline comparison, ledger and version bump), the runtime conformance hook (`test/src/conformance.py`, floored by `test/CONFORMANCE_FLOOR`) and the T0 tests above. A green run of one is not a green run of the others.

## Semantic-break checklist

- Does this change what an existing value, field or description *means* under the same name? Then it is a `semantic` ledger entry and a bumped version, not an edit.
- Does it add an enum value or union variant? Additive, but every success check on it must still fail closed.
- Does it rename or remove a flag, env name, slug or field? It is a break: the retired name goes into the registry, and an in-flight spelling into `deprecated.rs`.
- Does it add an exit code or an `error.kind`? Cite the row of `adr_exit_code_taxonomy.md` for its new next action; a feature is a slug, not a code.

## Override of the vendored exit-code rows

The vendored `rust-quality/cli-contract.md` carries a generic exit table. Where it conflicts with IC-22, **IC-22 wins**, per [`adr_exit_code_taxonomy.md`](../artifacts/adr_exit_code_taxonomy.md). The vendored file is never edited; these three rows are overridden:

- **EXIT-06** ("a shipped number and its meaning are never reassigned") holds, with one named exception: 82 changes from `DirtyRcBlock` to `Unsupported` once, before any contract baseline exists. The error document stays at v1 until the contract baseline exists, and the commit subject names the break. No other number is reassigned.
- **The row-82 text** (`DirtyRcBlock`, "Refused to rewrite a shell-RC block carrying user edits") is void. 82 is `Unsupported`; the dirty-profile refusal exits 81 `PolicyBlocked`.
- **"83–99 unassigned"** ("allocate upward from 83") is void. 83–87 are retired forever (`ocx_exit::RETIRED`) and a new number needs a next action no existing code has (IC-22).

## Environment reads

- Every read goes through `ocx_env`; `std::env::{var, var_os, vars, vars_os, set_var, remove_var}` are banned by the root `clippy.toml`, tests included. Tests set variables through `ocx_env::overrides`.
- **Never add a crate-local `clippy.toml`.** It shadows the root file and silently drops the ban.
- The ban does not see reads inside dependencies (a proxy, TLS or credential variable read by a library). Re-audit env reads inside dependencies on each bump, and declare any the product depends on.
- `env!` and `option_env!` are build inputs, not reads.
