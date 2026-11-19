# Goal: "OpenTelemetry export for OCX"

Source: .agents/discussions/otel-telemetry.md · Written: 2026-11-18 by /hex-loop

## Definition of done

- [x] "ADR accepted: OBS-12 exemption and amendments" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: commit 825d3cb6, `.claude/artifacts/adr_otel_telemetry.md` (Accepted; § Amends and respects; OQ-1 to OQ-13). Scope: An accepted ADR in `.claude/artifacts/` records the OBS-12 exemption with its conditions, names every earlier decision it amends, and answers each entry under the source's Open questions.
- [x] "[telemetry] loads from every config tier" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_export.py`, `test/tests/test_telemetry_precedence.py` (managed tier), `ocx_config` unit tests; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: The section loads from every tier, the managed tier included, with Grimoire's keys as the starting set; its treatment in `retain_system_locked_sections` is decided and unit-tested.
- [x] "OCX_CONFIG is a path list" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_config_list.py` (8), `test/tests/test_env_received_policy.py`, `test/tests/test_shell_state_report.py`; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: A later file wins, a single path behaves as before, and the list still loads under `OCX_NO_CONFIG`; each of the three is acceptance-tested.
- [x] "Switch and header variables declared and documented" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: the `ocx_env` registry, regenerated golden `cli.json`, `reference/environment.md`; `task website:reference:check` exit 0. Scope: The switch variable and the secret header variable are declared in `crates/ocx_env`, the honoured standard variables are registered as foreign names, and the environment reference lists all of them.
- [x] "Unconfigured ocx sends nothing" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test_unarmed_sends_nothing_with_ambient_otel` shown red with `OCX_TELEMETRY=1` in the unarmed half (1 failed), then green (1 passed); green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: Zero requests when unconfigured, proven by a test that is shown red by arming export; a switch that is on with no endpoint also sends nothing.
- [ ] "One request per armed command; exec and prompt hooks silent" — evidence: the commit, PR comment or
  artifact that satisfies it. Linux evidenced, box left open: `test/tests/test_telemetry_export.py`, `test/tests/test_telemetry_commands.py` (16); green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Windows and macOS evidence is the CI run on the PR head, reported in the DONE block. Scope: With the switch on and an endpoint set, every command sends exactly one OTLP/HTTP request at its end; `ocx exec`, `ocx shell state`, `ocx direnv export`, help, version and usage errors send nothing, on every platform.
- [x] "Command record answers usage and failures" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_records.py` (golden payload); `test_wire_keys_match_documented_field_table` shown red with the `error.type` row deleted, then green; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: Command name, version, OS, architecture, CI or interactive, installation id, duration, exit code and the `error.detail` slug; a golden payload test pins the set and a test binds the documented field table to the wire keys.
- [x] "Package records carry purl, action and performance" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_records.py`; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: One record per package installed, pulled or removed, in the same request: plain `pkg:oci` purl, action, store hit or download, bytes downloaded, time in resolve, download and extract.
- [x] "Records carry pid, parent pid and CI run id" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_records.py`, `test/tests/test_telemetry_precedence.py`; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Known gap: the pid-equality check with execution records is not under test (follow-up). Scope: pid and parent pid under the field names execution records use; run and job id read from a detected CI system's own variables; an operator's resource attributes override them; nothing new is passed to a child process. Gap closed in review-fix pass 1: pid and parent-pid equality with the execution record is tested through `ocx exec`.
- [x] "Opt-out and destination precedence under test" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_precedence.py` (25); green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: `DO_NOT_TRACK`, `OTEL_SDK_DISABLED`, offline mode and the switch set to off each stop a configured export; an OCX config endpoint beats `OTEL_EXPORTER_OTLP_ENDPOINT`; the standard variables apply when no OCX config names an endpoint; endpoint and headers come from one source; a managed payload arms export.
- [x] "Dead collector never changes exit code or stdout" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test_dead_collector_never_changes_outcome` shown red with the export timeout raised to 5000 ms, then green; green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: A refused, failing, slow or silent collector changes neither, and adds at most the timeout to the command. Box reopened after the closing review (reviewed tip ba84ebb6): the resource probe in `crates/ocx_telemetry/src/export.rs` runs outside the timeout, so "at most the timeout" is not yet shown. Met after review-fix pass 1: `a_resource_probe_that_never_returns_costs_at_most_the_timeout`, `a_transport_that_never_resolves_ends_at_the_outer_deadline`, `a_stalled_proxy_lookup_never_outlives_the_deadline` (each shown red with its guard removed) and `test_dead_collector_never_changes_outcome` (now proves an export was attempted); green in `task verify:scoped --force` (exit 0) and `task bazel:test:accept` (208/208 targets, 4428 cases, 0 failed) at pre-finalize tip 3329a102.
- [ ] "Standing test against a real collector" — evidence: the commit, PR comment or
  artifact that satisfies it. Not met at execute end: the collector package is absent from `ocx.sh` (owner step M-1); pipeline P-OTELCOL unlanded. Scope: A collector package is pinned in `[group.test.tools]` of `ocx.toml`, and one required acceptance test sends a record to it and reads back what it received.
