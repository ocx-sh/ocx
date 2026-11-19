# Discussion: OpenTelemetry export for OCX

State: handed-off → loop · Updated: 2026-11-18
Ratified: 2026-11-18 → loop
Confidence: ratified by the owner, who ran `/hex-loop` on this file after the restate; the five research files under `## Research` are all dated 2026-11-18 and expire 2027-05-18.

## Intent

Owner's words (2026-11-18): implement telemetry in OCX. Config files must be
able to append: an environment variable that can hold several configuration
files, so OCX can be patched in an environment through a managed config to
reach the right telemetry service. Grimoire's implementation is "already kind
of good" and is the bootstrap. OCX must adhere to all the standard
environment-variable configuration and also adopt OCX-specific config.
Requested outcome of this discussion: everything found in Grimoire, and how
OCX can adopt it.

Who it is for: whoever operates an environment that runs `ocx` (a CI image,
a devcontainer, a fleet of developer machines) and wants `ocx` usage in a
collector they own. The owner's own collector is the first such
environment. Why now: Grimoire shipped the same feature and its design is
the starting point, and OCX already has a managed config tier that can
deliver settings to a fleet.

Out of scope: traces, spans and metrics; reading or propagating
`TRACEPARENT`; any default or vendor-chosen destination; a crate shared
between Grimoire and OCX; drop-in directories and `include` directives; a
first-run prompt; the existing CI telemetry (Bazel build events and JUnit
timings exported from workflows), which is not product code.

## Requirements

Provisional prose. The receiving orchestrator assigns identifiers. Each
bullet is one observable end-state check; how it is built is in Decisions.

- An accepted ADR in `.claude/artifacts/` records the exemption from rule
  OBS-12 (`.claude/rules/rust-quality/docs-and-tracing.md:133`) with its
  conditions, names every earlier decision it amends, and answers each
  entry under Open questions.
- A `[telemetry]` section loads from every config tier, the managed tier
  included, with Grimoire's keys as the starting set (`enabled`,
  `endpoint`, `headers`, `timeout`, `protocol`, `service_name`,
  `resource_attributes`). Its treatment in `retain_system_locked_sections`
  (`crates/ocx_config/src/loader.rs:1110`) is decided and unit-tested.
- `OCX_CONFIG` (`crates/ocx_env/src/vars.rs:63`) accepts a path list. A
  later file wins, a single path behaves as before, and the list still
  loads under `OCX_NO_CONFIG`. Each of the three is acceptance-tested.
- The switch variable (working name `OCX_TELEMETRY`) and the secret header
  variable are declared in `crates/ocx_env`, the honoured standard
  variables (`OTEL_*`, `DO_NOT_TRACK`) are registered there as foreign
  names, and `website/src/content/docs/reference/environment.md` lists all
  of them.
- An unconfigured `ocx` sends zero requests. The test that proves it is
  shown red by arming export, so the zero is not a server that never
  listened. A switch that is on with no endpoint also sends nothing.
- With the switch on and an endpoint set, every command sends exactly one
  OTLP/HTTP request at its end, holding one command record. `ocx exec`,
  the per-prompt commands (`ocx shell state`, `ocx direnv export`), help,
  version and usage errors send nothing, on every platform.
- The command record answers usage and failures: command name, `ocx`
  version, OS, architecture, CI or interactive, installation id, duration,
  exit code and the `error.detail` slug on failure. A golden payload test
  pins the attribute set, and a test binds the documented field table to
  the keys on the wire.
- A command that installs, pulls or removes packages adds one record per
  package to the same request: the package as a `pkg:oci` purl in plain
  text, the action, whether the store already held it, the bytes
  downloaded, and the time spent in resolve, download and extract.
- Records relate without anything new being passed to a child process:
  each carries its pid and its parent's pid under the field names
  execution records use, and in a detected CI system each carries the run
  and job id that `ocx` read from that system's own variables. An
  operator's resource attributes override the detected values.
