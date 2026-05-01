# Research: Deprecation Telemetry Patterns for CLI Escape-Hatch Flags

**Date:** 2026-04-29
**Driven by:** [`adr_visibility_two_axis_and_exec_modes.md`](./archive/adr_visibility_two_axis_and_exec_modes.md) §633 Open Question (track `--mode=full` usage drift post-ship)
**Constraints:** OCX product principles — backend-first, offline-first, private-first ([`product-context.md`](../rules/product-context.md))

## Question

ADR Tension 5 introduces `ocx exec --mode=consumer|self|full`. `--mode=full` is the "permanent debug escape hatch" — but if it becomes the de-facto default, that signals consumer-mode migration was insufficient. **How do we monitor drift while honoring backend-first / offline-first / private-first principles?**

## Industry Context and Trends

- **Established:** Compile-time / pull-time advisory (Cargo, npm). Travels with the artifact, no phone-home.
- **Established:** Stderr-only deprecation lines with stable prefix (Bazel, Terraform, dbt). CI log retention is the aggregation layer; platform teams grep.
- **Declining:** Phone-home opt-in telemetry for tools targeting CI/private-registry use. Incompatible with offline + private-first constraints.
- **Hard rule from real incidents:** Never put deprecation messages on stdout — breaks JSON consumers (gh CLI #5674, sf CLI #1896).

## Per-Mechanism Findings

### Cargo — Compile-Time Lint, No Runtime Telemetry

Rust ecosystem's deprecation signal is entirely static: `#[deprecated(since, note)]` emits a `warn`-level compiler lint at build time, not invocation time. `cargo tree -e features` exposes feature graphs locally but is introspection, not drift sensing. RustSec covers security advisories; Crates.io download counts measure installs — none reveals runtime flag/mode usage.

**OCX signal:** No-phone-home is mainstream and viable. For a CLI flag (not a library API), compile-time lint does not apply; signal must come from the invocation layer.

Sources: [Rust `#[deprecated]` reference](https://doc.rust-lang.org/reference/attributes/diagnostics.html#the-deprecated-attribute), [cargo tree --edges](https://doc.rust-lang.org/cargo/commands/cargo-tree.html)

### npm Deprecation — Pull-Time Advisory, No Phone-Home

`npm deprecate` writes a message into registry metadata. At `npm install`, the client reads metadata and prints `npm WARN deprecated <pkg>: <message>` to stderr. This is pull-time advisory: the warning travels with the package, not back to the publisher.

**OCX signal:** OCX installs from local index snapshots (offline-first). A registry-side advisory would only be seen on `ocx index update`, not on each `ocx exec`. Also, `--mode=full` is a runtime CLI flag, not a package version — no registry hook for flag-level behavior. **The npm pattern does not transfer.**

Source: [npm deprecate docs](https://docs.npmjs.com/cli/v10/commands/npm-deprecate)

### GitHub Actions Default Flips — Deadline-Driven, Not Data-Driven

`actions/setup-node` v4 (October 2023) flipped runtime default from Node 16 to Node 20. Decision driver: Node 16 EOL on 2023-09-11 — not telemetry, not usage analysis. PR #846 contains no compatibility impact assessment. Pattern: external deadline (EOL) = forcing function; major-version boundary = migration contract.

**OCX signal:** `--mode=full` has no inherent expiry. Deadline-based forcing function does not apply. Without a signal from the tool itself, the team will not know when to revisit.

Source: [actions/setup-node PR #846](https://github.com/actions/setup-node/pull/846)

### OpenTelemetry Opt-In — Incompatible With OCX's Three Constraints

GitHub CLI uses opt-out telemetry with `GH_TELEMETRY=log` to print the JSON payload to stderr for local inspection. dbt aggregates deprecation counts in a hosted dashboard. Terraform emits warnings to CI but aggregation requires Terraform Cloud. None satisfies all three OCX constraints simultaneously: backend-first (clean stdout), offline-first (no network call), private-first (no shared telemetry endpoint across orgs on private registries).

Source: [GitHub CLI Telemetry](https://cli.github.com/telemetry)

### Structured Stderr — The Backend-First Canonical Pattern

Multiple production breakage reports establish: deprecation signals must go to stderr, not stdout, and must be prefix-stable for log scraping.

- [GitHub CLI #5674](https://github.com/cli/cli/issues/5674) — deprecation on stdout broke shell scripts; fixed by moving to stderr.
- [Salesforce CLI #1896](https://github.com/forcedotcom/cli/issues/1896) — deprecation on stdout produced invalid JSON; broke every automation consumer.
- [Python deprecation design discussion](https://discuss.python.org/t/mitigating-python-deprecation-message-frustrations-by-improving-the-design-of-deprecation-message-handling/61985) — proposed `--warnings-output-file` and env-var suppression to keep deprecation out of machine-readable streams.

Bazel uses `--incompatible_*` flags to gate deprecated behavior, emitting warnings at invocation time. Terraform emits `Warning: Argument is deprecated` unconditionally on every `plan`/`apply`. Platform teams grep for `Warning:` in CI logs — de-facto aggregation mechanism for backend tools that cannot phone home.

**The pattern:** stable-prefix line to stderr on every escape-hatch invocation; CI log retention is the aggregation layer; platform teams grep. No phone-home, no opt-in, no privacy issue.

Sources: [Bazel command-line reference](https://bazel.build/reference/command-line-reference), [Terraform plugin deprecations](https://developer.hashicorp.com/terraform/plugin/framework/deprecations), [dbt deprecations reference](https://docs.getdbt.com/reference/deprecations)

## Recommendation for OCX

**Option A (recommended): structured stderr deprecation marker on every `--mode=full` invocation.**

```
[OCX-WARN] --mode=full is a debug escape hatch; consumer-default migration may be incomplete. https://ocx.sh/docs/exec-modes
```

Properties:
- **Zero network** — works fully offline.
- **Zero opt-in** — CI log retention is the aggregation layer; platform teams grep `OCX-WARN.*mode=full`.
- **Private-registry safe** — nothing leaves the machine.
- **Backend-first** — stderr is canonical for diagnostics; stdout stays clean.
- **Suppressible** via `OCX_NO_MODE_FULL_WARNING=1` for deliberate automation users (matches `OCX_NO_UPDATE_CHECK` pattern already in OCX's env-var table).

**Option B (do nothing):** Accept permanent ignorance. Viable only if the team commits to treating `--mode=full` as permanently supported regardless of adoption pattern — deliberate product decision, not a free default.

**Option C (registry-side aggregation):** Structurally incompatible. Private-first users do not share a common telemetry channel.

**Verdict:** Option A. One `eprintln!` per `--mode=full` invocation. CI platforms retain logs 30–90 days. Identical mechanism to Terraform, Bazel, dbt for backend tools. Cost negligible; violates none of OCX's seven product principles.

| Mechanism | Backend-first? | Offline-first? | Private-first? | Recommended? |
|---|---|---|---|---|
| Phone-home opt-in | No (network call) | No | Sometimes | No |
| Registry-side advisory (npm-style) | Yes | Partial (pull-time only) | No (private registries don't aggregate) | No |
| Compile-time lint (Cargo-style) | Yes | Yes | Yes | N/A — runtime flag |
| Structured stderr (Bazel/Terraform-style) | Yes | Yes | Yes | **Yes** |

## Sources

- [Rust `#[deprecated]` attribute](https://doc.rust-lang.org/reference/attributes/diagnostics.html#the-deprecated-attribute)
- [cargo tree --edges](https://doc.rust-lang.org/cargo/commands/cargo-tree.html)
- [npm deprecate docs](https://docs.npmjs.com/cli/v10/commands/npm-deprecate)
- [GitHub CLI Telemetry](https://cli.github.com/telemetry)
- [actions/setup-node PR #846](https://github.com/actions/setup-node/pull/846)
- [gh cli issue #5674](https://github.com/cli/cli/issues/5674)
- [Salesforce CLI issue #1896](https://github.com/forcedotcom/cli/issues/1896)
- [Python deprecation design discussion](https://discuss.python.org/t/mitigating-python-deprecation-message-frustrations-by-improving-the-design-of-deprecation-message-handling/61985)
- [dbt deprecations reference](https://docs.getdbt.com/reference/deprecations)
- [Bazel command-line reference](https://bazel.build/reference/command-line-reference)
- [Terraform plugin deprecations](https://developer.hashicorp.com/terraform/plugin/framework/deprecations)