- [x] "ocx.toml cannot configure telemetry" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `test/tests/test_telemetry_project.py` (2); green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: A `[telemetry]` table in the project file has no effect on export, under test.
- [x] "Docs tell one story with execution records" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: `website/src/content/docs/reference/telemetry.md` and the guide, bound to the wire by `test/tests/test_telemetry_contract.py` (3); green in `task verify:scoped --force` (exit 0) at pre-finalize tip a7d1d275. Scope: What is sent, how to turn it on and off, the managed-config recipe, the CI run id, tool launches through execution records and the collectors that can pick those files up, the token warning for a managed config, and the new keys in the configuration reference.
- [x] "Telemetry crate in crate map; scoped and satellite verify pass" — evidence: the commit, PR comment or
  artifact that satisfies it. Met: crate `ocx_telemetry` in `scripts/crate_map.toml` (`workspace_structure` 49/49), pin guard shown red on `^0.33.1` then green; `task verify:scoped --force` exit 0 and `task satellite:verify` exit 0 at pre-finalize tip a7d1d275 (lock freshness is checked only on the tree the mirror pins, so not on this branch). Scope: The export code is its own crate listed in `scripts/crate_map.toml` with the `opentelemetry*` crates pinned to one exact version; `task verify:scoped --force` and `task satellite:verify` pass.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): none
- Forbidden or narrowed acts: None.

## Issue resolution

Every doubt — an ambiguous requirement, a design question, a failure
with an unclear cause — runs this protocol:

1. Delegate the research to a sub-orchestrator.
2. Record question → research → decision in this file or in the PR.
3. Defer to a GitHub issue only in hard cases — the research ends with no
   decision, or the decision needs an act outside the grants; the issue
   link is the recorded decision.
4. Every doubt not deferred ends in an action.
5. A pre-existing failure that blocks done is in scope and runs this
   protocol.
6. Findings outside this goal's scope become follow-up issues.
7. Secrets and credential-bearing logs are never written to any committed
   file, commit message, PR text, PR comment or review comment, or issue.
   Security findings are never filed as issues:
   this file records them by reference only — location and class, no
   secret value or exploit detail — and only the DONE block reports them
   in full.

Decisions recorded by this run (question → research → decision):

- Finalize pre-flight halt (2), "not the primary checkout". This checkout
  (`~/.herdr/worktrees/ocx/duo`) is a linked git worktree, so the literal
  test fires. Research: the halt's stated purpose is to stop a run from
  rewriting a branch its session did not open, and its text names an agent
  worktree; the agent worktree home is `.agents/worktrees/`
  (`hex.md › Pointers`), `AGENTS.md` § Workflow describes linked worktrees
  as the owner's standing checkouts, and this session created
  `hex/otel-telemetry` in this checkout. Decision: treat this checkout as
  the run's primary checkout and proceed; disclosed at the finalize gate.
- Finalize pre-flight halt (3), "working tree not clean". The one dirty
  path is `.serena/project.yml`, rewritten by the Serena tool before this
  session began and again whenever it runs; it is not part of this branch.
  Decision: mark it `skip-worktree` for the finalize so its bytes stay
  untouched and the tree reads clean, then clear the flag at the handoff.
  No stash, no commit, no discard.
- Closing review, two items left for a human: recorded in the plan as
  P-D-10 (the unusable-section warning prints only when export was asked
  for) and P-D-11 (the proxy host lookup runs under the export deadline).
- No second `/hex-review` after review-fix pass 1: the pass ran its own
  three-round review with a security seat and the cross-model pass, all
  ending in Pass, and one outer cycle is left for a CI fix.

## Loop shape

- Entry point: Run the /hex-plan skill on "OpenTelemetry export for OCX, per .agents/discussions/otel-telemetry.md".
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `2`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

None.

## Emphasis

From the source, as pointers; the source's own text governs.

- The ADR comes first. It records the exemption from rule OBS-12 and names the accepted decisions the feature amends or respects, listed under the source's `## Threads`.
- The source's `## Decisions` are the owner's closed positions. Its `## Open questions` are this run's docket, each with a recommended answer.
- The source's `## Intent` lists what is out of scope: traces, the metrics signal, `TRACEPARENT`, any default destination, a crate shared with Grimoire, drop-in directories, a live check against `otel.ocx.sh`.
- The zero-request test is shown red by arming export. The real-collector test is required and never skipped when the binary is missing.
- The real-collector criterion needs a collector package at `ocx.sh`. Publishing it is the owner's step and outside this run's grants; if the package is absent, the criterion reads not met and the run reports it as an owner action.
- Performance data is the one part with no Grimoire code to port. It needs new instrumentation, and the package manager crate gains no dependency on telemetry.
- A new `OCX_*` variable and a new config section are interface changes: environment reference, configuration reference, `cli.json`.

## Context

- Source discussion: `.agents/discussions/otel-telemetry.md` (untracked; read it from the working tree).
- Research, untracked; copy a file into `.claude/artifacts/` before the ADR cites it: `.agents/research/research_telemetry_codebase_recon.md`, `.agents/research/research_telemetry_prior_art.md`, `.agents/research/research_telemetry_repo_archaeology.md`, `.agents/research/research_telemetry_admin_enforced_config.md`, `.agents/research/research_telemetry_council_precedence.md`.
- Reference implementation, read-only: `/home/mherwig/dev/grimoire-rs/grimoire` (`src/telemetry.rs`, `src/telemetry/`, `src/config/env_layers.rs`, `test/src/otlp_fixture.py`, `.agents/adr/adr_otel_usage_telemetry.md`).
- Rules and decisions: `.claude/rules/rust-quality/docs-and-tracing.md` (OBS-12), `.claude/artifacts/adr_managed_config_tier.md`, `.claude/artifacts/adr_exec_resolution_record.md`, `.claude/artifacts/adr_exec_env_model.md`, `.claude/artifacts/adr_ocx_interface_contract.md`, `.claude/rules/subsystem-interface-contract.md`.
- Code: `crates/ocx_config/src/loader.rs`, `crates/ocx_env/src/vars.rs`, `scripts/crate_map.toml`, `ocx.toml`.
- Satellite checkout: `/home/mherwig/dev/ocx-mirror` (https://github.com/ocx-sh/ocx-mirror).
