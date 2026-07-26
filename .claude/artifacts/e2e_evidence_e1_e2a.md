# E1 / E2a — live acceptance gate, post-change (tag `1.0.4`)

Companion to [`e2e_evidence_e1_pre.md`](./e2e_evidence_e1_pre.md), which captured the same
assertion against the **pre-change** stack and observed the bug class live. Together they form
the failing-first → fixed pair for `adr_oci_index_only_dispatch.md`.

| | |
|---|---|
| Date | 2026-07-26 |
| ocx build under test | `feat/oci-index-only-dispatch` @ `9c8db1fc`, deployed via `Deploy Dev` run `30194927769` |
| Dev channel | `dev.ocx.sh/ocx/cli:0.5.0-dev` → build `0.5.0-dev_20260726083641` (floating tag confirmed advanced) |
| Publisher tag | `1.0.4` → `michael-herwig/ocx-e2e-publisher` |
| Announce PR | [`ocx-sh/index#64`](https://github.com/ocx-sh/index/pull/64), head `indexbot-announce-michael-herwig-ocx-e2e-hello` on `michael-herwig/index` |
| Package | `michael-herwig/ocx-e2e-hello`, physical repo `oci://ghcr.io/michael-herwig/ocx-e2e-hello` |

## Pre-flight — the two ways this run could have passed while proving nothing

Both were closed **before** the one-shot tag was spent (`push_publisher_tag` hard-fails on an
existing tag, so there is no retry).

1. **The floating dev tag might not have advanced.** `ocx.toml`'s own comment warns that a failed
   deploy leaves `0.5.0-dev` resolving the previous digest, and the fix is the deploy, never a
   pinned `_<TS>` build segment. The deploy log shows `0.5.0-dev` cascaded onto
   `0.5.0-dev_20260726083641`, built from the pushed tip. Advanced.
2. **The announce step degrades to a still-green skip.** `e2e-publish.yml` probes
   `ocx package push --help` for `--announce-file` and emits a `::notice::` skip when absent —
   a green run that announces nothing. Probed the binary directly: flag present, and the new
   validator string (`invalid OCI image index`) present. Closed.

## E1 — the acceptance gate

The committed CAS object must be byte-identical to what the registry serves under the same digest.

| Step | Result |
|---|---|
| Object hashes to its own filename | `sha256(bytes)` = `50e02438…5ee1` = filename ✅ |
| GHCR serves it by digest | **HTTP 200** ✅ |
| `Docker-Content-Digest` == requested | `sha256:50e02438…5ee1` ✅ |
| `cmp` committed vs served | **byte-identical**, 725 == 725 bytes ✅ |

`Docker-Content-Digest` equalling the *requested* digest is the assertion that separates
"the registry serves these bytes" from "the registry converted them" — a registry that converts
cannot return the digest that was asked for.

**Coverage.** All six root tags (`1`, `1.0`, `1.0.2`, `1.0.3`, `1.0.4`, `latest`) bind to this one
object, so verifying it covers every tag→object binding in the root. `--cascade` aliasing several
tags onto one index is exactly why the gate is specified over *every* tag rather than this run's.

### Direct inversion of E1-pre

| | E1-pre (pre-change) | E1 (post-change) |
|---|---|---|
| Object | `sha256:4ee19e66…fe91` — bot-invented `{"platforms":[…]}` | `sha256:50e02438…5ee1` — OCI image index, `schemaVersion: 2`, `mediaType: application/vnd.oci.image.index.v1+json` |
| GHCR | **404 `MANIFEST_UNKNOWN`** across all 5 bindings | **200**, byte-identical across all 6 |

The invented object was a projection no registry ever held. The new object is the registry's own
bytes, carried verbatim.

## E2a — no reserved tag class reaches the index (D7)

| | |
|---|---|
| Root tags | `1`, `1.0`, `1.0.2`, `1.0.3`, `1.0.4`, `latest` — 6 judged, non-vacuous |
| `^__ocx` (case-insensitive) hits | none ✅ |
| `^sha256\.[0-9a-f]{64}$` hits | none ✅ |
| Registry actually carries the classes | **yes** — 10 tags total, of which `__ocx.desc` and 2 × `sha256.<hex>` |

**Verdict: PASS**, with the ADR's caveat stated rather than elided. The publisher announces via
`--announce-file`, which under D7 never carries a canonical tag, so a clean root is *consistent
with no filter at all* — "current cleanliness is omission, not policy." E2a is a cheap regression
tripwire. **E2b and the local A4/A6 cases are the proof that the filter fired.**

## E2b — reconcile sweep (the gate that proves the filter fired)

`indexbot reconcile` is verify-only (`--dry-run` was removed because it never writes to `p/` at
all), but it **does** open an anomaly issue on findings. It was therefore run with a
deliberately write-incapable token, so a dirty sweep would surface its findings and fail at the
write rather than perform one against the production repo.

### First attempt was vacuous — recorded because the near-miss is the lesson

Sweeping `ocx-sh/index` `main` returned `verified 1 package(s); 0 anomalies`. That is **not** a
pass: `main`'s root carries **no tags and zero `o/` objects**, so a sweep over zero tags trivially
finds zero bad entries. E2b requires *"every version tag recorded"*, which an empty root cannot
satisfy. Reporting that number as a pass would have been the same fixture-that-cannot-fail defect
this change has already tripped over four times.

### Second attempt was invalid — wrong bot code

Sweeping the PR head directly returned **3 anomalies**
(`pinned-tag-mutation committed=50e02438… fresh=4ee19e66…`). Cause: the per-package announce
branch persists and is never rebased, so its checked-out `bot/` source predates `4c8238d` (#62)
and still contains the invented writer (`core/validate_entry.py: {"platforms": …}`). The sweep was
re-deriving with **stale** bot code against **new** data.

### Valid run — `main`'s bot code against the PR's data

```
verified 1 package(s); 0 anomalies
```

Non-vacuous: 6 tags judged, 2 CAS objects present. Exited without reaching the GitHub API at all,
so this is an absence of findings, not a suppressed write.

**Verdict: PASS.** Zero `tag-unrecordable` (the `TagIsNotAnImageIndex` refusal class), zero
`cas-object-missing` / `cas-object-hash-mismatch`, every version tag re-derived to exactly the
committed digest, and no reserved-class entry anywhere.

### The discriminator

Identical data, two bot versions:

| Bot code | Re-derives | Result |
|---|---|---|
| Announce branch (pre-#62, invented writer) | `fresh = 4ee19e66…` | **3 anomalies** |
| `main` (post-#62) | `fresh = 50e02438…` = committed | **0 anomalies** |

This is the strongest single result in the gate. It shows ocx's Rust writer and the bot's Python
re-derivation now produce **byte-identical** output for the same registry state — producer/consumer
agreement across two independent implementations, and exactly what ADR R3 predicted would fail
without D4(b).

## Blocker — PR #64 is red, and not because of this change

`schema-validate-pr` fails:

```
p/michael-herwig/ocx-e2e-hello.json: FAIL (VALIDATION_FAILURE)
  - malformed observation object structure: 'platforms'
```

**Cause: E1-pre residue, not a defect.** The announce branch is **per-package**, and closing a PR
does not delete its branch. E1-pre's PR was deliberately closed unmerged, but its commit stayed on
`indexbot-announce-michael-herwig-ocx-e2e-hello`, so this announce stacked on top and the tree
still carries the old-format object `4ee19e66…fe91`. The validator walks the package's `o/`
directory and rejects it there.

The recorded trap said *"if a PR from it is still open, lands on that same PR."* That was checked
and clear — no PR was open. The real trap is broader: **a stale branch, open PR or not.**

Two things this incidentally proves:

- The root itself is clean — all six tags bind to the new image index; `4ee19e66` is an orphan
  file, not a live binding.
- The updated bot validator correctly refuses the old invented format.

`ocx-sh/index` `main` is unaffected: it carries zero `o/` objects for this package (404).

**Not self-resolved.** Clearing it means deleting the orphan from — or resetting — a branch on the
owner's fork. That is an outward-facing mutation, and the gate's remaining value (E1, E2a) was
obtained without it. Left for the owner.

## Status

| Gate | Verdict |
|---|---|
| **E1** | **PASS** — byte-identity proven live against real GHCR |
| **E2a** | **PASS** — tripwire, per ADR caveat above |
| **E2b** | **PASS** — 0 anomalies over 6 tags, `main` bot vs PR data; stale-bot control run gives 3 |
| PR #64 merge | **Blocked** on E1-pre branch residue (owner decision) |
| `1.0.5` machine lane | Not started |

## Reproduction

```sh
# E1
bash e1_check.sh      # object→filename hash, GHCR fetch by digest, Docker-Content-Digest, cmp
# E2a
bash e2a_source.sh    # proves the registry carries the reserved classes the root must not
```

Both scripts are self-contained (anonymous GHCR pull token; no credential required).
