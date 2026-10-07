# Research: Interface vocabulary and its enforcement

## Metadata

**Date:** 2026-10-03
**Domain:** cli
**Triggered by:** OCX interface contract ADR — coherence vocabulary + lints
**Expires:** 2027-04-03

Method note: sources were read via a summarising fetcher; wording is paraphrased and some counts
and dates are marked "unverified". Nothing was run locally.

## Direct Answer

Mature projects get a consistent machine vocabulary in one of three ways, in descending strength:

1. **Make the inconsistent form unwritable.** The vocabulary is a typed source of truth and the
   wire format is generated from it (protobuf with `buf`, TypeSpec, Kubernetes Go types,
   Cargo's config-key to env-var mapping). Consistency is a by-product of the generator.
2. **Lint the source types, not the output.** Google api-linter (on `.proto`),
   kube-api-linter (on Go types), buf lint. The linter sees the author's intent (`+optional`,
   field name, Rust-type-equivalent), so false positives are low and fixes are suggestible.
3. **Prose guideline plus human review.** Microsoft Graph, clig.dev, 12-factor, the Kubernetes
   conventions doc before kube-api-linter. This drifts; the drift is documented in the wild
   (Docker, gh, GitLab CLI below).

Linting the generated JSON Schema with a generic tool (Spectral, `sourcemeta/jsonschema`) is the
weakest of the enforcement options for vocabulary: generic schema linters check schema hygiene
(anti-patterns, missing `description`), not domain vocabulary, and the generated artifact has
already lost the intent that makes a lint precise. It still earns a place as a cheap secondary
gate for the properties only visible in the output (naming regex over every property, required
`description`, `format` on every digest/time field).

For OCX the best-fit combination: shared concept types in Rust (compile-time, tier 1), a small
number of project-specific lints in tests that walk the generated schema (tier 2 equivalent,
because schemars preserves enough structure), a committed snapshot of the schema, and an
`oasdiff`-style breaking-change diff over successive schemas. Do not adopt a prose-only guide.

## Findings

### Google AIPs and api-linter (mechanically enforced; the strongest precedent)

- AIP-140 field names: `lower_snake_case`; repeated fields plural; no prepositions; adjective before
  noun; booleans omit `is_`; `uri` vs `url`. https://google.aip.dev/140
- AIP-142 time: `google.protobuf.Timestamp`, names end `_time`; durations use the `Duration`
  type; relative offsets `_offset`; legacy integers carry a unit suffix. https://google.aip.dev/142
- AIP-129: server-owned values get an `effective_` twin and `OUTPUT_ONLY`; the type
  `google.api.field_info` marks formats (UUID, IPv4, IPv6, email). https://google.aip.dev/129
- AIP-180 backward compatibility: renames count as remove-plus-add; type changes are breaking even
  when wire-compatible; new values in response enums need caution. https://google.aip.dev/180
- api-linter (Go, ~768 stars): rule ids are AIP-numbered (`core::0140::lower-snake`). Core and
  client-library rules default on, cloud rules default off. Disable by CLI flag, config
  (`included_paths` / `excluded_paths`), or an inline `(-- api-linter: rule=disabled --)` comment
  with field-level granularity. https://linter.aip.dev/configuration
- Project states that it does not follow semver because "the addition or correction of virtually
  any rule is breaking"; minor releases add rules. Recent: v2.0.0 (2025-10-22, protobuf-go
  migration), v2.1.0 (2025-12-10), v2.2.0 (2026-01-22), v2.4.0 (2026-09-10).
  https://github.com/googleapis/api-linter
- What it catches: naming, plurality, missing standard fields, wrong method shapes: everything
  expressible about a field's name or type. What it cannot catch: semantics (is this really an
  `effective_` value?), and the docs say it "should not be a substitute" for reading the AIPs.

### Kubernetes API conventions and kube-api-linter (prose, then mechanised)

- Conventions doc: CamelCase constants (`ClusterFirst`), camelCase fields, units in duration names
  (`timeoutSeconds`), `{field}Ref` for references, PascalCase short condition types (`Ready`),
  explicit `+optional` / `+required` with pointers and `omitempty` for optional.
  https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md
- kube-api-linter (kubernetes-sigs, ~141 stars, no tagged releases, pseudo-versions): the
  conventions doc turned into a Go analyzer. 31 linters listed in its docs, 22 on by default
  (count taken from the linters table; its own heading says 39). Examples: `jsontags`
  (camelCase regex), `notimestamp` (Timestamp to Time, with auto-fix), `nodurations` (use `fooSeconds`),
  `nonullable`, `optionalorrequired`, `conditions`, `noreferences` (Ref/Refs),
  `namingconventions` (configurable regex with Inform/Drop/Replacement). Runs as a standalone
  binary or a golangci-lint module. https://github.com/kubernetes-sigs/kube-api-linter
