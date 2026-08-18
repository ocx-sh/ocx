# Data, Serialization and On-Disk Formats

How a struct written by one binary version stays readable by another, and how
the same logical content always produces the same bytes. Covers version
envelopes, strict-vs-tolerant deserialization, canonical output policy, and
digest handling. Loads whenever a lockfile, cache, state file, manifest,
`--json` payload or content digest is in play. Bounds and resource limits on
untrusted input live in `security.md`.

Contents: [Format Evolution](#format-evolution) · [Deterministic Output](#deterministic-output) ·
[Digests](#digests) · [What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers. **The mechanism** — a first-field version enum, probe-then-dispatch
reads, a canonicalization policy owned by one module — is general Rust practice.
**A pinned decision**: strict vs tolerant is chosen on the *producer axis*, not
per project. Files this binary wrote and reads back are strict; anything a
sibling binary, plugin author or LLM caller may legitimately extend is tolerant.
That resolves the standing fleet-compat disagreement in both directions and is
not re-litigated.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Format Evolution

A shipped on-disk format is forever. The version envelope must exist in the
commit that creates the format — retrofitted later, it can never distinguish
"old file, no version" from "corrupt file", and that ambiguity arm lives in the
reader permanently.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DATA-FMT-01 | Every top-level on-disk struct carries `version` as its **literal first field**, typed as a closed `#[repr(u8)]` enum via `serde_repr`, from the commit that creates the format. | `rg -n --type rust --glob '!external/**' -e 'struct \w+File' -e 'struct \w+Lock' -e 'struct \w+State' -e 'struct \w+Cache' -e 'struct \w+Manifest' .` — each hit needs a `version` field deriving `Deserialize_repr`; the pattern also catches purely in-memory structs, so restrict to added lines on a diff. History smell: `git log -p --follow` showing the field added in a later commit than the struct | MUST |
| DATA-FMT-02 | The read path probes the version, then dispatches on an **exhaustive** `match` over the closed enum. Never a direct `from_slice::<Current>` / `from_str::<Current>` on raw bytes — that turns "predates a field" into an opaque field-level error. | `rg -n --type rust --glob '!external/**' 'match .*version' .` shows one arm per variant — discard hits outside the read path the change touches; the probe struct must *not* carry `deny_unknown_fields`; a `_` arm on a closed internal enum is a finding | MUST |
| DATA-FMT-03 | When the typed probe fails, fall back to an untyped `{ version: u8 }` probe before surfacing a parse error; a number above the highest known variant returns an error naming the version and telling the user to upgrade. | `rg -n --type rust --glob '!external/**' -e 'RawVersionProbe' -e 'raw.version >' .`; feeding `{"version": 255, …}` must produce an error containing "newer"/"upgrade", not a bare `serde_json::Error` | MUST |
| DATA-FMT-04 | Strict vs tolerant is a declared per-type decision with the reason in the doc comment. `#[serde(deny_unknown_fields)]` for what this binary wrote and reads back, or a hand-authored declaration where a typo must be loud. `#[serde(flatten)] extra: BTreeMap<..>` for anything a different or newer producer may extend. The two are mutually exclusive at the serde level, so the choice is forced once. | `rg -n --type rust --glob '!external/**' -e 'serde\(deny_unknown_fields\)' -e 'serde\(flatten\)' .` — every hit needs a doc comment containing "strict", "tolerant" or "forward-compat" plus the reason; restrict to added lines on a diff. A persisted struct with **neither** attribute is the smell | MUST |
| DATA-FMT-05 | Never `#[serde(default)]` a semantically required field — name, hash, digest, URL, identifier, version. Route cross-field validation through `#[serde(try_from = "RawT")]` with one hand-written `TryFrom`, so every deserialization path gets it. | Per `#[serde(default)]` in a diff, the field type must have a genuinely safe zero value (`Option`, `Vec`, `bool`, enum with explicit `#[default]`). `rg -l --type rust --glob '!external/**' 'serde\(try_from' .` and `rg -l --type rust --glob '!external/**' 'impl TryFrom<Raw' .` return the same set | MUST |
| DATA-FMT-06 | `#[non_exhaustive]` only where a `match` genuinely lives outside the defining crate — public error enums, cross-binary wire enums. Internal version/kind enums stay total; in-crate the attribute converts a forgotten-variant compile error into a silent wildcard. | `rg -n --type rust --glob '!external/**' 'non_exhaustive' .` — each hit needs at least one match site outside the crate; restrict to added lines on a diff | SHOULD |
| DATA-FMT-07 | Wire enums expected to gain variants use externally-tagged (default) or adjacently-tagged representation. `#[serde(untagged)]` only when variants are trivially field-shape-distinguishable **and** a test asserts on the no-variant-matched error text — untagged discards every variant's failure reason. | `rg -n --type rust --glob '!external/**' 'serde\(untagged\)' .` — each needs a test feeding a value matching no variant and asserting on message content, not `is_err()` | SHOULD |
| DATA-FMT-08 | Mark every on-disk-format type with the literal doc-comment first line `/// On-disk format:`. Informal phrasing means no grep finds the full set. | `rg -n --type rust --glob '!external/**' '^\s*/// On-disk format:' .`; CI asserts every struct with a `*Version`-typed `version` field carries the marker | SHOULD |
| DATA-FMT-09 | Keep a checked-in fixture per format version (`tests/fixtures/<format>/v1.json`, `v2.json`, plus a synthetic `v99`), loaded by one table-driven test asserting every historical version still parses or migrates and the future version refuses. | `rg --files --glob '**/tests/fixtures/**' . .` per format; the refusal assertion is on the specific error kind, not `is_err()` | SHOULD |
| DATA-FMT-10 | Reserialize machine-generated files with plain `serde`; reach for `toml_edit` only for a file a human hand-authors **and** the tool rewrites. Plain serde reconstructs from the typed value, destroying comments — desired normalization for a lockfile, data loss for a hand-edited config. | For any file both hand-editable and tool-rewritten, a test round-trips a fixture *containing a comment* and asserts the comment survives. Struct-comparison round-trips cannot see comment loss | CONSIDER |

Additive change is safe: a new optional field, a new enum variant behind a new
version. Renaming a field, changing its type, or repurposing a discriminant is
breaking and needs a new version arm — not a `#[serde(alias)]` patch.

```rust
/// On-disk format: the lockfile envelope. Strict — this binary wrote it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
    version: LockVersion, // first field, always
    artifacts: BTreeMap<ArtifactId, LockedArtifact>,
}

#[derive(Serialize_repr, Deserialize_repr)]
#[repr(u8)]
enum LockVersion { V1 = 1, V2 = 2 }

/// Probes only the version. No `deny_unknown_fields` — later fields are fine.
#[derive(Deserialize)]
struct VersionProbe { version: LockVersion }

/// Reached only when the typed probe failed: tells "from the future" from "corrupt".
#[derive(Deserialize)]
struct RawVersionProbe { version: u8 }
```

## Deterministic Output

Byte determinism is a property of the *type*, not of the writer. Enforce it at
the field declaration, not with a sort call before each write.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DATA-DET-01 | Never let a `HashMap`/`HashSet` reach a `Serialize` field, a `serde_json::to_*` call, a hasher, or a writer. Use `BTreeMap`/`BTreeSet`. Iteration order is per-instance randomly seeded by design, so identical content serializes to different bytes every process. | Cross-reference `rg -l --type rust --glob '!external/**' -e HashMap -e HashSet .` against `rg -l --type rust --glob '!external/**' -e 'derive\(.*Serialize' -e 'serde_json::to_' .`; any path in both lists is a finding — the steady-state intersection is dozens of files, so restrict to added lines on a diff. The map is usually built far from the write site | MUST |
| DATA-DET-02 | `IndexMap`/`IndexSet` only where insertion order **is** the declared semantics, with an adjacent `// order: <reason>` comment on the field. `BTreeMap` self-sorts and removes "did we insert in the right sequence" as a bug class. | `rg -n --type rust --glob '!external/**' -e IndexMap -e IndexSet .` — every hit needs a nearby `// order:` comment | SHOULD |
| DATA-DET-03 | Never depend on `serde_json`'s default key sortedness. Either fail CI if `preserve_order` appears in the feature graph, or call `.sort_keys()` before every hash/write — a transitive dependency's feature choice flips the whole workspace to insertion order with zero code change here. | `cargo tree -e features -i serde_json` as a CI step, failing if `preserve_order` appears in its output; `rg -n --type rust --glob '!external/**' 'sort_keys\(\)' .` if the feature is knowingly on | MUST |
| DATA-DET-04 | Declare one canonicalization policy per output format (lockfile, cache index, `--json`, SBOM) in a single module, covering: key order; `None` omit-vs-`null`; empty collections omit-vs-`[]`/`{}`; floats (forbidden in any hashed document); trailing newline; `\n` written as raw bytes; Unicode NFC at the input boundary. Serde's default is to *emit* `null`/`[]`; omission is opt-in per field, so two authors' habits produce an undiagnosable digest mismatch. | A `canonical.rs` exists and every serializer entrypoint references it; `rg -n --type rust --glob '!external/**' -e f32 -e f64 .` — discard hits in files not reachable from a hashing path, any that remain are a finding | MUST |
| DATA-DET-05 | Building a tar/OCI layer normalizes all four axes explicitly: entry order byte-sorted by path (never `readdir` order), one fixed mtime from `SOURCE_DATE_EPOCH` or a constant, `uid`/`gid` = 0, one fixed mode policy. None is the default of a `WalkDir` + `tar::Builder` loop. | `rg -l --type rust --glob '!external/**' -e 'tar::Builder' -e 'tar::Header' .` lists the layer builders; `rg -n --type rust --glob '!external/**' -e 'set_mtime' -e 'set_uid' -e 'set_gid' -e 'SOURCE_DATE_EPOCH' .` must show every axis set inside those files; `rg -n --type rust --glob '!external/**' 'SystemTime::now\(\)' .` must return no hit in any of them — discard hits in files the first command did not list. Build the layer twice and `cmp` | MUST |
| DATA-DET-06 | CI builds every reproducibility-sensitive artifact **twice in one run** and diffs the bytes; a second job pins a golden fixture's digest. `HashMap` reseeding is per-process, so a single run cannot tell deterministic from lucky. | The CI workflow has a step running the serialize command twice into `diff`/`cmp` | MUST |

## Digests

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DATA-DIG-01 | All digest formatting and parsing lives in one `digest` module. `format!("{:x}", ..)`, `hex::encode`, `hex::encode_upper` or a hand-rolled byte loop for a digest anywhere else is a defect — a case or padding mismatch fails as a silent cache miss, never as a crash. | `rg -n --type rust --glob '!**/digest.rs' --glob '!external/**' -e 'format!\("\{:[xX]\}"' -e 'hex::encode' .` — any hit fails review | MUST |
| DATA-DIG-02 | `Display` emits `algorithm:` + lowercase hex always; `FromStr` **rejects** uppercase hex rather than silently lowercasing it. The OCI descriptor grammar states `[A-F]` MUST NOT be used, so accepting it masks a non-compliant producer. | Round-trip proptest `Digest::from_str(&d.to_string()) == Ok(d)`; a unit test asserting `from_str("sha256:ABCD…")` is `Err` | MUST |
| DATA-DIG-03 | Digest storage is a fixed-size type (`[u8; 32]`/`[u8; 64]`) inside an algorithm-tagged enum, never `Vec<u8>` plus a separate algorithm field — `sha2::finalize()` already hands you the length invariant, and a `(Vec<u8>, Algorithm)` pair lets length and tag drift apart. | `rg -ni --type rust --glob '!external/**' -e 'digest\w*: Vec<u8>' -e 'hash\w*: Vec<u8>' -e 'Vec<u8>,\s*Algorithm' .` — reading heuristic; clippy cannot see this | SHOULD |
| DATA-DIG-04 | Hash exactly the bytes received from the registry. Never re-serialize a parsed value before computing a verification digest — OCI's model is "hash the bytes that arrived", so re-serializing yields *our* canonical form, not the one every other client computed. | At each verify/pull path, the slice handed to the hasher is the original `Bytes`/`&[u8]`. Regression test: a fixture manifest with non-canonical whitespace and key order still verifies | MUST |
| DATA-DIG-05 | `subtle::ConstantTimeEq` is for secret-vs-secret comparisons only (tokens, credentials). Compare content digests with `==` — they are public integrity values with no timing channel to defend. | `rg -n --type rust --glob '!external/**' -e ConstantTimeEq -e ct_eq .` — every hit adjacent to a secret type with a one-line justification naming it; a `Digest`-typed argument fails review | SHOULD |

## What Agents Get Wrong Here

1. **`#[serde(default)]` to make a failing test pass.** One attribute silences a
   missing-field error and converts it into a silently wrong empty value. The
   single most common shortcut in this domain.
2. **`HashMap` for "a map of X to Y"**, with nothing local signalling that a
   struct three call sites downstream derives `Serialize`.
3. **Version-blind recall on `serde_json` ordering.** Told "make output
   deterministic", adds `preserve_order` plus manual sorting — inverting the
   actual default and making it strictly worse.
4. **`from_str::<CurrentShape>` because it is the shortest happy path.** Every
   fixture the agent writes is freshly generated, so cross-version reads are
   never exercised.
5. **Hashing a re-serialized `Value` instead of the response bytes.** Invisible
   to any test that round-trips locally-constructed values.
6. **Adding the version field on the second format-changing PR** — "there's only
   one shape so far" — leaving already-written files indistinguishable from
   corrupt ones forever.
7. **Tar built from a plain `WalkDir` + `append_file` loop**, inheriting
   `readdir` order, real mtimes and the build machine's uid/gid/umask. All four
   axes wrong, none surfacing as a compile error or a single-run test failure.
8. **`#[non_exhaustive]` on an internal enum for "future-proofing"**, plus the
   wildcard arm that then silently absorbs the next forgotten variant.
9. **`subtle`/`ConstantTimeEq` on public content digests**, because the crate
   name reads as unconditionally more secure.
10. **`#[serde(untagged)]` for a "one of several shapes" enum** — least code,
    worst diagnostics, never tested for the no-variant-matched message.
11. **Reserializing hand-edited TOML with plain `serde`**, dropping every
    comment, because the round-trip test compares parsed values not bytes.

## Sources

- [OCI image-spec: descriptor](https://github.com/opencontainers/image-spec/blob/main/descriptor.md) — the `algorithm:hex` grammar and the explicit `[A-F]` MUST NOT rule
- [OCI distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — client verification hashes the bytes received
- [RFC 8785: JSON Canonicalization Scheme](https://www.rfc-editor.org/rfc/rfc8785) — key sort, number rendering, `-0` normalization, NaN rejection
- [`serde_json::map::Map`](https://docs.rs/serde_json/latest/serde_json/map/struct.Map.html) — `BTreeMap`-backed unless `preserve_order` flips it project-wide
- [`std::collections::HashMap`](https://doc.rust-lang.org/std/collections/struct.HashMap.html) — iteration order is arbitrary and per-instance seeded, deliberately
- [serde container attributes](https://serde.rs/container-attrs.html) — `deny_unknown_fields`, its incompatibility with `flatten`, and `try_from`
- [Rust reference: `#[non_exhaustive]`](https://doc.rust-lang.org/reference/attributes/type_system.html) — exact in-crate vs out-of-crate effect
- [reproducible-builds.org: archives](https://reproducible-builds.org/docs/archives/) and [SOURCE_DATE_EPOCH](https://reproducible-builds.org/specs/source-date-epoch/) — the four tar normalization axes
