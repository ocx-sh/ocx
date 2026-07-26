# Research: Execution / Provenance Record Formats

<!--
Technology Landscape Research
Owner: worker-researcher (3 passes) + architect verification
Handoff to: adr_exec_resolution_record.md
-->

## Metadata

**Date:** 2026-07-26
**Domain:** packaging | security | observability
**Triggered by:** [#214](https://github.com/ocx-sh/ocx/issues/214) — managed config option to log package digests at invocation time. ADR: [`adr_exec_resolution_record.md`](./adr_exec_resolution_record.md)
**Expires:** 2027-01 (purl/ECMA-427 stable; OTel semconv `process.*` is release-candidate and `host.*`/`os.*` are Development — re-verify those names first)
**Companion:** [`research_execution_record_consumers.md`](./research_execution_record_consumers.md) — *which software actually reads these formats*. Read that one before trusting any "we can adopt X" conclusion here.

## Direct Answer

**Question:** OCX wants to write one JSON record per tool invocation, naming the resolved packages (identifier + digest) that composed the execution environment. Is there an adopted standard to emit, rather than inventing a format?

**Answer:** No standard describes "a command invocation plus the resolved software that composed its environment." Verified by search across supply-chain, observability and SIEM schema families. Three *vocabularies* are worth borrowing at the field level; the envelope must be ours.

| Layer | Verdict |
|---|---|
| Per-package identity string | **Adopt** purl, type `oci` — ECMA-427 standard, exact semantic match |
| Per-package entry shape | **Adopt** in-toto `ResourceDescriptor` (`name`/`uri`/`digest`/`annotations`) |
| Process/host/os fields | **Considered and dropped** — see "The OTel reversal" below |
| Record envelope | **Invent** — nothing to adopt |
| Attestation wrapper (in-toto `Statement`) | **Defer** — buys nothing without signing + subject + registry attachment |

## Technology Landscape

### Established (adopt or borrow from)

| Standard | Status | Notes |
|---|---|---|
| **purl (Package URL)** | **ECMA-427, 1st edition, December 2025** — an international standard, no longer only a GitHub spec | The `oci` type fits OCX with zero impedance: purl **version *is* the sha256 digest**, and `tag` is documented as *"artifact tag that may have been associated with the digest at the time"* — precisely OCX's digest-is-identity / tag-is-advisory model. Qualifiers: `repository_url`, `tag`, `arch`. Namespace **prohibited** for this type. |
| **in-toto attestation** (`Statement`, `ResourceDescriptor`) | Stable, and the genuine interoperability layer of the supply-chain space (Sigstore, Tekton Chains, GUAC all consume it) | `ResourceDescriptor` = `{name, uri, digest{alg:hex}, annotations, downloadLocation, mediaType, content}`. SLSA's `resolvedDependencies` is an array of these. |
| **SLSA v1 provenance** | Stable | `buildDefinition.resolvedDependencies` is the closest *semantic* match to what OCX records — but only ever exists inside a full Statement. |
| **Elastic Common Schema (ECS)** | Mature, donated to OTel 2023, convergence explicitly **incomplete** | Elastic's own docs: *"in some areas convergence is not achievable due to conceptual differences."* |
| **OpenTelemetry semconv** | `process.*` = release-candidate; `host.*`/`os.*` = Development | Names are documented and versioned but **not frozen upstream**. |

### Emerging (watch, do not adopt)

| Standard | Signal | Why not yet |
|---|---|---|
| **OCSF** | Real and growing — AWS Security Lake, Splunk, CrowdStrike, 200+ orgs | Process Activity (`class_uid: 1007`) **requires** `cloud` and `osint` blocks plus alert/disposition/malware/attack fields. Assumes a security-agent producer. A package manager would stub half of it. |
| **SPDX 3.0 build/runtime profiles** | Active development | JSON-LD `@context`/`@type`/`@id` ceremony, disproportionate for one process. |
| **CycloneDX formulation / declarations** | Added 1.5/1.6, real schema | CI/CD-pipeline shaped (`workflow` → `task` → `step`). Heavier than one invocation. |
| **SCITT** (RFC 9943, June 2026) | IETF WG, real | A transparency-*service* architecture (append-only ledger, COSE), not a record schema. Needs a backend we do not have. |

### Rejected outright

| Candidate | Reason |
|---|---|
| **OpenLineage** (LF AI & Data; Airflow/dbt/Spark) | Object model is dataset lineage — `run`/`job`/`inputs[]`/`outputs[]` as datasets in a DAG. Packages are not pipeline inputs; a real consumer would try to trace *data flow through them*. Its typed-namespaced `facets` pattern is good prior art, the top-level model actively misleads. |
| **CloudEvents** (CNCF graduated 2024-01) | A transport envelope, and we have no transport. Every shipped use is "event on a bus"; no precedent as an at-rest record format. Implies a `type`/`source` routing contract no consumer reads. |
| **W3C Trace Context env propagation** | The env-carrier spec is OTel release-candidate and does not even mandate the variable name. No build tool (Bazel, Gradle, Tekton, Buildkite, GH Actions) ships documented `TRACEPARENT` child-process linkage — tutorials only. |
| **in-toto `runtime-trace`** | **Exists** (verified — `spec/predicates/runtime-trace.md`, alongside `provenance.md`, `scai.md`, `vsa.md`). But it is syscall-level tracing (`monitoredProcess`, `monitorLog.{process,network,fileAccess}`, Tetragon/Tekton use case), not environment composition. Wrong subject. *One research pass reported this file as "not findable" — that was wrong; confirmed present by directory listing.* |

## Design Patterns Worth Considering

- **In-band schema version, bumped only for breaks** — pip's installation report uses a top-level **string** `"version": "1"`, documented as changing *"only if and when backward incompatible changes are introduced."* No minor churn, no additive bumps. Adopted.
- **One file per invocation, not an append log** — Bazel (`--execution_log_json_file`) and LLVM (`LLVM_PROFILE_FILE` with `%p`/`%h`/`%Nm`) independently arrived at per-process filenames. LLVM's docs state the rationale outright: the specifiers exist *"to avoid corruption due to concurrency."* Adopted.
- **Path templating with a closed specifier set** — LLVM's `%p` (pid), `%h` (hostname), `%t` (TMPDIR), `%Nm` (merge pool), `%c` (continuous sync). Note `%Nm`/`%c` carry *behaviour*, not just substitution — a filename grammar that encodes behaviour is a contract to regret. Adopt substitution only.
- **Persisted-queryable-state instead of per-run records** — npm (`package-lock.json`), conda (`conda-meta/history` + `conda list --explicit`), Nix (`manifest.json`), Guix (`guix describe`), Docker (`RepoDigests`). The dominant pattern outside Python. Conda splitting the job across a command log *and* a separate query tool is an anti-pattern worth avoiding: one self-contained record instead.

## Key Findings

1. **No portable, spec-backed guarantee exists for concurrent append — anywhere.** [POSIX `write()`](https://pubs.opengroup.org/onlinepubs/9699919799/functions/write.html) guarantees non-interleaving only for **pipes/FIFOs** at ≤`PIPE_BUF`; for regular files under `O_APPEND` it guarantees only that the seek-to-end and write are one operation, *not* that the write is atomic against other writers. The common "O_APPEND is PIPE_BUF-safe on regular files" belief is folklore. NFS is strictly worse — no append opcode, so the client simulates it by reading size then writing. On Windows, `FILE_APPEND_DATA` *is* a real atomic-append primitive but the CRT's `O_APPEND` emulation does not inherit it. **This is the single decisive constraint on sink design.**
2. **purl was standardised as ECMA-427 in December 2025.** Materially strengthens adoption; the GitHub repo is now the maintenance vehicle for a standard.
3. **The sha256 colon encoding is genuinely unresolved.** purl's canonical build rule says "append the percent-encoded version" (→ `sha256%3A…`), and the `oci` type doc's own four examples **contradict each other** (three encoded, one not). [purl-spec#786](https://github.com/package-url/purl-spec/issues/786) is **open and unresolved**. Trivy and Red Hat's security-data guidelines both emit **unencoded**. → **Emit unencoded**, matching the higher-volume real corpus, and document the choice so nobody "fixes" it later.
4. **No standard predicate exists for runtime environment composition.** Searched SLSA predicates, the in-toto predicate directory, CycloneDX formulation, SPDX 3.0 build profile. Unclaimed territory.
5. **ECS and OTel conflict *structurally*, not just in naming.** `process.executable` is a **keyword string** in ECS (`/usr/bin/ssh`) and an **object** (`.path`, `.name`) in OTel. Also `process.args` vs `process.command_args`, `host.architecture` vs `host.arch`. Identical: `process.pid`, `process.working_directory`, `process.command_line`, `os.type`. **You cannot satisfy both** — an Elastic ingest of the OTel object form is a mapping type-conflict, not a missing field.
6. **`@timestamp` is the one field name with real payoff.** Elasticsearch/Kibana default index templates and time-based views key on that literal name. The `process.*`/`host.*`/`os.*` names have no equivalent payoff anywhere found.
7. **Digest shape splits ~50/50** across specs: map form (in-toto `{"sha256":"hex"}`, CycloneDX `{"alg","content"}`, SPDX `{"algorithm","checksumValue"}`) vs prefixed string (pip `"sha256=hex"`, Nix `"sha256-<b64>"`, **OCI descriptors `"sha256:<hex>"`**). Key-naming style: **camelCase dominates** the provenance cluster (in-toto, SLSA, CycloneDX, SPDX, OpenLineage); snake_case appears only in Python-ecosystem tools; CloudEvents is the outlier (flat lowercase).
8. **Kubernetes `imageID` is the cautionary tale.** Format inconsistent across CRI implementations for a decade (`docker-pullable://` prefixing, multi-arch index digest vs platform manifest digest), still being unwound via a dedicated CRI field. **Lesson: define the digest format once, precisely, and never let a transport prefix into it.**
9. **`pkg:oci` cannot express index-vs-manifest digest.** The `arch` qualifier is a *hint*, not a guarantee; nothing in the format distinguishes a multi-arch index digest from a platform manifest digest without dereferencing the registry. The Kubernetes ambiguity carries straight into purl, unsolved.
10. **Annotation keys have no convention to violate.** No registry or reserved-prefix rule for `ResourceDescriptor.annotations`. Nearby precedents are flat, not reverse-DNS: SLSA extension fields use `<vendor>_<fieldname>`; CycloneDX properties use `namespace:name`. **OCX decision: keep `sh.ocx.*`** — the entries describe OCI artifacts, where reverse-DNS *is* the convention (`org.opencontainers.image.*`), and the codebase already ships `sh.ocx.layer.*` annotations and `application/vnd.sh.ocx.*` media types. Internal consistency wins a tie with nothing at stake.

## The OTel reversal (a finding that changed the decision)

The first pass recommended adopting OTel `process.*`/`host.*`/`os.*` names as the single strongest compatibility play. The consumer pass ([companion artifact](./research_execution_record_consumers.md)) killed it:

- **No pipeline auto-recognises OTel or ECS field names.** That capability does not exist generically anywhere. Filebeat's ECS-awareness lives in its own bundled modules, not in inspecting arbitrary JSON. **Every consumer needs an explicit mapping regardless of what the fields are called.**
- Therefore OTel names buy **zero** ingest advantage over ECS names, invented names, or SLSA-shaped names.
- Cost of adopting them: the ECS structural conflict (finding 5), mixed key casing in one document, a pin to a release-candidate spec, and a `semconv` version field to maintain.

**Net: dropped.** Three complications for marginal human legibility, in service of consumers that do not exist for this record. What *does* help ingest is the **file shape** — see companion finding on line-oriented log shippers.

## Recommendation

1. **purl `pkg:oci` for package identity**, emitted with an **unencoded** `sha256:` colon. Type `oci`, never an invented `pkg:ocx` — OCX packages *are* OCI artifacts. Qualifiers limited to `repository_url`, `tag` (only when one was genuinely resolved), `arch`, per the spec's own "keep qualifiers minimal" guidance.
2. **in-toto `ResourceDescriptor`** for each package entry, digest as the **map** form (`{"sha256": "<hex>"}`) — mandated by the shape, and the OCI string form survives inside the purl anyway.
3. **Plain camelCase ocx names** for the envelope and invocation block. Do not borrow OTel or ECS vocabulary. Consider `@timestamp` if Elastic ingest ever becomes a stated requirement.
4. **Compact single-line JSON, one document per file.** The only change that materially improves pipeline ingest (see companion).
5. **No in-toto `Statement` wrapper in v1.** It unlocks no consumer without signing + a subject convention + registry attachment. Adding it later is *additive* (a new optional output mode), not a reshape.
6. **`sh.ocx.*` annotation prefix.**
7. **Rust dependency, if a purl builder is taken:** `packageurl` v0.7.0 (scm-rs), released 2026-07-22, MIT (clean against `deny.toml`), ~33.5k downloads/month, serde support. Alternative `purl` (phylum-dev) v0.1.6 is stale (>1yr) though more widely vendored. Neither validates `oci`-type shape — OCX still owns qualifier set, ordering and casing. Hand-rolling the string instead would be the "own a wire format" anti-pattern `quality-core.md` blocks.

## Sources

| Source | Type | Relevance |
|---|---|---|
| [purl-spec](https://github.com/package-url/purl-spec) · [oci type definition](https://github.com/package-url/purl-spec/blob/main/docs/types/definitions/oci-definition.md) · [ECMA-427](https://ecma-international.org/publications-and-standards/standards/ecma-427/) | Spec | Adopted identity format; ECMA-427 1st ed. Dec 2025 |
| [purl-spec#786](https://github.com/package-url/purl-spec/issues/786) | Issue (open) | Colon-encoding ambiguity, unresolved |
| [in-toto attestation spec](https://github.com/in-toto/attestation) · [predicates dir](https://github.com/in-toto/attestation/tree/main/spec/predicates) | Spec | `ResourceDescriptor`, `Statement`, predicate list |
| [SLSA v1 provenance](https://slsa.dev/spec/v1.0/provenance) | Spec | `resolvedDependencies`; `<vendor>_<field>` extension convention |
| [OTel semconv — process](https://opentelemetry.io/docs/specs/semconv/registry/attributes/process/) · [host](https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/) | Spec | Evaluated, dropped; stability levels |
| [ECS process fields](https://www.elastic.co/docs/reference/ecs/ecs-process) · [ECS & OpenTelemetry](https://www.elastic.co/docs/reference/ecs/ecs-opentelemetry) | Spec | Structural conflict with OTel; convergence incomplete |
| [pip installation report](https://pip.pypa.io/en/stable/reference/installation-report/) · [PEP 710](https://peps.python.org/pep-0710/) | Docs | Schema-version discipline; per-install provenance precedent |
| [Bazel user manual](https://bazel.build/docs/user-manual) · [execlog README](https://github.com/bazelbuild/bazel/blob/master/src/tools/execlog/README.md) · [bazel#14209](https://github.com/bazelbuild/bazel/issues/14209) | Docs/Issue | Per-invocation record precedent; the not-valid-JSON cautionary tale |
| [LLVM source-based coverage](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html) | Docs | `LLVM_PROFILE_FILE` specifier set and its concurrency rationale |
| [POSIX `write()`](https://pubs.opengroup.org/onlinepubs/9699919799/functions/write.html) · [append atomicity](https://nullprogram.com/blog/2016/08/03/) | Spec/Analysis | Finding 1 — the decisive sink constraint |
| [OCSF Process Activity 1007](https://schema.ocsf.io/1.5.0/classes/process_activity) | Schema | Evaluated, rejected |
| [OpenLineage examples](https://openlineage.io/docs/spec/examples/) · [CloudEvents spec](https://github.com/cloudevents/spec) · [W3C Trace Context](https://www.w3.org/TR/trace-context/) · [SPDX 3.0 model](https://github.com/spdx/spdx-3-model) | Spec | Evaluated, rejected |
| [packageurl crate](https://lib.rs/crates/packageurl) · [purl crate](https://lib.rs/crates/purl) | Crate | Rust dependency options |
