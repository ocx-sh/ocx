# Rebase ledger — `feat/signing-and-trust` onto `main`

Record of the semantic rebase of PR [#203](https://github.com/ocx-sh/ocx/pull/203) (Signing & Trust v1)
from merge-base `fa74af1c` onto `main` at `40f637f2` (84 commits ahead). Each of the branch's 22 commits
was re-evaluated against main's current contracts rather than replayed — the question asked per commit was
"given what main now does, how should this have been written?".

Result: 21 commits, 186 files, +34 425 / −354.

## Submodule (`external/rust-oci-client`)

| | |
|---|---|
| Change | `pull_referrers_native` — `Ok(None)` on a referrers-endpoint 404 instead of the tag-schema fallback; without it exit 84 (`ReferrersUnsupported`) degrades to 79 (`NoSignaturesFound`) |
| Was | `09b647c`, based on `f83e9e0`, on no branch |
| Now | `a5bd2e0` = `ocx/integration` (`dda72c0`) + the same patch (identical `patch-id`) |
| PR | [ocx-sh/rust-oci-client#3](https://github.com/ocx-sh/rust-oci-client/pull/3) → base `ocx/integration` |
| Gate | submodule `cargo test --lib`: 73 passed |
| Consequence | the superproject pointer is **two** commits past main's `099ca3c` — it also carries `dda72c0` (*stop a cross-host upload Location receiving registry credentials*) |

## Section A — main's contract changes that reached us

| Axis | Main's shape | What changed on our side |
|---|---|---|
| Exit codes | `ExitCode` stops at `DirtyRcBlock = 82` | `RekorUnavailable = 83` / `ReferrersUnsupported = 84` keep their numbers |
| Error taxonomy | `fccdd447` / `9ead0fd2` added `ClientError::RegistryTransient` and the 75-vs-69 split; `registry_error()` classifies 401/403 → `Authentication`, 429/502/503/504 → transient | **Our four HTTP-status variants deleted** (see C1) |
| `OciTransport` | `mount_blob` (`eb7144c9`), upload retry (`531ddbea`), 30 s connect bound (`50b4ee5a`) | `mount_blob` kept beside our two referrer methods in the trait and both impls |
| `physical_reference` | `825ac467`: local-first under Default/Frozen/Offline + `guard_local_physical`; signature unchanged | our four routing commits replay; `867739d8` re-derived on the new shape (C2) |
| `read_reference` | `825ac467` added a `ReadAddressing` switch whose `Canonical` arm is `identifier.canonical_reference()` | our `transport_write_reference` kept as the write-path peer, cross-documented (C3) |
| Index | `e8a13860` local index is the package-tier lock; `LocalWritePolicy::ReadOnly` | `read_only_view` used by verify — applied unchanged |
| CLI format | `9eb81d7c` `--json` = `--format json`; `ContextOptions.format` is now a flattened `Format` resolved by `mode()` | envelope gate rewritten to `format.mode() == FormatMode::Json` (C4) |
| Commands | `config test`, `index sync`, `index regenerate`, `package announce`, `package cascade {check,repair}`, `launcher shim` | `canonical_command_name` extended; AI-config coverage map extended (C5) |
| Test harness | `cc948621` parametrized both compose services with `OCX_TEST_REGISTRY_PORT` / `OCX_TEST_MIRROR_PORT` | our rival `OCX_ZOT_PORT` deleted (C6) |
| Lazy tools | `26926b28` — a tool joins `PATH` with no content downloaded | **no bypass**: first invocation re-enters `ocx launcher shim` → `materialize_deferred` → `find_or_install_all` → `pull()`, and our auto-verify hook sits inside `pull()` (`tasks/pull.rs:259`), not in a CLI handler. Proven by mutation, see Verification |
| Metadata | `8e6eb3ea` build receipt; `339383af` `${…}` grammar | sign/verify read neither — doc-surface merges only |

## Section B — our commits, and what each became

| # | replayed as | original | verdict |
|---|---|---|---|
| 1 | `c57497c1` | `5b620301` | replay + adapt (49 colliding files); absorbed the AI-config repair (#9) and the third-party-notice regeneration (#8) |
| 2 | `fix(oci): tolerate unknown [[trust.policy]] keys` | `397bafa1` | replay |
| 3 | `fix(package): reserve dash-form referrer and cosign tags` | `cb9ceeec` | replay |
| 4 | `test(project): [env] cannot declare trust-sensitive OCX keys` | `72960b5a` | replay |
| 5 | `fix(oci): verify through the read-only index view` | `d7db0570` | replay (main's `read_only_view` is the API it already used) |
| 6 | `fix(cli): budget plain-mode sign/verify reports` | `a824109e` | replay |
| 7 | `test: [[trust.policy]] survives in-place ocx.toml edits` | `037f3d3d` | replay |
| 8 | `chore(claude): fold the trust-root cache into the StateStore rule` | `3b6dbef9` | **reworded** — the notice regeneration moved into #1, where the sigstore deps enter |
| — | folded into #1 | `2a3d7a5b` | **fold** — its skill-description trims still apply (the branch adds `swarm-loop` + `swarm-x`, pushing the budget to 4263 > 4000); its command-table half was re-derived against main's newer command set |
| 9 | `fix(oci): sign and verify against the physical registry` | `7eac9b9d` | replay |
| 10 | `fix(oci): route sign/verify/capability refs through the mirror seams` | `6247a456` | replay — restores main's T-arch-G1 guard, which #1 alone reds |
| 11 | `fix(cli): suppress the JSON error envelope after a report` | `eb88c84a` | re-derive (C4) |
| 12 | `fix(oci): push signatures to the canonical host, never a mirror` | `da4d7603` | re-derive (C3) |
| 13 | `docs(oci): trust-policy masking + two unchecked cert properties` | `67c0a1ce` | replay |
| 14 | `test(oci): walk the full verify read chain` | `42d16f54` | replay |
| 15 | `fix(oci): bind the signed subject descriptor to the resolved digest` | `224bd74d` | replay |
| 16 | `test: make the assertion-free checks around signing discriminate` | `d34dc2f1` | replay |
| 17 | `fix(oci): apply the SSRF floor to the offline physical answer` | `867739d8` | **re-derive** (C2) |
| 18 | `docs(oci): correct trust-policy and envelope claims, pin the reserved-tag match` | `9339b8ad` | **reworded** — the "rebase invalidated" framing is gone; the `verify_client` call-site half moved into #17, which performs the rename |
| 19 | `fix(trust): let a system-tier policy pin its trust scope` | `3d11cfe7` | replay |
| 20 | `docs: the JSON error envelope covers every command` | `aadde0d5` | replay onto main's `--json` section |
| 21 | `fix(trust): report the pin that discards a tighter policy` | `141d9066` | replay |

## Section C — decisions where main's answer and ours disagreed

| # | Question | Decision |
|---|---|---|
| C1 | Our `ClientError::{Unauthorized, Forbidden, RateLimited, ServiceUnavailable}` vs main's `Authentication` + `RegistryTransient` | **Delete ours.** Main's `registry_error()` already classifies 401/403 → 80 and 429/5xx → 75; keeping a second taxonomy would have given one wire fault two exit codes. `ReferrersUnsupported` (84) stays — it is a capability answer, not a status echo |
| C2 | Offline SSRF floor after main made `physical_reference` local-first | Re-derived: main's `guard_local_physical` skipped the floor under `--offline` on the premise that offline builds no client; this branch builds one in every mode for offline verify, so the carve-out is removed and the `trusted_hosts` exemption rides `LocalIndex`. Main's `ChainedIndex::trusted_hosts_for` was **replaced**, not shadowed — the inherent copy our commit added would have answered differently from the trait method through `dyn IndexImpl` |
| C3 | `transport_write_reference` vs main's `read_reference(_, ReadAddressing::Canonical)` | Keep both, cross-documented: the read switch answers "which host does this read address", the write seam answers "where may a push land". Same address, different question |
| C4 | Error-envelope gate against main's flattened `Format` | `format.mode() == FormatMode::Json`; the `reported` latch keeps its role |
| C5 | Our new command-table coverage guard now sees main's commands | Extended the map and **documented the two commands nobody had documented** (`config test`, `index regenerate`) rather than exempting them |
| C6 | `OCX_ZOT_PORT` vs `OCX_TEST_REGISTRY_PORT` | Main's two variables; ours deleted. One canonical spelling |
| C7 | Third-party notices | Regenerated in #1 (where the sigstore stack enters), not replayed as a diff |

## Verification

| Gate | Result |
|---|---|
| `task rust:verify` | green — 4981 unit tests, 0 failed |
| `task verify` (fmt, clippy `-D warnings`, hawkeye, cargo-deny, notices, actionlint, shellcheck/shfmt, AI-config, link check, build) | green, exit 0 |
| `task test:parallel` (acceptance, zot + registry:2 fixtures) | green — 2136 passed, 48 skipped, 4 xfailed, 2 xpassed |
| Diff size vs pre-rebase | 186 files / +34 425 vs 185 / +34 365 (delta = regenerated `LICENSE-THIRD-PARTY.md` + this file) — no lost hunks |

**Discrimination check (auto-verify).** With the hook at `tasks/pull.rs:259` short-circuited and the binary
rebuilt, `test_auto_verify.py` goes 9 failed / 3 passed — including `test_run_is_policy_gated`,
`test_package_exec_is_policy_gated`, `test_package_env_is_policy_gated` and `test_pull_is_policy_gated`.
Restored and rebuilt, the same file plus `test_referrers_capability.py` is 14/14 green. The green is
therefore distinguishable from "never ran", and the lazy-tool surface is covered by construction.

**Intermediate state.** Commit #1 alone reds main's `native_reference_direct_construction_restricted_to_seams`
guard; #10 is the commit that satisfies it. That ordering is inherited from the original history.

## Residual finding — not fixed here

`825ac467` split main's SSRF floor in two: a resolve-time guard (`guard_local_physical`, tolerant of a DNS
lookup failure, which our pipelines DO traverse via `Index::physical_reference`) and a **dial-time**
re-validation (`Index::guard_physical_dial`, fail-closed on everything). The dial-time half has exactly one
production call site — `package_manager/tasks/pull.rs:903`, inside layer extraction. The sign and verify
pipelines dial the transport directly (`pull_manifest_raw`, `push_blob`, referrer list/push), so they never
reach it: a hostile local index answering NXDOMAIN at resolve time and a private address at dial time would
be admitted for signature traffic, while the same trick is refused for layer traffic.

Narrow (needs a writable local index tree, which is already in the operator's trust domain per
`adr_index_indirection.md` A2) and pre-existing in shape — but it is a gap main's own model considers worth
closing. Left untouched deliberately: one hat, this is a rebase. Belongs with the #205 production-hardening
pass, or its own fix.

## Out of scope

The max-tier review backlog (`review_max_feat_signing_and_trust.md` — B1–B4, bundle size cap, Sigstore HTTP
timeouts, facade bypass, ADR Amendment 1, ANY-of vs first-referrer) was deliberately not touched here. One
hat: this is a rebase.