- The precedence rules hold under test: `DO_NOT_TRACK`,
  `OTEL_SDK_DISABLED`, offline mode and the switch set to off each stop a
  configured export; an endpoint in OCX config beats
  `OTEL_EXPORTER_OTLP_ENDPOINT`; the standard variables are used when no
  OCX config names an endpoint; endpoint and headers always come from the
  same source; a managed payload with `[telemetry]` arms export.
- A refused, failing, slow or silent collector changes neither the exit
  code nor stdout, and adds at most the timeout (Grimoire's default is
  500 ms) to the command.
- A collector package is pinned in `[group.test.tools]` of `ocx.toml`, and
  one standing acceptance test sends a record to that real collector and
  reads back what it received.
- A `[telemetry]` table in the project file `ocx.toml` has no effect on
  export, under test.
- The user documentation tells one story: what is sent, how to turn it on
  and off, the managed-config recipe, the CI run id, and tool launches
  through execution records, including how a collector picks the record
  files up and which collectors can. It warns that anyone who can pull a
  managed config can read a token in it. The configuration reference gains
  the new keys.
- The export code is its own crate (working name `ocx_telemetry`) listed in
  `scripts/crate_map.toml`, with the `opentelemetry*` crates pinned to one
  exact version. `task verify:scoped --force` and `task satellite:verify`
  pass.

## Decisions

- Export needs an explicit switch (owner, 2026-11-18): an OCX variable or
  `[telemetry] enabled = true`, as in Grimoire. A standard `OTEL_*` endpoint
  alone never starts export; the standard variables only supply the
  destination.
- First version sends one usage record per command (owner, 2026-11-18):
  Grimoire parity, the OTel logs signal, one export at the end of the
  command. No trace id, no spans, no metrics. Later additions are additive.
- `OCX_CONFIG` becomes a path list (owner, 2026-11-18): PATH-style separator
  through `std::env::split_paths`, later file wins, a single path stays
  valid. Same shape as Grimoire's `GRIM_CONFIG` and OCX's
  `OCX_CONSENT_PATHS`. The one break is a path containing the separator.
  Why a list and not the managed tier alone: an environment can add its file
  without taking the single slot a job or user already set, and explicit
  files still load under `OCX_NO_CONFIG` while the managed tier does not
  (`crates/ocx_env/src/vars.rs:64-73`).
- Grimoire's telemetry code is ported into a new OCX crate, not extracted
  into a crate both tools use (owner, 2026-11-18).
- The managed config may carry the whole `[telemetry]` section, token
  included (owner, 2026-11-18). The owner's reason: managed config is
  config, and the managed tier's trust model makes the operator's registry
  the trust root; its payload already sets CA roots and mirrors. No
  telemetry carve-out is added to the managed fold. Two council seats had
  proposed stripping `headers` from the payload; the owner rejected that as
  inconsistent with the trust model, and the orchestrator agreed: every
  machine that syncs the config needs the token to export anyway.
- The collector token may live in a config file at any tier, and in the
  secret header variable (owner, 2026-11-18).
- A destination named by OCX config wins over the standard
  `OTEL_EXPORTER_OTLP_*` variables (owner, 2026-11-18). The standard
  variables supply the destination only when no OCX config names an
  endpoint. The destination is one unit: endpoint and headers come from the
  same source and are never mixed.
- OCX-specific environment variables win over config (owner, 2026-11-18).
  The named case is disabling: the switch variable set to an off value
  stops export whatever any config file says. Which further OCX variables
  override config is left to the ADR.
- Acceptance tests run against two targets (owner, 2026-11-18): Grimoire's
  capture server, ported, for every behaviour test, and one standing test
  that sends to a real OpenTelemetry Collector binary run locally and reads
  back what it received. Reason: the `opentelemetry*` crates break on every
  minor version, so a one-time manual interop run goes stale at the first
  pin bump. No live check against `otel.ocx.sh` in the gate.
