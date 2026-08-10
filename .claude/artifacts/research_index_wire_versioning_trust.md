# Research: Sparse-index format versioning and provenance-gated trust

## Metadata

**Date:** 2026-08-09
**Domain:** security, packaging
**Triggered by:** the servable-index-snapshot design — whether a locally authored index tree may be read leniently (absent `format_version` ⇒ assume 1) while the byte-identical tree read over a transport stays fail-closed, and how to evolve a frozen wire format without a fleet flag day.
**Expires:** 2027-02-09 (re-verify; CVE landscape moves)

## Direct Answer

**No surveyed sparse-index format makes trust of the version pin depend on how the
bytes arrived.** Checked directly and found provenance-based leniency absent from
all six: crates.io sparse index, PyPI/PEP 691, Debian, Alpine `apk`, Nix
`nix-cache-info`, Go checksum DB. Where leniency-on-absent-version exists it is
**uniform across every reader**, regardless of transport.

Provenance-based leniency — trusting bytes because "we wrote them" — is
[CWE-501 Trust Boundary Violation](https://cwe.mitre.org/data/definitions/501.html)
and has produced two recent, high-severity supply-chain CVEs in this exact shape.

The recommended alternative gets the same practical benefit with strictly less
code: **absent ⇒ assume version 1 for every reader; unrecognized ⇒ hard fail for
every reader** (PEP 629/691's rule).

## Technology Landscape

### Established (proven, widely accepted)

| Pattern | Status | Notes |
|---|---|---|
| Additive, must-ignore-unknown versioning | Standard | PEP 691/629 `api-version`; Cargo per-entry `v`; COSE `crit`; OCI `artifactType` MUST-NOT-error-on-unknown. All converge on: absence defaults to the *lowest* meaning, never to rejection, for **every** reader |
| Dual-publish then retire | Standard | crates.io `features` → `features2`; Debian canonical path + `by-hash`. The only zero-flag-day strategy for a genuine break; cost is indefinite duplication until a deliberate sunset |
| Fail-closed on a major-version floor | Standard | TUF `spec_version`; adopter chooses exact-vs-major granularity, but never *per-reader* granularity |

### Emerging (early but promising)

| Pattern | Signal | Worth watching because |
|---|---|---|
| Capability/feature-presence signaling instead of one version integer | Debian `Acquire-By-Hash`; hash-algorithm-floor fields | Lets a fleet upgrade one capability at a time rather than bumping a monolithic version |
| COSE `crit` must-understand lists (RFC 9052) | Standardized | Producer declares which extensions are load-bearing *per message*; unlisted extensions are ignorable, listed-but-unrecognized is a rejection |

### Declining (losing mindshare)

| Pattern | Signal | Avoid because |
|---|---|---|
| "First writer into a shared namespace is implicitly trusted by the next reader" | CVE-2025-36852 (CVSS 9.4), CVE-2026-5223 | This is the literal architecture behind both CVEs. Any design letting two trust levels resolve to the same on-disk path is trending toward known-bad |
| Purely advisory minimum-client-version fields | npm `engines` ignored ~a decade until pnpm added enforcement | Advisory-only hints are ignored in practice; if the constraint matters, enforce it |

## Design Patterns Worth Considering

- **Uniform absent-version default (PEP 629/691)** — *"if that data does not exist
  clients MUST assume that it is version 1.0"*; major mismatch ⇒ hard fail, minor
  ⇒ warn. Notably this leniency is precedent for the **untrusted network** case:
  PyPI's simple API is only ever fetched over HTTP.
  [PEP 629](https://peps.python.org/pep-0629/) ·
  [PEP 691](https://peps.python.org/pep-0691/)
- **Structural namespace isolation over caller-based trust** — CREEP's durable fix
  was making trusted and untrusted producers physically unable to share a cache
  key, not distinguishing them by which code called the reader.
  [Nx writeup](https://nx.dev/blog/cve-2025-36852-critical-cache-poisoning-vulnerability-creep)
- **Write-path guarantees, not read-path checks** — CVE-2026-5223's fix rejected
  symlinks in tarballs unconditionally rather than re-verifying the cache on read.
  [Rust advisory](https://blog.rust-lang.org/2026/05/25/cve-2026-5223/) ·
  [PR #17031](https://github.com/rust-lang/cargo/pull/17031)
- **Additive path sharding (Debian `by-hash`)** — canonical path untouched, new
  sharded paths advertised by a new *ignorable* field. The pattern to copy if
  `c/index.json` ever needs sharding.
  [DebianRepository/Format](https://wiki.debian.org/DebianRepository/Format)

## Key Findings

1. **PEP 691/629 is the closest structural analog** to the OCX index — static,
   HTTP-served, sparse JSON with an explicit version field — and its absent-version
   leniency applies identically to every client, over an untrusted transport.
   [PEP 629](https://peps.python.org/pep-0629/)
2. **Cargo's sparse index has no document-level version field at all.** Versioning
   is per-entry (`v`, defaulting to 1 when absent), and Cargo 1.51+ *ignores*
   schema versions it does not recognize — must-ignore-unknown, applied uniformly
   whether the index came from crates.io or a self-hosted mirror.
   [registry-index](https://doc.rust-lang.org/cargo/reference/registry-index.html)
3. **CVE-2026-5223 (Cargo, 2026-05-25)** — the local extraction cache was
   implicitly trusted as "state we wrote"; a malicious tarball planted a symlink
   writing one directory below its own slot, corrupting another crate's cached
   source. The fix closed the **write** path, unconditionally, rather than adding a
   read-path checksum.
   [advisory](https://blog.rust-lang.org/2026/05/25/cve-2026-5223/)
4. **CVE-2025-36852 "CREEP" (CVSS 9.4)** generalizes it across build caches:
   untrusted PR branches and trusted `main` resolved to the same cache key, so a
   poisoned artifact was indistinguishable to any later reader — *"checksums always
   match because poisoning occurs before hashing."*
   [SentinelOne record](https://www.sentinelone.com/vulnerability-database/cve-2025-36852/)
5. **CWE-501 is the precise taxonomy match** for "same bytes, same path, two
   readers with different checks selected by caller" — safety rests entirely on a
   provenance guarantee the type system does not encode.
   [CWE-501](https://cwe.mitre.org/data/definitions/501.html)
6. **TUF requires preserving unknown fields** when re-serializing. OCX's
   `CatalogDocument` is a typed struct that drops them — a forward-compat hazard
   **independent of** the provenance question, and worth fixing on its own merits.
   [TUF spec](https://theupdateframework.github.io/specification/latest/)
7. **JSON gets no free unknown-field preservation.** Protobuf preserves unknown
   fields only in its binary encoding; `protojson` does not. OCX's hand-rolled
   preservation (order-preserving `Value` + `serialize_root`) is therefore load-
   bearing and divergence between readers is exactly the hazard.
   [protobuf best practices](https://protobuf.dev/best-practices/dos-donts/)
8. **Advisory-only minimum-version fields have a poor record.** npm/pnpm `engines`
   was warning-only for over a decade and widely ignored; pnpm added enforcement
   because advisory demonstrably did not prevent incompatible installs. Python's
   `Requires-Python` is enforced but self-reported and produces confusing
   "no matching distribution" errors.
   [pnpm#9142](https://github.com/pnpm/pnpm/issues/9142) ·
   [pip#12216](https://github.com/pypa/pip/issues/12216)

## Recommendation

**Do not implement the provenance-based leniency split.** Adopt PEP 629/691's
uniform rule: `format_version` absent ⇒ assume 1 for *every* reader, local or
fetched; unrecognized ⇒ hard fail for every reader. This delivers the benefit the
split was chasing — a locally authored tree with no explicit pin still round-trips
— while deleting a code path instead of adding one.

If a genuine need arises to treat definitely-just-written-by-us bytes specially,
gate it on an **in-memory value never round-tripped through the filesystem in the
same call**, never on which function called the parser: both CVEs show that "this
path is only ever written by us" erodes the moment shared caches, CI restores,
`git clone`, or symlinks enter — and it erodes silently and remotely.

For future format evolution, adopt COSE's must-understand model (or minimally:
unknown *sibling* fields always ignored, `format_version` itself always
load-bearing) plus Debian's additive path sharding. Reserve dual-publish for the
one break that cannot be avoided.

For a minimum-client-version hint: confine it to the message on a path that
**already refuses**. That sidesteps both the "advisory fields get ignored" evidence
and the "config.json must never be load-bearing for trust" constraint, because it
changes no outcome — only a diagnostic.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [PEP 629](https://peps.python.org/pep-0629/) | Spec | 2020 | Absent-version and fail/warn semantics; closest analog |
| [PEP 691](https://peps.python.org/pep-0691/) | Spec | 2022 | `meta.api-version` field definition |
| [Cargo registry-index](https://doc.rust-lang.org/cargo/reference/registry-index.html) | Docs | current | `config.json`, per-entry `v`, must-ignore-unknown, `features2` dual-publish |
| [RFC 2789](https://rust-lang.github.io/rfcs/2789-sparse-index.html) | RFC | 2019 | Original sparse-index proposal; **silent** on version pinning — a notable absence |
| [CVE-2026-5223 advisory](https://blog.rust-lang.org/2026/05/25/cve-2026-5223/) | Advisory | 2026-05 | Cache-poisoning via symlink; write-path fix |
| [cargo#17031](https://github.com/rust-lang/cargo/pull/17031) | Repo | 2026-05 | The actual fix |
| [CREEP writeup](https://nx.dev/blog/cve-2025-36852-critical-cache-poisoning-vulnerability-creep) | Blog | 2025 | Cache-trust conflation; structural-isolation fix |
| [CWE-501](https://cwe.mitre.org/data/definitions/501.html) | Taxonomy | current | Trust Boundary Violation |
| [RFC 9052](https://datatracker.ietf.org/doc/rfc9052/) | RFC | 2022 | COSE `crit`, must-understand vs may-ignore |
| [TUF spec](https://theupdateframework.github.io/specification/latest/) | Spec | current | Unknown-field preservation; `spec_version` granularity |
| [DebianRepository/Format](https://wiki.debian.org/DebianRepository/Format) | Wiki | current | `Acquire-By-Hash`, additive sharding |
| [protobuf.dev](https://protobuf.dev/best-practices/dos-donts/) | Docs | current | Unknown-field preservation is binary-only |
| [pnpm#9142](https://github.com/pnpm/pnpm/issues/9142) | Repo | current | `engines` advisory track record |
| [pip#12216](https://github.com/pypa/pip/issues/12216) | Repo | current | `Requires-Python` diagnostics |
| [Alpine apk spec](https://wiki.alpinelinux.org/wiki/Apk_spec) · [nix-cache-info](https://nix.dev/manual/nix/2.34/protocols/nix-cache-info.html) · [Go sumdb](https://go.googlesource.com/proposal/+/master/design/25530-sumdb.md) | Specs | current | Checked directly: **no** provenance-based leniency in any of them |
