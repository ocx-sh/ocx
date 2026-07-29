# Research: Who Actually Consumes Execution / Provenance Records

<!--
Technology Landscape Research
Owner: worker-researcher (3 parallel consumer axes) + architect verification
Handoff to: adr_exec_resolution_record.md
-->

## Metadata

**Date:** 2026-07-26
**Domain:** packaging | security | observability
**Triggered by:** owner question on [#214](https://github.com/ocx-sh/ocx/issues/214) — *"which software is available to consume or analyze which format, and do they have additional conventions?"* A format is only worth adopting if real tooling reads it.
**Expires:** 2027-01 (tool support moves; cosign referrers migration and purl-spec#786 both in flight)
**Companion:** [`research_execution_record_formats.md`](./research_execution_record_formats.md) — *what the formats are*. This artifact is the reality check on that one, and it **reversed two of its conclusions**.

## Direct Answer

**Almost nothing consumes this.** Two research passes converged independently on the same conclusion: the realistic consumer of a per-invocation provenance record is **a small policy check over raw JSON** (Conftest/OPA, or `jq` in a shell), not a log pipeline and not the attestation ecosystem.

Consequences that changed the ADR:

| Earlier belief | Reality |
|---|---|
| "Shape it as an in-toto predicate now, wrap it later for free" | **Not free.** Every verifier additionally demands DSSE signing, an OCI *image* subject, a recognised `predicateType`, or an out-of-band layout. |
| "purl buys SBOM-tool interop and vulnerability lookup" | **Overstated.** `pkg:oci` is not vuln-queryable anywhere, and is inert or non-compliant in most SBOM tooling. |
| "OTel field names buy ingest compatibility" | **False.** No pipeline auto-recognises OTel or ECS names. Mapping is always required. |
| "Field naming is the ingest problem" | **Wrong layer.** The *file shape* is. All seven log shippers are line-oriented. |

## Axis 1 — Supply-chain / SBOM tooling and `pkg:oci`

| Tool | Handles `oci` purl type? | What it would actually do |
|---|---|---|
| **Trivy** (Aqua) | **Yes — the one genuine producer at scale** | Emits `pkg:oci/alpine@sha256:21a3de…?repository_url=index.docker.io/library/alpine&arch=amd64` in CycloneDX output. De facto reference implementation. But no import path for someone else's SBOM purl — this is *shape familiarity*, not an ingestion feature. |
| **Dependency-Track**, **Sonatype IQ**, **JFrog Xray** | Generically (type-agnostic parsers) | Store it, show it, **no vuln correlation** — `oci` is not an enriched ecosystem. Inert. |
| **GUAC** | Models OCI, but via a **non-spec shape** | Uses `namespace: "docker.io/library"` — which the purl `oci` type **explicitly prohibits** (registry belongs in `repository_url`). A spec-correct purl may not round-trip into GUAC's graph. Its OCI collector also historically rejected digest refs ([guac#1407](https://github.com/guacsec/guac/issues/1407)). |
| **OSV-Scanner / OSV.dev** | **No** | ~50-entry ecosystem enum, no OCI/container entry. `purl` is optional and *informational*; queries key on required `ecosystem`+`name`. **`pkg:oci` is not vuln-queryable, full stop.** |
| **deps.dev** (Google) | **No** | Fixed 7-type allowlist: cargo, gem, golang, maven, npm, nuget, pypi. |
| **Snyk** | **No** | Explicit SBOM-Test allowlist; `oci` absent → "analysis will be skipped". |
| **Grype** (Anchore) | Effective no-op | Decomposes images into constituent OS/language packages and matches *those*. A whole-artifact purl gets zero CVE matches. |
| **Syft** (Anchore) | Producer only, and unreliable | Does not reliably emit `pkg:oci` even for the image it scans ([syft#4595](https://github.com/anchore/syft/issues/4595)). |

**Nobody consumes a bare `ResourceDescriptor` array.** Every in-toto/SLSA/GUAC consumer requires the full `_type`/`subject`/`predicateType`/`predicate` Statement. An unwrapped list is a file to grep, not an ingestible artifact.

## Axis 2 — Attestation verifiers, and why the wrapper is not free

| Consumer | Accepts a custom `predicateType`? | What it demands beyond a valid Statement |
|---|---|---|
| **cosign `verify-attestation`** | Structurally yes — `--type custom` is the default, raw string match, no schema check | **DSSE signature** + the attestation attached to an **OCI image subject**. No documented path to verify a bare local unsigned file. `attest-blob` needs a real blob to hash; [cosign#4019](https://github.com/sigstore/cosign/issues/4019) asks for exactly our case (subject with no local blob) — **open, unimplemented**. |
| **slsa-verifier** | **No** — hardcoded to SLSA provenance families / VSA | Not pluggable. |
| **in-toto-verify / attestation-verifier** | With a layout | Requires a YAML layout defining supply-chain steps + per-step public keys. Self-described "must not be used in production". |
| **witness / Archivista** | No generic input mode | Attestors are Go-compiled and run *during* a wrapped command. Archivista stores **signed only**, by design. |
| **Kyverno / Sigstore policy-controller** | Via cosign | Thin wrappers over cosign verification, then CUE/Rego over the decoded predicate. Inherits the signed + OCI-image-subject constraint. |
| **GUAC** | **No** | Closed ingestor set: SPDX, CycloneDX, SLSA provenance, OSV, Scorecard, CSAF/OpenVEX, in-toto link/vuln. An unrecognised `predicateType` has no ingestor — invisible, not stored-but-unparsed. (Unsigned is *not* the blocker; unrecognised-type is.) |
| **Conftest / OPA** | **Irrelevant — and that is the point** | Generic structured-data policy runner. Any JSON in, decision out. **No signature, no predicateType, no OCI subject, no registry.** Works on our plain unsigned file today. |

### The subject problem

in-toto requires `subject: [{name, digest}]` — the artifact the predicate is *about*. Our record describes an execution that has produced nothing yet.

- **No precedent exists.** VSA's subject is the verified artifact; `test-result`'s is the source artifacts tested; **`runtime-trace` — the closest analog — still uses the produced build artifact** (`{"name": "ttl.sh/testin123", "digest": {...}}`), with trace data in the predicate body.
- Every consumer surveyed keys off "subject = digest of a thing that exists," because they all look up by OCI image digest or blob hash.
- Empty/placeholder subject: unaddressed by spec text, deferred to [in-toto/attestation#28](https://github.com/in-toto/attestation/issues/28), **open and unresolved**. No verifier treats an empty subject as legal.

**If OCX ever wraps**, the workable subject is the **resolved-environment digest** — an RFC 8785 JCS-canonical hash over the record's `packages` array. A real, hashable artifact existing at record-write time, tier-independent. `serde_json_canonicalizer` already computes `ocx.lock`'s `declaration_hash` this way (`crates/ocx_lib/src/project/hash.rs`), so the machinery exists.

**But this does not need deciding now** — one research pass argued "decide the subject today or face a breaking reshape." That is wrong for OCX's v1: since no Statement is emitted, adding one later is a **new optional output mode**, purely additive. Nothing breaks by deferring.

### Predicate type registration

No mandatory registry. Spec: *"New predicate types MAY be vetted… Your predicate is yours."* Vetting only gates inclusion in the official directory. Officially-vetted types live at `https://in-toto.io/attestation/<name>/<version>` with a resolving redirect; a self-minted `https://ocx.sh/predicates/exec/v1` is legal and need not resolve. **Registration changes nothing** — slsa-verifier is still hardcoded and GUAC still has no ingestor.

### Storage, if ever attached to a registry

OCI 1.1 **Referrers API + `artifactType`** is the current convention. The `sha256-<digest>.att` tag scheme is now explicitly a fallback for registries that 404 the referrers API. cosign is mid-migration ([cosign#4335](https://github.com/sigstore/cosign/issues/4335)).

## Axis 3 — Log pipelines and SIEM

### The finding that matters: every shipper is line-oriented

Tested against our sink shape (a directory of complete, one-JSON-document-per-file records):

| Pipeline | Whole-file JSON doc? | Evidence |
|---|---|---|
| OTel Collector `filelog` | **No** — line-based; `json_parser` breaks on multi-line | [contrib#21893](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/21893) |
| OTel Collector `otlpjsonfile` | Whole file, but **only** the OTLP protobuf-JSON envelope shape | receiver README |
| Vector `file` source | **No** — maintainers: "not a good way to read the whole contents of a file at once" | [vector#21261](https://github.com/vectordotdev/vector/discussions/21261) |
| Fluent Bit `tail` | **No** — line-based; multi-line JSON needs a regex workaround | [fluent-bit#8232](https://github.com/fluent/fluent-bit/issues/8232) |
| Fluentd `in_tail` | **No** — documented: "the file as a whole is not valid JSON" | docs.fluentd.org |
| Filebeat `filestream` | **No** — "JSON decoding only works if there is one JSON object per message" | Elastic docs |
| Grafana Alloy / Promtail | **No** — inherits line-based `json` stage | Grafana docs |
| Splunk UF | **Yes, with config** — `SHOULD_LINEMERGE=0` + `LINE_BREAKER` + `INDEXED_EXTRACTIONS=JSON` | Splunk docs |

→ **The actionable fix is compact single-line JSON per file**, so "read whole file" degenerates to "read one line". Free, and it is the entire ingest story. Changing *field names* does nothing here.

### No pipeline auto-recognises OTel or ECS names

That capability does not exist generically. Filebeat's ECS-awareness comes from its own bundled modules (syslog, docker, …), not from inspecting arbitrary JSON. **Explicit mapping is required regardless of vocabulary** — roughly 10–30 lines of pipeline config per shipper, plus field renames on top for ECS-specific consumers (Elastic Security detection rules hardcode `process.args`, `host.architecture`).

### ECS ↔ OTel divergence (current, live-verified 2026-07-26)

| Concept | ECS | OTel | Same? |
|---|---|---|---|
| args | `process.args` | `process.command_args` | **name differs** |
| executable | `process.executable` = **flat keyword string** | `process.executable.{path,name}` = **object** | **shape differs — not reconcilable** |
| host arch | `host.architecture` | `host.arch` | **name differs** |
| pid | `process.pid` | `process.pid` | same |
| working dir | `process.working_directory` | `process.working_directory` | same |
| command line | `process.command_line` | `process.command_line` | same name; OTel prefers `command_args` |
| os type | `os.type` | `os.type` | same |

Convergence status: ECS was donated to OTel in April 2023 with OTel semconv as intended successor, but Elastic's own docs state *"in some areas convergence is not achievable due to conceptual differences."* **Not a completed merge.**

### Type discriminator and timestamp conventions

- **Discriminator** — three incompatible shapes: ECS uses a 4-level hierarchy (`event.kind`/`category`/`type`/`outcome`, category and type are *arrays*); OCSF uses numerics (`class_uid`, `category_uid`, `type_uid = class_uid*1000 + activity_id`); CloudEvents uses a single reverse-DNS `type` string.
- **Timestamp** — the one field where the name genuinely matters: **ECS mandates `@timestamp`**, and Elasticsearch/Kibana default index templates and time views key on that literal name. OCSF's `time` and CloudEvents' `time` carry no equivalent tooling payoff.
- **File extension / directory manifest** — no dominant convention in the observability world; the one-doc-per-file pattern belongs to the supply-chain world, where consumers address artifacts by reference/digest rather than tailing directories.

## Key Findings

1. **The realistic consumer is Conftest/OPA or `jq`** — reached independently by the attestation and SIEM passes. It asks nothing of us beyond valid JSON: no signing key, no registry, no layout, no recognised type. A user can gate CI on approved digests with three lines of Rego, today.
2. **The in-toto wrapper unlocks no consumer without also shipping signing + subject + registry attachment.** It buys a mechanical rename later, nothing more. One pass recommended wrapping as "highest payoff by far" — it asserted the payoff; the other pass tested each named beneficiary and found every one blocked. **The tested pass wins.**
3. **`pkg:oci` is identity, never scanning.** OSV, deps.dev, Snyk and Grype all fail to resolve CVEs against a whole-artifact OCI purl. If CVE visibility into packaged tools is ever a goal, it needs the tool's *upstream ecosystem* purl carried alongside — a separate feature.
4. **OCX would be more spec-correct than GUAC** on `pkg:oci` (GUAC populates the prohibited `namespace`). Correctness here is not a compatibility win.
5. **OTel field names earn nothing on ingest** and cost an unresolvable ECS conflict, mixed key casing, and a release-candidate pin. Dropped.
6. **File shape, not vocabulary, is the ingest lever.** Compact single-line JSON.
7. **Counter-case, stated honestly:** large orgs often mandate that *all* audit-relevant output route to a central SIEM for retention regardless of query fit. In that world OTel-familiar names help whoever writes the ingest mapping — a real but modest benefit, and not the automatic compatibility the question was probing for. No evidence was found of SIEM pipelines being used for digest-approval gating; every OCSF/ECS source frames itself around detection and threat-hunting.

## Recommendation

1. **Document Conftest/OPA as the consumer story**, with a worked Rego example gating on approved digests. This is the one path a user can walk next week with no infrastructure.
2. **No in-toto Statement in v1.** Defer the subject question — it is additive, not breaking.
3. **State plainly in docs that `pkg:oci` gives identity, not vulnerability lookup.** Prevents a promise we cannot keep.
4. **Compact single-line JSON per file.**
5. **Do not adopt OTel or ECS vocabulary.** Plain camelCase ocx names; revisit `@timestamp` only if SIEM routing becomes a stated requirement.
6. **Target the Referrers API + `artifactType`** if these are ever pushed to a registry, not the legacy `.att` tag scheme.

## Sources

| Source | Type | Relevance |
|---|---|---|
| [Sigstore attestation docs](https://docs.sigstore.dev/cosign/verifying/attestation/) · [cosign#4019](https://github.com/sigstore/cosign/issues/4019) · [cosign#4335](https://github.com/sigstore/cosign/issues/4335) | Docs/Issues | DSSE + image-subject requirement; no-local-blob gap; referrers migration |
| [slsa-verifier](https://github.com/slsa-framework/slsa-verifier) · [in-toto/attestation-verifier](https://github.com/in-toto/attestation-verifier) | Repo | Hardcoded types; layout requirement |
| [witness attestors](https://witness.dev/docs/docs/concepts/attestor/) · [GUAC components](https://docs.guac.sh/guac-components/) | Docs | Compiled attestor model; closed ingestor set |
| [in-toto predicate.md](https://github.com/in-toto/attestation/blob/main/spec/v1/predicate.md) · [new_predicate_guidelines.md](https://github.com/in-toto/attestation/blob/main/docs/new_predicate_guidelines.md) · [attestation#28](https://github.com/in-toto/attestation/issues/28) | Spec/Issue | Registration optional; subject question unresolved |
| [runtime-trace predicate](https://github.com/in-toto/attestation/blob/main/spec/predicates/runtime-trace.md) | Spec | Closest analog; still uses build artifact as subject |
| [Conftest](https://github.com/open-policy-agent/conftest) | Repo | The one format-agnostic consumer |
| [OSV schema](https://ossf.github.io/osv-schema/) · [deps.dev API proto](https://github.com/google/deps.dev/blob/main/api/v3/api.proto) · [Snyk SBOM docs](https://docs.snyk.io/snyk-api/reference/sbom) | Schema/Docs | No OCI ecosystem / type allowlists |
| [syft#4595](https://github.com/anchore/syft/issues/4595) · [trivy#9398](https://github.com/aquasecurity/trivy/pull/9398) · [guac#1407](https://github.com/guacsec/guac/issues/1407) | Issues | Producer gaps; GUAC OCI path immaturity |
| [Red Hat purl guidelines](https://redhatproductsecurity.github.io/security-data-guidelines/purl/) | Guidelines | Unencoded-colon corpus evidence |
| [ECS process fields](https://www.elastic.co/docs/reference/ecs/ecs-process) · [ECS & OpenTelemetry](https://www.elastic.co/docs/reference/ecs/ecs-opentelemetry) · [OTel semconv process](https://opentelemetry.io/docs/specs/semconv/registry/attributes/process/) | Spec | Divergence table |
| [contrib#21893](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/21893) · [vector#21261](https://github.com/vectordotdev/vector/discussions/21261) · [fluent-bit#8232](https://github.com/fluent/fluent-bit/issues/8232) · [Fluentd in_tail](https://docs.fluentd.org/input/tail) · [Filebeat filestream](https://www.elastic.co/docs/reference/beats/filebeat/filebeat-input-filestream) | Docs/Issues | Line-oriented shipper evidence |
| [OCSF Process Activity 1007](https://schema.ocsf.io/1.5.0/classes/process_activity) | Schema | Required `cloud`/`osint` blocks; EDR-shaped |