- The exported records answer four questions from the first version
  (owner, 2026-11-18): usage (command, `ocx` version, OS, architecture, CI
  or interactive, installation id); failures (exit code, the `error.detail`
  slug from `crates/ocx_exit`, duration); packages (one record per package
  a command installed, pulled or removed, with identifier and action); and
  performance (store hit or download, bytes downloaded, time split into
  resolve, download and extract). They are answered by queries over log
  records in the operator's backend, not by the OTel metrics signal.
  Performance is the one part with no Grimoire code to port: it needs new
  instrumentation in `crates/ocx_package_manager`.
- `ocx exec` and the per-prompt commands (`ocx shell state`,
  `ocx direnv export`) send nothing (owner, 2026-11-18). Tool launches stay
  in execution records. The user documentation tells both as one story:
  the new export for commands, execution records for launches, and how a
  collector picks the record files up. It names collectors that can do
  that; `.claude/artifacts/adr_exec_resolution_record.md:614` already lists
  seven line-oriented shippers (OTel Collector `filelog`, Vector, Fluent
  Bit, Fluentd, Filebeat, Alloy, Splunk UF).
- Package identifiers are sent in plain text as `pkg:oci` purls (owner,
  2026-11-18), the form execution records use, so both data sets join on
  one key. No opt-in key and no hashing. This replaces the orchestrator's
  earlier suggestion to redact by default.
- The real collector is an OCX package at `ocx.sh`, pinned in
  `[group.test.tools]` of `ocx.toml` as `zot` is, and published before the
  loop starts (owner, 2026-11-18). The publish is the owner's step, not the
  loop's. Not checked: which collector distribution ships as one binary
  that can write received logs to a file.
- Records relate through a CI run id and the process tree (owner,
  2026-11-18). `ocx` reads the run and job id from a detected CI system's
  own variables, and each record carries pid and parent pid as execution
  records do. Nothing new is written into a child's environment, and
  `TRACEPARENT` stays out of scope.
- The outcome of this discussion is a goal loop, not an ADR hand-off
  (owner, 2026-11-18). The ADR is the first requirement inside the loop.
- Working position, Grimoire parity and unanimous in the council, not
  separately asked: `DO_NOT_TRACK`, `OTEL_SDK_DISABLED` and offline mode
  stop export and beat every layer.
- Working position, inherited from Grimoire and not re-asked: the maintained
  `opentelemetry-otlp` crate over a hand-written encoder. Grimoire's
  `.agents/adr/adr_otel_usage_telemetry.md` records that as the owner's
  preference and grants an exemption from the vendored rule OBS-12. OCX
  carries the same rule at `.claude/rules/rust-quality/docs-and-tracing.md:133`
  ("No `opentelemetry*` dependency in a short-lived CLI"), so the OCX ADR
  has to record the same exemption with its conditions.

## Threads

- Closed: council landed 2026-11-18. Unanimous: the opt-outs beat every
  layer including a system lock, and a token never lives in the managed
  artifact. Split two to one on three points, which the owner settled on
  2026-11-18 (see Decisions). On the token the owner overruled both the
  council's unanimous point and the orchestrator's recommendation.
- Closed: what the first version emits, the list variable, and port versus
  shared crate. See Decisions.
