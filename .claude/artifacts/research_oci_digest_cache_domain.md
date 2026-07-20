# Research: OCI Digest-Addressed Cache — Domain/Security Axis

> Axis: domain (OCI spec + security). Produced by worker-researcher (sonnet) for the inspect-binaries-closure plan, 2026-07-20. Persisted by orchestrator (researcher session has no Write tool).

## 1. Verification duty (spec)

- Blobs: client "SHOULD verify that the response body matches the requested digest"; size-check BEFORE hashing to shrink collision space (image-spec descriptor.md).
- Manifests: client "SHOULD verify the returned manifest matches this digest" when digest-addressed; registry "MUST store the manifest in the exact byte representation provided" — byte-for-byte immutability is the spec's trust anchor.
- Practical rule: verify once, on write/ingest (hash-as-you-stream, compare before persisting). A digest-addressed cache hit needs **no re-verification on read** in a single-trust-domain store.
- OCX already conforms: `HashingAsyncReader::finalize()` compares computed vs descriptor digest before any extraction/persist error surfaces (`oci/client/hashing_reader.rs`) — the containerd/ORAS "ingest-then-commit" pattern. Preserve for any new digest-cache-first read path.

## 2. Media types / config blobs

Image manifest, image index, artifact manifest are the three manifest types to expect. Config blob: implementations "MUST NOT attempt to parse" unrecognized config media types — opaque bytes unless the media type is known. OCX's `Manifest::Image`/`ImageIndex` cover the structural split.

## 3. Digest algorithms

Registered: sha256 (MUST), sha512 (MAY), blake3 (MAY). Grammar tolerance for unrecognized algorithms is parsing tolerance, NOT a trust grant — **never treat a grammar-valid-but-unsupported-algorithm digest as cacheable/trusted**; fail closed as unverifiable. OCX's `Digest` enum (Sha256/Sha384/Sha512) defines its own supported set; don't broaden without a verifier.

## 4. Manifest/config size caps

Spec: registries "SHOULD enforce some limit," recommends ≥4 MiB push support, 413 on excess. No client-side read cap is spec-mandated — **real gap**: OCX caps compressed/decompressed layer bytes (CWE-400 guard in `oci/client.rs`) but nothing analogous for manifest/config blob body reads before JSON parse. Recommend extending the cap-before-buffer pattern to manifest/config fetches (unbounded body read from a malicious/MITM'd registry = memory-exhaustion DoS, same class as the layer case).

## 5. Cache poisoning / shared-store trust boundary

`BlobStore::write_blob` is stateless and trusts the caller's digest param — verification lives upstream in `HashingAsyncReader`. Fine today (all writers internally trusted, pre-verify). PR #169 (shareable store) widens the trust boundary to multi-writer: a buggy/malicious writer could plant `blobs/sha256/<hex>` content that doesn't hash to `<hex>`, poisoning every cache-first reader trusting path-implies-content. Recommend: defense-in-depth re-hash inside `BlobStore::write_blob` once the store goes multi-writer (bytes already in hand, cheap). quality-core: "validate external input at system boundaries" — storage becomes a boundary once shared.

## 6. Path traversal / symlink risk

No gap found. `cas_shard_path()` derives `{algorithm}/{hex[0..2]}/{hex[2..32]}` from already-validated hex (post `Digest` parse) — attacker strings never reach path construction. Invariant to preserve: digest parsing always precedes `cas_shard_path`. CVE-2024-9676 (Podman/Buildah/CRI-O, GHSA-wq2p-5pc6-wpgf) reminder: symlink-target validation matters where shared-store content gets *extracted*; OCX's `escapes_root`/`validate_symlinks_in_dir` (`utility/fs/path.rs`) cover layer extraction — reuse, don't hand-roll, in any new read path that follows symlinks.

## 7. Offline/air-gap conformance

distribution-spec is a wire protocol, no "offline mode" clause. Digest content immutable by spec → serving digest-addressed requests purely from cache is spec-conformant forever. Tag-addressed requests inherently not answerable offline (tags mutable) — client policy, not spec. OCX's `ChainMode` encodes exactly this split (digest: local-first every mode; unpinned-tag resolve: blocked in Offline/Frozen). No changes needed.

## 8. Tag freshness (NOT digest)

HEAD `/v2/<name>/manifests/<tag>` returns `Docker-Content-Digest` + `Content-Length`, no body — spec-sanctioned cheap freshness probe vs cached tag→digest mapping. ETag/If-None-Match → 304 equivalent. Prefer HEAD-then-conditional-GET for freshness-only checks.

## Sources

- https://github.com/opencontainers/distribution-spec/blob/main/spec.md
- https://github.com/opencontainers/image-spec/blob/main/manifest.md
- https://github.com/opencontainers/image-spec/blob/main/descriptor.md
- https://github.com/opencontainers/image-spec/blob/main/image-layout.md
- https://github.com/advisories/GHSA-wq2p-5pc6-wpgf (CVE-2024-9676)
- https://pkg.go.dev/oras.land/oras-go/v2/content/oci

## Recommendation

Design already ~90% spec-aligned (verify-on-write, digest-immutability-as-cache-forever, offline/tag split via ChainMode). Two concrete additions: (a) size-cap manifest/config blob reads before buffering/parsing (this plan may include); (b) once PR #169's shared store lands, enforce digest verification inside `BlobStore::write_blob` (that PR's scope, not this one).