- Its stated purpose is to automate the mechanical share of API review so humans spend time on
  design. Adoption signals: enabled in Kubernetes' own golangci config; a downstream project
  opened an integration issue 2026-03-04; a conference talk 2025-11-20 (via search result,
  unverified). Note the lag: the conventions doc is older than the linter by years, and the
  linter's default-off set (9 of 31) shows rules the maintainers could not make
  false-positive-free for every consumer.
- kubectl: scripts should use `-o json|yaml|name|jsonpath` and fully qualified API versions;
  no cross-command schema guarantee beyond "it is the API object".
  https://kubernetes.io/docs/reference/kubectl/conventions/

### Zalando guidelines, Zally, Spectral rulesets (prose plus linter, OpenAPI-shaped)

- Zalando rule set (RFC 2119 MUST/SHOULD): snake_case properties (Rule 118, regex
  `^[a-z_][a-z_0-9]*$`), kebab-case paths (129), snake_case query params (130),
  UPPER_SNAKE enums (240), `date-time` UTC format (169), time property names contain
  date/time or end `_at` (235), common field `id`/`xyz_id`/`etag` (174), booleans never null
  (122), absent and null must mean the same (123), empty array is `[]` not null (124), arrays
  plural (120). https://opensource.zalando.com/restful-api-guidelines/
- Rules 122-124 are the best-specified optional-versus-null answer found. Rule 100 ties the
  guideline to the Zally linter. Zally: ~946 stars, 2,891 commits, Kotlin/Java custom rules; a
  Spectral ruleset translation exists. Status of Zally vs the Spectral ruleset (archived or
  not) was not confirmed. https://github.com/zalando/zally

### Microsoft Graph and Azure guidelines (prose and Spectral; split by team)

- Graph: lowerCamelCase for all names; date and time properties suffixed Date/Time/DateTime
  (`createdDateTime`); booleans prefixed `is`; singular type names, plural collections; non-
  compliance must be justified at API review. No linter named in the document.
  https://github.com/microsoft/api-guidelines (Graph guidelines)
- Azure: camelCase JSON fields, kebab-case headers, extensible enums unless the set will never
  change, list responses are an object with a top-level array and `nextLink` absent (or null) on
  the last page. Enforced by `azure-api-style-guide` (a Spectral ruleset of selected `oas`
  rules plus custom rules; docs say service teams found it "very useful") and by
  `azure-openapi-validator` (53 stars, 407 commits, 149 open issues, lintdiff mode that only
  reports new violations). https://github.com/Azure/azure-api-style-guide
  https://github.com/Azure/azure-openapi-validator
- "lintdiff" (report only violations introduced by the change) is the adoption pattern for a
  legacy surface; buf calls the same thing "ignore what exists, enforce going forward".

### clig.dev (prose only; no enforcement)

- Machine output: "Display output as formatted JSON if `--json` is passed"; offer `--plain`
  for line-oriented output; `-q`; standard flag names (`-a/--all`, `-f/--force`, `-n/--dry-run`,
  `--json`, `-h/--help`); full-length form of every flag; one-letter flags only for common
  ones. Env vars: uppercase letters, digits, underscore only; honour `NO_COLOR`, `HTTP_PROXY`,
  `TMPDIR`; do not read secrets from env. Precedence: flags, env, project config, user config,
  system config. Exit codes: zero success, non-zero mapped to important failure modes.
  https://clig.dev/

### gh CLI and kubectl (de facto vocabulary imported from the backend)

- gh: `--json <fields>` + `--jq` + `--template`; fields enumerated by running the command without
  an argument, which makes the allowlist discoverable and gives a built-in "unknown field"
  rejection. `gh pr list` exposes 34 fields, all camelCase, imported from GraphQL names:
  `createdAt`/`mergedAt` (`*At` timestamps) but `closed` (no `is`) beside `isDraft`,
  `isCrossRepository`; `mergeable`, `state`, `headRefName`, `baseRefOid`. Flags are
  kebab-case with short letters (`-A`, `-B`, `-L`, `-R`, `-S`). https://cli.github.com/manual/gh_pr_list
  https://cli.github.com/manual/gh_help_formatting
  Booleans mix `is`/no-`is`: the vocabulary is imported, not designed.
