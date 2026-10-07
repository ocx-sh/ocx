# ADR: Exit codes name the caller's next action

## Metadata

**Status:** Accepted
**Date:** 2026-10-07
**Deciders:** Michael Herwig (owner decision, 2026-10-07: collapse inside [ocx-sh/ocx#580](https://github.com/ocx-sh/ocx/pull/580), before any contract baseline; reuse 82 for `Unsupported`)
**Related PR:** [ocx-sh/ocx#580](https://github.com/ocx-sh/ocx/pull/580)
**Tech Strategy Alignment:**
- [x] No new tool or dependency; `ocx_exit` stays the one exit-code enum for every OCX binary.
**Domain Tags:** api, integration
**Supersedes:** [`research_exit_codes.md`](./research_exit_codes.md) § Direct Recommendation's "private range for tool-specific codes" (a research artifact, not an ADR).
**Amends:** [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md) § Enum policy (the error document goes to v2); [`adr_typed_contract_registries.md`](./adr_typed_contract_registries.md) Decision Driver "Zero wire change"; the exit-code rows of `adr_oci_referrers_signing_v1.md`, `adr_index_claim_command.md`, `adr_snapshot_lifecycle.md`, `adr_self_setup.md`, `adr_self_update_handoff.md`, `adr_managed_config_tier.md`, `adr_key_reference_grammar.md` and `design_spec_cosign_parity.md`.
**Superseded By:** —

## Context

Exit codes 82–87, and the five `error.kind` values that twin 83–87, each name one feature's failure. Each carries 0–2 distinct `error.detail` slugs. The general codes carry 7–177 each. Naming a single failure is the `error.detail` layer's job, which holds 499 slugs.

The codes were allocated as "the first free slot" each time a feature shipped a new failure, so the private range grew by one number per feature. Every consumer kept a per-feature table: `rules_ocx` hints for 83–85, the Python SDK's `IntEnum` for 82–86, and ocx-mirror's transient set.

Two facts make the collapse cheap now. First, no contract baseline exists: `compat_gate.rs` returns early, so no ledger entry is owed. Second, v0.6.5 shipped error documents with `schema_version: 1` carrying the old kinds and codes, but `errors/v1.json` was never published.

## Decision Drivers

- A script must be able to choose its next step from the exit code alone, without a per-feature table.
- Feature identity already has a home with stable names: the `error.detail` slug.
- A shipped number keeps its meaning; the one exception has to be named, bounded and justified.
- The rule must be held by a test and a review seam, not by the next author's memory.

## Industry Context & Research

| Source | What it teaches |
|---|---|
| [gRPC status codes](https://grpc.io/docs/guides/status-codes/) | Few codes, chosen by what the caller does next: `UNAVAILABLE` means retry the call, `FAILED_PRECONDITION` means fix state first, `UNIMPLEMENTED` means not supported or not enabled. Specifics go in error details. |
| [sysexits.h](https://man7.org/linux/man-pages/man3/sysexits.h.3head.html) | Generic codes (64–78); not one of them is tied to a feature. |
| [RFC 9457](https://datatracker.ietf.org/doc/html/rfc9457) | A coarse status plus a fine `type`: the same split as OCX's exit code plus `error.detail`. |

**Key insight:** every one of these three sources puts the specific failure in a second field. OCX already has that field; the five feature codes duplicated it.

## Decision

### The next-action rule

An exit code, and its `error.kind`, names what the caller can do next. It never names which feature failed. Feature identity lives in the `error.detail` slug. A new code is justified only by a next action no existing code offers. A retired number is never reused.

### The resulting code set (13)

| Code | `ExitCode` | `error.kind` | Caller's next action |
|---|---|---|---|
| 0 | `Success` | `internal` | — |
| 1 | `Failure` | `internal` | None known; read the message |
| 64 | `UsageError` | `usage_error` | Fix the command line |
| 65 | `DataError` | `data_error` | Fix or replace the input data |
| 69 | `Unavailable` | `unavailable` | Restore the service; rerunning alone will not help |
| 74 | `IoError` | `io_error` | Fix the local filesystem |
| 75 | `TempFail` | `temp_fail` | Retry the same command; the only code where automated retry is safe |
| 77 | `PermissionDenied` | `permission_denied` | Obtain the permission (a grant, an allowlist entry) |
| 78 | `ConfigError` | `config_error` | Fix the configuration |
| 79 | `NotFound` | `not_found` | Name something that exists |
| 80 | `AuthError` | `auth_error` | Supply or refresh credentials |
| 81 | `PolicyBlocked` | `permission_denied` | Loosen the local policy flag, or pass `--force` (gRPC `FAILED_PRECONDITION`) |
| 82 | `Unsupported` | `unsupported` | Use another registry, forge or build, or have its operator enable the capability; never retry (gRPC `UNIMPLEMENTED`) |

New summaries:

| Item | Summary |
|---|---|
| `ExitCode::Unsupported` and `ErrorCategory::Unsupported` | "The operation as requested is not supported or not enabled by this registry, forge or build; retrying will not help" |
| `ExitCode::PolicyBlocked` | "A local policy or safeguard refused the operation; loosen the flag or pass --force" |

### The `error.kind` set (11)

`usage_error`, `config_error`, `data_error`, `auth_error`, `permission_denied`, `not_found`, `unavailable`, `temp_fail`, `unsupported`, `io_error`, `internal`.

Added: `unsupported`. Removed: `transparency_log_unavailable`, `referrers_unsupported`, `unsupported_key_backend`, `forge_capability_unavailable`, `registry_delete_unsupported`. The four of these that name a capability gap survive as `error.detail` slugs under 82; `transparency_log_unavailable` survives as a slug under 75.

### The boundary of 82 against 64 and 65

`Unsupported` (82) holds only when all three are true:

- The invocation is valid and well-formed.
- The registry, forge or build lacks the capability, or does not enable it.
- Retrying will not help.

Ask in this order; the first yes decides.

| Question | Code | Examples |
|---|---|---|
| Did the CLI reject the option or combination pre-flight, before contacting anything, as a rule of the command's grammar? | 64 | `forge_transport_unsupported`: `--transport git` on a forge driver without a git transport (drop the flag); `forge_transport_operation_unsupported` |
| Is the data received or supplied of a shape or kind this build cannot process? | 65 | `unsupported_tlog_entry_kind`: a Rekor entry kind the verifier does not process; `rekor_set_absent_tsa_present` (same genus) |
| Otherwise | 82 | `registry_delete_unsupported`: the registry answers 405 to a tag delete; `referrers_unsupported`: no Referrers API and no fallback tag; `unsupported_key_backend`: `awskms://` names a backend this build lacks; `forge_capability_unavailable`: job-token push disabled on the project |

### Old → new mapping

No slug is removed. Only `exit_code` values move, and three slugs are added.

| Old | Condition (site) | New | `error.detail` slug |
|---|---|---|---|
| 82 `DirtyRcBlock` | Success-path status of `ocx self setup`; no error document | 81 `PolicyBlocked` (fix the profile, or `--force`) | none; kind `permission_denied` unchanged |
| 84 `ReferrersUnsupported` | — | 82 | `referrers_unsupported` |
| 85 `UnsupportedKeyBackend` | — | 82 | `unsupported_key_backend` |
| 86 `ForgeCapabilityUnavailable` | `JobTokenPush` disabled, or a refusal promoted after an `Unknown` preflight (`git_stderr.rs`) | 82 | `forge_capability_unavailable` |
| 86 `ForgeCapabilityUnavailable` | Publisher not on the job-token allowlist (`gitlab.rs`) | 77 `PermissionDenied` (an authorisation gap; gRPC `PERMISSION_DENIED`) | **new** `forge_publisher_not_allowlisted` |
| 87 `RegistryDeleteUnsupported` | — | 82 | `registry_delete_unsupported` |
| 83 `TransparencyLogUnavailable` | Rekor upload or key fetch that is transient per the transport retry policy (`ocx_oci::transport_policy`: 408/429/502/503/504, a refused or timed-out connect), or a mid-stream transport break | 75 `TempFail` | `transparency_log_unavailable` (meaning kept) |
| 83 | Key fetch answers 4xx or another non-transient status such as 500, or its send fails non-transiently (a refused certificate) (`pipeline.rs`) | 69 `Unavailable` | **new** `transparency_log_key_unavailable` |
| 83 | Upload answers a non-transient 5xx such as 500, or its send fails non-transiently (a refused certificate) (`rekor.rs`) | 69 `Unavailable` | **new** `transparency_log_unreachable` |
| 83 | Key body over the cap, or not UTF-8 (`pipeline.rs`) | 65 `DataError` | **new** `transparency_log_response_invalid` |
| 83 | Sign side gets 2xx with no inclusion proof (`bundle.rs`) | 65 | `rekor_set_malformed` (existing) |
| 83 | Offline with no pinned key (`pipeline.rs`); offline with no trust material at all, or a well-formed trusted root that pins no Rekor key (`trust_resolve.rs`, formerly 78 `trust_root_load`: online the same run fetches it) | 81 | `offline_mode` (existing) |
| 83 | `rekor_set_absent_tsa_present` | 65 (same genus as `unsupported_tlog_entry_kind`) | kept |
| 77 | `offline_sign_refused`, `offline_attest_refused` | 81 (a local policy refusal, like every other offline refusal) | kept |

When several refused referrers share a `failure_rank` tier, the aggregate verify error breaks the tie by how definitive the next action is, so the exit never depends on referrer order: 81 `offline_mode` over 65 `transparency_log_response_invalid` over 69 `transparency_log_key_unavailable` over 75 `transparency_log_unavailable`; the non-retryable outcome wins over the retry.

After the move, 82 carries four distinct slugs: `referrers_unsupported`, `unsupported_key_backend`, `forge_capability_unavailable`, `registry_delete_unsupported`.

### Retired forever: 83–87

`ocx_exit::RETIRED = [83, 84, 85, 86, 87]`. No future `ExitCode` takes one of these numbers.

### The one-time reuse of 82

IC-16 forbids changing a value's meaning under the same name. 82 changes from "dirty shell-profile block" to "unsupported" once, before the baseline. This is acceptable because:

- No contract baseline exists yet, so no published contract version is broken behind a consumer's back.
- The error document moves to v2. Removing kinds (§ Enum policy) and reusing 82 (IC-16) each require it on their own.
- The changelog subject (`feat!: …`) names the break, and its body maps each old code to its new one.
- A v0.6.x script keyed on 82 for a dirty profile now reads "unsupported". `ocx self setup` never exits 82 again, so a `case` arm scoped to that command goes dead rather than misfiring.

### The self-update hand-off degrade

`ocx self update` spawns the newly pulled binary's `ocx self setup --handoff` and reads its exit code (`adr_self_update_handoff.md`).

| Case | Effect |
|---|---|
| A v0.6.5 parent hands off to a child built after this ADR, and the profile is dirty | The child exits 81. The parent's `advisory_for` only knows 82, so it reports `SetupIncomplete` "setup exited 81". The machine state is correct, because the verdict comes from the `current` symlink; only the advisory text degrades, once per machine. |
| `self update` report, dirty profile | `handoff.exit_code` changes from 82 to 81. |
| New `advisory_for` | Keys `DirtyProfile` on the `Installed` verdict plus exit 81. Since 81 also covers offline and frozen refusals, a child that exits 81 before it selects leaves `current` alone, so the verdict is `Pulled` and the advisory is `NotActivated`, never `DirtyProfile`. |

### The package-sign sweep legs

`category_slug` (`command/package_sign_common.rs`) labels each failed leg of a sign sweep in `package sign` and `package push`. Today it emits the leg's `error.kind`. After the collapse that would flatten `referrers_unsupported` to `unsupported`. It now emits the leg error's `error.detail` slug, so `referrers_unsupported` survives on the row.

### Enforcement

| Mechanism | Holds |
|---|---|
| Test `every_exit_code_carries_at_least_three_distinct_detail_slugs` (`crates/ocx_cli/src/exit.rs`) | Counts **distinct** slugs per `exit_code`, since the registry repeats a slug for each enum that declares it; only `Success` is exempt. Proven red by one slug declared in three enums under a new code. |
| Test `retired_numbers_are_never_reused` (beside `RETIRED` in `crates/ocx_exit`) | No value in `ExitCode::ALL` is in `RETIRED`. Proven red by reusing 84. |
| Rule IC-22 in `.claude/rules/subsystem-interface-contract.md` | "Exit code and `error.kind` name a caller's next action, never a feature; feature identity is an `error.detail` slug; ≥3 distinct slugs per code; 83–87 retired." Lint: the two tests above. Authority: this ADR. Plus a semantic-checklist line: a new code or kind cites this ADR's row for its new next action. |
| Named override in the same rule | Overrides the vendored `rust-quality/cli-contract.md`: EXIT-06 ("never reassigned"), its row-82 text and "83–99 unassigned". It states the one-time pre-baseline reuse of 82; IC-22 wins over vendored rows. |
| Shareable `quality-rust-exit_codes.md` § "Choosing a code" | The project-independent rule: a code names the caller's next action, feature identity goes in a detail field, a new code needs a next action no existing code has, a retired number is never reused. It cites gRPC, sysexits.h and RFC 9457, and replaces "79–127 free … tool-specific codes". Listed in `_SHAREABLE_RULES` (`.claude/tests/test_ai_config.py`). |
| Hex review seam | A `reviewer:spec` perspective in `.agents/memory/hex.md` `perspectives.always`, scoped to `crates/ocx_exit/**`, `crates/ocx_exit_derive/**`, `crates/ocx_cli/src/exit/**`, `crates/ocx_cli/src/exit.rs` and `crates/ocx_schema/tests/golden/errors.json`. Its focus ("IC-22 + `adr_exit_code_taxonomy.md`") is a YAML comment, not read by a reviewer; the IC-22 anchor reaches reviewers through `worker-reviewer.md` "Always Apply". |

## Considered Options

The chosen option is the collapse above. Rejected:

| Option | Why rejected |
|---|---|
| Keep the codes (status quo) | Five codes with 0–2 slugs each duplicate the detail layer. Every feature would mint a number, and consumers would keep growing per-feature tables. |
| 86 wholly to 77 | Job-token push disabled is a capability an administrator enables, not a permission the caller can be granted. Only the allowlist case is an authorisation gap. |
| 85 to 78 | The key reference is valid configuration; 78 tells the caller to fix config, but the fix is another build or another backend. |
| One kind per code | `error.kind` is the coarse layer. 77 and 81 both mean "refused, do not retry", and the code already separates them; a new kind adds wire vocabulary without a new next action. |
| Rename `permission_denied` for 81 | Breaks the most-matched kind for a naming gain, when the code already separates policy from permission. |
| Fresh 88 for `Unsupported`, retire 82 too | No reuse at all is the safer reading of IC-16, but it leaves a gap at 82 in a set no baseline has frozen yet. The owner chose the reuse; the error document v2 and the changelog subject signal it. |
| Reuse 83 instead of 82 | 83 is equally a shipped number, with more scripted consumers (`rules_ocx` hints, the SDK's `IntEnum`). 82's old meaning had one producer, on a success path. |
| Move the `forge_transport_*` slugs to 82 | `forge_transport_unsupported` and `forge_transport_operation_unsupported` are pre-flight argv rejections (64, fix the flag). `forge_transport_transient` and `forge_transport_failed` are reachability failures (75, 69). None is a capability gap. |

## Consequences

**Positive:**
- A script's `case $?` needs 13 arms, and none of them changes when a feature ships.
- One rule decides every future code: a new next action, or no new code.
- `TempFail` (75) is now the complete retry set. A transient Rekor failure becomes retryable by the same arm as a transient registry failure.

**Negative:**
- One shipped number (82) changes meaning, and five shipped kinds disappear.
- Consumers that keyed on 83–87 must move to the slug (below).
- One self-update advisory degrades per machine, as described above.

**Risks:**
- A generic, cross-command handler that treated 82 as "rerun with `--force`" now reruns an unsupported operation, and fails again with 82. The changelog subject names the break.

## Downstream consumers

| Consumer | Change | Tracking |
|---|---|---|
| ocx-mirror | Lockstep, in the same change series: `ocx_mirror_error` mapping, the push transient set becomes `TempFail` only, and `sign_backfill` `severity_rank`. Gated by `task satellite:verify`. | [ocx-sh/ocx#583](https://github.com/ocx-sh/ocx/issues/583) |
| ocx-sdk-python | `_errors.py` `IntEnum` lists 82–86 | Issue opened by the owner at the end of [ocx-sh/ocx#580](https://github.com/ocx-sh/ocx/pull/580) |
| rules_ocx | `repo_utils.bzl` hints for 83–85; `_RETRYABLE` will now retry a transient Rekor failure (75) | Issue opened by the owner at the end of [ocx-sh/ocx#580](https://github.com/ocx-sh/ocx/pull/580) |
| Vendored lore `rust-quality/cli-contract.md` | EXIT-06 and its exit table are overridden locally by IC-22 | Upstream issue on `ocx-sh/lore`, opened by the owner |

## Validation

- `errors.json` lists 13 `ExitCode` and 11 `ErrorCategory` entries.
- The slug-set diff, before against after, shows 0 removals and exactly 3 additions.
- Each mutation red is shown: the 3-slug test, `RETIRED`, `advisory_for`, and `_SHAREABLE_RULES`.
- Acceptance: `package prune` on a delete-refusing registry exits 82 `unsupported`; an edited profile makes `self setup` exit 81; Rekor down exits 75; a Rekor key 404 exits 69.
- A numeric sweep finds no exit `83`–`87`, and no 82 in its old dirty-profile meaning, outside this ADR, `RETIRED` and the IC-22 override.

## Links

- [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md) § Enum policy
- [`adr_typed_contract_registries.md`](./adr_typed_contract_registries.md)
- [`adr_self_update_handoff.md`](./adr_self_update_handoff.md)
- [`research_exit_codes.md`](./research_exit_codes.md) (superseded recommendation)
- [ocx-sh/ocx#580](https://github.com/ocx-sh/ocx/pull/580), [ocx-sh/ocx#583](https://github.com/ocx-sh/ocx/issues/583)

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-10-07 | Michael Herwig (owner decision), recorded by architect | Initial, accepted |