- Closed 2026-11-18, see Decisions and Requirements: the required end
  state (owner, 2026-11-18). The owner declined a
  hand-off to an ADR and wants a goal loop instead, with the end state
  discussed first: whether there is an OTel acceptance test, whether it runs
  against a real collector, and which questions the exported data has to
  answer. A discussion handed to `/hex-loop` yields one Done criterion per
  bullet in `## Requirements`, so that section is rewritten as observable
  end-state checks once this thread closes. Facts read for it:
  Grimoire tests against an in-process capture server
  (`test/src/otlp_fixture.py`, decoding with the `opentelemetry-proto`
  Python package) and ran a real `otel/opentelemetry-collector-contrib`
  once by hand, not as a standing test. On Unix `ocx exec` replaces the
  process (`crates/ocx_package_manager/src/launch/child_process.rs:83`), so
  it has no end-of-command point to export from, and `ocx shell state` and
  `ocx direnv export` run on every prompt. OCX's test group pins `zot` and
  no collector (`ocx.toml`, `[group.test.tools]`).
  The owner recalled an existing OTel-compatible export. That is the
  execution record (`.claude/artifacts/adr_exec_resolution_record.md`,
  `[records]` in `crates/ocx_config/src/lib.rs:155`,
  `website/src/content/docs/reference/execution-records.md`): one
  single-line JSON file per tool launch, written to a directory, shaped so
  a collector's `filelog` receiver can ship it. It borrows OTel names for a
  few fields (`os.type`, the `host.arch` vocabulary) and identifies packages
  as `pkg:oci` purls. It is a file sink, not OTLP, and it covers tool
  launches only. So launches already have a sink with no network cost, and
  the new export covers what records do not: every other command.
