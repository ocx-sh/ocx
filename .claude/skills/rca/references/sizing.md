# Fix / feature size rubric

Shared by `/rca`, `/assess` and `/fix`. Size by blast radius, not hours. State the basis beside the letter.

| Size | Criteria |
|---|---|
| **S** | One function or file. No interface surface. An existing test pattern covers it. Zero siblings. |
| **M** | Up to 5 files in one crate, or the cause plus 1–3 sibling sites. New tests, no design choice. |
| **L** | Crosses crates or layers, **or** touches any interface surface below, **or** needs a choice between designs. |
| **XL** | Needs an ADR, the architectural assumption itself is wrong, a migration, or a concurrency/locking redesign. Split before building. |

**Bump +1** when the repro is flaky or Windows-only, or confidence is below medium.

## Interface surfaces (AGENTS.md › Stability tiers)

CLI command and flag grammar · exit codes and `error.detail` slugs · `--format json` reports · the error document · `cli.json` · `ocx.toml` / `ocx.lock` · package metadata · OCI manifests · the index format · `OCX_*` environment variables · `ocx-sdkgen` CLI and output layout.

Touching one is a decision, not a refactor: one-way door unless additive. A break also needs a contract-ledger entry (`subsystem-interface-contract.md`).