- Docker: Go-struct field names leak into `--format json`. In v29 the output of some commands
  changed key casing (`ApiVersion` to `APIVersion`), and
  [docker/cli#6727](https://github.com/docker/cli/issues/6727) (opened 2025-12-29, still open)
  shows `docker version --format '{{json .}}'` mixing a human date
  (`Wed Oct 8 12:18:19 2025`) and RFC 3339 for the same `BuildTime` concept inside one document;
  VS Code Dev Containers crashed on it.
- GitLab CLI: replacing the raw API response with an internal struct changed `access_level` to
  `AccessLevel`, a breaking change for parsers, fixed by adding struct tags.
  https://gitlab.com/gitlab-org/cli/-/merge_requests/2859/commits
- These three are the clearest evidence of what happens with no enforcement: breakage came from a
  refactor of internal types, not from a deliberate interface decision. That is directly relevant
  to a Rust CLI where JSON is serialised from internal types.

### Environment variable conventions (naming consistent by construction)

- Cargo: config key to env var by mechanical rule (`.` and `-` to `_`, uppercase,
  `CARGO_` prefix); same merge semantics across file, env and `--config`; precedence is
  `--config`, then env, then files. https://doc.rust-lang.org/cargo/reference/config.html
- uv: `UV_` prefix; each variable documented as "equivalent to" a flag; positive and
  negative booleans both exist (`UV_COMPILE_BYTECODE` and `UV_NO_CACHE`); deliberate
  pass-through of ecosystem names (`NO_COLOR`, `SSL_CERT_FILE`, `VIRTUAL_ENV`, `RUST_LOG`).
  https://docs.astral.sh/uv/reference/environment/
- mise: setting `jobs` becomes `MISE_JOBS`; settings carry types and defaults, which drives schema
  generation; "early init" settings are env-only. https://mise.jdx.dev/configuration/settings.html
- Terraform: `TF_` prefix, `TF_VAR_name`, `TF_CLI_ARGS[_cmd]`; precedence flags over env.
  https://developer.hashicorp.com/terraform/cli/config/environment-variables
- 12-factor: env vars as granular, orthogonal controls, never grouped into named environments.
  https://12factor.net/config

### protobuf / buf (enforced by a generator-side linter; the closest precedent to schemars)

- `buf lint` default `STANDARD` ruleset with no config; `buf breaking` with `WIRE`,
  `WIRE_JSON`, `FILE` strictness levels; the recommended adoption path is to ignore existing
  violations and enforce going forward. https://buf.build/docs/lint/
- Two separate tools, one for style and one for compatibility, is the shared structure of buf and
  of `oasdiff` (OpenAPI breaking-change detector, ~1.4k stars, OpenAPI 3.1/3.2 support,
  GitHub Action) https://github.com/oasdiff/oasdiff

### Linting JSON Schema itself and linting generated schemas

- Spectral (~3.2k stars, now SmartBear/Stoplight): a generic JSON/YAML linter driven by JSONPath
  `given` plus functions (pattern, casing, schema, truthy). Built-in rulesets target OpenAPI,
  AsyncAPI and Arazzo; for a bare JSON Schema document you write your own ruleset. The
  release cadence in the fetched releases page ended v6.17.0 (2024-10-01); the 2026 date
  seen in a search aggregator for v6.16.x looks mis-labelled, so treat 2026 activity as
  unverified. Spectral needs Node: a new toolchain dependency for a Rust repo.
  https://github.com/stoplightio/spectral
- `sourcemeta/jsonschema lint`: a CLI that detects JSON Schema anti-patterns, full support for
  2020-12, ~307 stars; docs read did not show custom rules. Native binary.
  https://github.com/sourcemeta/jsonschema
- Linting wins at the typed source: kube-api-linter lints Go types, not
  the CRD schema; Azure's TypeSpec flow lints the TypeSpec source (rule set in
  `typespec-azure-core`) not the emitted OpenAPI; buf lints `.proto`, not the descriptor; Google
  lints `.proto`, not the REST mapping. I found no major project that lints a generated JSON
  Schema for domain vocabulary as its primary gate.
- schemars (1.x, default draft 2020-12, ~1.4k stars) generates from `serde`-compatible types
  and offers `#[schemars(...)]` attributes to override; it keeps `$defs` and `$ref`, so a shared
  concept type appears as one definition referenced everywhere, which is mechanically checkable.
  https://github.com/GREsau/schemars

## Trade-offs

