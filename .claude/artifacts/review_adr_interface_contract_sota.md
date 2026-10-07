# Review: ADR interface contract, state-of-the-art gap check

**Date:** 2026-10-03
**Role:** researcher, hex-architect review panel (gap check, not a re-review)
**Reviewed:** `adr_ocx_interface_contract.md`, `system_design_ocx_interface_contract.md`
**Method:** web search plus primary-source fetches (dated as the pages state; "current" = undated living doc, read 2026-10-03). A claim I could not confirm at a primary source is marked UNVERIFIED.

Ranked by consequence. Only gaps the design does not already address.

## G1. Published schema says "closed enum" while the contract says "open enum" (medium-high)

The ADR's enum policy lives in prose and in generator output (Rust `Unknown(String)` etc.). The published `reports`/`errors` JSON Schema still carries `enum` / `oneOf`+`const` for every status, `error.kind`, `exit_code` and union tag. Any consumer that validates against `ocx.sh/schemas/*` rejects a value added by a newer `ocx`. MCP-style clients are exactly that consumer.
- Zalando RESTful API Guidelines, rule 112 (SHOULD use open-ended value lists via `examples` / `x-extensible-enum`, "strict enums force clients to update immediately"). https://opensource.zalando.com/restful-api-guidelines/ (current)
- MCP spec 2025-06-18: when a tool declares `outputSchema`, "Clients SHOULD validate structured results against this schema". https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- Cargo states the same policy in prose: new enum-like values are not an incompatible change within a format version. https://doc.rust-lang.org/cargo/commands/cargo-metadata.html (current)

Consequence: add a marker to every output enum `$def` (e.g. `x-ocx-open: true`, lint-required, read by the IR loader and the differ), and say in `machine-interface.md` that strict validation of an enum field is unsupported. Without it, IC-15 is true for generated SDKs only.

## G2. `additionalProperties: false` is not banned, and it defeats "additive is free" (medium)

The lint set (L01-L13) and differ rules (B01-B08) never mention `additionalProperties` or `unevaluatedProperties`. The current `reports.json` golden already contains one closed object (`/$defs/Prior/oneOf/1` has `additionalProperties: false`), plus 21 schema-valued `additionalProperties` (maps). A closed output object turns every added property into an incompatibility for a validating consumer.
- Confluent Schema Registry, JSON Schema compatibility: with a closed content model, "add optional field" is compatible only for Backward, and "add required field" is compatible for none of Backward, Forward or Full. https://docs.confluent.io/platform/current/schema-registry/fundamentals/schema-evolution.html (current)

Consequence: add lint L14 (no `additionalProperties: false` / `unevaluatedProperties: false` on output objects; map-valued `additionalProperties` allowed only if the lint can name the value schema) and make the differ return `unmodelled` for it. Both are a few lines in the walker.

## G3. Generator conformance is Rust-only; nothing shared across the six backends (medium-high)

V9 and V10 test the Rust SDK; Java, Swift, Go, TypeScript and Python get "later plan". Each backend will then re-derive the unknown-value, unknown-field, absent-optional and union-dispatch semantics independently, and the IR will only ever be exercised against Rust's constraints. Projects that run many generators share a language-neutral corpus:
- Smithy: protocol tests are traits in the model; "each implementation uses them via code generation of test cases or dynamically loaded at runtime". https://smithy.io/2.0/additional-specs/http-protocol-compliance-tests.html (current)
- protobuf: a conformance runner drives each language as a subprocess over a pipe, with dedicated JSON cases (`JSON_TEST`, `JSON_IGNORE_UNKNOWN_PARSING_TEST`). https://github.com/protocolbuffers/protobuf/blob/main/conformance/conformance.proto (current)
- quicktype: fixture suite compiles and runs generated output per target language. https://github.com/glideapps/quicktype (current)

Consequence: commit `contract/conformance/` as language-neutral cases (`instance.json` plus `expect.json` re-emitted canonical form), covering one valid instance per root, unknown enum value, unknown union `type`, unknown property, omitted optional, a `ByteSize` above 2^53, and a version-mismatched root. Every backend's SDK CI must run it. Generate the valid instances from real `ocx` output (the V10 integration run) so they cannot drift from the goldens. This is the cheapest guard for "6 generators correct" and it belongs in phase 2 before the second backend, not after.

## G4. `CommandSpec` carries no output link; `ocx exec` breaks "one JSON document" (medium)

Reading the sketch in § 3.6: `CommandSpec`/`ArgSpec` have no field saying what a command prints (report root `$def`, none, or child passthrough). The generator in § 3.11 emits "one fn per non-hidden command, `--format json` always" and returns a typed report, but the design never states where the command-to-root mapping comes from. `ocx exec` runs a child whose stdout is the child's, so IC-01 ("stdout carries one JSON document") cannot hold for it; shell-export commands are text by design.
- OpenCLI 1.0.0 models this per command (`responses`, `environment`, `platforms` listed in its root properties). UNVERIFIED in detail: the spec page returned only its title; the structure comes from search snippets of an aggregator page. https://openclispec.org
- Spectre.Console.Cli 0.52 (2025-10-10) emits OpenCLI 0.1-draft via `--help-dump-opencli`, so the format has a real producer. https://spectreconsole.net/blog/2025-10-10-spectre-console-0.52-released/

Consequence: add `output: { kind: "report" | "text" | "passthrough" | "none", root?: <$def name> }` to `CommandSpec`, golden-pinned, with a lint that every `report`-kind command names a registered root and every registered root is named at least once. The generator skips or marks non-`report` commands instead of guessing. This is the single field that also makes the grammar convertible to OpenCLI or MCP tool definitions later.

## G5. MCP/OpenCLI as a downstream backend (low, informational)

