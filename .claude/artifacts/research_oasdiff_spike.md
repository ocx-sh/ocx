# Research: can oasdiff, through an adapter, replace or back the in-house schema differ?

**Axis:** technology verification (phase-2 go/no-go, criterion (c))
**Date:** 2026-10-04
**For:** [`plan_ocx_interface_contract.md`](./plan_ocx_interface_contract.md) C-026 (c), feeding WP-35's decision in
[`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md) § Phases row 2 (c) and tension 4
**Measured against:** the compat mutation corpus `crates/ocx_sdkgen/tests/compat_corpus/` (61 cases, tree
`ce15b11c04a9a5905d64917574d69f8a9c501512` at `fcef179ae`), codes per
[`system_design_ocx_interface_contract.md`](./system_design_ocx_interface_contract.md) § 3.9
**Status:** measured. oasdiff cannot replace the differ. As a backend it covers the structural B01–B07 subset and
none of the OCX-specific codes.

## Verdict

- **Replace: no.** With default severities, oasdiff misses 22 of the corpus's 37 breaks and flags 3 of its 24
  non-breaks. With a 12-line severity override it still misses 12 breaks (tuned scores are in-sample, see Totals). Those 12 are
  exactly the codes none of oasdiff 1.33.0's 755 rules classify: `x-ocx-enum` entry members (B08, R02, R03 exit code, U01 entry member), description
  semantics (D01), and grammar facts with no OpenAPI construct (G01 hidden/removal release, G03, G10, G11, G14,
  G15).
- **Back: possible only for the generic structural slice.** On `reports`/`errors` the tuned adapter matches
  27 of 32 verdicts with no false positives. But its findings carry no schema pointer, are reported once per use
  site instead of once per `$def`, and one oasdiff id stands for up to three SD codes. The ledger gate keys on
  `(document, rule, pointer)` (SD § 3.9 gate step 2), so every backed finding would still go through owned code
  that reconstructs the pointer and assigns the code.

## Setup

| Item | Value |
|---|---|
| Tool | oasdiff **v1.33.0** (latest release, published 2026-10-01), Apache-2.0, static Go ELF, 18 MB |
| How obtained | No OCX package exists: `ocx --remote index catalog` lists 131 repositories, none matching `oas`; `ocx --remote index list ocx.sh/oasdiff` and `ocx.sh/oasdiff/oasdiff` → not found. Downloaded the pinned release asset with `gh release download v1.33.0 --repo oasdiff/oasdiff --pattern 'oasdiff_1.33.0_linux_amd64.tar.gz' --pattern checksums.txt` (no `curl \| sh`, no install script) |
| Tarball sha256 | `43a4e328e2d13ba1552d760aa68d2485c75c5621f309f6ff64ae895188345247`, verified two ways: `sha256sum -c --ignore-missing checksums.txt` → OK, and against the digest GitHub's release API reports for the asset → OK |
| `checksums.txt` sha256 | `0f3bd96823bf5417ad0f1198091c740d448a73de10980fa9e9d6859b7832a868` |
| Extracted binary sha256 | `fa65aae43c867e9f79da9cb121d3c1a113637883afe014c0680fd5b50696f416` (`oasdiff --version` → `oasdiff version 1.33.0`) |
| Provenance | None. `gh attestation verify … --repo oasdiff/oasdiff` → HTTP 404, and the release has no `.sig`/`.pem`/SBOM assets. Integrity rests on a `checksums.txt` that ships in the same release |
| Isolation | Every invocation ran as `unshare -rn env -i PATH=/usr/bin:/bin HOME=<scratch> oasdiff … --allow-external-refs=false`: no network namespace, empty environment, no credentials, external `$ref` fetching off. `--open` (uploads the comparison to oasdiff.com) and `--fetch` (writes git objects) were never used |
| Checks catalogue | `oasdiff checks changelog`: 755 rules, **0** with `description` in the id; `x-extensible-enum` rules exist for the request side only (`request-parameter-x-extensible-enum-value-removed`, `request-property-x-extensible-enum-value-removed`) |

Spike code (throwaway, gitignored, not committed): `.agents/ic/wp34-oasdiff/adapter/{adapter.py, run.py,
score.py, table.py, probe.py, golden.py, count.py}`, plus `severity_tuned.txt`. Persisted results beside them:
`results_default.json`, `results_tuned.json`, `results_probe.json`, `results_golden.json`, `timings_default.txt`,
`timings_tuned.txt`, `probe_summary.txt`.

Every number below comes from these commands, run from `.agents/ic/wp34-oasdiff/` (`C` is the corpus directory
`crates/ocx_sdkgen/tests/compat_corpus`, `R` the repository root). The severity file is passed by absolute path
because oasdiff runs with its working directory in `t/`.

```
python3 adapter/run.py $C > results_default.json 2> timings_default.txt
python3 adapter/run.py $C $PWD/severity_tuned.txt > results_tuned.json 2> timings_tuned.txt
python3 adapter/score.py results_tuned.json raw,mapped,ext,cli      # same with results_default.json
python3 adapter/probe.py $C > results_probe.json 2> probe_summary.txt
python3 adapter/golden.py $R $PWD/severity_tuned.txt > results_golden.json
python3 adapter/count.py adapter/adapter.py
```

## The adapter

Wrapping follows C-026 (c): each report root and the `errors` document become a response schema.

| Variant | What it does | Code lines |
|---|---|---|
| `raw` | OpenAPI 3.1 shell; `$defs` → `components.schemas` with `$ref` rewrite; each `reports.<Root>` → `GET /reports/<Root>`, 200 response `$ref` to its wrapper; `errors` whole document → `GET /errors`. `x-ocx-*` passed through as extensions | ~29 |
| `mapped` | `raw` plus `x-ocx-enum` → `enum: [values]`, the unknown arm's derived `not` dropped, the `schema_version` property dropped (the version step owns it, SD § 3.9) | +8 (in `rewrite_refs`) |
| `ext` | `mapped`, but `x-ocx-enum` → `x-extensible-enum` | 0 extra |
| `cli` | Goes beyond C-026 (c), which wraps only schema documents; included so the 29 `cli` cases get a measured answer. Commands → `GET /<path>`; args → query params (`required`, type, choices → `enum`, default, `num_args` → array/`maxItems`); global args inherited by descendants; deprecated → `deprecated: true`; report output → response media type `application/vnd.ocx.<Root>+json`; Public env → header params on `/__env`, retired-in-window names kept with the replacement's schema. Everything else → `x-` extension | ~73 |
| `severity_tuned.txt` | `--severity-levels` override: 10 ids raised to ERR (optional-property removed, enum/oneOf removed, pattern added/removed/changed, `minItems` set, request default changed, request type generalized, request param removed; the last is WARN by default, so raising it changes no verdict), 2 lowered to INFO (response enum value added, response oneOf added: OCX enums and unions are open) | 12 lines |

Code-line counts exclude blanks, comments and the module docstring (`adapter/count.py`): `adapter.py` is 151 lines,
120 of code. The schema adapter (`rewrite_refs`, `op`, `schemas_doc`) is 37 lines of code, 41 with the shared
`convert` dispatcher; the `cli` adapter is 73 with its constants. A production port would be Rust in `ocx_sdkgen` and would
need its own count.

**A verdict is `break` when oasdiff emits any finding at WARN or ERR**, which is what `oasdiff breaking` lists. Run
over the corpus, every case, every variant, with the first two commands of the block above. Each run makes 125
conversions (32 schema cases × 3 variants + 29 cli cases) and 250 oasdiff invocations (`changelog -f json` and
`diff -f json`). Wall time over three repeats per configuration (`timings_*.txt`): default 2.9, 2.9, 2.8 s; tuned
2.9, 2.8, 4.3 s (the 4.3 s run overlapped host load, average about 6). The earlier recorded 2.7 s and 2.9 s fall in
the same range. The re-run results are byte-identical to the first run's `results_default.json` and
`results_tuned.json`. Scored with `score.py`.

## Totals

Corpus: 37 `break`, 24 `non_break` (reports/errors 19 + 13, cli 18 + 11).

| Configuration | n | TP | FN (missed break) | FP | TN |
|---|---|---|---|---|---|
| reports/errors `raw`, default levels | 32 | 8 | 11 | 2 | 11 |
| reports/errors `mapped`, default levels | 32 | 7 | 12 | 3 | 10 |
| reports/errors `ext`, default levels | 32 | 7 | 12 | 1 | 12 |
| cli, default levels | 29 | 8 | 10 | 0 | 11 |
| reports/errors `raw`, tuned | 32 | 11 | 8 | 2 | 11 |
| **reports/errors `mapped`, tuned** | 32 | **14** | **5** | **0** | 13 |
| reports/errors `ext`, tuned | 32 | 11 | 8 | 0 | 13 |
| **cli, tuned** | 29 | **11** | **7** | **1** | 10 |

Best configuration over the whole corpus (`mapped` + `cli`, tuned): 25 of 37 breaks caught, 12 missed, 1 false
positive, so 48 of 61 verdicts match. Default levels, same variants: 15 caught, 22 missed, 3 false positives.

The tuned levels were chosen against this same corpus, so every tuned score is in-sample and an upper bound on what
the override would score on unseen changes.

`ext` loses to `mapped` because oasdiff has no response-side `x-extensible-enum` rule: an enum carried that way is
invisible in `changelog` (b05/r01/r03 removals: no finding). `raw` keeps the unknown arm's `not.enum`, so under tuned levels a
variant addition also reads as `response-property-one-of-removed` ERR (FP on `union_variant_added`). It keeps
`schema_version` too, so a version bump reads as `response-property-const-changed` ERR (FP on
`version_fields_excluded`).

### False negatives by name

- **Default levels, `mapped` + `cli` (22):** `b02_property_removed`, `b04_map_value_changed`,
  `b05_enum_value_removed`, `b05_union_variant_removed`, `b08_enum_name_changed`, `d01_description_semantic`,
  `r01_exit_code_removed`, `r02_exit_code_category_changed`, `r03_slug_exit_code_changed`, `r03_slug_removed`,
  `u01_enum_entry_member`, `u01_min_items`, `g01_hidden_without_deprecated`, `g01_removed_before_removal_release`,
  `g03_short_changed`, `g06_value_spec_changed`, `g07_num_args_narrowed`, `g09_default_changed`,
  `g10_conflicts_added`, `g11_require_equals_on`, `g14_env_on_invalid_error`, `g15_stdin_secret_changed`.
- **Tuned, `mapped` + `cli` (12):** `b08_enum_name_changed`, `d01_description_semantic`,
  `r02_exit_code_category_changed`, `r03_slug_exit_code_changed`, `u01_enum_entry_member`,
  `g01_hidden_without_deprecated`, `g01_removed_before_removal_release`, `g03_short_changed`,
  `g10_conflicts_added`, `g11_require_equals_on`, `g14_env_on_invalid_error`, `g15_stdin_secret_changed`.
- **False positives:** default `mapped`: `enum_value_added`, `slug_added`, `union_variant_added`. oasdiff treats
  response enums and `oneOf` as closed, OCX treats them as open (ADR tension 6). Tuned `cli`: `num_args_widened`,
  because raising `request-parameter-type-generalized` to ERR to catch G06/G07 also catches the widening.

The tuned misses split into two kinds. **Invisible**: the `mapped` adapter drops the `x-ocx-enum` members, and
`oasdiff diff` reports no difference for b08, r02, r03 exit code or u01 entry member. **Visible but unclassified**:
`oasdiff diff` sees the change as an extension or description diff, but none of the 755 changelog rules classifies
it (all D01 cases; every `raw`-variant enum-member change; G01 hidden, G03, G10, G11, G14, G15 as `x-`
extensions).
`g01_removed_before_removal_release` is classified, but as `api-path-removed-with-deprecation` INFO, the same id
as the legitimate `hidden_deprecated_removed`. oasdiff decides deprecation by sunset date (`x-sunset`,
`--deprecation-days-*`); OCX decides by release version (`removal` against `CARGO_PKG_VERSION`). The adapter cannot
reconcile the two without a version-to-date mapping that does not exist.

## Per-case results

Adapter `mapped` for reports/errors and `cli` for cli. "Default" and "Tuned" give the verdict against
`expect.json` (ok / **FN** / **FP**). "Diff sees it" is whether `oasdiff diff` reports any difference: the `raw`
variant for schema cases, the `cli` variant for cli cases. Levels: E error, W warning, I info.

| Case | Expected | oasdiff findings (default levels) | Default | Tuned | Diff sees it |
|---|---|---|---|---|---|
| `b01_root_removed` | break (B01) | `api-path-removed-without-deprecation` E<br>`api-schema-removed` I | ok | ok | yes |
| `b02_property_removed` | break (B02) | `response-optional-property-removed` I | **FN** | ok | yes |
| `b03_required_to_optional` | break (B03) | `response-property-became-optional` E | ok | ok | yes |
| `b04_array_items_changed` | break (B04) | `response-property-min-set` I<br>`response-property-pattern-removed` E<br>`response-property-type-changed` E | ok | ok | yes |
| `b04_map_value_changed` | break (B04) | `response-property-pattern-added` I | **FN** | ok | yes |
| `b04_property_type_changed` | break (B04) | `response-property-type-changed` E | ok | ok | yes |
| `b05_enum_value_removed` | break (B05) | `response-property-enum-value-removed` I | **FN** | ok | yes |
| `b05_union_variant_removed` | break (B05) | `response-property-one-of-removed` I | **FN** | ok | yes |
| `b06_pattern_changed` | break (B06) | `response-property-pattern-changed` W | ok | ok | yes |
| `b07_structured_to_opaque` | break (B07) | `response-property-type-changed` E | ok | ok | yes |
| `b08_enum_name_changed` | break (B08) | — | **FN** | **FN** | yes |
| `choice_added` | non_break (none) | `request-parameter-enum-value-added` I | ok | ok | yes |
| `command_added` | non_break (none) | `endpoint-added` I | ok | ok | yes |
| `d01_description_doc` | non_break (D01) | — | ok | ok | yes |
| `d01_description_semantic` | break (D01) | — | **FN** | **FN** | yes |
| `d01_enum_entry_description` | non_break (D01) | — | ok | ok | yes |
| `def_renamed` | non_break (none) | `api-schema-removed` I | ok | ok | yes |
| `enum_entries_reordered` | non_break (none) | — | ok | ok | yes |
| `enum_value_added` | non_break (none) | `response-property-enum-value-added` E | **FP** | ok | yes |
| `env_entries_reordered` | non_break (none) | — | ok | ok | no |
| `env_renamed_with_window` | non_break (none) | `new-optional-request-parameter` I<br>`request-parameter-deprecated` I | ok | ok | yes |
| `flag_added` | non_break (none) | `new-optional-request-parameter` I | ok | ok | yes |
| `g01_command_removed` | break (G01) | `api-path-removed-without-deprecation` E | ok | ok | yes |
| `g01_hidden_without_deprecated` | break (G01) | — | **FN** | **FN** | yes |
| `g01_removed_before_removal_release` | break (G01) | `api-path-removed-with-deprecation` I | **FN** | **FN** | yes |
| `g02_long_flag_renamed` | break (G02) | `new-optional-request-parameter` I<br>`request-parameter-removed` W | ok | ok | yes |
| `g03_short_changed` | break (G03) | — | **FN** | **FN** | yes |
| `g04_arg_became_required` | break (G04) | `request-parameter-became-required` E | ok | ok | yes |
| `g04_required_arg_added` | break (G04) | `new-required-request-parameter` E | ok | ok | yes |
| `g05_choice_removed` | break (G05) | `request-parameter-enum-value-removed` E | ok | ok | yes |
| `g06_value_spec_changed` | break (G06) | `request-parameter-default-value-changed` I<br>`request-parameter-type-generalized` I | **FN** | ok | yes |
| `g07_num_args_narrowed` | break (G07) | `request-parameter-type-generalized` I | **FN** | ok | yes |
| `g08_no_longer_global` | break (G08) | `request-parameter-removed` W | ok | ok | yes |
| `g09_default_changed` | break (G09) | `request-parameter-default-value-changed` I | **FN** | ok | yes |
| `g10_conflicts_added` | break (G10) | — | **FN** | **FN** | yes |
| `g11_require_equals_on` | break (G11) | — | **FN** | **FN** | yes |
| `g12_output_root_renamed` | break (G12) | `response-media-type-added` I<br>`response-media-type-removed` E | ok | ok | yes |
| `g13_public_env_removed` | break (G13) | `request-parameter-removed` W | ok | ok | yes |
| `g14_env_on_invalid_error` | break (G14) | — | **FN** | **FN** | yes |
| `g15_stdin_secret_changed` | break (G15) | — | **FN** | **FN** | yes |
| `hidden_deprecated_removed` | non_break (none) | `api-path-removed-with-deprecation` I | ok | ok | yes |
| `hidden_with_deprecated` | non_break (none) | `endpoint-deprecated` I | ok | ok | yes |
| `named_args_reordered` | non_break (none) | — | ok | ok | no |
| `num_args_widened` | non_break (none) | `request-parameter-type-generalized` I | ok | **FP** | yes |
| `optional_to_required` | non_break (none) | `response-property-became-required` I | ok | ok | yes |
| `payload_def_reached_from_another_root` | break (B04) | `response-property-type-changed` E (×2: PushReport, CopyReport) | ok | ok | yes |
| `plumbing_env_removed` | non_break (none) | — | ok | ok | no |
| `property_added` | non_break (none) | `response-optional-property-added` I | ok | ok | yes |
| `public_env_added` | non_break (none) | `new-optional-request-parameter` I | ok | ok | yes |
| `r01_exit_code_removed` | break (R01) | `response-property-enum-value-removed` I | **FN** | ok | yes |
| `r02_exit_code_category_changed` | break (R02) | — | **FN** | **FN** | yes |
| `r03_slug_exit_code_changed` | break (R03) | — | **FN** | **FN** | yes |
| `r03_slug_removed` | break (R03) | `response-property-enum-value-removed` I | **FN** | ok | yes |
| `root_added` | non_break (none) | `endpoint-added` I | ok | ok | yes |
| `slug_added` | non_break (none) | `response-property-enum-value-added` E | **FP** | ok | yes |
| `u01_enum_entry_member` | break (U01) | — | **FN** | **FN** | yes |
| `u01_min_items` | break (U01) | `response-property-min-items-set` I | **FN** | ok | yes |
| `union_arms_reordered` | non_break (none) | — | ok | ok | no |
| `union_variant_added` | non_break (none) | `response-property-one-of-added` E | **FP** | ok | yes |
| `unreachable_def_changed` | non_break (none) | — | ok | ok | yes |
| `version_fields_excluded` | non_break (none) | — | ok | ok | yes |

## Codes and pointers: what a backing would still own

**Per SD code, tuned verdicts** (a code counts as covered when every corpus case carrying it is a TP):

- reports/errors, 13 codes: covered B01, B02, B03, B04, B05, B06, B07, R01. Partial: U01 (`minItems` yes, entry
  member no) and R03 (removal yes, exit-code change no). None: B08, D01, R02.
- cli, 15 codes: covered G02, G04, G05, G06, G07, G08, G09, G12, G13. Partial: G01 (1 of 3). None: G03, G10, G11,
  G14, G15.

**D01 findings are never emitted.** The three D01 cases get no finding at all, including the two `non_break` ones
whose `doc` ledger entry needs a finding to match (gate step 3). This follows from the catalogue having 0
description rules.

**One oasdiff id stands for several SD codes**, so an owned mapping has to assign the code:
`response-property-type-changed` covers B04 and B07, `response-property-enum-value-removed` covers B05, R01 and
R03, `request-parameter-removed` covers G02, G08 and G13, and `request-parameter-type-generalized` covers G06 and
G07.

**Attribution is per use site, with no pointer.** A finding carries `id, text, level, operation, operationId,
path, section, fingerprint`. The schema location exists only inside `text`, as a property path such as
`` `layers/items/` ``. `b06_pattern_changed` expects one finding at `/$defs/Digest` with subjects
{PushReport, StatusReport}; oasdiff emits three, one per property that reaches `Digest`.
`payload_def_reached_from_another_root` expects one finding at the payload `$def`; oasdiff emits one per root.
Subjects can be recovered from `path` (`/reports/<Root>`). The ledger `pointer` cannot be recovered without
owned code that parses `text` and walks the schema back to the `$def`.

**U01 is not an allowlist.** `python3 adapter/probe.py $C` adds each of 17 keywords outside the § 3.7 allowlist to
a new property of `PushReportRoot` (base: `property_added/base.json`, `mapped`, default levels) and reads
`oasdiff changelog`. Each keyword sits on the JSON Schema type it applies to (`typed`); a second pass puts all 17 on
a string property (`uniform`). Results in `results_probe.json`, summary in `probe_summary.txt`:

- `typed`: 14 of 17 produce a finding, 12 at INFO (`minItems`, `maxItems`, `contains`, `minLength`, `multipleOf`,
  `exclusiveMinimum`, `allOf`, `if`, `dependentRequired`, `minProperties`, `readOnly`, `deprecated`) and 2 at ERR
  (`anyOf`, `contentEncoding`).
- `uniform`: 15 of 17, the same 12 INFO and 2 ERR plus `prefixItems` at WARN. This reproduces the 17/15/12/3
  recorded earlier. The extra finding is `prefixItems` on a string property; on an array property it is silent.
- `unevaluatedProperties: false` and an unknown `x-made-up` produce **no** changelog finding in either pass
  (`oasdiff diff` does see both).

So U01's "every unlisted keyword is red" stays an owned keyword pass whatever the backend.

## Real-document check and toolchain cost

- **Real golden** (`python3 adapter/golden.py $R $PWD/severity_tuned.txt > results_golden.json`; reads the blobs
  with `git show HEAD:`; `mapped`, tuned levels; wall and peak RSS from `/usr/bin/time -f '%e %M'` around
  `oasdiff changelog`). `crates/ocx_schema/tests/golden/reports.json`, blob `3ee29dd6`, pre-phase-1 shape, 54 roots,
  206 `$defs`: the adapter converts it to 54 paths and 206 schemas. Identity diff → `[]`, 0.05 s wall, 28.4 MB peak
  RSS (the earlier run recorded 0.03 s and 26 MB; the gap is run-to-run noise, the document is unchanged).
  Red seen: dropping optional `bundle_digest` from `AttestationReport` gives 2 findings,
  `response-optional-property-removed` ERR at `/reports/AttestationReport` and at
  `/reports/SweepReport<AttestationReport>`, which also shows cross-root attribution; the script asserts the
  property existed before removing it. `errors.json`, blob `6796a84a`: converts, identity diff `[]`, 0.01 s, 18.7 MB.
- **Speed is not the cost.** 250 invocations in 2.8 to 2.9 s is about 11 to 12 ms per invocation on corpus
  documents, including the `unshare`/`env` start; one loaded run took 4.3 s.
- **Supply chain**: no OCX package, so adopting means a new mirrored package or a Bazel `http_file` for each of
  five assets (linux amd64/arm64, darwin universal, windows amd64/arm64). The differ would then sit outside the
  Rust/Bazel graph as an 18 MB Go binary with no provenance attestation. It would also reverse the ADR's
  supply-chain row ("No new tool in `ocx.toml`; oasdiff only in a pinned one-off spike"). Two flags must be pinned
  off for safety: `--allow-external-refs` defaults to **true** (its own help text says to disable it to prevent
  SSRF), and `--open` uploads the comparison to a third-party service.

## Recommendation for WP-35

Record criterion (c) as **no-adopt: keep the in-house schema differ over the § 3.7 allowlist** (ADR tension 4 and
the § Don't-own assessment "Schema differ" row stand, and the "provisionally" qualifier can be dropped). oasdiff does not
meet the don't-own bar's adoption test. It lacks no capability the corpus requires, and the in-house differ is
still a specification only (WP-35 stubs it), so the corpus's 61 cases are the one measured yardstick for both. It
lacks capabilities the gate needs: 12 of 37 corpus breaks are missed even tuned; no D01 finding exists in its 755 rules;
no schema pointer, so ledger matching is impossible without parsing finding text; ids are ambiguous across SD
codes; open-enum/union semantics are inverted until severities are overridden; U01 is not an allowlist. The
in-house differ must exist anyway for `cli.json`, the registry (R02, R03) and `x-ocx-enum` members (B08, U01).
Backing would cover B01–B07, R01, half of U01 (`minItems` yes, entry member no), half of R03 (removal yes, exit-code
change no) and, through the `cli` adapter that goes beyond C-026 (c), 9 of the 15 G codes with 1 false positive
(`num_args_widened`). That is not worth adopting: 6 G codes stay uncovered (G01 partly, G03, G10, G11, G14, G15; 7
corpus cases), the false positive is the price of the override that catches G06 and G07, findings carry no pointer,
and they arrive per use site. Backing would still need a ~41-line adapter, a 12-rule severity file, owned pointer
reconstruction and code assignment, and an unattested binary outside the Bazel graph. One input does carry over to
WP-36: 14 of the 17 probed non-allowlisted keywords produced an oasdiff finding on the type they apply to (15 on a
string property, where `prefixItems` fires), a ready list for widening the U01 corpus beyond `minItems` and the
entry member.

**Evidence:** Totals table (tuned `mapped` + `cli`: 25 TP / 12 FN / 1 FP / 23 TN; default: 15 / 22 / 3 / 21); the
false-negative lists; § Codes and pointers (zero description rules, use-site attribution, id collisions, keyword
probe); § Setup (pin, checksums, no attestation, isolation flags); all produced by the commands above against
oasdiff v1.33.0 (`fa65aae4…` binary) on the corpus at `fcef179ae`.
