- **Severity:** Block  
  **Anchor:** System design, § 3.2 “Report roots” and § 3.7 “Lint walker” ([lines 141, 283](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:141)).  
  **Argument:** `NeverNull` erases the evidence L03 needs without changing serialization. For example, removing `skip_serializing_if` from [SignatureReport.public_key_hint](/home/mherwig/dev/ocx/crates/ocx_cli/src/api/data/signature.rs:46) leaves an `Option<String>` that emits null. Schemars 1.2.2’s serialization contract makes that field required; the proposed transform removes its null alternative, and § 3.8 classifies optional→required as additive. The specified checks can therefore accept a non-nullable schema while the binary emits null. V8’s version-command smoke test cannot detect this.  
  **Fix:** Preserve serializer-derived nullability for linting; only narrow optional schemas where omission is established. Require a mutation proof that removing an omission annotation fails, plus serialized-instance validation covering absent optionals.

- **Severity:** Block  
  **Anchor:** ADR, § “The six tensions,” nullable-form decision ([line 184](/home/mherwig/dev/ocx/.claude/artifacts/adr_ocx_interface_contract.md:184)); system design, § 5 “Interface style guide,” IC-04.  
  **Argument:** The universal null prohibition conflicts with an existing opaque-payload contract. [IntegrationAttribution.payload](/home/mherwig/dev/ocx/crates/ocx_cli/src/api/data/env.rs:92) is arbitrary JSON copied into reports; [integration interpolation](/home/mherwig/dev/ocx/crates/ocx_package/src/metadata/integrations.rs:168) preserves null scalars and arbitrary object keys. These values can come from already-published packages. Rejecting or rewriting them to satisfy never-null changes their meaning; leaving them untouched falsifies the stated invariant. This is more than converting 91 optional fields.  
  **Fix:** Scope vocabulary, naming and nullability rules to OCX-owned structure. Explicitly model opaque JSON payloads, preserve their contents, and test published-package payloads containing nested nulls and non-snake-case keys.

- **Severity:** Block  
  **Anchor:** System design, § 3.6 “Grammar export” and § 3.8 “Compat gate” ([line 255](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:255)).  
  **Argument:** `ArgSpec` cannot represent argument dependencies, conflicts, value-count bounds, `require_equals`, or separator semantics. These already matter: [toolchain_env.rs](/home/mherwig/dev/ocx/crates/ocx_cli/src/command/toolchain_env.rs:54) declares optional values, equals-only syntax, conflicts and `requires = "ci"`; [toolchain_exec.rs](/home/mherwig/dev/ocx/crates/ocx_cli/src/command/toolchain_exec.rs:77) declares `--` termination and trailing operands. Changing those constraints can break previously valid argv without changing the exported model. G07 cannot check arity that was discarded. The ADR rejects `clap_usage` partly for dropping `requires`, then specifies the same omission.  
  **Fix:** Represent these clap semantics and define their directional compatibility rules. Mutation-test each against actual parsing, and distinguish adding a required argument from the blanket additive “flag added” case.

- **Severity:** Block  
  **Anchor:** System design, § 3.11 “Generator” ([line 374](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:374)).  
  **Argument:** None of the generator’s three inputs binds a command invocation to its response root. `CommandSpec` contains grammar only, while [report_roots!](/home/mherwig/dev/ocx/crates/ocx_schema/src/reports.rs:18) maps type names to schemas. Naming conventions cannot recover mode-dependent output: [package sign](/home/mherwig/dev/ocx/crates/ocx_cli/src/command/package_sign.rs:168) selects ordinary versus sweep reports. Moreover, the promised function for every non-hidden command includes `exec`, whose [implementation](/home/mherwig/dev/ocx/crates/ocx_cli/src/command/toolchain_exec.rs:265) transfers execution to the child, and shell-output modes explicitly exempted by `subsystem-cli-api.md` § “Channel rules.” Adding `--format json` does not turn these into report documents.  
  **Fix:** Export an authoritative command/mode-to-response mapping, including report alternatives, empty success, child-process output and shell streams. Gate mapping changes and generate appropriate return types or explicit exclusions.

