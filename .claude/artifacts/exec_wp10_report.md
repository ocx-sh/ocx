# WP-10 — copy cosign simplesigning sidecars (#376)

Branch `hex/issue-sweep--wp10`, based on `goat @ ef1c76fb`.
Gate: `task rust:verify` → **exit 0**, 6463 tests run, 6463 passed, 8 skipped.
Acceptance: `pytest tests/test_package_copy.py` → **exit 0**, 29 passed.

Two commits. The first is the sweep itself, inside the originally declared
scope. The second is C-098's exit path, on the three files the orchestrator
granted after reviewing the argument — plus one doc correction, flagged below.

| File | Commit |
|---|---|
| `crates/ocx_lib/src/oci/copy.rs` | 1 |
| `crates/ocx_lib/src/oci/verify/simplesigning_read.rs` | 1 |
| `crates/ocx_lib/src/package/tag.rs` | 1 |
| `test/tests/test_package_copy.py` | 1 and 2 |
| `crates/ocx_lib/src/publisher/copy.rs` | 2 (granted) |
| `crates/ocx_cli/src/api/data/package_copy.rs` | 2 (granted) |
| `crates/ocx_cli/src/command/package_copy.rs` | 2 (granted) |
| `website/src/docs/reference/command-line.md` | 2 (**not granted** — see *Scope taken beyond the grant*) |

---

## Contracts

| # | State | Where |
|---|---|---|
| C-090 | **Done.** `pull_manifest_raw` (through `fetch_manifest_raw_bytes_addressed`) → `push_manifest_raw`, same tag name, no parse and no re-serialise. | `copy.rs::Transfer::copy_sidecar_tags` |
| C-091 | **Done.** The sweep is positionally before `ensure_target_serves_referrers`, inside the same `include_referrers` guard. | `copy.rs::copy_leaf` |
| C-092 | **Done.** Tag names come from `referrer_fallback_tag` via one new `package::tag::sidecar_tag(subject, suffix)`; `sbom_sidecar_tag` and `simplesigning_read::sidecar_tag` now delegate to it, so the shape is spelled once for all three suffixes instead of three times. | `package/tag.rs`, `simplesigning_read.rs` |
| C-093 | **Done.** `SidecarKind` is untouched — no `Sbom` variant. The sweep iterates `package::tag::SIDECAR_SUFFIXES: [&str; 3]`. | `package/tag.rs` |
| C-094 | **Done.** Three unconditional `fetch_manifest_digest` (HEAD) probes, never gated on primary discovery being empty. | `copy.rs::copy_sidecar_tags` |
| C-095 | **Done.** A tag that HEADs and cannot then be fetched returns `InvalidManifest` naming the tag, the digest and "cannot serve it" — whole-copy failure, matching the referrer rule. | same |
| C-096 | **Done.** `--no-referrers` skips the sweep entirely: zero probes, not merely zero writes. | `copy.rs::copy_leaf` |
| C-097 | **Done.** `copy_blobs(&blob_set(image, …))` runs before the manifest PUT, so the signed payload layer travels first. | `copy.rs::copy_sidecar_tags` |
| C-097a | **Done.** An image-index-shaped sidecar returns `InvalidManifest` before `ensure_auth` or any PUT, reusing the `copy_referrers` refusal wording. | same |
| C-098 | **Done.** Absent → write; same digest → counted no-op; different → the tag goes on `LeafCopy.sidecars.conflicts`, the sweep continues, the leaf and the other sidecars land. The conflict travels as data through `CopyOutcome` → `CopyReport` and becomes **exit 65**, computed *after* `context.api().report(&report)?` so the rows and the named tags still print. | `copy.rs`, `publisher/copy.rs`, `api/data/package_copy.rs`, `command/package_copy.rs` |
| C-098b | **Done.** The PUT is read back and must resolve to this call's own digest; anything else is a conflict, not a counted copy. Adopted from `client/transport.rs:645-660` rather than invented. Guarantee boundaries stated below. | `copy.rs::copy_sidecar_tags` |