| Approach | Enforceability | Maintenance cost | False-positive rate | Fit for schemars + 2020-12 |
|---|---|---|---|---|
| Shared concept types in Rust (`Digest`, `Platform`, `Timestamp`, `ByteSize`, status enums), one `JsonSchema` impl each | Highest: cannot be bypassed without a new type; reviewable in one place | Low; the cost is a one-time migration of ad-hoc `String` fields | None (type system) | Native. Each type yields one `$defs` entry with `format`/`pattern`; schemars attributes cover it |
| Lint over source types (syn / clippy-style) | High, intent-aware (sees `Option<T>` vs `skip_serializing_if`) | High: custom analyser, tied to Rust syntax; kube-api-linter's 31 linters are the scale reference | Low if limited to naming and type use | Possible but over-built for one repo; precedent assumes a platform team |
| Test that walks the generated schema (`serde_json::Value` walk over `$defs`/`properties`) | Medium-high for name regexes, `format` presence, `required` vs nullable, `description` presence | Low: tens of lines per rule, in `cargo test`, no new toolchain | Low-medium: the walker sees flattened schemas, so a lint needs `$defs` aware traversal | Best direct fit; same shape as buf lint on a descriptor |
| Spectral with a custom ruleset over the schema | Medium: JSONPath over a `$ref`-heavy 2020-12 doc is awkward (`$ref` is not resolved, `oneOf`/`anyOf` from `Option`) | Medium: Node dependency, ruleset language, version churn (v6 cadence slow) | Medium: generic rules fire on schemars output idioms (`anyOf: [T, null]`) | Weak: its value is OpenAPI rulesets, which OCX does not publish |
| `sourcemeta/jsonschema lint` | Medium for hygiene, not vocabulary | Low (native binary) | Low | Fine as an extra hygiene pass; no custom vocabulary |
| Prose guide + review (clig.dev, Graph, Kubernetes before KAL) | Lowest; drift is documented (Docker, gh, GitLab CLI) | Lowest to write, highest in review time | n/a (humans) | Needed as the *why*, insufficient as the gate |
| Snapshot of the schema + breaking-diff (`oasdiff`, `buf breaking`) | Catches accidental renames, retyped fields, new enum values | Low: snapshot file; diff tool is the only cost | Low | Complements, does not replace, a vocabulary lint. oasdiff targets OpenAPI, so a JSON Schema diff needs a different tool or a small custom comparison; tool choice here is unverified |

Vocabulary dimensions, what precedent says:

- **Timestamps:** every strong guide fixes both the wire format and the name suffix: AIP `_time`,
  Zalando `_at`/`date-time` UTC, Graph `DateTime`, Kubernetes `*Time` (and KAL's `notimestamp` lint).
  Docker #6727 shows the failure when the format is not fixed.
- **Durations and sizes:** precedent splits: a typed value (AIP `Duration`) or a unit in the name
  (Kubernetes `Seconds`, KAL `nodurations`, AIP legacy `_millis`). Whichever is chosen,
  carry the unit once, in the type or the name, never in prose.
- **Identifiers and digests:** Zalando's `id` opaque string, `xyz_id` references; AIP `name` plus
  `effective_` twins; Kubernetes `{field}Ref`. A digest is its own concept type in none of them;
  OCX has a stronger case than these guides, because its digests have a fixed grammar.
- **Enums:** Kubernetes PascalCase, Zalando UPPER_SNAKE, Azure extensible; the shared point is
  that clients must tolerate unknown values (AIP-180, Azure). A closed Rust enum serialised to a
  `oneOf`/`enum` schema tells a consumer the opposite; this needs an explicit decision.
- **Case:** snake_case (Google, Zalando), camelCase (Kubernetes, Azure, Graph, gh). Neither wins
  by adoption; every durable guide picked exactly one and applied it to *all* surfaces, not just
  JSON. mise/cargo/uv additionally derive env names from the same key.
- **Optional vs null:** Zalando 122-124 (absent equals null; no null booleans; `[]` not null),
  Kubernetes (`omitempty`, pointer types, `nonullable` lint), Azure (omit `nextLink` on last
  page). Consensus: absent for "no value"; null is a smell and is the only item where a lint
  reliably pays off.
- **Flags:** clig.dev standard names; gh's kebab-case with discoverable `--json` fields; Terraform
  and uv link env to flag one-to-one ("equivalent to `--frozen`").

## Trends

- 2025-2026: linters moved from OpenAPI-document tools to *source-type* tools: kube-api-linter
  (in golangci-lint, now in Kubernetes' own lint config), api-linter v2 rewrite on protobuf-go
  (2025-10-22) with `--skip-compilation` descriptor linting (2026-02-10), TypeSpec as Azure's
  authoring source. Direction: lint where intent is visible.
- Spectral: widely used, slow-moving, a SmartBear product component; gravity is OpenAPI.

## Recommendation

Opinionated: **make the vocabulary a set of Rust types, then enforce it with a small test-time
lint over the generated schema, plus a committed schema snapshot. Do not adopt Spectral or a
prose-only guide as the gate.**

1. **Shared concept types first.** One Rust type per concept (identifier, digest, platform,
   version, timestamp, size, duration, status enums) with a single `JsonSchema` impl producing a
   named `$defs` entry carrying `format`/`pattern` and `description`. This is the cheapest and
   only unbypassable layer; every precedent that reached consistency (protobuf well-known types,
   Cargo key-derived env names) did it by construction. Reason: lints can only reject, types can
   also guide.
2. **Lints as `cargo test` over the schema.** Walk `$defs` and `properties` with a `$ref`-aware
   visitor and assert: property-name regex per the chosen case; time fields use the shared
   timestamp `$ref`; no field named `*Timestamp`/`*_ts` (KAL `notimestamp` shape); every enum is
   a named `$def`; every property has a `description`; no bare `null` type except by declared
   exception; no `string` property whose name ends `digest`/`id` without the shared `$ref`.
   Reason: this is KAL/api-linter's rule list, minus the platform; it runs in the existing gate,
   adds no toolchain, and keeps waivers next to the type via a schemars `extend` marker (copy
   api-linter's inline-waiver idea: a waiver is greppable and carries a reason).
3. **Floor the walker (Unchecked Green).** Assert it visited at least N properties and N `$defs`,
   and keep one deliberately broken fixture schema that must make each lint red. A schema walker
   that stops at an unresolved `$ref` reports clean on an empty visit; the project's own
   verification-honesty rule applies to this lint.
4. **Roll out lintdiff-style.** Commit a baseline waiver list of today's violations and make the
   gate fail only on new ones; shrink the list as commands are migrated. A big-bang lint over an
   already-shipped surface is the documented anti-pattern, and renames of published fields are
   interface breaks (AIP-180), so the waiver list also records which fixes need a changelog line.
5. **Pair with a snapshot + breaking diff.** Commit the generated schema, and fail review on an
   unreviewed diff; classify rename, retype and enum-value change as breaking (AIP-180 /
   buf `WIRE_JSON` is the model). Tool choice for a JSON Schema diff is unresolved (oasdiff is
   OpenAPI-oriented); a 100-line custom comparer may be enough.
6. **Env and flags: derive, do not lint.** Follow Cargo/uv/mise: one declared key per setting;
   derive the env name from it; document env as "equivalent to" the flag. Adopt clig.dev's
   standard flag names as a short allowlist rather than a lint. One test asserting
   `OCX_` prefix, uppercase+underscore only, and a flag-to-env table is enough.
7. **Skip:** Spectral (Node dependency, OpenAPI-centric, `$ref` friction on schemars output), a
   custom `syn` source linter (kube-api-linter scale), and a prose guide as the authority. Revisit
   Spectral only if OCX ever publishes an OpenAPI description.

## Sources

- https://google.aip.dev/140, https://google.aip.dev/142, https://google.aip.dev/129, https://google.aip.dev/180
- https://linter.aip.dev/, https://linter.aip.dev/configuration, https://github.com/googleapis/api-linter,
  https://github.com/googleapis/api-linter/releases
- https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md
- https://github.com/kubernetes-sigs/kube-api-linter and its docs/linters.md
- https://kubernetes.io/docs/reference/kubectl/conventions/
- https://opensource.zalando.com/restful-api-guidelines/, https://github.com/zalando/zally
- https://github.com/microsoft/api-guidelines (Graph and Azure guideline documents)
- https://github.com/Azure/azure-api-style-guide, https://github.com/Azure/azure-openapi-validator
- https://clig.dev/
- https://cli.github.com/manual/gh_pr_list, https://cli.github.com/manual/gh_help_formatting
- https://github.com/docker/cli/issues/6727
- https://gitlab.com/gitlab-org/cli/-/merge_requests/2859/commits
- https://doc.rust-lang.org/cargo/reference/config.html, https://docs.astral.sh/uv/reference/environment/,
  https://mise.jdx.dev/configuration/settings.html,
  https://developer.hashicorp.com/terraform/cli/config/environment-variables, https://12factor.net/config
- https://buf.build/docs/lint/, https://github.com/oasdiff/oasdiff
- https://github.com/stoplightio/spectral, https://github.com/sourcemeta/jsonschema,
  https://github.com/GREsau/schemars