- **Severity:** Block  
  **Anchor:** System design, § 3.12 “Rust SDK” ([line 389](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:389)); ADR, § “Root convention,” item 3.  
  **Argument:** The SDK cannot represent the report-then-fail behavior the ADR explicitly preserves. Its command result is `Result<Report, Error>`, but `Error` only carries an `ErrorDocument`, transport/decode failures or compatibility failures. [Signing](/home/mherwig/dev/ocx/crates/ocx_cli/src/command/package_sign.rs:180) deliberately emits a valid partial-success report and exits nonzero; sweeps do likewise. Returning `Ok(report)` loses failure status, while parsing an error document loses the completed work or yields a misleading decode failure. Mirror needs precisely these outcomes.  
  **Fix:** Define an outcome carrying both process status and the typed report, or a typed report-failure variant. Test partial signing and mixed-success sweeps before replacing mirror’s subprocess layer.

- **Severity:** Block  
  **Anchor:** ADR, § “Consumers” and § “Verification,” V11 ([line 383](/home/mherwig/dev/ocx/.claude/artifacts/adr_ocx_interface_contract.md:383)).  
  **Argument:** V11’s claimed red state is not exercised by its named gate. [satellite:verify](/home/mherwig/dev/ocx/taskfiles/satellite.taskfile.yml:28) overlays OCX, runs `cargo build --workspace`, and checks dependency closure; it neither runs mirror’s parsers nor invokes the changed binary. Flattening the sweep report can leave that gate green while [SweepEnvelope](/home/mherwig/dev/ocx-mirror/crates/ocx_mirror_pipeline/src/ocx_cli/sign.rs:365) silently defaults its missing `data`. The current [CI job](/home/mherwig/dev/ocx/.github/workflows/verify-deep.yml:516) is also explicitly non-blocking.  
  **Fix:** Add a blocking integration gate that exercises the six consumer sites against the candidate OCX binary. Demonstrate red on a report-only mutation and green with the same-series mirror fix; retain the build/closure gate separately.

- **Severity:** Block  
  **Anchor:** System design, § 3.4 “Env registry” ([line 224](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:224)); ADR, § “Phases” and § “Consumers.”  
  **Argument:** Phase 0 deletes ecosystem APIs already used by mirror, but consumer migration and its gate appear only in phase 1. [Mirror TLS initialization](/home/mherwig/dev/ocx-mirror/crates/ocx_mirror_http/src/lib.rs:100) calls both `ocx_util::env::var` and `ocx_config::env::keys`; [create policy](/home/mherwig/dev/ocx-mirror/crates/ocx_mirror_pipeline/src/ocx_cli/create.rs:56) calls the deleted `flag` API. Phase 0’s declared exit criteria can be satisfied while mirror no longer compiles, violating `CLAUDE.md` § “Stability tiers” before phase 1 begins.  
  **Fix:** Include mirror’s env-API migration and a green satellite build in the phase-0 change series and exit criteria, covering all deleted keys/read helpers and credential-list consumers.

- **Severity:** Block  
  **Anchor:** System design, § 3.3 “Error document and registry” and § 3.7 “Lint walker” ([lines 175, 284](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:175)).  
  **Argument:** The specified error schema cannot pass its specified lint. `ExitCode` must be a `oneOf` of scalar `{const, title, description}` arms, while L04 requires every `oneOf` arm in errors to be an object with a required `type` discriminator. Phase 1 requires zero waivers, and the generator rechecks L04. Implementing both sections literally makes the phase-2 entry condition unreachable.  
  **Fix:** Define separate supported representations for scalar enums and tagged object unions, with matching lint, differ and generator rules. Include the actual generated exit-code registry in the green fixture.