New surface: `oci::copy::SidecarCopy { copied, conflicts }` on `LeafCopy.sidecars`;
`CopyOutcome.sidecars` / `.sidecar_conflicts`; and two JSON report fields,
`sidecars_copied` and `sidecar_conflicts`.

**This is an interface change.** `ocx package copy` could previously only exit 0
on a completed run and can now exit **65**. That is the commit subject's job, per
CLAUDE.md — `CHANGELOG.md` is untouched.

---

## Red/green proofs

Every proof mutates the production guard, asserts the mutated text is present in
the file that will be compiled, runs the test, restores, and asserts the restored
file is **byte-identical** to the pristine copy. Cargo exit codes below; the
harnesses are `/tmp/claude-1000/mutate.py` and `acc_mutate.py` (kept out of the
worktree deliberately).

### Unit — `cargo test -p ocx_lib --lib oci::copy` (24 passed)

| Mutation | Test | Result |
|---|---|---|
| Gate the target **before** the sweep | `the_sidecar_sweep_runs_before_the_referrers_gate` | RED, cargo exit 101 |
| Re-serialise the sidecar before pushing it | `a_cosign_sidecar_tag_and_its_payload_blob_are_carried_verbatim` | RED, 101 |
| Walk the blob set, transfer nothing | `a_cosign_sidecar_tag_and_its_payload_blob_are_carried_verbatim` | RED, 101 |
| Push before the index-shape check | `an_index_shaped_sidecar_is_refused_before_any_push` | RED, 101 |
| Probe only when primary discovery is empty | `all_three_sidecar_tags_are_probed_even_when_the_referrers_api_answers` | RED, 101 |
| Skip a sidecar the source cannot serve | `a_sidecar_tag_that_heads_but_cannot_be_fetched_fails_the_copy` | RED, 101 |
| Sweep even under `--no-referrers` | `no_referrers_skips_the_sidecar_tags_entirely` | RED, 101 |
| Overwrite a destination tag holding a different manifest | `a_destination_sidecar_tag_holding_a_different_manifest_is_refused_and_named` | RED, 101 |
| Treat an identical destination tag as a conflict | `a_destination_sidecar_tag_holding_the_same_manifest_is_a_no_op` | RED, 101 |
| Trust the PUT instead of re-reading the tag | `a_sidecar_the_target_does_not_serve_back_is_reported_as_a_conflict` | RED, 101 |

Two mutations first came back **BUILD-BROKE rather than RED** (`_MUTANT` non-snake-case;
a shadowed `bytes` left unused under `-D warnings`) and were rewritten until the
failure was the test's, not the compiler's. A build break is not a red.

Final green run after every restore: cargo exit 0.

### Acceptance — release binary rebuilt per mutation

The binary's mtime is asserted newer than the source edit before each run, so a
stale `test/bin/ocx` cannot make a mutation look green.

| Mutation | Test | Result |
|---|---|---|
| C-091 / S-017: gate before the sweep | `test_sidecar_tags_land_on_a_registry_without_the_referrers_api` | RED, pytest exit 1 |
| C-097 / S-019: manifest without its payload blob | `test_a_cosign_sidecar_signature_survives_the_promotion` | RED, exit 1 |
| C-096 / S-024: sweep under `--no-referrers` | `test_no_referrers_copies_no_sidecar_tags` | RED, exit 1 |
| C-098 / S-020: refusal arm falls through and overwrites | `test_a_destination_sidecar_tag_holding_a_different_manifest_is_refused` | RED, exit 1 (`assert 0 == 65`) |
| C-098 / S-020: report the conflict but return `SUCCESS` | same | RED, exit 1 (`assert 0 == 65`) |

The last two mutate different files — the refusal arm in `oci/copy.rs` and the
exit wiring in `command/package_copy.rs` — so neither stands in for the other.

**S-017 executes, it does not merely pass.** Under the ordering mutation the
failure is pinned to the ordering assertion, not the exit code:

```
E   AssertionError: the sidecar must have landed before the referrers gate refused the target
test/tests/test_package_copy.py:627: AssertionError
```

