# Security Audit: Exec-Time Execution Record (`[records]`)

## Executive Summary

**Audit Date:** 2026-09-04
**Auditor:** Security reviewer (Opus 5), swarm reviewer role
**GitHub Issue:** [ocx-sh/ocx#214](https://github.com/ocx-sh/ocx/issues/214) — PR [ocx-sh/ocx#238](https://github.com/ocx-sh/ocx/pull/238)
**Scope:** The exec-time execution record — data model, sink write path, `[records]` policy resolution, forwarded environment, and the `Launch` spawn firewall, on branch `feat/exec-resolution-record` at `c10df99e`.
**Overall Risk Level:** Low

This is a pre-merge threat model of an unreleased feature. Every finding is fixable inside the PR.

The feature is unusually well hardened for its stage. Path traversal, sink substitution, no-clobber
publication, the fail-closed posture, the `OCX_NO_CONFIG` bypass and the forged-scratch-root
exemption escape are all closed **and** carry both-direction tests. The design record and the user
reference are precise about the limits of the controls rather than overclaiming — the `refuse_substituted_sink`
doc and `website/src/docs/reference/execution-records.md:317` each enumerate what the check does *not*
catch, which is the opposite of the usual failure mode.

One real gap remains: a configuration value that can carry a credential is copied verbatim into every
record, in a feature whose design record explicitly excluded `process.args` for exactly that reason.

### Summary of Findings

| Severity | Count | Remediated |
|----------|-------|------------|
| Critical | 0 | 0 |
| High | 0 | 0 |
| Medium | 1 | 1 |
| Low | 2 | 2 |
| Informational | 3 | N/A |

## Scope

### In Scope

- `crates/ocx_lib/src/record/{execution_record,purl,policy,sink,name_template,options,environment,error}.rs`
- `crates/ocx_lib/src/launch.rs` and its private `launch/child_process.rs` submodule
- Frame producers: `crates/ocx_cli/src/command/{exec.rs,toolchain_exec.rs,launcher/exec.rs,launcher/shim.rs}`
- Exemption sites: `crates/ocx_cli/src/command/{package_test.rs,patch_test.rs}`
- `[records]` config plumbing: `crates/ocx_lib/src/config/loader.rs`, `crates/ocx_cli/src/app/context.rs`, `crates/ocx_cli/src/options/records.rs`
- Forwarded environment: `crates/ocx_lib/src/env.rs` (`apply_ocx_config`, `records()`)
- Native Windows shim: `crates/ocx_shim/src/main.rs` (spawn path only)

### Out of Scope

- The `packageurl`, `tempfile`, `dunce`, `sysinfo` and `serde_json` crates themselves (delegated, not hand-rolled — correct per `quality-core.md` "Don't Own Non-Domain Code")
- The managed-config tier's own fetch/identity-gate machinery, audited under `adr_managed_config_tier.md`
- Package resolution, OCI transport and signing, which the record only *reports*

### Methodology

- [x] Static code analysis — every cited file opened, not read from the design record
- [ ] Dynamic testing — a `task verify` run was in flight; no build or test was started by this audit
- [ ] Dependency scanning — no new runtime dependency is introduced by the feature
- [x] Config review
- [x] Threat modeling (STRIDE)

## STRIDE Threat Analysis

| Threat | Description | Mitigated | Notes |
|--------|-------------|-----------|-------|
| **S**poofing | A frame claims a package it did not run; a caller forges a record | Partial | Digest and executable are derived from the same resolution the launch uses (`launch.rs:120-128`). The *sink* has no writer authentication — see FINDING-003. |
| **T**ampering | Redirect, replace or pre-create a record | Partial | Sink pinned at designation and re-checked before every write; no-clobber publish. TOCTOU window is documented and accepted (`sink.rs:150-159`). |
| **R**epudiation | The record names a different object than what ran | Yes | One resolution feeds both record and launch; the deferred-tool frame records after materialisation. |
| **I**nformation Disclosure | Secrets or over-broad context reach a fleet-aggregated sink | Partial | `process.args` correctly absent; absolute paths documented. Mirror URL userinfo is not — FINDING-001. |
| **D**enial of Service | A record failure stops the fleet | By design | `required = true` turning a full disk into an outage is the accepted consequence (ADR §"Failure posture"). |
| **E**levation of Privilege | Run a child without a record under a SYSTEM lock | Yes (for tool launches) | Every tool-launch path enumerated below records or is refused. Two non-tool spawn paths remain by design — FINDING-004, FINDING-005. |

## Findings

### Medium Findings

#### [FINDING-001] `[mirrors]` endpoint URLs are copied verbatim into every record, so embedded credentials reach the sink

**Severity:** Medium
**Status:** Fixed in this PR — `mirror_endpoints` strips userinfo before the value reaches the record
**CWE:** [CWE-522](https://cwe.mitre.org/data/definitions/522.html) (Insufficiently Protected Credentials), [CWE-532](https://cwe.mitre.org/data/definitions/532.html) (Insertion of Sensitive Information into Log File)

**Description:**

`resolution.mirrors` is projected straight from the configured `[mirrors]` values with no
transformation:

```rust
// crates/ocx_lib/src/record/execution_record.rs:914-922
.map(|(host, config)| {
    (
        host.clone(),
        MirrorEndpoints {
            registry: config.registry.clone(),
            index: config.index.clone(),
        },
    )
})
```

`config.registry` / `config.index` hold the **raw configured string**, before role parsing
(`config/mirror.rs`, `ResolvedMirrors.merged` is documented as "not-yet-role-parsed entries ...
forwarded verbatim"). The mirror URL parser accepts userinfo without comment:

```rust
// crates/ocx_lib/src/config/mirror.rs:474-481
let (host, raw_prefix) = match rest.split_once('/') {
    Some((host, prefix)) => (host, prefix),
    None => (rest, ""),
};
if host.is_empty() { return Err(MirrorConfigError::MissingHost { .. }); }
```

`https://svc:t0ken@artifactory.corp/ghcr-remote` yields `host = "svc:t0ken@artifactory.corp"`, and
that host is interpolated straight back into a request URL at
`crates/ocx_lib/src/oci/index/ocx_index.rs:939`
(`format!("{}://{}{}", target.protocol, target.host, path)`), so the embedded credential is
*functional* — reqwest parses the userinfo and sends Basic auth. Embedding credentials in an
Artifactory/Nexus remote-repository URL is a mainstream idiom, and nothing in OCX refuses it,
warns about it, or strips it.

The design record excluded `process.args` on precisely this reasoning: *"A command line routinely
carries access tokens and passwords, and this record's sink is operator-collected and often
fleet-aggregated — exactly the destination that turns one leaked argv into many."* The same
sink, the same amplification, a different field. `website/src/docs/reference/execution-records.md:318-322`
warns about absolute home-directory paths in records and says nothing about this.

**Location:**
- `crates/ocx_lib/src/record/execution_record.rs:910-924` (`mirror_endpoints`, the emit site)
- `crates/ocx_lib/src/config/mirror.rs:455-491` (`parse_url`, which admits userinfo)
- `crates/ocx_lib/src/record/execution_record.rs:867` (`mirrors: composed.then(...)`, the caller)

**Impact:**

An operator who configures a mirror with an embedded credential — a supported-in-practice, silently
accepted spelling — publishes that credential into **every execution record on the fleet**. Records
are written `0600` locally, but the whole purpose of the sink is collection: they are rsynced,
shipped to a SIEM, or long-retained in an archive with a wider audience than the host operator. One
misconfiguration becomes N copies of a registry credential in a log store.

Precondition: an operator (SYSTEM, user, `$OCX_HOME`, or managed tier) writes userinfo into
`[mirrors."<host>"] registry` or `index`, or into a forwarded `OCX_MIRRORS` payload. No unprivileged
principal is required — this is an operator-error amplifier, not an attack path.

**Proof of Concept:**

```toml
# /etc/ocx/config.toml
[records]
dir = "/var/log/ocx/records"

[mirrors."ghcr.io"]
registry = "https://svc-account:AKCp8k...@artifactory.corp.example/ghcr-remote"
```

Every record then contains:

```json
"resolution": { "mirrors": { "ghcr.io": { "registry": "https://svc-account:AKCp8k...@artifactory.corp.example/ghcr-remote" } } }
```

**Remediation:**

Smallest fix — redact userinfo at the one emit site, so the field keeps its audit value (which host
traffic was rewritten to) without the secret:

```rust
// crates/ocx_lib/src/record/execution_record.rs — in mirror_endpoints
/// Strip any `user:password@` userinfo before a mirror endpoint reaches the
/// record. The sink is operator-collected and routinely fleet-aggregated, which
/// is the same reason `process.args` is not carried: one credential in a config
/// file must not become one per invocation in a log store.
fn without_userinfo(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => {
            let stripped = format!("{}{}", &rest[at + 1..authority_end], &rest[authority_end..]);
            if scheme.is_empty() { stripped } else { format!("{scheme}://{stripped}") }
        }
        None => url.to_string(),
    }
}
```

Apply to both roles in `mirror_endpoints`. Add one test asserting a userinfo-bearing mirror renders
without it, and a discriminator asserting an ordinary mirror URL survives byte-for-byte.

Stronger fix, worth its own issue rather than this PR: refuse userinfo at `parse_url`
(`config/mirror.rs:455`) with a named `MirrorConfigError` pointing at `OCX_AUTH_<slug>_*`, which
`website/src/docs/reference/configuration.md:348` already documents as *the* mirror credential
channel. That would close the same exposure for logs, error messages and the forwarded `OCX_MIRRORS`
payload at once, but it is a config-surface break and belongs to a separate decision.

**References:**
- [OWASP A09:2021 — Security Logging and Monitoring Failures](https://owasp.org/Top10/A09_2021-Security_Logging_and_Monitoring_Failures/)
- `adr_exec_resolution_record.md` §"The process/host/os split, field by field" — the `args` row, whose reasoning this finding extends

---

### Low Findings

#### [FINDING-002] The record filename's random component is not drawn from a CSPRNG

**Severity:** Low
**Status:** Fixed in this PR — `random_component` draws from `OsRng`, hasher retained as a never-fail fallback
**CWE:** [CWE-338](https://cwe.mitre.org/data/definitions/338.html) (Use of Cryptographically Weak PRNG)

**Description:**

```rust
// crates/ocx_lib/src/record/name_template.rs:63-66
pub fn random_component() -> String {
    let value = RandomState::new().build_hasher().finish();
    format!("{:08x}", (value ^ (value >> 32)) as u32)
}
```

`std::collections::hash_map::RandomState` is documented as *not* cryptographically secure; it is
SipHash-1-3 over empty input with a thread-local key whose low half increments by one on every
`new()`. The 64-bit output is then folded to 32 bits. The module doc is accurate about the property
it claims (OS-seeded, differs per process and per call, so two containers that are both PID 1 in the
same millisecond draw different values) and never claims unpredictability.

Correctly, nothing security-relevant depends on this value: it is a filename collision breaker, and
the sink's integrity comes from the no-clobber publish (`sink.rs:212`), not from the name.

**Location:** `crates/ocx_lib/src/record/name_template.rs:63-66`

**Impact:**

Bounded, and only on a sink that is writable by principals other than the recording user — a
group- or world-writable drop-box, which a multi-user shared sink requires. There, a local
unprivileged user who can predict the name sequence can pre-create the target names and force
publication through all eight attempts (`sink.rs:47`), producing exit 74 under `required = true` and
stopping the job. The `MAX_PUBLISH_ATTEMPTS` bound plus 32 fresh bits per retry makes this
impractical in practice; a CSPRNG makes it structurally impossible.

Not reproduced — no attempt was made to recover a `RandomState` key from observed outputs, and
whether SipHash-1-3 key recovery from ~2^k empty-input outputs is feasible was not researched. Report
this as a hardening item, not a demonstrated break.

**Remediation:**

`getrandom` is already in `Cargo.lock` (two entries, transitive). One-line change:

```rust
pub fn random_component() -> String {
    let mut bytes = [0u8; 4];
    // Falls back to the hasher only if the OS entropy source is unavailable, which
    // cannot fail an invocation — a filename is not a security token.
    if getrandom::fill(&mut bytes).is_err() {
        let value = RandomState::new().build_hasher().finish();
        bytes = ((value ^ (value >> 32)) as u32).to_le_bytes();
    }
    format!("{:08x}", u32::from_le_bytes(bytes))
}
```

The existing `successive_random_components_differ` test (`sink.rs:420`) still discriminates.

---

#### [FINDING-003] The sink has no writer authentication, so a shared sink accepts forged records

**Severity:** Low
**Status:** Documented in this PR — unsigned records stay a v1 decision; the sink section now names the residual integrity signals
**CWE:** [CWE-345](https://cwe.mitre.org/data/definitions/345.html) (Insufficient Verification of Data Authenticity)

**Description:**

A record is an unsigned JSON file in a directory. Any principal who can write to the sink can create
a file that looks exactly like a record and claims any package digest, any executable, and — since
`process.user.name` is read from `$USER`/`$LOGNAME` (`environment.rs:123-132`) — any user name. On
the single-user sink the feature's primary scenario assumes (a batch node, fresh profile per job)
this is irrelevant. On a shared or group-writable sink it is not.

The v1 decision not to ship an in-toto `Statement` wrapper or signing is deliberate and correct
(`adr_exec_resolution_record.md` §"Explicitly not in the design": *"it unlocks no consumer without
also shipping signing"*). The gap is not the missing signature — it is that nothing tells the
collector what the residual integrity signal actually is.

Two signals do exist and are not documented together:

1. Each record is created `0600` (`NamedTempFile::new_in`, published by rename/link — verified by
   `test/tests/test_execution_records.py:1524` and stated at
   `website/src/docs/reference/execution-records.md:311`), so the **file owner** is the writing uid.
2. `process.user.id` comes from `geteuid` and cannot be moved by the environment
   (`environment.rs:98-103`), unlike `process.user.name`.

A collector that cross-checks the file's owning uid against `process.user.id` rejects a record
forged under another identity. Nothing says so.

**Location:**
- `crates/ocx_lib/src/record/sink.rs:193-231` (publish; no authenticity step, correctly)
- `website/src/docs/reference/execution-records.md:303-322` (the sink section, where the guidance is missing)

**Impact:**

On a multi-writer sink, an audit built from record files alone can be poisoned: an attacker adds
records claiming approved digests for work that ran with different bits. Not a confidentiality or
availability issue; it degrades the compliance claim the feature exists for (design driver D1,
"compliance-grade, not decorative").

**Remediation:**

Documentation, in this PR. Add to the sink section of
`website/src/docs/reference/execution-records.md`, beside the existing "ignore `.tmp*`" collector
rule:

> **A record is not signed, and the sink is not authenticated.** Any principal who can write to the
> sink directory can create a file that parses as a record. On a single-writer sink this costs
> nothing. On a shared sink, the only integrity signal is the file's own ownership: OCX creates every
> record `0600` under the invoking user, and `process.user.id` comes from the kernel rather than the
> environment. A collector on a shared sink should reject a record whose owning uid disagrees with
> its `process.user.id`, and should treat `process.user.name` as a display field throughout.

Signing the record is a v2 question that belongs with the in-toto `Statement` wrapper, not here.

---

### Informational

#### [INFO-001] The Windows shim's `CreateProcessW` is invisible to the spawn firewall and absent from its allowlist

`no_process_spawn_outside_launch` (`crates/ocx_lib/src/launch.rs:1113`) walks every `.rs` file under
`crates/`, so `crates/ocx_shim/src/main.rs` **is** scanned. It matches none of
`SPAWN_TOKENS = ["process::Command", "process::{", "process as ", "CommandExt"]`
(`launch.rs:903`) because it spawns through `windows_sys::Win32::System::Threading::CreateProcessW`
(`crates/ocx_shim/src/main.rs:238, 633, 710`). It therefore passes without an `SPAWN_ALLOWED` entry.

**This is not a recording bypass.** The shim spawns `ocx launcher exec "<pkg_root>" -- "<stem>" <argv>`
(module doc, `crates/ocx_shim/src/main.rs:9`), never the tool — and `launcher exec` is one of the four
recording frames. Containment is checked twice, in the shim (`core::pkg_root_allowed`,
`main.rs:348`) and authoritatively in `validate_launcher_pkg_root`
(`crates/ocx_cli/src/command/launcher/exec.rs:433`).

The observation is about the firewall's own honesty. `SPAWN_ALLOWED`'s doc says *"each is listed by
name, because a blanket allowlist would be worse than no firewall"* — but the one spawn site in the
tree that does not go through `std`/`tokio` `Command` is neither caught nor listed, so the list
under-reports. The module doc already scopes the claim correctly (*"read the claim as 'the
primitives are private and the common escapes are caught', never as 'spawning outside the seam is
impossible'"*), which is why this is informational rather than a finding.

Cheapest improvement: add `"CreateProcessW"` to `SPAWN_TOKENS` and `crates/ocx_shim/src/main.rs` to
`SPAWN_ALLOWED` with the reason ("re-entry into `ocx launcher exec`/`launcher shim`, both recording
frames — the shim never launches a tool itself"). That makes the list complete and would red on a
new native-spawn site. Prove it goes red before trusting it: the token must match the shim's real
call before the allowlist entry is added.

#### [INFO-002] Plugin dispatch runs an arbitrary binary with no record, including under a SYSTEM lock

`crates/ocx_cli/src/app/plugin_dispatch.rs` is `SPAWN_ALLOWED` entry 1. `ocx <name>` executes
`ocx-<name>` from `PATH` with the ambient environment, and writes no record even when a SYSTEM-scope
`[records]` policy has `required = true`.

Consistent with the firewall's stated subject (*"running a program that this invocation resolved out
of a package and composed an environment for"*) and with the ADR's threat framing (*"the lock
protects against error, not malice"* — anyone who can put `ocx-foo` on `PATH` can run `foo`
directly). Recorded so a future reviewer does not read the SYSTEM lock as covering every child ocx
starts. No change recommended.

#### [INFO-003] What a compromised `[managed]` config package can do to the sink — the SYSTEM-lock defence holds

The owner accepted the managed tier as a `[records]` source pinned by floating tag, with the SYSTEM
lock as the defence. That defence is verified:

- The managed payload merges via `accumulator.merge(parsed)` (`config/loader.rs:453`) into an
  accumulator that already carries the SYSTEM tier, and `RecordsOptions::merge` early-returns when
  `self.system_locked` (`record/options.rs:60-62`). A locked `[records]` block is therefore
  untouchable by the payload — `dir`, `name` and `required` alike.
- The payload cannot declare itself locked: `system_locked` is `#[serde(skip)]`
  (`record/options.rs:43-45`), proven by `toml_cannot_set_system_locked` (`options.rs:127`).
- `OCX_NO_CONFIG=1` no longer drops the lock (`loader.rs:1490` `retain_system_locked_sections`,
  keeping `records` iff `system_locked` at line 1508).

**Without** a SYSTEM lock, a compromised managed package can do three things, all worth stating so
the accepted risk is legible:

1. Set `dir` to a local path it can later read — the record's contents (hostname, uid, `$USER`,
   `project_root`, `working_directory`, absolute executable paths, the full digest closure) become
   readable to whoever controls that path's permissions. Records are `0600`, so this needs a directory
   the attacker can reach, not merely name.
2. Set `required = true` with an unwritable or absent `dir` — every `ocx exec`, `package exec`,
   launcher re-entry and shim materialisation on the fleet exits 74. A one-line fleet-wide outage.
   `RequiredWithoutSink` (`policy.rs:154`) catches only the `required = true` *with no dir anywhere*
   spelling; `required = true` plus a bad `dir` is exactly the fail-closed behaviour, as designed.
3. Redirect a *previously working* sink, so collection silently stops from the collector's side while
   sources keep exiting 0.

None is reachable under a SYSTEM lock. The mitigation the ADR names — pin `[managed] source` by
digest, or lock `[records]` at SYSTEM scope — is the right one and is already documented.

## OWASP Top 10 Assessment

| Category | Status | Notes |
|----------|--------|-------|
| A01: Broken Access Control | Pass | SYSTEM clamp is binary and per-block, applied inside `merge` so every file tier inherits it from one line (`options.rs:60`). Env and CLI cannot set `required` (`policy.rs:129-130`), verified in both directions. |
| A02: Cryptographic Failures | Pass | No cryptography introduced. FINDING-002 is a non-crypto PRNG used for a filename, not a secret. |
| A03: Injection | Pass | purl construction delegated to `packageurl` (`purl.rs:96-107`); JSON via `serde_json`; `identity_note` is a compile-time constant (`execution_record.rs:801`). Filename injection closed at three layers — see below. |
| A04: Insecure Design | Pass | The design record carries an explicit, honest threat model and states the limits of every control rather than overclaiming. |
| A05: Security Misconfiguration | Partial | FINDING-001 is a misconfiguration amplifier. FINDING-003 is a missing collector-side instruction. |
| A06: Vulnerable Components | Pass | No new runtime dependency. |
| A07: Auth Failures | N/A | No authentication surface. |
| A08: Data Integrity Failures | Partial | No-clobber publish and the sink pin are solid; record authenticity is deliberately unaddressed in v1 (FINDING-003). |
| A09: Logging Failures | Partial | The feature *is* the audit trail; FINDING-001 puts a credential class into it. |
| A10: SSRF | N/A | The record path performs no network I/O (`ExecutionRecord::build` is documented infallible with no I/O, `execution_record.rs:747-750`). |

## Verified Handled — Do Not Re-Report

Each of the following was reached for as a candidate finding and closed by opening the code.

| Threat | Handled at |
|---|---|
| Path traversal via `--records-name` / `OCX_RECORDS_NAME` / template literal | `record/name_template.rs:118-122` refuses `/`, `\`, `.`, `..` at parse; `record/sink.rs:247-255` `resolve_in_sink` refuses any rendered name that is not one `Component::Normal` compared verbatim against the input, so nothing is normalised into looking well-formed. Both directions tested (`sink.rs:470`, `sink.rs:492`). |
| Traversal via a hostile `{host}` expansion (`/`, `..`) | `record/name_template.rs:224-230` `sanitize_host` slugs to `[A-Za-z0-9._-]` and drops a dots-only result; `{host}` expands **last** (`name_template.rs:188`) so a hostname cannot introduce a placeholder earlier passes would expand. Tested at `name_template.rs:451` and `:469`. |
| Sink directory swapped for a symlink after designation | Pinned once at `record/policy.rs:189-191` `pin_sink`; re-checked before **every** write at `record/sink.rs:73` and in the pre-spawn probe at `sink.rs:121`. The `#[cfg(not(unix))]` probe placement (`launch.rs:258-260`) is why the check also lives in `emit` — otherwise it would be inert on Linux and macOS. Tested at `launch.rs:720`. |
| Record overwriting another record (predictable name, cross-container PID reuse) | `record/sink.rs:210-222` retries under a fresh name via `persist_temp_file_noclobber`; the NFS lost-reply case resolves on device+inode identity, never link count (`utility/fs.rs:322-329`). Tested at `sink.rs:338`. |
| Symlink or pre-created file at the target name | `NamedTempFile` creates `O_CREAT\|O_EXCL`; `persist_noclobber` never replaces, and `published_at` uses `symlink_metadata` so a symlink at the target is never mistaken for our own link (`utility/fs.rs:319-328`). |
| `process.args` / environment values leaking into the record | `Process` (`execution_record.rs:203-245`) has no `args` field; `argv` is a `RecordInputs` input only and never reaches `build` (`execution_record.rs:751-769`). A regression test plants `Authorization: Bearer` in argv (`execution_record.rs:2466`). No env entries are in `RecordInputs` at all. |
| Record file world-readable | `0600` by `NamedTempFile` construction, preserved through rename; asserted by `test/tests/test_execution_records.py:1524` and documented at `execution-records.md:311`. |
| `OCX_NO_CONFIG=1` defeating a SYSTEM `[records]` lock | `config/loader.rs:1490-1536` `retain_system_locked_sections` (line 1508 for `records`), plus `loader.rs:170-171` loading the system candidate under the flag. |
| Symlinked or unreadable `/etc/ocx/config.toml` silently dropping the policy | `config/loader.rs:636-663` — fatal for the SYSTEM candidate only (`Error::SystemConfig`, exit 78); `NotFound` stays a silent skip; user tiers keep best-effort. Acceptance test at `test/tests/test_execution_records.py:2096`. |
| A repository redirecting the audit trail via project `ocx.toml` | `config/loader.rs:1408-1413` strips `[records]` from the project tier with a warning. |
| `--config` / `OCX_CONFIG` overriding a locked block | `loader.rs:213` merges the overlay onto an accumulator whose `records` is locked; `RecordsOptions::merge` early-returns (`options.rs:60`). |
| Managed payload overriding a locked block | Same early return, reached from `loader.rs:453`. See INFO-003. |
| Forged scratch pkg-root escaping a fail-closed policy | `launch.rs:189-194` `exemption_allowed` refuses when `required() && is_recording()`; `launcher/exec.rs:81-84` folds `[records]` on the exempt path too. Acceptance test at `test/tests/test_execution_records.py:2148`. |
| `ocx package test --script` / `patch test --script` running unrecorded under a fail-closed policy | Closed — both call `launch::exemption_allowed` before the Starlark interpreter starts (`package_test.rs:290`, `patch_test.rs:239`), so the bound holds for frames that never reach a `Launch`. |
| Deprecated `ocx run` bypassing the seam | `command.rs:156` `DeprecatedRun(toolchain_exec::ToolchainExec)` — the same type, so the same recording frame. |
| The record naming a package other than the one launched | `launch.rs:120-128` derives `executable` and `args` from `RecordInputs` rather than taking them separately; each frame resolves once and hands the result to both (`launcher/exec.rs:332`, `launcher/shim.rs:193`). The deferred-tool frame records **after** `materialize_deferred` (`launcher/shim.rs:127-138`), so its digest names the bits actually on disk. |
| A synthetic launcher identifier dressed up as a published one | `record/purl.rs:61-63` `has_logical_identity`; the descriptor stays digest-only with `sh.ocx.identity: "synthetic"` (`execution_record.rs:1147`). |
| purl encoding ambiguity from the unencoded-colon rule | Delegated to `packageurl` (`purl.rs:96-107`); the colon is in the crate's non-encode set, and `version_carries_the_digest_with_an_unencoded_colon` (`purl.rs:168`) asserts the round trip through the parser, not a literal string. No ambiguity: `repository_url` and `tag` are separate qualifiers the crate encodes itself. |
| Records built and discarded when recording is off | `launch.rs:336-338` early-returns before `ExecutionRecord::build`; observed by a thread-local counter rather than inferred (`launch.rs:758`). |
| Doc overclaiming what the sink pin catches | It does not. `sink.rs:150-159` and `execution-records.md:317` each enumerate the three cases the check misses and label it *"an integrity control, not a security boundary"*. |

## Prior Max-Tier Review — Fixes Verified Intact After Rebase

PR [#238](https://github.com/ocx-sh/ocx/pull/238)'s body lists five fix classes from the pre-rebase
max-tier review. Each was re-checked on `c10df99e`; none regressed. They are not re-reported above.

| Prior fix | Still in place at |
|---|---|
| `OCX_NO_CONFIG=1` defeated a SYSTEM lock | `config/loader.rs:1490-1536` |
| Symlinked-ancestor sink guard made macOS unusable | Replaced by pin-at-designation, `record/policy.rs:189` + `record/sink.rs:169`; the macOS discriminator test survives at `sink.rs:563` |
| Windows `parent_pid` walked the whole process table | `record/environment.rs:148-151` omits the key |
| The record was assembled even with no sink | `launch.rs:336-338` |
| `process.args` removed; `registries` reports the content host; per-package platform; `binaries`/`entrypoints` as arrays | `execution_record.rs:203` (no `args`), `:893-903` (`transport_registry`), `:1046` + `:1076` (selected vs omitted platform), `:1222-1231` (`Vec<Value>`) |

The PR body predates the 2026-09-04 amendments, so it describes three frames and camelCase keys; the
branch now has four frames (`launcher shim` added) and snake_case throughout, matching the amended
design record. That divergence is the amendment landing, not drift.

## Recommendations

### Immediate (before merge)

1. Redact userinfo from `[mirrors]` endpoints in `mirror_endpoints` (`crates/ocx_lib/src/record/execution_record.rs:910`), with a red/green pair of tests. — FINDING-001

### Short-term (this PR or the next)

2. Draw `random_component` from `getrandom` with the current hasher as fallback (`crates/ocx_lib/src/record/name_template.rs:63`). — FINDING-002
3. Add the sink-authenticity paragraph to `website/src/docs/reference/execution-records.md`, naming file ownership and `process.user.id` as the residual integrity signals. — FINDING-003

### Long-term (own issues)

4. Refuse userinfo in `[mirrors]` at `config/mirror.rs:455` with a diagnostic pointing at `OCX_AUTH_<slug>_*`. Config-surface break; needs its own decision.
5. Add `CreateProcessW` to `SPAWN_TOKENS` and list `crates/ocx_shim/src/main.rs` in `SPAWN_ALLOWED`, proving the token reds first. — INFO-001

## Remediation Tracking

| Finding | Severity | Owner | Status |
|---------|----------|-------|--------|
| FINDING-001 mirror URL userinfo in records | Medium | builder | Open |
| FINDING-002 non-CSPRNG filename component | Low | builder | Open |
| FINDING-003 unauthenticated sink, collector guidance | Low | doc-writer | Open |

## Appendix

### Tools Used

| Tool | Purpose |
|------|---------|
| Manual source review | Every cited `file:line` opened directly; no finding taken from the design record alone |
| `grep` (real binary, not the proxy) | Call-site enumeration — the `rg` emulation false-negatives alternation patterns |

### Not Verified

- **No test was executed.** A `task verify` run was in flight for the branch and this audit was
  instructed not to start builds. Every claim above rests on reading source and existing test
  assertions, not on observing a run. Where a test is cited it is cited for what it asserts, not as
  evidence that it passed today.
- **Windows behaviour** — the `#[cfg(not(unix))]` probe ordering, `CreateProcessW` argument quoting,
  and the reserved-device-name filename class (`NUL.json`) were reasoned about from source only. The
  reserved-name path appears unreachable because `TemplateNotUnique` (`name_template.rs:150`) refuses
  a constant template, but this was not exercised on a Windows host.
- **NFS behaviour** — the `renameat2` → `link`/`unlink` fallback and the lost-reply identity check
  (`utility/fs.rs:295-329`) were read, not exercised against a real NFS mount.
- **FINDING-002 exploitability** — no attempt was made to recover a `RandomState` key from observed
  outputs; the finding is reported as hardening, not as a demonstrated break.
