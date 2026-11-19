# Research: Telemetry council — which config layers may arm export and set the destination

## Metadata

**Date:** 2026-11-18
**Domain:** observability
**Triggered by:** hex-discuss otel-telemetry (council lane, three seats)
**Expires:** 2027-05-18

## Question

Which configuration layers may turn telemetry export on and set its
destination in OCX, and what precedence holds between operator-supplied
config, user config, the standard `OTEL_*` variables and the opt-outs,
including under `OCX_NO_CONFIG`?

Fixed for every seat: export is off by default; it needs an explicit on
switch; a standard `OTEL_*` endpoint alone never starts it; `ocx.toml` is not
a config tier; a failed or slow export never changes exit code or stdout.

Three seats ran, blind to each other: premortem, operability, simplicity.
Each seat stated that it took its `crates/ocx_config/src/loader.rs` claims
from `.agents/research/research_telemetry_codebase_recon.md` and did not read
the loader itself. The operability seat read two of the four research
artifacts. The orchestrator verified two loader facts afterwards, listed
under "Verified".

## Where all three seats agree

- The opt-outs (`DO_NOT_TRACK`, `OTEL_SDK_DISABLED`, offline mode, an explicit
  off value of the OCX switch) beat every layer, including a system lock.
- A token-bearing header never lives in the managed artifact.
- Under `OCX_NO_CONFIG` the managed tier and the discovered user and home
  files contribute nothing; environment variables can still arm export.
- The system, user and `$OCX_HOME` files and the explicit files may set
  `enabled` and `endpoint`.

## Where the seats split

| Point | Premortem | Operability | Simplicity |
|---|---|---|---|
| Managed payload sets `enabled` and `endpoint` | no, strip `[telemetry]` with a warning, as `[update]` is | yes, both; never `headers` | yes, both; never `headers` |
| `headers` in a local config file | no, environment only | yes, in a system (0600) or user file | no, environment only |
| A file `endpoint` shuts `OTEL_EXPORTER_OTLP_*` out | yes | yes | no, environment beats files as elsewhere |
| OCX-scoped endpoint variable | yes, beats files unless locked | yes, beats files unless locked | none; only `OTEL_EXPORTER_OTLP_*` |
| System lock on `[telemetry]` in the first version | yes, pins destination and switch | yes, pins destination, not the switch | none; add later with one call |
| Locked system section survives `OCX_NO_CONFIG` | no, environment only | yes, as `[records]` does | not applicable, no lock |

## Evidence each seat leaned on

- Premortem: the managed-config ADR's accepted "compromised publisher" risk
  (`.claude/artifacts/adr_managed_config_tier.md`), read as fleet-wide egress
  for one tag move; a repository file redirecting an API endpoint
  (CVE-2026-21852); per-signal OTLP variables bypassing an operator's
  collector in Claude Code before v2.1.217; `PIP_*` inherited from a base
  image beating `pip.conf`; the system lock that was a no-op on map entries
  (commit `1c99ef4c`). All cited from
  `.agents/research/research_telemetry_admin_enforced_config.md` and
  `.agents/research/research_telemetry_repo_archaeology.md`.
- Operability: one `ocx config push` tag moves a fleet and rolling back is
  re-pointing the tag; token rotation must not need a republish; a job's own
  `OTEL_EXPORTER_OTLP_ENDPOINT` must not capture usage data; an operator needs
  one command that prints the effective `enabled`, `endpoint` and header
  names with the layer each came from, and the first-match reason when export
  is off.
- Simplicity: every rule that differs from the other config sections is a
  cost; the compromised-publisher risk already covers `[mirrors]`,
  `[registry]` and trust roots, which are worse than a redirected usage
  record; a lock can be added later without changing the precedence chain.

## Strongest argument against each seat, in its own words

- Premortem: a lock that the opt-outs and `OCX_NO_CONFIG` both defeat is not
  enforcement, and refusing the managed tier forces operators to ship
  `/etc/ocx/config.toml` by other means.
- Operability: letting the managed artifact set the endpoint means a stolen
  publisher token redirects a whole fleet's usage data in one push.
- Simplicity: an operator cannot enforce collection, and an authenticated
  collector in CI needs the token as a runner environment variable.

## Verified by the orchestrator after the seats returned

- `crates/ocx_env/src/vars.rs:64-73`: under `OCX_NO_CONFIG` "explicit
  `--config` / `OCX_CONFIG` paths still load and a system-tier lock
  survives"; the `[managed]` snapshot and `OCX_MANAGED_CONFIG` are dropped.
  So an explicit config file is the operator path that reaches a hermetic
  run, and the managed tier is not.
- `crates/ocx_config/src/loader.rs:1110-1144`: `retain_system_locked_sections`
  destructures `Config` exhaustively and decides per section. `[records]`,
  `[registry]`, `[patches]` and the extra-CA pair survive when locked;
  `[update]` ("a personal setting, never operator policy"), `[shell]` and
  `[managed]` never do. A new `[telemetry]` field cannot compile without a
  line here, and `[records]` (a sink with a `required` flag) is the nearest
  existing precedent.

## Orchestrator synthesis

> **Superseded in part (owner, 2026-11-18).** The first two bullets below are
> overruled: the managed payload may carry the whole `[telemetry]` section,
> `headers` and the token included, and the token may live in a config file
> at any tier as well as in the secret header variable. The authority is
> `.agents/discussions/otel-telemetry.md` § Decisions and
> `adr_otel_telemetry.md`. The rest of this file stands as research input.

Recommended, pending the owner's answers on the three split points that
decide direction:

- Managed payload may set `enabled` and `endpoint`, never `headers` (two
  seats; it is the delivery path the owner named, and the risk it adds is
  smaller than the one the managed-config ADR already accepts).
- The token lives in an environment variable only in the first version (two
  seats; no secret in a readable file or a registry artifact).
- A destination named by OCX config wins over the standard
  `OTEL_EXPORTER_OTLP_*` variables, which are read only when no OCX config
  names an endpoint (two seats; Grimoire's rule; it keeps a CI job's own
  tracing endpoint and token from mixing with OCX's).
- The opt-outs beat everything (unanimous).

Left for the ADR, with the seats' positions as input: whether `[telemetry]`
is lockable in the first version, whether a locked section survives
`OCX_NO_CONFIG`, whether an OCX-scoped endpoint variable exists beside the
config key, and the introspection command.

negative:
- No seat read `crates/ocx_config/src/loader.rs` directly; their loader
  claims are second-hand from the recon.
- Two seats argued a list-valued config variable is not needed. The owner
  decided otherwise on 2026-11-18; the seats did not weigh that an explicit
  file is the only operator path that survives `OCX_NO_CONFIG`.

leads:
- Read `apply_system_locks` and the `[records]` lock in
  `crates/ocx_config/src/loader.rs` before deciding the lock question: it is
  the worked pattern for a lockable sink.
- Check whether `ocx config show` (or its equivalent) already reports the
  layer a value came from: the operability seat's introspection ask may be
  partly free.