- Open: accepted OCX decisions a telemetry feature has to amend or respect.
  `.claude/artifacts/adr_exec_env_model.md:76` ("ocx never writes a public
  `OCX_*` into a child, except `OCX_BINARY_PIN`"),
  `.claude/artifacts/adr_exec_resolution_record.md:210` ("No new environment
  propagation", and W3C `traceparent` propagation rejected),
  `.claude/rules/subsystem-cli.md:382` (background work may not fail a
  command or write to stdout), and the hermetic meaning of `OCX_NO_CONFIG`
  (`crates/ocx_env/src/vars.rs:64`), under which a telemetry destination
  needs an explicit survival rule or is dropped.
- Open: a repository-level file must never set the destination. The
  competitive lane found that class of defect in three tools (a repo
  settings file redirecting an API endpoint, a repo `.npmrc` leaking CI
  tokens, a local config granting itself trust). The project tier
  (`ocx.toml`) is not part of the OCX config chain today and has to stay out
  of `[telemetry]`.
- Open: the destination is one unit. Claude Code had a defect where
  per-signal OTLP variables bypassed the operator's collector; Grimoire
  avoids it by reading no `OTEL_EXPORTER_OTLP_*` once a file sets `endpoint`.
- Open: "adhere to all standard variables" is not free. The Rust SDK 0.33
  does not read the `OTEL_EXPORTER_OTLP_CERTIFICATE` and client-key
  variables, and the spec says a set `OTEL_CONFIG_FILE` makes every other
  `OTEL_*` variable ignored. OCX either implements the missing ones or
  documents a named subset, as Grimoire does.
- Open: the earlier OCX research rejected phone-home telemetry on
  private-first grounds; operator-configured export has to stay "no endpoint,
  no export" to be consistent with it.

## Research

- `.agents/research/research_telemetry_prior_art.md` — landed 2026-11-18.
  OTel env-var spec status, Rust SDK state (0.33.x), short-lived-process
  export, comparable CLIs, env-carrier propagation, privacy.
- `.agents/research/research_telemetry_codebase_recon.md` — landed 2026-11-18.
  Read from code, nothing run. Grimoire: export off by default; armed only by
  `GRIM_TELEMETRY` or `[telemetry] enabled = true`, never by `OTEL_*` alone;
  `DO_NOT_TRACK`, `OTEL_SDK_DISABLED` and offline always win; `GRIM_CONFIG` is
  a path list (`std::env::split_paths`, absolute regular files, later scalar
  replaces, `headers` and `resource_attributes` merge per entry) that reads
  only `[telemetry]`; logs signal only, one OTLP/HTTP protobuf POST per
  invocation, `opentelemetry*` pinned `=0.33.1`, 500 ms timeout, a dead
  collector changes neither exit code nor stdout. OCX: tier chain is compiled
  defaults, system, user, `$OCX_HOME`, managed snapshot, `OCX_CONFIG`,
  `--config`, env, flags; `OCX_CONFIG` and `--config` each name one file;
  only `[[trust.policy]]` appends; the managed tier is an OCI artifact holding
  `config.toml`, read from a local snapshot; `ocx_env` accepts non-`OCX_`
  names as `Foreign` (`HTTP_PROXY`, `RUST_LOG`); no OTel crates in
  `Cargo.lock`, reqwest 0.13.5 and prost 0.14.4 are present.
- `.agents/research/research_telemetry_repo_archaeology.md` — landed
  2026-11-18. The managed tier's ADR is
  `.claude/artifacts/adr_managed_config_tier.md` (trust root is the
  operator's registry; the payload may override everything outside
  `[managed]` except what a system lock holds). `include` and drop-in
  directories were rejected on 2026-04-12 as human-ergonomics features,
  before the managed tier existed; a path-list variable was never proposed;
  `OCX_CONSENT_PATHS` is the existing PATH-separated precedent. The earlier
  "no telemetry" position was research on one removed flag, never an ADR.
- `.agents/research/research_telemetry_admin_enforced_config.md` — landed
  2026-11-18. Twelve tools compared across eight injection mechanisms.
  Path-list precedence differs by tool (kubectl first wins, Bazel later
  wins), so the order has to be documented. Only Claude Code makes an
  operator value non-overridable and pins a telemetry destination. No
  sampled tool has a signed or digest-pinned config artifact, which OCX's
  managed tier already is.
- `.agents/research/research_telemetry_council_precedence.md` — landed
  2026-11-18. Three seats on layer precedence; agreement, the split table,
  two loader facts verified afterwards. No seat read the loader directly.

## Related

- Grimoire repository (outside this repo, `/home/mherwig/.herdr/worktrees/grimoire/duo`,
  main checkout `/home/mherwig/dev/grimoire-rs/grimoire`):
  `.agents/adr/adr_otel_usage_telemetry.md`, `.agents/adr/adr_grim_config_layers.md`,
  `.agents/discussions/otel-usage-telemetry.md`, `.agents/plans/plan_otel_usage_telemetry.md`,
  `.agents/plans/plan_grim_config_telemetry.md`, `src/telemetry/`, `src/config/env_layers.rs`,
  `test/src/otlp_fixture.py`.
- `.claude/artifacts/research_deprecation_telemetry_patterns.md` — earlier OCX
  research; rejected phone-home.
- `crates/ocx_config/src/loader.rs`, `crates/ocx_config/src/managed.rs`,
  `crates/ocx_config/src/managed_config.rs`, `crates/ocx_env/src/vars.rs`
  (`OCX_CONFIG`, `OCX_MANAGED_CONFIG`).
- `.claude/artifacts/adr_managed_config_tier.md` — the managed tier and its
  trust model.
- `.claude/rules/subsystem-interface-contract.md` and
  `.claude/artifacts/adr_ocx_interface_contract.md` — obligations a new
  variable or config key triggers.
- The five research artifacts under `.agents/research/` are gitignored.
  `.agents/memory/hex.md` › Pointers says to copy any of them into
  `.claude/artifacts/` before an ADR cites it.

## Open questions

- [NEEDS CLARIFICATION: Is `[telemetry]` lockable in the system file, and
  does a locked section survive `OCX_NO_CONFIG`?]
  Recommended: lockable and surviving, as `[records]` is — it is the
  existing operator-owned sink (`crates/ocx_config/src/loader.rs:1130`). The
  opt-outs still win over a lock. The council split three ways on this.
- [NEEDS CLARIFICATION: Which OCX variables beyond the switch and the header
  variable override config? Is there an endpoint variable?]
  Recommended: none in the first version — the config list and the standard
  variables already cover an environment that has no file.
- [NEEDS CLARIFICATION: Does the switch variable set to an on value beat a
  file that says `enabled = false`?]
  Recommended: yes, environment beats files in both directions, below a
  system lock — the rule every other OCX setting follows.
- [NEEDS CLARIFICATION: Which standard `OTEL_*` variables does OCX honour?]
  Recommended: Grimoire's named subset (endpoint, headers, timeout,
  protocol, each with its logs variant, `OTEL_SERVICE_NAME`,
  `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SDK_DISABLED`), documented by name. The
  prior-art lane found, from two source files only, that the Rust exporter
  does not read the certificate and client-key variables, and did not
  verify that the SDK honours `OTEL_SDK_DISABLED`; OCX implements the
  opt-out itself, as Grimoire does, and takes TLS roots from its existing
  extra-CA settings.
- [NEEDS CLARIFICATION: What are the exact list semantics of `OCX_CONFIG`?]
  Recommended: `std::env::split_paths`; every entry must exist, as the
  single path must today; later entry wins; `--config` stays above the
  whole list; an empty value stays unset.
- [NEEDS CLARIFICATION: Does the record carry `host.name`?]
  Recommended: yes, best-effort, as execution records do — pid and parent
  pid only join commands to tool launches when both sides name the
  machine. Grimoire leaves it out on purpose.
- [NEEDS CLARIFICATION: Which CI systems does `ocx` detect, and under
  which attribute names?]
  Recommended: GitHub Actions and GitLab CI in the first version, using
  the OTel CI/CD attribute names where they exist. Not checked: those
  names and their stability status.
- [NEEDS CLARIFICATION: Which collector distribution is packaged?]
  Recommended: the smallest one that ships as a single binary and can
  write received logs to a file the test reads. Not checked which that is.
- [NEEDS CLARIFICATION: How do the phase times and byte counts reach the
  record?]
  Recommended: the package manager returns them in its existing result
  types and the CLI hands them to the telemetry crate, so
  `crates/ocx_package_manager` gains no dependency on telemetry.
- [NEEDS CLARIFICATION: What does a nested `ocx` invocation do?]
  Recommended: each invocation exports its own record and nothing new is
  written into a child's environment —
  `.claude/artifacts/adr_exec_env_model.md:76` and
  `.claude/artifacts/adr_exec_resolution_record.md:210` both forbid new
  propagation.
- [NEEDS CLARIFICATION: Are a new public variable and a new config section
  gated by the interface-contract compat check?]
  Recommended: read `.claude/artifacts/adr_ocx_interface_contract.md` first —
  the recon could not settle it; `AGENTS.md` names only report roots, the
  error document and `cli.json`.
- [NEEDS CLARIFICATION: Does an operator need a command that prints the
  effective telemetry state and the layer each value came from?]
  Recommended: reuse whatever config introspection exists; a new command is
  not part of the first version.
- [NEEDS CLARIFICATION: Should the docs warn about a token in a managed
  config whose registry repository is broadly readable?]
  Recommended: one sentence in the user docs — anyone who can pull the
  artifact can read the token.

## Verification

Every bullet under Requirements names its own check. The shared pieces:

- A capture server ported from Grimoire's `test/src/otlp_fixture.py`
  (modes ok, status, hang, delay, redirect; decodes the request with the
  `opentelemetry-proto` Python package) backs every behaviour test in
  `test/tests/`.
- One test runs against the real collector binary from the test tool
  group. It is a required test, never skipped when the binary is missing.
- Unit tests in `crates/ocx_config` cover the merge of the new section and
  its decision in `retain_system_locked_sections`; unit tests in the new
  crate cover the switch and opt-out truth table.
- `task satellite:verify`: `ocx_config` and `ocx_env` are ecosystem crates
  and `ocx-mirror` links them.
- `task verify:scoped --force` per step; CI is the full gate.
- Owner's step before the loop starts: publish the collector package to
  `ocx.sh`. Without it the real-collector requirement cannot go green.