The ADR rejects `usage`/`clap_usage` but does not mention OpenCLI or MCP at all. MCP 2025-06-18 tools carry `inputSchema` plus optional `outputSchema` and `structuredContent`; a CLI that already has JSON Schema per command output (this design, after G4) maps almost one-to-one. https://modelcontextprotocol.io/specification/2025-06-18/server/tools (2025-06-18)
Consequence: no phase-0 work beyond G4. Record in the ADR that an `opencli.json` / MCP `tools.json` emitter is a future IR backend, so nobody builds a second grammar walker for it. Caveat: several unrelated projects share the name "OpenCLI", and none of them is verified as the Spectre.Console one beyond the blog above; treat adoption as a re-check at phase-2 entry.

## G6. SDK spawn contract is thinner than the shell-out precedent (medium)

The design specifies argv-vector spawn, a handshake and per-payload version checks. It leaves open what the SDK does about the process environment, stdin and cancellation. terraform-exec, the reference shell-out SDK, treats these as part of the contract:
- forbids callers from setting a deny-list of variables (including `TF_VAR_*` and `TF_CLI_ARGS_*` prefixes), forces `TF_IN_AUTOMATION=1`, clears `TF_INPUT` to prevent prompts; on cancel sends `os.Interrupt` with a `WaitDelay`, and can close stdio itself to avoid hangs. https://github.com/hashicorp/terraform-exec/blob/main/tfexec/cmd.go (main, read 2026-10-03)
- Pulumi's Automation API likewise "drives the Pulumi CLI under the hood, so the CLI must be available at runtime". https://www.pulumi.com/docs/iac/using-pulumi/automation-api/ (current). I found no primary source for a move away from shell-out; treat that premise as UNVERIFIED.

The design's `Ocx { env: Vec<(OsString, OsString)> }` is free-form and inherits the parent environment, so an ambient `OCX_*` variable (offline, config path, auth) in the caller silently changes what the SDK sees.
Consequence: put in the SDK contract (and the generated `Error`): a registry-derived deny-list of variables the SDK owns, stdin closed by default, a `Cancelled` error plus interrupt-then-kill with a grace period, and `Reader`/`Visibility` flags telling the generator which `Public` variables are safe to pass through. The registry already has the data; only the flag "affects output shape" is missing.

## G7. Version carrier: integer-per-root has no request-side pin and no additive signal (low-medium)

Ecosystem practice splits two ways the design does not weigh:
- Terraform: one `format_version` per document, `major.minor`; "ignore unrecognized object properties", "reject any input which reports an unsupported major version". https://developer.hashicorp.com/terraform/internals/json-format (current). The design matches the reject rule and the ignore rule; it differs in having no minor.
- Request-side pinning: `cargo metadata --format-version 1` and GitHub's `X-GitHub-Api-Version` (previous version supported 24 months) let an old consumer keep working against a new producer. https://docs.github.com/en/rest/about-the-rest-api/api-versions (current); https://github.blog/2022-11-28-to-infinity-and-beyond-enabling-the-future-of-githubs-rest-api-with-api-versioning/ (2022-11-28)

Consequence: the design chooses lockstep (SDK pinned to one contract release, handshake refuses the rest), which is coherent pre-1.0 and I do not recommend changing it. Two cheap adds: (1) state in the ADR that request-side pinning was weighed and deferred, with the post-1.0 trigger; (2) because "additive" bumps nothing, an SDK cannot tell "field absent because optional" from "ocx too old"; the handshake's semver range must therefore be the generator's minimum-`ocx` rule, recorded in `contract.rs`, not left to SDK authors.

## G8. 64-bit integers in non-Rust backends (low)

`ByteSize` is `integer`, `minimum: 0`, no `maximum`. I-JSON says a receiver cannot be expected to treat an integer beyond 2^53-1 as exact. https://datatracker.ietf.org/doc/html/rfc7493 (2015-02). TypeScript and Python-via-float decoders, and `jq` in older builds, round silently.
Consequence: give `ByteSize` a `maximum: 9007199254740991` in the `$def` (real sizes never reach it), and add the 2^53 case to the G3 corpus. A schema constraint is cheaper than a per-backend bigint decision.

## Areas with no gap worth adding

- **clippy `disallowed-methods` at workspace scale:** no new gap. The design's own measurements (DefId resolution, V0 inverse proof) cover the known failure modes. Two confirmations only: `allow-invalid` defaults to false, so a mistyped path errors instead of passing quietly (https://doc.rust-lang.org/clippy/lint_configuration.html, current); and clippy takes the first `clippy.toml` it finds walking up from `CARGO_MANIFEST_DIR` (https://doc.rust-lang.org/clippy/configuration.html, current). No per-crate `clippy.toml` exists in the tree today (checked), so the root file applies everywhere; a later crate-local file would shadow it, which the sync rule could state in one line. I could not find a primary source that documents merge-versus-replace; UNVERIFIED.
- **schemars 1.x transforms:** no gap. `transforms` apply to "generated root schemas" per docs.rs and the page does not say whether `$defs` entries are included (https://docs.rs/schemars/latest/schemars/generate/struct.SchemaSettings.html), but L03 (no `null`) over the final document is the backstop, so a transform that misses `$defs` fails loudly. One note: hand-written impls with a fixed `schema_name` should also override `schema_id`, since the docs require different schemas to have different ids (https://docs.rs/schemars/latest/schemars/trait.JsonSchema.html); the design pins only the name.
- **Env registry vs clap `env =`:** no gap; checked that `ocx_cli` uses no `#[arg(env = ..)]`, so clap never reads the environment behind the ban.
- **Per-root versus per-document versioning:** covered by G7; the per-root choice matches kubectl-style per-kind and Terraform per-document practice, with no contrary evidence found.