- **Severity:** Block  
  **Anchor:** System design, § 3.8 “Compat gate,” Baseline ([line 323](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:323)).  
  **Argument:** Baseline rotation conflicts with the existing release sequence. The design copies current goldens, writes the release tag and empties the ledger during `release:prepare`; T0 immediately requires those bytes to equal that tag. But [release:prepare](/home/mherwig/dev/ocx/taskfiles/release.taskfile.yml:127) runs verification before the human commits and creates the new tag. Using the new tag fails because it does not exist; using the previous tag fails whenever the contract changed. No valid rotation sequence is specified.  
  **Fix:** Verify and release against the previous immutable baseline with the ledger intact. Rotate the baseline and clear the ledger only after the new tag exists, in a separately verified update; specify bootstrap and tag availability in CI.

- **Severity:** Block  
  **Anchor:** System design, § 3.8 “Compat gate,” B01 and gate algorithm ([line 336](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:336)).  
  **Argument:** Acknowledged root removal has no reachable green state. B01 reports a removed root, which needs a ledger subject, but step 4 requires every listed subject’s current `schema_version` to equal baseline + 1. A removed root has no current version to bump. Conversely, an additive new root has no baseline version for the “every other root = baseline” rule. This accidentally makes published roots permanent despite promising recorded breaking removals and free additions.  
  **Fix:** Specify separate algorithms for surviving, added and removed roots, including how retirement is acknowledged without retaining a fictitious report. Prove green for an acknowledged deletion and an unacknowledged additive root.

- **Severity:** Block  
  **Anchor:** System design, § 3.12 “Rust SDK,” Handshake ([line 401](/home/mherwig/dev/ocx/.claude/artifacts/system_design_ocx_interface_contract.md:401)).  
  **Argument:** Accepting `contract.cli` mismatches with only a warning bypasses the version signal for input breaks. Section 3.8 explicitly bumps that version for changed defaults, argument types and flag meanings. For a mismatch still inside the SDK’s supported semver range, the SDK can execute an operation under changed input semantics; checking the report version afterward cannot undo it. Output versions may remain unchanged for a grammar-only break.  
  **Fix:** Refuse unsupported CLI contract versions before executing commands, or require an explicit adapter/capability match proving compatibility. Add a test where only the CLI contract changes and assert that no operation is spawned.

- **Severity:** Warn  
  **Anchor:** ADR, § “Consumers,” Python SDK freeze ([line 342](/home/mherwig/dev/ocx/.claude/artifacts/adr_ocx_interface_contract.md:342)).  
  **Argument:** A README upper bound does not implement the proposed compatibility window. The current Python SDK [accepts every version above its minimum](/home/mherwig/dev/ocx-sdk-python/src/ocx_sdk/_client.py:845), merely debug-logging versions newer than 0.6.2. Freezing the classes while phase 1 changes their inputs leaves accidental use with a newer binary failing during result decoding, potentially after an operation has completed.  
  **Fix:** Ship a small pre-phase-1 SDK update that enforces the supported upper bound before executing operations, and test refusal against the phase-1 version. This does not require rewriting the result classes.

- **Severity:** Warn  
  **Anchor:** ADR, § “Don't-own assessment,” Schema differ ([line 290](/home/mherwig/dev/ocx/.claude/artifacts/adr_ocx_interface_contract.md:290)).  
  **Argument:** “No single tool covers reports + errors + grammar + registry” does not justify owning the standard schema-diff component. The ADR itself concedes that oasdiff implements output-schema diffing. `quality-core.md` § [“Don't Own Non-Domain Code”](/home/mherwig/dev/ocx/.claude/rules/quality-core.md:90) requires delegating the standard portion and owning the deviation. An OpenAPI adapter built with a JSON library is not automatically a hand-written serializer. Agreement on an OCX-authored mutation corpus also cannot establish coverage of cases that corpus omitted.  
  **Fix:** Compare maintained schema diffing plus an adapter against the custom differ independently of the grammar/registry components. Document a concrete missing capability or measured integration cost before accepting ownership; keep the OCX-specific rules separate.

Verdict: Request changes; the design contains unreachable gates, undetected contract breaks and incomplete consumer migration contracts. Static review only; no builds or mutation tests were run.