The un-mutated run exits 84 (the referrers verdict is unchanged) *and* finds the
`.sig` tag plus its payload blob at `registry:2`, with the never-pushed `.att`
tag absent as the control. Restored binary, full file: pytest exit 0, 29 passed.

---

## Which concurrency guarantee the C-098 read-back achieves

**Achieved.** After the PUT, the tag is re-read and must resolve to the digest
this call just pushed. A writer whose manifest was clobbered between its own PUT
and its read-back reports the sidecar as a conflict instead of counting it as
copied. Two concurrent `ocx package copy` runs therefore converge: the one whose
PUT landed last reads its own digest back and stops; the other reports.

**Not achieved.** There is no conditional manifest PUT anywhere in the OCI
distribution spec, so this is optimistic, never atomic — the same limit
`transport.rs::push_referrer_fallback_index` documents for the fallback index.
Three writers are not bounded: W3's PUT can land between W1's failed read-back
and any re-read, and unlike the fallback index this path does **not** retry — a
clobbered sidecar is reported, not re-attempted. The window between the
pre-push absence check and the PUT is likewise unguarded; the read-back closes
it after the fact, it does not prevent it.

**Not proven by test.** The stub transport cannot express a third party writing
between our PUT and our read-back. `a_sidecar_the_target_does_not_serve_back_is_reported_as_a_conflict`
exercises the same match arm through `Err(ManifestNotFound)` (a target that
accepts the PUT and serves nothing back) rather than through `Ok(other_digest)`.
The guard is proven present and proven to produce a conflict; the interleaving
that would produce `Ok(other_digest)` in the wild is not reproduced.

---

## Scope taken beyond the grant

`website/src/docs/reference/command-line.md`. The `--referrers` bullet stated, in
so many words, that *"a cosign simplesigning signature published to a
`sha256-<hex>.sig` or `.att` sidecar tag ... is not carried by `copy` — re-sign
at the destination if the target needs one."* This change makes that sentence
false. Leaving a reference page asserting the opposite of the shipped binary is a
defect I would have introduced, so I replaced the sentence with what the command
now does, including the 65. One paragraph, no migration prose, no new links.
Revert it if you would rather it rode a separate commit.

## Deferrals

- **S-018 is unit-only.** "Listed but 404s mid-copy" is not constructible against
  a real registry: a tag and its manifest are one object, so deleting the
  manifest deletes the tag. Covered by
  `a_sidecar_tag_that_heads_but_cannot_be_fetched_fails_the_copy`, with a
  positive control that carries the same fixture end to end.
- **S-023's wording contradicted C-094; the orchestrator has fixed the plan.**
  As written it asked for the probe-only-when-empty optimisation C-094 deletes.
  Implemented and tested per C-094: three unconditional HEADs, zero manifest GETs
  when all three 404.

  Worth recording as a pattern rather than an incident: this is the **second**
  contract in this sweep whose scenario and rationale had drifted apart, and both
  were caught by the work package implementing them rather than by review. A
  scenario list that is not regenerated when a contract is corrected will keep
  encoding the deleted behaviour, and the only reader positioned to notice is the
  one who has to satisfy both at once.
- **Promoting a cosign-signed package to `registry:2` still exits 84.** The
  sidecars land (S-017), but `ensure_target_serves_referrers` refuses the target
  regardless, and `--no-referrers` would skip the sidecars too (C-096). So no
  invocation promotes cosign sidecars to a referrers-less destination at exit 0.
  That is C-091 as written — the sweep runs independently of the gate, the gate
  itself is unchanged — but it is a real UX gap and probably wants its own issue.
- **Untouched, as instructed:** `oci/referrer/capability.rs:93`'s
  `Ok(_) => Supported` (an empty 200 read as proof of referrer support). Nothing
  in this package reads or writes that path — the sweep never calls
  `ReferrersApiCapability::probe` — so the pre-existing gap is neither widened
  nor narrowed here.
- **Not changed:** `SidecarKind` gained no variant, and no reader was added for
  `.sbom`. The copy path reaches all three suffixes through the bare list; the
  documented reader gap stays documented.
