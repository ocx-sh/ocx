# ADR: Exec-Time Resolution Record

## Metadata

**Status:** Accepted
**Date:** 2026-07-26 (accepted 2026-07-27, implemented in PR [#238](https://github.com/ocx-sh/ocx/pull/238))
**Deciders:** @michael-herwig, architect
**GitHub Issue:** [#214](https://github.com/ocx-sh/ocx/issues/214) — "Managed configuration option to always log digest when package is invoked"
**Related issues:** [#178](https://github.com/ocx-sh/ocx/issues/178) (JSON shape stability), [#177](https://github.com/ocx-sh/ocx/issues/177) (`--bins` listing), [#199](https://github.com/ocx-sh/ocx/issues/199) (SBOM/provenance tracking), [#213](https://github.com/ocx-sh/ocx/issues/213) / [#220](https://github.com/ocx-sh/ocx/issues/220) (root-flag grammar)
**Tech Strategy Alignment:**
- [x] Follows Golden Path — Rust 2024, Tokio, `serde_json`, no new runtime dependency
**Domain Tags:** security, api, integration, devops
**Scope:** Medium · **One-Way-Door: HIGH** — the record schema, the sink filename grammar, the `OCX_*` variable name and the managed key all become wire contract the moment a fleet consumes them. Landing before 1.0 makes the schema effectively permanent.

---

## Context

A corporate user ([#214](https://github.com/ocx-sh/ocx/issues/214)) must prove that the outputs of a batch job were produced by **approved package versions**. Today they do it with wrapper scripts that log metadata alongside controlled systems. OCX already resolves every package to an immutable digest — the datum they need exists in memory at the moment of invocation and is then discarded.

Their environment, from the issue thread:

- Batch execution cluster, **fresh user profile per job**, no user config, no project config.
- Therefore ~all usage is **`ocx package exec <tool>`** — the OCI tier, no `ocx.toml`, no `ocx.lock`.
- Wrapper scripts are the current control point, and *that is the problem*: a per-invocation flag would have to be trusted to every wrapper, which is exactly the status quo they want to leave.
- They asked for the mechanism to be settable by **environment variable** (their machines are provisioned that way) and enforced by the **managed tier** (so callers cannot opt out).

### Why pull-time state is not enough

`ocx --format json pull` already prints digests, and `ocx.lock` guarantees `pull` and `run` resolve identically. But that binds *pull-time* state. Between pull and exec:

- config or index can change;
- a floating tag can move (the OCI tier has no lock at all);
- `run` / `package exec` **auto-install missing packages on demand**, so an exec can materialise a package that no prior `pull` ever saw.

The gap is real, not theoretical, and it is widest in exactly the reporter's configuration (OCI tier, no lock).

### Why now

OCX targets 1.0 at end of 2026. This feature introduces a **persisted wire format**, which under the repo's own stability tiers is a real contract — unlike CLI internals. Getting the schema right matters more than getting the flag name right, and it should land inside the [#178](https://github.com/ocx-sh/ocx/issues/178) "stable-within-minor" declaration rather than becoming a fourth undeclared JSON shape.

---

## Decision Drivers

| # | Driver |
|---|---|
| **D1** | **Compliance-grade, not decorative.** The caller must not be able to disable what the operator requires. A record that silently fails to write is worse than no feature. |
| **D2** | **Backend-first (Product Principle 1).** Machine-readable; a *file*, never stdout/stderr — those belong to the invoked tool. |
| **D3** | **Complete coverage.** Any path that starts a tool must record, including the generated-launcher re-entry. A gap is a compliance hole, not a missing nicety. |
| **D4** | **No fork of the process model.** `execvp(2)` replacement stays. Reporting must not silently turn ocx into a supervisor process. |
| **D5** | **Reuse vocabulary, don't invent it.** Package entries should lift into SLSA/in-toto `resolvedDependencies` without a translator. |
| **D6** | **Corporate/air-gapped reality.** The sink will land on a shared network filesystem. Concurrency must be correct *there*, not just on ext4. |
| **D7** | **Extend, don't duplicate.** One seam (`Env::apply_child_env`), one data source (`InstallInfo`), existing file-sink precedent (`export_ci`), existing policy tier (`[managed]`). |

---

## Industry Context & Research

**Trending approach:** per-invocation machine-readable records written to a file, with an in-band schema version, and package identity expressed as *URI + digest*.

| System | Mechanism | Sink | Schema versioning |
|---|---|---|---|
| [pip](https://pip.pypa.io/en/stable/reference/installation-report/) | `pip install --report FILE` — full resolution report, works with `--dry-run` | one file per invocation | in-band `"version": "1"` string, documented as the compat contract |
| [PEP 710](https://peps.python.org/pep-0710/) / PEP 610 | `provenance_url.json` / `direct_url.json` written into the installed dist | one file per installed unit | file-name-scoped |
| [Bazel](https://bazel.build/docs/user-manual) | `--execution_log_json_file`, `--build_event_json_file` | one file per invocation (Bazel is the single coordinator, so no cross-process contention) | protobuf schema evolution |
| [LLVM](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html) | `LLVM_PROFILE_FILE` with `%p` (pid), `%h` (hostname), `%t` (TMPDIR), `%Nm` (signature pool), `%c` (continuous) | one file per **process**, path templated | binary format version |
| [SLSA v1](https://slsa.dev/spec/v1.0/provenance) / in-toto | `resolvedDependencies: [ResourceDescriptor]` — `name`, `uri`, `digest{sha256,…}`, `annotations` | attestation envelope | `predicateType` URI |
| [in-toto `runtime-trace` v0.1](https://github.com/in-toto/attestation/blob/main/spec/predicates/runtime-trace.md) | closest-named predicate — but it is **syscall-level tracing** (`monitoredProcess`, `monitorLog.{process,network,fileAccess}`, Tetragon/Tekton use case), not environment composition. v0.1, unresolved `FIXME`s, no adoption beyond the reference impl | n/a | draft |
| Kubernetes | resolved image digest recorded as `status.containerStatuses[].imageID` at run time | API object | API version |

**Key insights**

1. **Everyone writes a file.** No mainstream tool emits provenance on stdout when a child process owns stdout. This settles the sink medium.
2. **Everyone versions the schema in-band, and bumps it only for breaks.** pip's `"version"` is a top-level **string** documented as changing *"only if and when backward incompatible changes are introduced"* — no minor churn, no additive bumps. Adopt that discipline verbatim.
3. **Sink shape splits by concurrency model.** Bazel is a single coordinator → one file per invocation is trivially safe. LLVM instruments *many concurrent processes* → `%p`/`%h`/`%Nm` templating, with the docs stating the rationale outright: the specifiers exist *"to avoid corruption due to concurrency."* OCX is the LLVM case, not the Bazel case: a batch cluster runs thousands of concurrent `ocx package exec` calls against one directory.
4. **There is no portable, spec-backed guarantee for concurrent append — anywhere.** [POSIX](https://pubs.opengroup.org/onlinepubs/9699919799/functions/write.html) guarantees non-interleaved writes only for **pipes/FIFOs** at ≤ `PIPE_BUF`; for regular files under `O_APPEND` it guarantees only that the seek-to-end and the write are one operation — *not* that the write is atomic with respect to other writers. The widespread "O_APPEND is PIPE_BUF-safe on regular files" belief is folklore, not spec. [NFS is strictly worse](https://nullprogram.com/blog/2016/08/03/): the protocol has no append opcode, so the client simulates it by reading the size then writing. On Windows, `FILE_APPEND_DATA` *is* a real atomic-append primitive, but the CRT's `O_APPEND` emulation — the path Rust's `OpenOptions::append(true)` may take — does not inherit that guarantee. Bazel and LLVM both independently chose per-invocation filenames rather than solve this. **This is the decisive constraint on the sink.**
5. **No standard predicate exists for runtime environment composition.** SLSA, in-toto's predicate list, CycloneDX "formulation" and SPDX 3.0's build profile all describe *build* provenance; `runtime-trace` describes syscalls. Recording "what composed this execution" is unclaimed. Conclusion: OCX owns the **predicate concept and envelope**, and borrows the **vocabulary** (`ResourceDescriptor`, `Statement`) so the payload lifts into an attestation later ([#198](https://github.com/ocx-sh/ocx/issues/198), [#102](https://github.com/ocx-sh/ocx/issues/102)) with no reshaping.
6. **Nobody in the tool-manager space does this at all.** mise, asdf, proto, Homebrew: nothing. npm, conda, Nix, Guix, Spack and Docker all use the same *persisted-queryable-state* model — lockfile / `conda-meta` / store path / `RepoDigests`, queried after the fact — and emit no per-invocation artifact. pip's per-*install* report is the only real precedent in the category, and it is not per-exec. This is a genuine differentiator for a self-described backend tool.
7. **Two cautionary tales worth designing against.** (a) Bazel's `--execution_log_json_file` emits newline-delimited proto that is *not* a valid JSON document — an accident, now a [long-lived bug](https://github.com/bazelbuild/bazel/issues/14209). Choose one-document-per-file or a JSON array **deliberately**. (b) Kubernetes' `imageID` has been format-inconsistent across CRI implementations for a decade (`docker-pullable://` prefixing, manifest digest vs platform digest), still being unwound via a dedicated CRI field. **Define the digest format once, precisely, and never let a transport prefix into it.**
8. **Conda splits the job across two mechanisms** — an append-only `history` log of *commands* plus `conda list --explicit` as an on-demand query. Two half-answers. Avoid: one record, self-contained.

---

## Findings From the Current Code

These are the nuances that shape every option below. Each verified by reading the source.

| # | Finding | Consequence |
|---|---|---|
| **F1** | `child_process::exec` calls `execvp(2)` on Unix — the ocx image is *replaced* (`crates/ocx_lib/src/utility/child_process.rs:96`). On Windows it spawns+waits then `process::exit`s, deliberately skipping the Drop chain for symmetry. | **No post-exec hook exists.** No exit code, no duration, no completion event — unless reporting mode switches to spawn+wait, which would change PID, signal forwarding and job control *only in that mode*. The record is therefore a **pre-exec resolution record**, not an execution result. |
| **F2** | `composer::compose` returns `ComposeOutput { entries: Vec<Entry>, admitted: Vec<PinnedIdentifier>, admitted_binaries: Vec<(PinnedIdentifier, BinaryName)>, admitted_entrypoints: Vec<(PinnedIdentifier, EntrypointName)> }` (`crates/ocx_lib/src/package_manager/composer.rs:65-94`). `Entry { key, value, kind }` itself carries no source. | **Per-*entry* attribution does not exist; per-*package* attribution does — and better than expected.** The record must not be derived from `entries`, but `admitted_binaries` / `admitted_entrypoints` hand it the **claimed executable names per package** for free. That is the same datum [#177](https://github.com/ocx-sh/ocx/issues/177) (`--bins`) is trying to expose, already computed at this point. |
| **F3** | `InstallInfo { identifier: PinnedIdentifier, metadata, resolved: ResolvedPackage, dir: PackageDir, platform: Option<Platform> }` (`crates/ocx_lib/src/package/install_info.rs:9`). `PinnedIdentifier` **always** carries a digest by construction (`crates/ocx_lib/src/oci/pinned_identifier.rs:24`). `ResolvedPackage { dependencies: Vec<ResolvedDependency> }` is the transitive closure in topological order, each `{ identifier: PinnedIdentifier, visibility }` (`resolved_package.rs:16`). | The payload already exists, fully resolved, root **and** closure. No new resolution work, no extra I/O, no network. |
| **F4** | `Env::apply_child_env(ChildEnv { composed, forwarded }, config)` is documented as the mandatory seam: *"Every command that composes an environment and then spawns must go through this seam"* (`crates/ocx_lib/src/env.rs:452`). All three exec paths use it. | **One emit point, not three.** Adding a third field to `ChildEnv` makes the record structurally impossible to forget in a future command. |
| **F5** | `ocx launcher exec` receives a **package root path**, not an identifier, and `install_info_from_package_root` mints a *synthetic* `file-url-mode/<content-digest>` identifier because package directories are content-shared and carry no root registry/repository (`crates/ocx_cli/src/command/launcher/exec.rs:67`, `package_manager.rs:529`). | At the launcher, **logical identity (registry/repo:tag) is not recoverable** — only the content digest. A launcher-only record is digest-complete but name-degraded. This is the hardest nuance in the design. |
| **F6** | `ocx package exec cmake -- cmake …` composes the env, puts the package's `entrypoints/` on PATH, then `resolve_command` finds the *generated launcher*, which re-enters ocx as `ocx launcher exec` (`exec.rs:126`, and the comment at `launcher/exec.rs:85`). | **Double-recording is the default behaviour** unless suppressed. Precedent for the fix already exists: `OCX_ENV` uses a strict set-or-remove forwarding discipline so a stale value can never reach a child (`env.rs:402-409`). |
| **F7** | `InstallInfo::platform` is `Option<Platform>` — `None` on paths built without platform context (`find_symlink`, composer fixtures) (`install_info.rs:14-21`). | The record's platform field must be **nullable**, never fabricated from the host. Fabricating it would make the record lie in exactly the audit that matters. |
| **F8** | `export_ci(provider, export_file, entries)` in `crates/ocx_cli/src/conventions.rs:206` is the existing "write machine output to a caller-named file" precedent, already reached from two commands (`--export-file` on `ocx env` / `ocx package env`). | The file-sink pattern is established; do not invent a second one. |
| **F9** | `$OCX_HOME` top-level stores are `blobs/ layers/ packages/ index/ symlinks/ state/ temp/` + `locks/` (`file_structure.rs:52`). `state/` holds `update-check/` and `managed-config/` (`state_store.rs:56,96`). `utility::fs::persist_temp_file` is the single atomic-publish primitive. | A sink **inside** `$OCX_HOME` would be wrong — records are output the operator collects, not ocx runtime state, and `$OCX_HOME` is per-user while the audit trail is fleet-wide. The sink is caller-designated. |
| **F10** | `Env::apply_ocx_config` forwards a fixed set of resolution-affecting `OCX_*` vars, each with an explicit set-**or**-remove branch (`env.rs:352-420`). `OCX_ENV` is **unconditionally removed** there (`env.rs:409`) before the invocation that owns a payload writes it back. | A new sink variable must join that list with the same discipline, or a child ocx silently inherits a stale sink. The unconditional-strip-then-rewrite pattern is the exact model for the re-entry marker. |
| **F11** | `ocx.lock` V3 stores `LockedTool { name, group, repository: Identifier (bare — tag/digest **rejected** by `validate()`), platforms: BTreeMap<String, Digest> }` (`crates/ocx_lib/src/project/lock.rs:139-175, 333-363`). | A project-tier record has **no tag to report** — the lock is digest-only by construction. Only the OCI tier (`package exec`, user-typed identifier) can carry a meaningful tag. The record must not invent one. |
| **F12** | `ResolvedPackage`'s doc states the root package's own identifier is deliberately **not** persisted in `resolve.json` — it "would couple the identity of a shared, deduplicated package directory to whichever installer won the cross-repo race" (`resolved_package.rs:21-28`). | Confirms F5 from the other side: the logical name is genuinely absent on disk, not merely unloaded. **But** `symlinks/{registry}/{repo}/candidates/{tag}` points *at* that package root — a reverse lookup could recover a plausible name. See option L2b. |
| **F13** | The record write needs **no lock**: a create-exclusive file in a directory sink has no shared offset to contend for. | Avoids adding an `await`ed lock acquisition between "env resolved" and "process replaced" — an I/O step that does not exist on this path today. |
| **F14** | The Unix launcher body is byte-exact `exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<root>' -- "$(basename "$0")" "$@"` (`package_manager/launcher/body.rs:153`), and `child_process::exec` is `execvp(2)`. Every hop replaces the image. On Windows, `child_process::exec` spawns, **waits**, then `process::exit`s (`child_process.rs:107-117`) — so `Child::id()` is available. | **PID semantics differ by platform and must be made uniform in the record, not left to the reader.** See "PID semantics" below. This is why no correlation ID is minted: the OS handle is good enough on both platforms once the field means the same thing on each. |
| **F15** | `Env::resolve_command` runs **after** `apply_child_env` and resolves `argv[0]` against the composed `PATH` (`exec.rs:126`, `run.rs`). | The resolved absolute executable — the record's highest-value field — does not exist at the `apply_child_env` seam. The emit must be a separate call after resolution. Also: on the outer frame this resolves to the **launcher**, which is what forces L1 over L2. |

---

## Considered Options

### 1. Sink shape

#### Option S1 — Append-only JSONL to a caller-named file

`--record-file /var/log/ocx/exec.jsonl`; every invocation appends one line.

| Pros | Cons |
|---|---|
| One file, trivially greppable, `wc -l` = invocation count | **No portable guarantee exists** (research §4): POSIX promises non-interleaving only for pipes, NFS has no append opcode at all, and the Windows CRT's `O_APPEND` does not inherit `FILE_APPEND_DATA`'s atomicity. The reporter's cluster is the exact hazard case |
| No filename grammar to freeze as a contract | To make it safe we would have to own a locking protocol — new code on the hot path, for a guarantee S2 gets for free (F13) |
| Familiar (`auditd`, syslog) | Rotation is the operator's problem, and ocx cannot help without owning a rotation policy |

#### Option S2 — Directory sink; ocx names the file (**recommended**)

`--records-dir /var/log/ocx/records`; each invocation writes one self-named file.

| Pros | Cons |
|---|---|
| Correct on **every** filesystem — create-exclusive, no shared-offset writes | Consumer must read a directory rather than tail one file |
| Uses the existing atomic-publish primitive (`persist_temp_file`) unchanged | Directory can grow large; operator owns collection/rotation (but that is true of S1 too) |
| Filename carries correlation keys (time, pid, host) without a template mini-language | One more concept than "a file path" |
| Collection is a `mv`/`rsync` of whole files — never a partial line | |

#### Option S3 — Templated file path (`%p`, `%h`, `%u`)

`--record-file '/var/log/ocx/%h-%p.json'`, LLVM-style.

| Pros | Cons |
|---|---|
| Proven at scale by LLVM's profile runtime | **Freezes a substitution grammar as a 1.0 wire contract** — every specifier is forever |
| Maximum operator control over layout | Users will template themselves into collisions (`%h` alone) and blame ocx |
| | Strictly more surface than S2 for the same guarantee |

---

### 2. Record scope

| Option | Description | Verdict |
|---|---|---|
| **R1** | Root packages only | Rejected — a dependency's binaries are on `PATH` too. "Which versions produced this output" is unanswerable without the closure. |
| **R2** | Root packages **+ full transitive closure**, each tagged `root` / `dependency` | **Chosen.** `ResolvedPackage.dependencies` (F3) already holds it in topological order with visibility, at zero cost. |

### 3. Configuration layering

| Option | Description | Verdict |
|---|---|---|
| **P1** | Flag only | Rejected — reproduces the reporter's status quo (trust every wrapper). |
| **P2** | Flag + `OCX_*` env var | Necessary (their machines are provisioned by env) but insufficient: a caller can `unset` it. |
| **P3** | Flag + env + `[records]` config (managed tier included), resolved by **one four-layer fold**, with a `system_locked` clamp | **Chosen.** Ordinary precedence for ordinary hosts; the compliance override is one explicit branch reusing the existing `system_locked` mechanism (`config/managed.rs:52-64`). |

```
default ▸ config (managed folded in) ▸ env ▸ CLI      — one fold, every field
                                                      then: system_locked clamp
```

Full struct, function and clamp in "Configuration surface" below. The clamp is deliberately *outside* the fold: a rule embedded in the merge would make every field's precedence conditional and unreadable, and the project already models "SYSTEM scope wins unconditionally" as a separate marker rather than a merge rule.

### 4. Failure posture

| Option | Description | Verdict |
|---|---|---|
| **W1** | Best-effort: warn on stderr, exec anyway | Rejected under policy. An audit trail with silent holes is worse than none, because it *looks* complete. |
| **W2** | Fail closed: unwritable sink ⇒ exit `74` (`IoError`), child never starts | **Chosen** whenever reporting is active. Matches the managed tier's own `required = true` fail-closed posture (ADR `adr_managed_config_tier.md` §E). |

Deliberate consequence: a full disk or a bad mount stops the job. That is the correct behaviour for a compliance control, and it is what "approved versions or nothing" means. Operators who disagree set `required = false`.

### 5. Launcher re-entry (from F5/F6)

The two frames record **different facts**, and this is the finding that decides the option. When `ocx package exec cmake -- cmake …` resolves the command against the composed `PATH`, it hits the package's `entrypoints/` directory first — so the **outer** frame's resolved executable is the *launcher shim*, not `cmake`. Only the **inner** `ocx launcher exec` frame ever sees the real leaf binary. They are complementary halves, not duplicates:

| Frame | Has | Lacks |
|---|---|---|
| Outer (`run` / `package exec`) | full logical identity (purl, registry, repo, tag, digest), composed env, closure, what the user asked for | the leaf binary — its `executable` is the launcher |
| Inner (`launcher exec`) | **the actual leaf binary that ran** | logical identity — synthetic `file-url-mode/<digest>` only (F5/F12), so it cannot even emit a valid purl |

| Option | Description | Verdict |
|---|---|---|
| **L1** | Record at every frame; **join on the package's content digest**, which both frames already carry | **Chosen.** Two records per entrypoint invocation is the correct answer, not a defect: together they answer "which package, and which binary out of it, actually ran". |
| **L2** | Record only at the outermost frame, forward a suppression marker | **Rejected** (reversed from the first draft). Suppression discards the leaf binary — the single field the audit exists to capture. It also costs a new internal `OCX_*` forwarding channel on a surface already carrying 13+ (see "Related concern" below). |
| **L2b** | Reverse-look-up the logical name at a direct launcher invocation by scanning `symlinks/{registry}/{repo}/candidates/*` for a link resolving to this package root | **Open, low priority.** Under L1 the outer record already supplies the name in the common case, so this only helps a *direct* shim invocation with no ocx parent. Costs a directory walk on the exec path and the answer is not unique (a content-shared root may be reachable from several repos). If ever taken, annotate `sh.ocx.identity: "recovered"` — never authoritative. |
| **L3** | Record only at the launcher | Rejected — misses `run` / `package exec` of a non-entrypoint binary entirely, and loses logical identity everywhere. |

**Linkage needs no new state.** Both frames carry the package's content-addressed store path, which *is* a digest. Join on it. Timestamps order the frames; on Unix the PID is identical across them anyway, because the whole chain is `exec`-based (see F14). No minted correlation ID, no environment propagation, no parent pointer — deliberately. We are recording invocations, not modelling a process graph.

A **direct** launcher invocation with no ocx parent (a user calls the shim from `PATH` after `ocx env`) still records, with `"identity": "degraded"`, no purl, and a digest-only package entry. Better a truthful partial record than a fabricated complete one — F12 shows the omission is structural: ocx deliberately does not persist the root identifier next to a content-shared package.

---

## Decision Outcome

**Chosen: S2 + R2 + P3 + W2 + L1.**

A **pre-exec resolution record**: one JSON file per ocx frame that launches something, written to a caller- or operator-designated directory, containing the full resolved package closure with digests plus the resolved executable, emitted immediately before `child_process::exec`, fail-closed, with the managed tier acting as a floor no caller can lower.

**Rationale.** S2 is the only sink that is correct on the filesystem the reporter will actually use, and the only one that adds no frozen grammar to the 1.0 surface. R2 costs nothing and is the difference between an answer and half an answer. P3 is the literal ask in the issue. W2 follows the managed tier's existing posture. L1 keeps the leaf binary, which suppression would have thrown away.

**Explicitly not in the design**, after being considered and dropped:

- **No minted execution ID.** The record's filename is its identity. Nothing needs that name inside the child process.
- **No new environment propagation.** Not for correlation, not for suppression. The forwarding surface into child ocx invocations is already 13+ variables; this feature adds zero.
- **No parent/child linkage field.** Records join on the content digest they already carry. We are not modelling an invocation graph.
- **No exit code, no duration, no completion event.** Structurally impossible on Unix (F1) and deliberately not faked on Windows, where it *would* be available — divergent per-platform record content is a wire-format defect, not a feature.
- **No in-toto `Statement` wrapper.** Researched and rejected for v1: it unlocks no consumer without also shipping signing, a subject convention and registry attachment. Additive later. See "Why there is no in-toto Statement in v1".
- **No OTel/ECS vocabulary adopted wholesale.** The two conflict structurally and no pipeline auto-recognises either; names are chosen per field on cost. See "The process/host/os split".

---

## Technical Details

### Architecture

```
ocx run ──┐
ocx pkg   ├─ resolve packages ──► Vec<Arc<InstallInfo>>   (F3: identity + digest + closure)
  exec    │                       ComposeOutput           (F2: admitted_binaries/entrypoints)
ocx       │                       Vec<Entry>              (F2: per-entry provenance absent)
 launcher─┘                                 │
   exec                                     ▼
                        Env::apply_child_env(ChildEnv{composed, forwarded}, config)
                                            │                    (F4 — UNCHANGED, no new field)
                                            ▼
                              env.resolve_command(argv[0])
                                            │
                                            ▼
                              resolved: PathBuf  ◄── the leaf executable; exists ONLY here
                                            │
                                            ▼
                        records::emit(&sink, &policy, RecordInputs{
                            install_infos, compose_output, resolved, argv, config,
                        })
                        ├─ build ExecutionRecord
                        ├─ NamedTempFile in sink dir      (no lock needed — F13)
                        └─ persist_temp_file(…) ── atomic ─┐
                                                           ▼
                                          <sink>/<utc>-<pid>-<rand>.json
                                                           │
                                                           ▼
                                     child_process::exec()  ← execvp, never returns (F1)
```

**Why the emit is a sibling call at three sites, not a field on `ChildEnv`.** The first draft put it inside `apply_child_env`, which would have guaranteed no future command could forget it. That is impossible: `Env::resolve_command` runs *after* `apply_child_env` (it resolves against the env that call just built), and the resolved executable is the record's highest-value field. So the record cannot be assembled at the `ChildEnv` seam. `ChildEnv` stays exactly as it is; the emit is three call sites, and the plan owes an acceptance test that all three produce a record.

### On-disk placement — what lands where

```
$OCX_HOME/                                   # unchanged; NO record state lives here
├── blobs/ layers/ packages/ index/ temp/ locks/
├── symlinks/{registry}/{repo}/candidates/   # ← untouched unless option L2b is ever taken
├── projects/                                # flat GC-ledger symlink store (not a FileStructure field)
├── config.toml                              # [managed] seed pointer  (unchanged)
└── state/
    ├── update-check/{slug}                  # zero-byte throttle markers
    ├── patch-descriptors/{registry}/{repo}.json
    └── managed-config/
        ├── snapshot.json                    # metadata: source, tag, digest, fetched_at
        └── config.toml                      # ← operator payload; gains the [records] section
                                             #   (delivered, not authored, on the host)

<project>/                                   # unchanged; records are NOT project artifacts
├── ocx.toml
└── ocx.lock                                 # hash recorded IN the record, file untouched

<sink>/                                      # operator-designated; outside $OCX_HOME by design (F9)
│                                            # e.g. /var/log/ocx/records, or $JOB_SCRATCH/ocx
├── 20260726T140311482Z-48123-9f3a1c07.json  # ocx package exec cmake  → outer frame (launcher)
├── 20260726T140311494Z-48123-1d4b7e22.json  # ocx launcher exec       → inner frame (leaf binary)
│                                            #   ↑ same pid on Unix (exec chain, F14);
│                                            #     joined by package digest on every platform
└── 20260726T140312771Z-48131-c0d54a19.json  # an unrelated concurrent invocation

     default filename grammar (STABLE — part of the contract):
     <utc-basic-ms>-<pid>-<8 hex random>.json
      │              │     └─ collision break for same-pid-same-ms across hosts/containers
      │              └─ the owning process (F14) — on Unix this IS the tool's pid
      └─ ISO-8601 basic, UTC, millisecond — sorts lexicographically = chronologically
```

**Overridable filename, at every layer.** The default above is what a site gets for free; `name` is settable from config, `OCX_RECORDS_NAME`, or `--records-name` like any other field (see the fold below). It accepts a template over a **closed** placeholder set so a site can match its own collection convention:

| Placeholder | Expands to |
|---|---|
| `{time}` | `20260726T140311482Z` — same basic UTC form as the default |
| `{pid}` | the owning process id (F14) |
| `{rand}` | 8 hex characters |
| `{host}` | hostname |

**Amended 2026-07-27.** The original text of this section stated "every placeholder is ocx-generated, and that is the selection rule" — false the moment `{host}` shipped. The corrected rule: **a placeholder is in the set only if it is cheap to make safe as a filename.** `{time}`, `{pid}` and `{rand}` clear that bar because they are ocx-generated and safe by construction. `{host}` is the one placeholder that is *not* ocx-generated — a hostname is read from the environment and may legitimately contain a `/` — but it still clears the corrected bar cheaply: `sanitize_host` reduces it to the project's relaxed slug (`[A-Za-z0-9._-]`, everything else becomes `_`) and drops a result that is dots-only, exactly as an undeterminable hostname already expands to (`crates/ocx_lib/src/record/name_template.rs:195`). A `{command}` placeholder was considered and rejected under the corrected rule too, not the retired one: the invoked command is user-controlled and unbounded — path separators, `..`, spaces, unicode, length — so making it filename-safe is real sanitizer surface for cosmetic value, when the command is already in the record (`process.executable`) and one `jq` away.

Closed set, and an **unknown placeholder is a config parse error, never a silent literal** — the failure mode of a silently-unexpanded `{jobid}` is a directory of identically-named files, discovered during an audit. Deliberately not LLVM's `%Nm`-style grammar: those specifiers carry *behaviour* (profile-merge pools, continuous sync), and behaviour in a filename is a contract we would regret. These are substitutions only.

Rationale for each placement decision:

- **Not under `$OCX_HOME`** — records are *output the operator collects*, not ocx runtime state; and `$OCX_HOME` is per-user while the audit trail is fleet-wide. Placing them in `state/` would also make them GC-adjacent, which they must never be.
- **Not in the project directory** — the reporter's jobs have no project. Project-tier runs record the `ocx.lock` digest *inside* the record instead.
- **Sink is a directory, never a file** — see S1 vs S2. The one guarantee ocx can make on any filesystem is "this whole file appeared atomically".

### Configuration surface

```toml
# $OCX_HOME/config.toml, or the managed tier's payload — same section either way
[records]
dir      = "/var/log/ocx/records"        # sink; absent ⇒ recording off
name     = "{time}-{host}-{pid}.json"    # optional; default <time>-<pid>-<rand>.json
required = true                          # fail closed (exit 74) if a record cannot be written
```

```sh
# sink and filename are settable at every layer; posture is not
ocx run --records-dir /var/log/ocx/records --records-name '{time}-{pid}.json' -- cmake --build build
OCX_RECORDS_DIR=/var/log/ocx/records OCX_RECORDS_NAME='{time}-{host}.json' ocx package exec cmake:3.28 -- cmake --version
```

#### Precedence: one folding struct, one resolve function

Every field resolves through the same four-layer fold — **default ▸ config ▸ env ▸ CLI**, highest last. No per-field special-casing, no second scheme.

```rust
/// Partial view of `[records]` from one layer. Every field `Option` so a layer
/// that says nothing cannot clobber a lower layer that did.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RecordsOptions {
    pub dir: Option<PathBuf>,
    pub name: Option<String>,      // filename template, closed placeholder set
    pub required: Option<bool>,    // config layer only — no env, no flag
}

impl RecordsOptions {
    /// Merge `other` into `self`; `other` is the higher-precedence layer.
    /// Mirrors `Config::merge` (`crates/ocx_lib/src/config.rs:125`) exactly —
    /// `Some` wins, `None` never clobbers.
    pub fn merge(&mut self, other: Self) {
        if other.dir.is_some()      { self.dir = other.dir; }
        if other.name.is_some()     { self.name = other.name; }
        if other.required.is_some() { self.required = other.required; }
    }
}

/// Fully resolved form — defaults applied, template parsed, no `Option` left.
pub struct ResolvedRecords {
    pub dir: PathBuf,
    pub name: NameTemplate,
    pub required: bool,
}

/// THE resolution function. Every caller goes through this; nothing else
/// reads `[records]`, `OCX_RECORDS_*`, or the flags directly.
pub fn resolve_records(
    config: RecordsOptions,   // merged config tiers (managed already folded in)
    env:    RecordsOptions,   // OCX_RECORDS_DIR / _NAME
    args:   RecordsOptions,   // --records-dir / --records-name
    system_locked: bool,      // SYSTEM-scope policy present
) -> Result<Option<ResolvedRecords>, RecordsError> {
    if system_locked {
        return finish(config);                    // whole block is the operator's
    }
    let mut merged = RecordsOptions::default();   // layer 1: defaults
    merged.merge(config);                         // layer 2
    merged.merge(env);                            // layer 3
    merged.merge(args);                           // layer 4 — CLI wins
    finish(merged)
    // `dir` absent ⇒ recording off ⇒ Ok(None). `name` defaults to the standard
    // grammar. Template parsed in `finish`, so an unknown placeholder is a
    // config error at resolve time, never a surprise filename at write time.
}
```

Returning `Option<ResolvedRecords>` makes "recording is off" a type-level state rather than a `dir.is_empty()` convention at each of the three emit sites.

| Field | Default | `[records]` | Env | Flag |
|---|---|---|---|---|
| `dir` | *(absent ⇒ recording off)* | `dir` | `OCX_RECORDS_DIR` | `--records-dir DIR` |
| `name` | `{time}-{pid}-{rand}.json` | `name` | `OCX_RECORDS_NAME` | `--records-name TEMPLATE` |
| `required` | *(see posture below)* | `required` | — | — |

`OCX_RECORDS_DIR` and `OCX_RECORDS_NAME` join `Env::apply_ocx_config`'s forwarded set with the standard set-**or**-remove discipline (F10). They are user-facing settings with flag and config peers, not internal channels — this feature adds **zero** internal forwarding channels.

#### `required` is config-only, deliberately

There is no `--records-required` and no `OCX_RECORDS_REQUIRED`. Recording posture is an operator decision, not a per-invocation one, and a "strengthen-only" flag would be a flag whose only legal value is already the default — it exists to be typed and do nothing. YAGNI.

#### The lock is an integrity control, not a security boundary

Stated plainly because the distinction governs how much machinery this deserves: **the SYSTEM lock protects against error, not malice.** There is no containment story — anyone who can pass `--records-dir` can equally just not run ocx, or run a different binary. No threat model has been written for this feature, and without one, "cannot be defeated" language would be a false assurance someone later relies on.

What it *does* buy is real: a well-meaning wrapper script cannot accidentally redirect records and silently break the operator's collection pipeline. Guarding against accident is a legitimate reason to keep it. It is not a reason to describe it as a security control, and this ADR does not.

#### The lock is binary and per-block, not per-field

A plain four-layer fold lets a caller accidentally break the collection contract — and **not only via `required`**. `name` is part of that contract too: if an operator's collector globs or parses filenames, a caller changing the pattern breaks it exactly as redirecting `dir` does. Per-field clamps were the wrong shape.

The hard part is that **ocx cannot tell an operator from a user on the env or CLI layer.** `OCX_RECORDS_DIR` baked into a machine image and `OCX_RECORDS_DIR` typed in a shell are the same variable with the same value and no distinguishing signal. "Env is the operator channel" is a convention, not something ocx can enforce.

**One channel is unambiguous:** `/etc/ocx/config.toml` requires root to write (`config/loader.rs:717-719`), and the loader already marks it non-overridable via `lock_as_system` — the same mechanism `[managed]` and `[patches]` use (`config/managed.rs:52-64`).

| | `dir` | `name` | `required` |
|---|---|---|---|
| **No SYSTEM policy** | config / env / CLI, free | config / env / CLI, free | config only |
| **SYSTEM policy present** | **locked** | **locked** | **locked** |

Applied as one clamp after the fold, never a rule inside it:

```rust
if system_locked {
    merged = config_from_system_scope;   // whole block, not field by field
}
```

The reasoning: with no operator policy the sink is *yours* — set it however you like, filename pattern included. The moment an operator declares one at SYSTEM scope, the whole block is theirs, because a collector downstream now depends on all of it. One rule instead of three clamps, and it makes "should `dir` be locked too?" moot rather than arbitrated.

#### Failure posture follows the lock

| State | Record cannot be written |
|---|---|
| **SYSTEM-locked** | Fail closed — exit `74`, child never starts (on Windows, via the pre-spawn probe). |
| **Unlocked** | Warn on stderr, run anyway. |

A developer who typed `--records-dir` once and fat-fingered the path should not have their build die for a policy nobody set. Fail-closed is a *policy* posture, which is exactly why `required` lives with the policy.

##### Two ways the posture was silently defeated (amended 2026-07-28)

Both were found by a max-tier review of the implementation, both silent, both untested, and both are the same defect as the launcher exemption above: the guarantee was specified as a property of the config *merge* and every hole was upstream of the point where posture is applied.

1. **`required = true` with no `dir` resolved to a policy that records nothing.** The launch path early-returns on `!policy.is_recording()` *before* `apply_posture` runs, so a SYSTEM file containing only `[records] required = true` — the plainest way an operator writes "recording is mandatory" — gave every child on the fleet an unrecorded run, exit 0, no warning. Now a typed `RecordsError::RequiredWithoutSink` → exit **78** at `resolve_records`, matching the `SinkSymlink` precedent (a configuration fault surfaces before the work, not as an I/O one during it). The trigger is an **explicit** `required = true` at some tier, never the value `required` resolves to from the SYSTEM-lock default: a locked `[records]` block with no `dir` is plausibly an operator locking recording *off* for the host, and that keeps working. `RecordsOptions::required` is `Option<bool>` and already carries exactly that distinction.

2. **A symlinked or unreadable `/etc/ocx/config.toml` dropped the locked block with a warning.** `ConfigLoader::existing_candidates` filtered the SYSTEM candidate with the same best-effort semantics as the user tiers. An operator symlinking `/etc/ocx/config.toml` at a config-management–owned fleet file — an ordinary move — took the whole fleet out of recording, silently. Now fatal for the **SYSTEM candidate only** (exit 78, `config::error::Error::SystemConfig`), including on the `OCX_NO_CONFIG` path, which runs the same check over that one candidate; `NotFound` stays a silent skip, and the user/`$OCX_HOME` tiers keep best-effort discovery. This is broader than `[records]` — it protects every `lock_as_system` section — which is why it lives in the loader rather than in this feature.

#### Why `[records]`, plural

The existing config surface is not uniformly plural — the rule is shape-dependent, and `[patches]` is this feature's exact structural twin:

| Section | Shape | Name |
|---|---|---|
| `[registries.<name>]`, `[mirrors]` | map keyed by name/host | plural |
| **`[patches]`** | **settings struct governing a plural domain concept, with a posture flag** | **plural** |
| `[managed]`, `[clean]`, `[registry]` | settings struct named for an adjective, a verb, or a subsystem | singular |

`[records]` is a settings struct governing a plural domain concept (many record files) with a `required` posture flag — row two, exactly. Flag and env var follow: `--records-dir`, `OCX_RECORDS_DIR`.

> **Naming is still an owner decision.** `--audit-dir` overpromises (this is not a general audit log); `--exec-report` collides with pip's "report" = *resolution plan*, a different thing. Whatever is chosen is permanent from 1.0.

### Related concern (out of scope, worth its own issue)

`Env::apply_ocx_config` forwards **13+** internal channels into every child ocx invocation: `OCX_ENV`, `OCX_PATCHES`, `OCX_PATCH_SNAPSHOT`, `OCX_MIRRORS`, `OCX_MANAGED_CONFIG`, `OCX_BINARY_PIN`, `OCX_ALLOW_YANKED`, plus six flag mirrors (`OFFLINE`/`REMOTE`/`FROZEN`/`GLOBAL`/`CONFIG`/`PROJECT`/`INDEX`). That surface has grown feature by feature, each addition locally justified. It is a 1.0-relevant simplification target in its own right, and it is the reason this ADR works hard to add exactly one variable and zero internal channels.

### Recording exemption for local-preview launches (added during implementation, 2026-07-27)

Undesigned in the original decision, so recorded here rather than folded silently into the prose above. Two commands materialise an *unpublished* package into a command-scratch root purely to preview it locally: `ocx package test` (`$OCX_HOME/temp/test/`) and `ocx patch test` (`$OCX_HOME/temp/patch-test/`). Neither should ever write an execution record — a record from a maintainer's local preview describes a package that was never published, and a downstream collector has no principled way to filter it back out from a genuine invocation. Both commands declare `Launch::exempt` with an `ExemptionReason` (`PackageTest` | `PatchTest`) at their own spawn site.

That alone is not sufficient. A package that declares an entrypoint bakes a generated launcher, and both preview commands invoke through that launcher exactly like a real install would (F6) — so the preview re-enters `ocx launcher exec`. That re-entry is a **fresh process**: it re-reads `[records]` from its own config/env/CLI chain from scratch, so the exemption the outer command declared at its own spawn site does not survive the hop by itself — forwarding a sink is not the problem, the problem is that a fresh process has no memory of why the outer command was exempt in the first place. Carrying the exemption over a new environment variable was considered and rejected: it would be exactly the new internal forwarding channel this ADR's "zero new internal forwarding channels" consequence exists to avoid, on a surface already carrying 13+ (see "Related concern" above).

The pkg-root path the launcher was baked with is the one carrier that does survive the hop, because it names the very scratch directory the preview command materialised it under. `ocx launcher exec` inherits its `ExemptionReason` by checking whether the validated pkg-root sits under one of exactly two known command-scratch roots — a short, explicit allow-list, deliberately never "anything under `temp/`", which would also admit in-progress download directories sharing that parent (`crates/ocx_cli/src/command/launcher/exec.rs:63-66` for the root/reason pairing; the enum lives in `crates/ocx_lib/src/launch.rs`, and its sanctioned call sites are enumerated by a source-scan test in the same file).

#### The exemption is bounded by the posture (amended 2026-07-28)

The paragraph above originally ended: *"An exempt launch skips `[records]` resolution entirely, so a `required = true` policy the operator set can never fail a preview that record was never meant to observe."* That was the defect, not the design.

**The claim it broke.** `website/src/docs/reference/execution-records.md` states "No caller can opt out of a sink the operator has locked at system scope". The exemption is granted on **path placement**: a caller-supplied pkg-root under `$OCX_HOME/temp/test/` or `$OCX_HOME/temp/patch-test/`. Both roots are inside the *invoking user's own* `$OCX_HOME`, and `ocx launcher exec` is hidden from `--help` but perfectly invocable — it is a wire ABI. Copy an installed package tree under `$OCX_HOME/temp/test/`, invoke the launcher against it, and the launch was exempt; because the exempt path skipped the `[records]` fold, the operator's policy was never even consulted.

**Why a capability token cannot fix it.** The obvious repair is for the preview command to mint a short-lived capability the launcher verifies. It does not work here: the preview command and the forger run as the **same uid**, so anything the parent can mint the caller can mint. A secret in the child's environment, a file under `$OCX_HOME`, a signed token whose key the parent can read — each is readable and reproducible by the party it is meant to exclude. The threat model this feature is written to ("protects against error, not malice", above) does not change that; even for the *accidental* case, a token adds a forwarding channel this ADR spent a whole consequence avoiding, for a property it cannot actually establish.

**What the operator does control** is the posture. So:

> When the resolved policy has `required() == true`, **no exemption is granted**. The launch is refused with a typed error, exit `74`, and the message names the `[records]` policy that refused it rather than only an I/O condition.

A fail-closed posture and an exemption are a contradiction — "this invocation must be recorded" against "this invocation will not be" — and it is resolved in the operator's favour. The cost is stated plainly and accepted: on a host carrying a fail-closed policy, `ocx package test` and `ocx patch test` do not run. That is the correct trade for a control whose whole value is that it has no exceptions; a maintainer previewing an unpublished artifact can relax the policy on their own machine, whereas an operator cannot un-ring a fleet that ran unrecorded.

Consequences, all load-bearing:

- `Launch::exempt` takes the resolved `&RecordingPolicy` and returns `Result`. The decision stays unfabricable at call sites — a policy is still mintable only by `record::policy::resolve_records`, so a frame cannot conjure a permissive one to hand in.
- `ocx launcher exec` folds `[records]` on the **exempt path too**. Skipping the fold was half the hole: a policy never resolved is a policy that cannot refuse anything.
- `ocx package test` and `ocx patch test` resolve and pass their own policy at their spawn sites.
- `policy.required()` is the trigger, deliberately not "locked at SYSTEM scope". `RecordingPolicy` carries no `system_locked` field by design (`forwarded()` drops it so the child re-derives it), and an unlocked config file that sets `required = true` has asked for the same thing.

**Residual, named rather than fixed:** `ocx package test --script` / `ocx patch test --script` reach the Starlark host's `ocx.run`, which spawns through its own sanctioned path (`crates/ocx_lib/src/script/ocx_module.rs`, allow-listed in `no_process_spawn_outside_launch`) and does not consult `[records]`. A script preview therefore still runs unrecorded under a fail-closed policy. Same class as the hole closed here, on a surface the launch seam does not own yet; worth its own issue rather than a second special case in the seam.

### Data model

```rust
// crates/ocx_lib/src/record/execution_record.rs — lib-side (owner doctrine: lib hosts substance)

pub struct ExecutionRecord {
    pub schema_version: String,        // in-band, pip-style string; bumped ONLY on incompatible change
    pub kind: String,                  // "sh.ocx.execution-record"
    pub recorded_at: DateTime<Utc>,
    pub ocx: OcxBuild,                 // version + binary (already on OcxConfigView)
    pub frame: Frame,                  // Run | PackageExec | LauncherExec + identity quality
    pub process: Process,              // pid, parent?, user?, arch?, executable, working_directory? — NO argv
    pub host: Host,                    // name? — NOT arch; see "process.arch is the running process" below
    pub os: Os,                        // type?
    pub executable: BTreeMap<String, String>, // sh.ocx.* — provenance, kind, package purl
    pub scope: ScopeBlock,             // Project { .. } | Package { .. } | Launcher
    pub resolution: Resolution,        // offline/remote/frozen, requestedPlatform, registries?, mirrors?, managedConfig?, autoInstalled?
    pub packages: Vec<ResourceDescriptor>,  // in-toto shape — root + closure, topological
}
// Built from: Vec<Arc<InstallInfo>> (identity + digest + closure, F3)
//           + ComposeOutput.admitted_binaries / admitted_entrypoints (claimed names, F2)
//           + Env::resolve_command output    (the leaf executable, F15)
//
// `argv` is still an input — `RecordInputs::argv` — because the child's
// arguments are its tail (`argv[0]` plus the rest); it is simply never carried
// into the built record. A command line routinely holds access tokens and
// passwords, and this record's sink is operator-collected and often
// fleet-aggregated, which turns one leaked argv into many. `process.executable`
// still names the binary that ran; see the field-by-field table below.

/// Deliberately field-for-field in-toto `ResourceDescriptor`, so a consumer can
/// drop `packages` straight into SLSA `resolvedDependencies` (D5).
pub struct ResourceDescriptor {
    pub name: String,                         // "cmake" — the last repository segment
    pub uri: Option<String>,                  // a purl; omitted for a synthetic, identity-less package
    pub digest: BTreeMap<String, String>,     // {"sha256": "…"}
    pub annotations: BTreeMap<String, Value>, // sh.ocx.* — role, platform (roots only), visibility, binaries/entrypoints as JSON arrays
}
```

Everything ocx-specific lives under `annotations` with the existing `sh.ocx.*` namespace (`oci/layer_layout.rs`, `patch/descriptor.rs`). That keeps the top level standard-shaped and leaves room to grow without touching the frozen fields.

### Where every field name comes from

The first draft invented the envelope. A second research pass says the split was right but the invented half used the wrong names. Revised — **borrow wherever a maintained standard already names the thing:**

| Part of the record | Source | Why this one |
|---|---|---|
| `packages[]` entry shape (`name`/`uri`/`digest`/`annotations`) | **in-toto `ResourceDescriptor`** | The interoperability layer of this whole space: Sigstore, Tekton Chains and GUAC all consume in-toto. Keeping this shape makes an in-toto Statement wrapper a v2 *addition*, not a rewrite. |
| `packages[].uri` | **purl, `pkg:oci` type** ([spec](https://github.com/package-url/purl-spec)) | Registered type, and the semantics match with zero impedance: the purl **version is the sha256 digest**, and `tag` is documented as "the artifact tag that may have been associated with the digest at the time" — that is exactly OCX's digest-is-identity/tag-is-advisory model. Consumed by CycloneDX, SPDX 3.0, OSV, GUAC, Syft, and OTel's `artifact.purl`. |
| `process` / `host` / `os` blocks | **ECS and OTel, per field** — see the split below | Neither wholesale. The two standards **conflict structurally**, and no pipeline auto-recognises either, so the choice is made field by field on cost. |
| envelope (`schemaVersion`, `recordedAt`, `frame`, `scope`, `resolution`) | ocx | Nothing standard describes "a CLI invoked a tool with these packages". Confirmed by search, not assumed. |

#### The process/host/os split, field by field

Adopting one standard wholesale is not available: **ECS and OTel disagree structurally, not just in naming** — `process.executable` is a flat keyword *string* in ECS and an *object* in OTel. And the decisive fact from the consumer research: **no ingestion pipeline auto-recognises OTel or ECS field names.** Filebeat's ECS-awareness lives in its own bundled modules, not in inspecting arbitrary JSON. Every consumer needs an explicit mapping regardless. So field names buy legibility, never zero-config ingest — which makes "pick the cheaper one per field" the honest rule.

| Field | ECS | OTel | Take | Why |
|---|---|---|---|---|
| `process.pid` | same | same | **both agree** | Free. |
| `process.working_directory` | same | same | **both agree** | Free. |
| `os.type` | same | same | **both agree** | Free. |
| `host.name` | same | same | **both agree** | Free. |
| parent pid | `process.parent.pid` (nested) | `process.parent_pid` (flat) | **ECS** | Consistent with the rest of the block, and the nested form leaves room for more parent fields without a rename. Lets a consumer reconstruct call trees — `make` → `ocx run` → tool — from records alone. |
| invoking user | `process.user.name` (nested; effective uid) | `process.owner` (flat string) | **ECS** | "Who ran this" is a real audit question, so it is recorded **deliberately** rather than leaking through a home-directory path. ECS's nesting also leaves room for uid later. |
| executable | `process.executable` = **string** | `process.executable.{path,name}` = object | **ECS** | Flat string is more legible under `jq`, and it leaves our custom fields nowhere to collide — they move to `sh.ocx.*`. |
| args | `process.args` | `process.command_args` | **neither — not recorded** | A command line routinely carries access tokens and passwords, and this record's sink is operator-collected and often fleet-aggregated — exactly the destination that turns one leaked argv into many. `argv` stays a launch-only input (`RecordInputs::argv`, driving `Env::resolve_command` and the child's actual arguments); it is never carried into the built record. `process.executable` is the field that names what ran. |
| process architecture | *(no ECS equivalent)* | `process.architecture` — **closed enum**: `amd64`, `arm32`, `arm64`, `ia64`, `ppc32`, `ppc64`, `s390x`, `x86` | **OTel's vocabulary, ocx's placement** | Typed to the closed OCI/OTel arch vocabulary — the vocabulary ECS lacks — but scoped to the **process**, not the host machine. See the next subsection for why a machine-level arch field does not exist at all. |

Mixing reads as inconsistent; the reason is per-field and documented, which beats a consistency that costs a mapping table. Both surviving vocabularies are flat lowercase, so the mixed-casing wart of the first draft is gone.

#### PID semantics: one meaning, two platforms

`process.pid` means **the process that runs the tool** — on both platforms, so a consumer never branches on OS.

| | Unix | Windows |
|---|---|---|
| Mechanism | `execvp(2)` — every hop replaces the image (F14) | spawn + wait + `process::exit` |
| `process.pid` | ocx's pid, which **becomes** the tool | the **spawned child's** pid, via `Child::id()` |
| `process.parent.pid` | the shell that invoked ocx | ocx's own pid |

Getting the child's real pid on Windows means writing the record **after** spawn, which would forfeit the pre-exec ordering guarantee — a record could land microseconds after the process starts. Resolution: **probe the sink for writability before spawning**, then spawn, then write with the true child pid.

The probe keeps fail-closed honest (an unwritable sink refuses *before* anything runs) without needing the pid, and it costs one `create`/`unlink` rather than raw Win32. The alternative — `CREATE_SUSPENDED` + `ResumeThread` to learn the pid before the child executes — needs the child's *thread* handle, which `std::process::Child` does not expose, so it means dropping to `windows-sys` for a guarantee the probe already provides.

Deliberately **not** doing: recording ocx's pid on Windows and calling it `process.pid`. Same field name, different referent per platform, is the defect class this ADR keeps rejecting.

#### `process.arch` is the running process; `sh.ocx.platform` is what ran

`host.arch` renamed to `process.arch` purely so the name matched the value it was always going to hold: the closed-vocabulary architecture value ocx has available at zero cost is `Architecture::current()`, which reads the target the running binary was **compiled for** (`std::env::consts::ARCH`) — an amd64 ocx under Rosetta 2 on an arm64 host reports `amd64` here, exactly true of the process, not the machine. Calling that value `host.arch` would have been a quiet lie in the one case (emulation) where it matters most, so the field moved to where its subject actually is: `process`.

**The record does not carry a genuine host-machine architecture field at all**, by the same "omit, never fabricate" contract every best-effort field follows. Recovering the machine's true native architecture reliably would need a per-OS native probe — telling Rosetta 2, qemu/binfmt emulation and a 32-bit binary on a 64-bit host apart — plus a `uname`-name-to-OCI mapping table this module would then own and have to keep correct; a wrong answer there is worse than no answer, so the field is not attempted rather than built and hedged.

`sh.ocx.platform` answers a different question — what OCX resolved to when it chose the package:

| Field | Answers | Vocabulary |
|---|---|---|
| `process.arch` | what architecture is *ocx itself* running as | OTel's closed enum (= OCI arch names) |
| `sh.ocx.platform` | **what OCX resolved to when it chose the package** | OCX's own grammar — `linux/amd64+libc.glibc` |

Neither standard has anywhere to put `+libc.glibc`, and that component is load-bearing for OCX resolution — so `sh.ocx.platform` is not a duplicate to be tidied away later. `process.arch` and `sh.ocx.platform` describe different subjects (the ocx process vs. the resolved package) and are not expected to agree or diverge the way a probed host arch and a resolved package arch would; the record simply states both truthfully and leaves the comparison, if any, to the consumer.

**Evaluated and rejected**, each for a specific reason rather than taste:

| Candidate | Why not |
|---|---|
| **OpenLineage** | Object model is dataset-lineage (`run`/`job`/`inputs`/`outputs` as datasets in a DAG). Packages are not pipeline inputs — a real OpenLineage consumer would try to trace data flow *through* them. Its typed-namespaced `facets` pattern is good prior art; the top-level model actively misleads. |
| **CloudEvents** | A transport envelope, and we have no transport. CNCF-graduated and real, but every shipped use is "event on a bus"; no precedent as an at-rest record format. Implies a `type`/`source` routing contract no consumer reads. |
| **W3C `traceparent` env propagation** | The env-carrier spec is OTel release-candidate and does not even mandate the variable name. No build tool (Bazel, Gradle, Tekton, Buildkite, GH Actions) ships documented `TRACEPARENT` child-process linkage — tutorials only. No interop payoff, and it would reintroduce the correlation-ID propagation this design deliberately dropped. |
| **SPDX 3.0 build profile / CycloneDX formulation** | Both real, both adjacent — but each pulls in a full document model (JSON-LD `@context`; BOM component graph) disproportionate to one process, one shot. |
| **SCITT** (RFC 9943) | A transparency-*service* architecture, not a record schema. Needs a ledger backend we do not have. |
| **in-toto `runtime-trace`** ([predicate](https://github.com/in-toto/attestation/blob/main/spec/predicates/runtime-trace.md)) | Verified to exist — it is syscall-level tracing (`monitoredProcess`, `monitorLog.{process,network,fileAccess}`), not environment composition. Wrong subject. |

#### Format rules frozen at v1

1. **`digest` is a map of `algorithm → bare lowercase hex`.** Never `sha256:abc…` inside the value, never a transport prefix (`docker-pullable://`, `oci://`), never uppercase. The algorithm is the key; that is the point of the map. *(This overrides the "just use OCI's `sha256:<hex>` string, we already own it" suggestion — that shape is mandated by `ResourceDescriptor`, and the "already own it" argument is a `split(':')`. The OCI string form survives anyway, inside the purl.)*
2. **`uri` is a purl and carries the digest as its version.** `pkg:oci/<name>@sha256:<hex>?repository_url=<registry/ns>&tag=<tag>&arch=<arch>`. Type is `oci`, **not** an invented `pkg:ocx` — OCX packages *are* OCI artifacts.
3. **The colon in the purl version is emitted UNENCODED** (`sha256:3f7a…`, not `sha256%3A3f7a…`). The spec's canonical build rule says percent-encode, the `oci` type doc's own four examples contradict each other, and [purl-spec#786](https://github.com/package-url/purl-spec/issues/786) is **open and unresolved**. Trivy — the only real `pkg:oci` producer at scale — and Red Hat's security-data guidelines both emit unencoded, and a literal colon parses correctly under both spec-correct and naive parsers. Documented so nobody "fixes" it later.
4. **purl name = the last repository segment, and it is already correct by construction.** `Identifier::name()` returns exactly that (`oci/identifier.rs:149`), and repository segments are lowercase-only at the parser (`is_repository_segment_byte`, `identifier.rs:451`) — so purl's "name must be lowercased / is the last fragment" rules need no normalization layer. **Caveat: `name` alone is not unique** — `ocx.sh/a/cli` and `ocx.sh/b/cli` both yield `cli`. Identity is name + `repository_url` + digest; a consumer joining on `name` alone is wrong.
5. **The pinned digest is the PLATFORM LEAF manifest digest, never the multi-arch index digest.** `ocx.lock` stores `platforms: BTreeMap<String, Digest>` per platform and `host_leaf_identifier` builds from that leaf. This names the exact bits that ran rather than the index that pointed at them, and it makes the purl's `arch` qualifier truthful rather than decorative. It is precisely the distinction Kubernetes has spent a decade unwinding in `imageID`. **Do not "simplify" this to the index digest.**
6. **A tag appears only when one was actually resolved.** `ocx.lock` stores no tags (F11), so a project-tier record has none and must not synthesise one. Same for the purl's `tag` qualifier.
7. **`schemaVersion` is a string, bumped only for backward-incompatible change** (pip's discipline). Additive fields never bump it; consumers must tolerate unknown keys.
8. **One record = one JSON document = one file, serialized COMPACT (single line).** Not JSON Lines, not concatenated documents. Compact because all seven mainstream log shippers — OTel Collector filelog, Vector, Fluent Bit, Fluentd, Filebeat, Alloy, Splunk UF — are line-oriented and choke on a multi-line JSON document; on one line, "read whole file" degenerates to "read one line" and every one of them copes. This is the **only** change that materially helps ingest; field naming does nothing for it. (Bazel's `--execution_log_json_file` shipped newline-delimited proto by accident and has carried the bug for years — choose the shape deliberately.)
9. **Key casing is flat lowercase in the borrowed blocks, camelCase in the envelope.** Both surviving borrowed vocabularies (ECS, OTel's arch enum) are flat lowercase; the envelope matches the provenance cluster (in-toto, SLSA, CycloneDX, SPDX). The first draft's mixed snake/camel wart came from OTel's object form and is gone with it.
10. **Borrowed names are pinned by this document, not by upstream.** OTel's arch enum is *release-candidate* and ECS is mid-convergence with OTel — neither is frozen upstream. Record which spec each name came from here; **an upstream rename does not license a break in this file.**
11. **Two field classes, and only one can fail the invocation.** The record must never fail an invocation because an *environmental* detail could not be determined.

    | Class | Fields | On failure |
    |---|---|---|
    | **Load-bearing** — the audit answer itself | `packages[]`, `digest`, `process.executable`, `frame` | Cannot fail: all are in hand from resolution (F3, F15). If one were somehow absent the record would be a lie, so absence is a bug, not a runtime condition. |
    | **Best-effort** — environmental context | `host.name`, `os.type`, `process.arch`, `process.user.id`, `process.user.name`, `process.parent.pid`, `process.working_directory` | **Omit the key.** Never fail, never guess, never emit a placeholder like `"unknown"`. |

    Concretely: an unreadable hostname, an undetectable architecture (exotic target, container without `/proc`), a missing username (no passwd entry — common in scratch containers) each drop one key. Consumers must already tolerate absent keys per rule 7, so this costs them nothing. **An absent key means "not determinable here"; a present key is always true.** A `"unknown"` sentinel would be indistinguishable from a host genuinely named `unknown`.

---

## Exemplary Records

### 1. `ocx run` — project tier, full closure

`/var/log/ocx/records/20260726T140311482Z-48123-9f3a1c07.json`

```json
{
  "schemaVersion": "1",
  "kind": "sh.ocx.execution-record",
  "recordedAt": "2026-07-26T14:03:11.482Z",
  "ocx": {
    "version": "0.4.1",
    "binary": "/home/ci/.ocx/bin/ocx"
  },
  "frame": {
    "command": "run",
    "identity": "complete"
  },
  "process": {
    "pid": 48123,
    "parent": { "pid": 47990 },
    "user": { "id": "1000", "name": "ci" },
    "arch": "amd64",
    "executable": "/home/ci/.ocx/packages/index.ocx.sh/sha256/3f/7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0/entrypoints/cmake",
    "working_directory": "/scratch/job-88213"
  },
  "host": { "name": "batch-node-17" },
  "os": { "type": "linux" },
  "executable": {
    "sh.ocx.provenance": "ocx-package",
    "sh.ocx.kind": "launcher",
    "sh.ocx.package": "pkg:oci/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0?repository_url=index.ocx.sh%2Focx"
  },
  "scope": {
    "tier": "project",
    "cleanEnv": false,
    "projectRoot": "/scratch/job-88213",
    "lock": {
      "path": "/scratch/job-88213/ocx.lock",
      "declarationDigest": { "sha256": "9c1f0b3a77d2e4518ab6c0f92d3e7a41b8c5d6e0f1a2b3c4d5e6f708192a3b4c" }
    },
    "groups": ["default"]
  },
  "resolution": {
    "offline": false,
    "remote": false,
    "frozen": true,
    "requestedPlatform": "linux/amd64+libc.glibc",
    "registries": ["index.ocx.sh"],
    "mirrors": { "ghcr.io": { "registry": "https://artifactory.corp.example/ghcr-remote" } },
    "managedConfig": {
      "source": "internal.corp.example/ocx-config:user",
      "digest": { "sha256": "4d2c8e1f5a90b7c36e4d1928f0a5b3c7d9e2f4a6b8c0d1e3f5a7b9c1d3e5f709" }
    }
  },
  "packages": [
    {
      "name": "cmake",
      "uri": "pkg:oci/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0?repository_url=index.ocx.sh%2Focx&arch=amd64",
      "digest": { "sha256": "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0" },
      "annotations": {
        "sh.ocx.role": "root",
        "sh.ocx.binding": "cmake",
        "sh.ocx.group": "default",
        "sh.ocx.platform": "linux/amd64+libc.glibc",
        "sh.ocx.visibility": "public",
        "sh.ocx.entrypoints": ["cmake", "ctest", "cpack"]
      }
    },
    {
      "name": "ninja",
      "uri": "pkg:oci/ninja@sha256:8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c?repository_url=index.ocx.sh%2Focx&arch=amd64",
      "digest": { "sha256": "8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c" },
      "annotations": {
        "sh.ocx.role": "root",
        "sh.ocx.binding": "ninja",
        "sh.ocx.group": "default",
        "sh.ocx.platform": "linux/amd64+libc.glibc",
        "sh.ocx.visibility": "public",
        "sh.ocx.binaries": ["ninja"]
      }
    },
    {
      "name": "libstdcxx-runtime",
      "uri": "pkg:oci/libstdcxx-runtime@sha256:c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3?repository_url=index.ocx.sh%2Focx",
      "digest": { "sha256": "c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3" },
      "annotations": {
        "sh.ocx.role": "dependency",
        "sh.ocx.visibility": "interface"
      }
    }
  ]
}
```

Several things in this record are load-bearing and easy to misread as noise:

**No `tag` qualifier on any purl — correct, not an omission.** `ocx.lock` V3 stores a bare repository plus a per-platform digest and *rejects* a tag at validation (F11). A project-tier run has no tag to report; synthesising one would be the first lie in an audit record.

**`sh.ocx.kind` is `"launcher"`.** This frame resolved `cmake` against the composed `PATH` and hit the package's `entrypoints/` directory (F15). The *real* cmake binary appears in the sibling record written by the launcher re-entry, joined to this one **by digest**. That is the L1 split working as intended, visible in the data.

**`sh.ocx.provenance`** is the field an auditor reads first. `"ocx-package"` here; `"external"` when the resolved path lands outside the store — `ocx run -- bash …` picking up the *system* bash is a fact this record must state out loud, and it is currently invisible in every other ocx output. Note it sits in its own `executable` block under the `sh.ocx.*` namespace, **not** nested under `process.executable`: ECS types that field as a flat keyword string, so hanging sub-fields off it would be an ingest type-conflict, not merely a naming quibble.

**No `process.args`.** The command line is what launched this record's own subject, and it can carry access tokens or passwords typed on it — a leak vector this operator-collected, often fleet-aggregated sink must not amplify. `process.executable` still names the binary that ran; that is the field an auditor keys on, not the argument list.

**`process.arch` is `amd64` and `resolution.requestedPlatform` is `linux/amd64+libc.glibc`** — not a duplication. The first is the architecture the running ocx binary was compiled for (never a probed host architecture — see "process.arch is the running process" above); the second is what OCX resolved to, in a grammar that carries `+libc.glibc`, which neither standard can express.

**`sh.ocx.platform` appears only on the two roots, never on `libstdcxx-runtime`.** A dependency is reachable here as an identifier, not as an install, so the platform it actually resolved to is not in hand at record-build time — the field is omitted rather than guessed, the same "omit, never fabricate" contract as every other best-effort field.

`sh.ocx.entrypoints` / `sh.ocx.binaries` come from `ComposeOutput.admitted_entrypoints` / `admitted_binaries` (F2) — already computed, zero extra work, and they answer the auditor's follow-up: *which executables did this package put on `PATH`?* That is also the datum [#177](https://github.com/ocx-sh/ocx/issues/177) wants; the two features should share one derivation. Each is a genuine JSON array of strings, not a comma-joined string: an in-toto descriptor's `annotations` values are arbitrary JSON, and no separator is forbidden in a binary name, so `["a,b"]` and `["a","b"]` would otherwise arrive indistinguishable.

### 2. `ocx package exec` — OCI tier, no lock (the reporter's actual case)

`/var/log/ocx/records/20260726T140312006Z-48124-2b71ee40.json`

```json
{
  "schemaVersion": "1",
  "kind": "sh.ocx.execution-record",
  "recordedAt": "2026-07-26T14:03:12.006Z",
  "ocx": { "version": "0.4.1", "binary": "/opt/ocx/bin/ocx" },
  "frame": {
    "command": "package-exec",
    "identity": "complete"
  },
  "process": {
    "pid": 48124,
    "user": { "id": "1000", "name": "ci" },
    "arch": "amd64",
    "executable": "/home/ci/.ocx/packages/internal.corp.example/sha256/aa/11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899/content/bin/solver",
    "working_directory": "/scratch/job-88213"
  },
  "host": { "name": "batch-node-17" },
  "os": { "type": "linux" },
  "executable": {
    "sh.ocx.provenance": "ocx-package",
    "sh.ocx.kind": "binary",
    "sh.ocx.package": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?repository_url=internal.corp.example"
  },
  "scope": {
    "tier": "package",
    "cleanEnv": true,
    "requested": ["internal.corp.example/solver:2024.3"]
  },
  "resolution": {
    "offline": false,
    "remote": false,
    "frozen": false,
    "requestedPlatform": "linux/amd64+libc.glibc",
    "registries": ["internal.corp.example"],
    "mirrors": {},
    "managedConfig": {
      "source": "internal.corp.example/ocx-config:user",
      "digest": { "sha256": "4d2c8e1f5a90b7c36e4d1928f0a5b3c7d9e2f4a6b8c0d1e3f5a7b9c1d3e5f709" }
    },
    "autoInstalled": ["internal.corp.example/solver"]
  },
  "packages": [
    {
      "name": "solver",
      "uri": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?repository_url=internal.corp.example&tag=2024.3&arch=amd64",
      "digest": { "sha256": "aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899" },
      "annotations": {
        "sh.ocx.role": "root",
        "sh.ocx.platform": "linux/amd64+libc.glibc",
        "sh.ocx.visibility": "public",
        "sh.ocx.resolved-from": "tag"
      }
    }
  ]
}
```

Here the purl **does** carry `tag=2024.3` — the user typed a tag, so one was genuinely resolved (contrast record #1, F11). The purl spec's own wording for the qualifier — *"artifact tag that may have been associated with the digest at the time"* — is precisely the semantics OCX wants: identity is the digest, the tag is a historical note.

`"resolution.autoInstalled"` and `"sh.ocx.resolved-from": "tag"` are what make the drift argument auditable: this invocation resolved a *floating tag* and materialised the package on the spot. That is exactly the state no pull-time record can capture, and it is the reporter's actual configuration.

Note `sh.ocx.kind` is `"binary"`, not `"launcher"` — this package declares no entrypoints, so the composed `PATH` resolved straight to the real binary and there is **no second record**. The two-record split happens only when an entrypoint launcher is involved.

### 3. Direct launcher invocation — degraded identity (F5)

A user runs `cmake` straight from `PATH` after `ocx env`, with no ocx parent frame.

```json
{
  "schemaVersion": "1",
  "kind": "sh.ocx.execution-record",
  "recordedAt": "2026-07-26T14:07:44.118Z",
  "ocx": { "version": "0.4.1", "binary": "/opt/ocx/bin/ocx" },
  "frame": {
    "command": "launcher-exec",
    "identity": "degraded",
    "identityNote": "package directories are content-shared and carry no registry/repository, so logical identity is not recoverable in this frame and no purl can be emitted"
  },
  "process": {
    "pid": 48123,
    "arch": "amd64",
    "executable": "/home/ci/.ocx/packages/index.ocx.sh/sha256/3f/7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0/content/bin/cmake",
    "working_directory": "/scratch/job-88213"
  },
  "host": { "name": "batch-node-17" },
  "os": { "type": "linux" },
  "executable": {
    "sh.ocx.provenance": "ocx-package",
    "sh.ocx.kind": "binary"
  },
  "scope": { "tier": "launcher" },
  "resolution": { "offline": false, "remote": false, "frozen": false, "requestedPlatform": null },
  "packages": [
    {
      "name": "file-url-mode/3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0",
      "digest": { "sha256": "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0" },
      "annotations": {
        "sh.ocx.role": "root",
        "sh.ocx.identity": "synthetic"
      }
    }
  ]
}
```

**This is the sibling of record #1, not a stray.** It is what the launcher re-entry writes twelve milliseconds later, and it carries the one thing record #1 structurally cannot: `content/bin/cmake`, the binary that actually ran. Record #1 stopped at `entrypoints/cmake`, the launcher.

Read them together and the audit question is fully answered:

| | record #1 (outer) | record #3 (inner) |
|---|---|---|
| logical identity | `pkg:oci/cmake@sha256:3f7a…` | none — synthetic |
| executable | `entrypoints/cmake` (launcher) | `content/bin/cmake` (**leaf**) |
| join key | `digest` — identical in both | |
| pid | 48123 | 48123 — same, the exec chain preserves it (F14) |

**No `uri` field at all**, rather than a fabricated one. An earlier draft emitted `ocx-store:/home/ci/.ocx/packages/…` here; that was an invented URI scheme dressed up as identity, and a purl cannot be constructed without a repository. Omitting the field is the honest encoding — `ResourceDescriptor` permits it, since `digest` alone identifies the resource. `"identity": "degraded"` states the limitation in-band. `"requestedPlatform": null` per F7 — not fabricated from the host.

### 4. The consumer that actually works today — Conftest / OPA

Two independent research passes converged here: the realistic consumer of this record is **a small policy check over raw JSON**, not a pipeline and not the attestation ecosystem. Conftest asks nothing of us — no signing key, no registry, no layout file, no recognised predicate type. It reads the file as-is.

```rego
package ocx.records

# Fail the invocation record if any package is not on the approved list.
approved := {
  "sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0",
  "sha256:8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c",
}

deny contains msg if {
  package := input.packages[_]
  digest  := sprintf("sha256:%s", [package.digest.sha256])
  not approved[digest]
  msg := sprintf("unapproved package %s (%s)", [package.name, digest])
}

# The thing that ran was not supplied by ocx at all.
deny contains msg if {
  input.executable["sh.ocx.provenance"] == "external"
  msg := sprintf("external executable: %s", [input.process.executable])
}
```

```sh
conftest test --policy approved.rego /var/log/ocx/records/*.json
```

That is the whole story for a user who wants to gate CI next week. **This is what the docs should lead with** — not the attestation ecosystem, which needs infrastructure they do not have.

### 5. Why there is no in-toto Statement in v1

The first draft claimed wrapping the record in an in-toto `Statement` later would be a free upgrade, unlocking Sigstore, Tekton Chains and GUAC. **The consumer research disproved it.** Every named beneficiary demands more than a well-shaped predicate:

| Consumer | Additional demand |
|---|---|
| `cosign verify-attestation` | DSSE signature **and** an OCI *image* subject. `--type custom` passes an arbitrary `predicateType` through, but there is no path to verify a bare local file; signing a Statement whose subject has no local blob is [cosign#4019](https://github.com/sigstore/cosign/issues/4019) — open, unimplemented. |
| `slsa-verifier` | Hardcoded to SLSA provenance families. Not pluggable. |
| GUAC | Closed ingestor set (SPDX, CycloneDX, SLSA, OSV, Scorecard, OpenVEX, in-toto link/vuln). An unrecognised `predicateType` has **no ingestor** — invisible, not stored-unparsed. |
| witness / Archivista | Compiled attestor model; Archivista stores signed only, by design. |
| in-toto-verify | Requires an out-of-band YAML layout with per-step keys. |

Registration would not help: there is no mandatory `predicateType` registry (*"Your predicate is yours"*), and registering changes nothing for the tools above.

**And the subject has no precedent.** in-toto requires `subject: [{name, digest}]` — the artifact the predicate is *about*. This record describes an execution that has produced nothing yet. Every existing predicate, including `runtime-trace`, uses the *produced build artifact*; no consumer surveyed accepts an empty subject, and the question is deferred to [in-toto/attestation#28](https://github.com/in-toto/attestation/issues/28), open and unresolved.

**Deferring is safe.** Since v1 emits no Statement, adding one later is a **new optional output mode** — purely additive, nothing reshaped. If it ever happens, the workable subject is the **resolved-environment digest**: an RFC 8785 JCS-canonical hash over the `packages` array, a real hashable artifact existing at record-write time, tier-independent. `serde_json_canonicalizer` already computes `ocx.lock`'s `declaration_hash` exactly this way (`project/hash.rs`), so the machinery exists when it is wanted.

### 6. What `pkg:oci` does and does not buy

Stated plainly so no one promises otherwise: **identity, not scanning.**

| | |
|---|---|
| **Does** | A stable, ECMA-427-standardised identity string; joins across tools; the same shape Trivy emits; forward-compatibility if these records ever enter an SBOM or attestation flow. |
| **Does not** | Vulnerability lookup. OSV has no OCI ecosystem and treats `purl` as informational, not a query key. deps.dev allowlists 7 types, none `oci`. Snyk skips it. Grype decomposes images into constituent packages and matches those — a whole-artifact purl gets zero CVE matches. Dependency-Track, Sonatype IQ and JFrog Xray store it inert. |

If CVE visibility into packaged tools is ever a goal, it needs the tool's *upstream ecosystem* purl carried alongside — a separate feature, not a side effect of this one.

### 7. Relationship to the SBOM work — chain them, do not couple them

The SBOM tracking issue ([#199](https://github.com/ocx-sh/ocx/issues/199)) and its slices — attach ([#100](https://github.com/ocx-sh/ocx/issues/100)), discovery ([#101](https://github.com/ocx-sh/ocx/issues/101)), dogfood ([#200](https://github.com/ocx-sh/ocx/issues/200)) — land on a similar timeline and are obviously adjacent. They should compose, and they already do **without a new field**:

```
execution record  →  "these exact digests ran"
        │ digest is the join key
        ▼
ocx package sbom <digest>  →  "and here is what is inside them"
```

Each package entry is digest-pinned, and an SBOM attached to that package is discoverable *from the digest* through the OCI referrers API. The record therefore answers "what ran" and the SBOM answers "what is in it", chained by a key already present. That is the whole integration.

**Deliberately not embedding or referencing SBOMs in the record.** Two reasons:

1. **It would block the call.** Discovering an SBOM referrer is a registry round-trip. Putting network I/O on the exec hot path contradicts the design's central constraint — the record must add one local file write and nothing else. Even a cached lookup is a lookup.
2. **It would duplicate a join that already works.** A `sh.ocx.sbom` annotation carrying an SBOM digest would be a denormalised copy of something derivable, with the usual consequence: it goes stale when an SBOM is attached, re-attached or corrected after the fact, and a record is immutable once written.

The one thing worth doing when the SBOM work lands: a docs section showing the two-step chain, so the connection is discoverable rather than inferred. If SBOM digests ever become known *locally* at exec time with no network — e.g. recorded in the package metadata at install — revisit; until then the digest is the correct and sufficient link.

---

## Consequences

**Positive**

- The reporter's wrapper scripts stop being the control point; the operator's managed config is.
- Zero extra resolution work, zero network, one small file write on a path that already does filesystem I/O.
- **Zero new internal forwarding channels** — on a surface already carrying 13+. The three new `OCX_RECORDS_*` variables are user-facing settings with flag and config peers, resolved by one shared fold, not private ocx-to-ocx signalling.
- The leaf binary is captured, and `executable["sh.ocx.provenance"]` states out loud when the thing that ran was *not* ocx-supplied — currently invisible in every ocx output.
- `packages` stays `ResourceDescriptor`-shaped, so if an attestation flow is ever built ([#198](https://github.com/ocx-sh/ocx/issues/198)/[#102](https://github.com/ocx-sh/ocx/issues/102)) the data model is already right — even though the wrapper itself buys nothing today.
- Field names are borrowed from purl, in-toto, ECS and OTel wherever a maintained standard already names the thing, so a consumer who knows any of them reads most of the record without our documentation.
- A user can gate CI on approved digests with a few lines of Rego and no infrastructure at all (Conftest example above).
- Fills a real gap: no competitor emits per-invocation composition; npm/conda/Nix/Guix/Docker all stop at queryable-store-after-the-fact, and pip's report is per-*install*.
- `admitted_binaries`/`admitted_entrypoints` (F2) answers "which executable did this package put on `PATH`" for free — shared derivation with [#177](https://github.com/ocx-sh/ocx/issues/177).
- No lock, no shared offset, no new hot-path `await` (F13).

**Negative**

- A new persisted wire format to keep stable through 1.0 and beyond.
- No exit code, no duration, no completion event (F1). The record answers *"what was about to run"*, not *"what happened"*. Callers correlate `$?` themselves.
- Fail-closed means a bad mount stops jobs. Intended, but it will generate at least one angry ticket.
- Directory sinks grow without bound; collection/rotation is the operator's job.
- **An entrypoint invocation produces two records.** Accepted deliberately (L1) — but consumers counting records will over-count invocations unless they filter on `frame.command`.
- Field names come from three sources (ECS, OTel, ocx), which reads as inconsistent until you know each field's reason. Documented per field; the alternative was owning an arch mapping table.
- **No ingest-ready consumer exists.** Every log shipper needs an explicit mapping regardless of naming; compact single-line JSON is the only thing that helps, and it helps only a little.
- ~~The emit is three call sites, not one seam (F15) — a future exec-ish command can forget it. Mitigated by test, not by types.~~ **Superseded during implementation.** `ocx_lib::launch` is that seam: `child_process` is a private submodule, so `Launch::recording` / `Launch::exempt` are the only reachable spawn paths, and `RecordingPolicy` is mintable only by `record::policy::resolve_records` — a caller cannot fabricate "recording off". Sanctioned non-recording launches are a closed `ExemptionReason` enum, so adding one is a visible diff. The mitigation is types plus a source-scan test, not a test alone.

**Risks**

| Risk | Mitigation |
|---|---|
| Record write adds latency to every exec | One serialise + one atomic file write of a few KB on an already-I/O-bound path. Measure in the plan; budget < 5 ms p99. |
| Schema churn after fleets consume it | `schemaVersion` in-band from day one; land inside [#178](https://github.com/ocx-sh/ocx/issues/178)'s stable-within-minor set with snapshot tests in CI. |
| **Upstream renames a borrowed field** (OTel `host.arch` is release-candidate; ECS is mid-convergence with OTel) | Format rule 10: borrowed names are pinned by this ADR, not by upstream. An upstream rename does not propagate into this file. |
| Consumers count records and over-count invocations | An entrypoint invocation emits two records by design (L1). Documented, and `frame.command` distinguishes them. |
| Records carry absolute paths embedding the invoking user's home directory | Not treated as a security finding — no threat model exists, and the operator who configures a SYSTEM-scope sink already has root on the box. **Documentation line instead:** records contain absolute paths including the user's home directory, so size sink permissions and retention accordingly. The case this matters is records *leaving* the machine (central SIEM, long retention, wider audience than the operator) — audience drift, not an adversary. If the managed tier ever grows teeth (signing, attestation), write a threat model then and run `/security-auditor` against it. |
| Someone spells the sink as a file and appends over NFS anyway | The setting takes a **directory**. There is no file form to misuse. |
| A `name` template expands to a constant, silently overwriting every record | Unknown placeholder is a parse error; and a template containing none of `{time}`/`{pid}`/`{rand}` should be rejected at config load, not discovered during an audit. |
| `--records-dir` collides with the pending `--local`/`--system` flag rework ([#213](https://github.com/ocx-sh/ocx/issues/213), [#220](https://github.com/ocx-sh/ocx/issues/220)) | Sequence after the root-flag grammar settles, so integrators absorb one break, not two. |

---

## Implementation Plan (outline — sequencing only)

1. [ ] **Settle the root-flag grammar first** ([#213](https://github.com/ocx-sh/ocx/issues/213), [#220](https://github.com/ocx-sh/ocx/issues/220)) so this lands into a stable CLI shape.
2. [ ] `ocx_lib::records` module: `ExecutionRecord` + `ResourceDescriptor` + `pkg:oci` purl construction + compact serializer + JSON schema via `ocx_schema`. Purl building goes through the `packageurl` crate (v0.7.0, MIT, clean against `deny.toml`) — hand-rolling the string is the "own a wire format" anti-pattern `quality-core.md` blocks. Runs through the `deps` process as a new direct dependency.
3. [ ] Emit call at the three sites, **after** `Env::resolve_command` (F15); `ChildEnv` unchanged. Classify the resolved executable as `ocx-package` / `external` by store-root containment.
4. [ ] `RecordsOptions` + `merge` + `resolve_records` (the single resolution function) in `ocx_lib::records`; `NameTemplate` parse with the closed placeholder set.
5. [ ] Wire the layers: `[records]` config section (managed tier folds in for free via `Config::merge`), `OCX_RECORDS_{DIR,NAME}` into `apply_ocx_config`'s forwarded set with set-or-remove discipline, `--records-{dir,name}` flags on `run` / `package exec`. `required` is config-only. Then the binary `system_locked` clamp.
6. [ ] Acceptance tests: all three frames emit; an entrypoint invocation emits **exactly two** records joining on digest; degraded-identity launcher case emits no `uri`; unwritable sink fails closed when SYSTEM-locked and warns when not; N concurrent invocations produce N distinct files; unknown placeholder rejected at resolve time; **precedence matrix — each field set at each layer, highest wins, `None` never clobbers a lower layer**; `system_locked` ignores env and CLI entirely; emitted purl round-trips through the `packageurl` crate with the unencoded colon; **`process.pid` is the tool's pid on Unix and the spawned child's on Windows**; **best-effort fields omit rather than fail** — assert a record is still written with hostname, architecture and username resolution all forced to fail.
7. [ ] Docs: env reference, `reference/execution-records.md` **leading with the Conftest/OPA example**, inclusion in [#178](https://github.com/ocx-sh/ocx/issues/178)'s declared-stable set. State plainly that `pkg:oci` gives identity, not vulnerability lookup, and that records contain absolute paths including the user's home directory.
8. [ ] Sink path handling: the operator-designated sink is canonicalized and **pinned to where it really is**, once, at policy resolution (`pin_sink` in `crates/ocx_lib/src/record/policy.rs`) — refusing a sink for merely *containing* a symlink was the wrong guard, since macOS reaches an ordinary `/var/log/ocx/records` through `/var` → `/private/var` and that check refused it on every launch, permanently under `required = true`. What is refused is **substitution of the sink after the operator designated it**: `emit`/`probe_writable` re-resolve the already-pinned path and fail if it no longer resolves to itself, so a symlink inserted since designation does not redirect the audit trail. The check compares a canonical *path*, not the directory's identity, and runs before the write rather than holding the directory open across it — so it does not catch a sink replaced by a *different real directory* at the same path, nor a symlink inserted between the check and the file being created. Both would need an `openat`-style handle pinned at designation with every write made relative to it. That is the correct scope here: this is an integrity control against accidental redirection, not a defence against an adversary racing it. (No security review — no threat model; see the risk table.)
9. [ ] **After the SBOM slices land** ([#199](https://github.com/ocx-sh/ocx/issues/199)): docs section showing the digest → `ocx package sbom` chain. No format change.

---

## Open Questions for the Owner

Settled during the 2026-07-26 discussion and owner-confirmed, recorded so they are not relitigated:

- No minted execution ID; no env propagation for correlation or suppression.
- Two records per entrypoint invocation, joined by content digest.
- `[records]` plural; `dir` + `name` settable at config/env/CLI via one four-layer fold; `required` config-only.
- **Binary lock**: a SYSTEM-scope policy (`/etc/ocx/config.toml`) locks the whole block; otherwise nothing is locked. Fail-closed when locked, warn when not.
- purl `pkg:oci` (unencoded colon, platform-leaf digest) + in-toto `ResourceDescriptor`; **ECS** for `process.executable`, **OTel's** closed arch enum for `process.arch` (scoped to the running ocx process, not a probed host — see "process.arch is the running process"), both agree on the rest; `sh.ocx.*` for everything ocx-specific. `process.args` is not recorded at all — see the Amendment below.
- Compact single-line JSON, one document per file.
- No in-toto `Statement` in v1; subject question deferred as genuinely additive.
- Conftest/OPA is the documented consumer story.
- `sh.ocx.store-path` dropped — redundant (frames join on digest; `process.executable` already shows the store location) and derivable from `$OCX_HOME` + registry + digest.
- **No threat model, so no security framing.** The SYSTEM lock guards against accident, not an adversary; the ADR says so rather than overclaiming. `process.user.name` records the invoking user *deliberately* instead of leaking it through a home-directory path.
- `process.parent.pid` recorded so call trees (`make` → `ocx run` → tool) reconstruct from records alone.
- Environmental fields are best-effort: an undetectable hostname, architecture or username omits a key, never fails the invocation (format rule 11).
- SBOM integration is the digest join, not a new field (section 7).
- Filename placeholders are ocx-generated only; `{command}` rejected rather than sanitised.
- **Exec-time only, and not as a scope compromise.** `ocx pull` / `ocx install` will not emit this record. They resolve the same data, so the *shape* stays reusable if a future need wants it — but emitting it at pull time would record precisely the state this ADR argues is insufficient (§Context: config and index drift between pull and exec, floating tags move, and both exec paths auto-install on demand, so an exec can materialise a package no prior pull ever saw). A pull-time record would be the wrong answer wearing the right shape.

Still open:

1. **Option L2b** — reverse-look-up the logical name at a direct launcher invocation (F12)? Recommend **no** for v1; under L1 the outer record already supplies it in the common case.
2. **Ask the reporter** — they mentioned a second issue with corporate nice-to-haves. Worth landing that *before* implementation so the managed surface is designed once, not twice.

---

## Amendment (2026-07-27) — owner decisions from the max-tier review

A max-tier adversarial review of the shipped implementation (PR [#238](https://github.com/ocx-sh/ocx/pull/238)) raised three questions the original decision left unsettled. The owner resolved all three; recorded here as an amendment rather than by rewriting the decision prose above.

1. **`process.args` is removed from v1.** Argv can carry access tokens and passwords — a secret typed on a command line is a well-known leak vector, and this record's sink is exactly the kind of destination that turns one leaked argv into many: operator-collected, and routinely fleet-aggregated into a central store with a wider audience than the invoking host. Removing the field now, before any fleet has consumed a v1 record, costs nothing. Removing it after a fleet has built tooling against `process.args` would be exactly the kind of break `schemaVersion` (format rule 7) exists to price honestly rather than absorb for free. A config-gated opt-in for operators who accept the exposure and want the field may follow later; it is not in v1.
2. **`RecordsError::SinkSymlink` classifies to exit `78` (`ConfigError`), not `64` (`UsageError`).** The Technical Details section above ("Failure posture follows the lock") matched this to the existing `SymlinkWalkError::Ancestor` symlink-refusal precedent, which is `64` because that precedent guards a path the *user typed as a CLI argument* — a usage fault the invoker fixes by retyping a flag. The execution-record sink is never typed as an argument: it arrives from `[records]` config, `OCX_RECORDS_DIR`, or a SYSTEM-scope lock. A symlinked sink under any of those is a configuration fault the operator fixes by editing a file, and `64` would send them looking at their invocation instead of their config — the same reasoning format rule 7's neighbour already applies to a bad name template (`TemplateUnknownPlaceholder` / `TemplateNotUnique` / `NameNotAFilename`, all `78`). The `SymlinkWalkError::Ancestor` precedent does not transfer across that distinction, and `SinkSymlink` joins those three at `78`.
3. **A managed (registry-delivered) `[records] required = true` is deliberately not an error.** "The lock is an integrity control, not a security boundary" (Technical Details, above) already establishes that no adversarial model exists for this feature — the SYSTEM lock guards against a well-meaning wrapper script overriding an operator's collection contract by accident, not against a hostile caller. A managed tier asserting the strictest posture is the intended use of that mechanism, not an edge case requiring a new error path or a distinct code branch.
4. **The sink-pinning guard (checklist item 8, above) protects within a process, not across the two-process entrypoint hop.** `pin_sink` canonicalizes and pins the sink once **per process**, in `resolve_records`; substitution between designation and write is refused on both the pre-spawn probe and the emit path, but only inside that one process's lifetime. An entrypoint invocation's two records come from **two processes** (F6): the launcher re-entry re-runs `resolve_records` against the forwarded `OCX_RECORDS_DIR` and re-pins from scratch, so a symlink planted before the child starts pins to its own target and the child records there silently. The property is "no substitution within a recording process," not "no substitution across the process boundary of one entrypoint invocation."

   Two further limits hold **within** a process, both from the same cause — the guard pins a canonical *path* and re-checks it before the write, rather than holding the designated directory open across it. A sink replaced by a *different real directory* at the same path re-resolves to itself and is accepted; and a symlink inserted in the window between the check and `NamedTempFile::new_in` is followed. So the honest statement of the whole guard is: **it catches a symlink present in the pinned path at check time — the accidental redirection it exists for — and nothing that races it.**

   Accepted, not closed, and the reasoning is one argument for all three limits: this is already an integrity control rather than a security boundary — planting any of these needs write access to the sink's parent, and whoever holds that already owns the trail, or can simply not run `ocx` at all. Closing the intra-process pair needs an `openat`-style directory handle pinned at designation with every write made relative to it; closing the cross-process one additionally needs the child to distinguish "pinned by my parent" from "typed by an operator," which means a new channel in `RecordsOptions` — exactly the internal forwarding channel this ADR rejected. The prior guard that *did* close more of this (refusing any symlinked ancestor) was removed because its price was a universal macOS outage, which prices the trade plainly.

---

## Links

- Issue [#214](https://github.com/ocx-sh/ocx/issues/214) — originating request
- [`research_execution_record_formats.md`](./research_execution_record_formats.md) — standards survey; why purl/in-toto in and OpenLineage/CloudEvents/OCSF out
- [`research_execution_record_consumers.md`](./research_execution_record_consumers.md) — **read before trusting any "we can adopt X" claim**; per-tool verdicts, the subject problem, the line-oriented-shipper table
- [`adr_managed_config_tier.md`](./adr_managed_config_tier.md) — the tier this hangs off; fail-closed precedent
- [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md) — project tier vs OCI tier split
- [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) — authority for current CLI model
- [`adr_ci_env_export_flag.md`](./adr_ci_env_export_flag.md) — `--export-file` file-sink precedent
- **Adopted:** [purl spec](https://github.com/package-url/purl-spec) / [ECMA-427](https://ecma-international.org/publications-and-standards/standards/ecma-427/) (`pkg:oci` type) · [in-toto attestation spec](https://github.com/in-toto/attestation) (`ResourceDescriptor`) · [ECS process fields](https://www.elastic.co/docs/reference/ecs/ecs-process) (`process.executable`, `process.args`) · [OTel `host.arch`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/) (closed enum = OCI arch vocabulary) · [Conftest/OPA](https://github.com/open-policy-agent/conftest) (the consumer)
- **Open upstream questions we depend on:** [purl-spec#786](https://github.com/package-url/purl-spec/issues/786) (colon encoding) · [in-toto/attestation#28](https://github.com/in-toto/attestation/issues/28) (subject semantics) · [cosign#4019](https://github.com/sigstore/cosign/issues/4019) (subject with no local blob)
- **Precedent:** [pip installation report](https://pip.pypa.io/en/stable/reference/installation-report/) · [PEP 710](https://peps.python.org/pep-0710/) · [SLSA v1 provenance](https://slsa.dev/spec/v1.0/provenance) · [Bazel user manual](https://bazel.build/docs/user-manual) · [LLVM source-based coverage](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html) · [POSIX `write()`](https://pubs.opengroup.org/onlinepubs/9699919799/functions/write.html) · [append atomicity](https://nullprogram.com/blog/2016/08/03/)
- **Evaluated, rejected:** [OpenLineage](https://openlineage.io/docs/spec/examples/) · [CloudEvents](https://github.com/cloudevents/spec) · [W3C Trace Context](https://www.w3.org/TR/trace-context/) · [in-toto `runtime-trace`](https://github.com/in-toto/attestation/blob/main/spec/predicates/runtime-trace.md) · [SPDX 3.0 model](https://github.com/spdx/spdx-3-model)
