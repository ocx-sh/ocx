# SPK-SIGN — SP-6: every network call in verify under `LookupOnly`

Claim: under DV-1 (release-build `Client::with_blob_overlay`) plus DV-16 (skip the dial guard), a `LookupOnly` auto-verify
issues no network call. Method: code read, exact lines, branch `hex/adr-package-export-import--spk-sign` at its base commit.
No prototype built (the overlay type does not exist yet; a read of every `client.transport()` / `reqwest` / DNS site is
decisive, and no test could exercise `LookupTransport` before A0).

Result: **SP-6 holds** under DV-1 + DV-16. The fallback (thread the lookup through `VerifyContext`) is NOT needed.

## Network-call inventory

Legend: (a) covered by an overlay client's transport; (b) held off by `offline = true`; (c) needs DV-16; (d) outside the
`Client`, held by the index chain mode, not by the overlay.

| # | Site | Call | Class |
|---|---|---|---|
| 1 | `ocx_sign/src/verify/pipeline.rs:413,423-427` (`read_unverified…` SBOM listing) | `transport.list_referrers_with_fallback` | a |
| 2 | `pipeline.rs:456,493-519,534-566,580-640` | `pull_sbom_sidecar_manifest`, `pull_referrer_manifest_capped`, `pull_blob_capped` on `transport` | a |
| 3 | `pipeline.rs:709,717-722,736` (`refuse_unsigned`) | `list_referrers_with_fallback`, `pull_sbom_sidecar_manifest` | a |
| 4 | `pipeline.rs:861,878` (`scan`), `list_signature_referrers` | referrers listing (+ tag-schema fallback) | a |
| 5 | `pipeline.rs:935,952,967,1008,1049,1064,1110-1190,1250-1286` | `pull_subject_manifest_verified`, `pull_referrer_manifest_capped`, bundle blob `pull_blob_capped`, `.sig` sidecar tag read | a |
| 6 | `pipeline.rs:1821-1880,2203-2215` helpers | `pull_blob_streaming`, `pull_manifest_raw` | a |
| 7 | `ocx_sign/src/verify/simplesigning_read.rs:110-193` | `pull_manifest_raw`, `pull_payload` -> `pull_blob_capped` (all take `&dyn OciTransport`) | a |
| 8 | `ocx_sign/src/verify/attestation_sidecar.rs:83-209` | `pull_manifest_raw`, `pull_envelope` -> `pull_blob_capped` | a |
| 9 | `ocx_sign/src/verify/candidates.rs:102,155,191,223` | referrers, `pull_manifest_raw`, `pull_blob_streaming` (used by sign backfill, not auto-verify) | a |
| 10 | `pipeline.rs:844` `client.transport_reference` | pure mirror rewrite, no I/O | a (kept) |
| 11 | `pipeline.rs:816-823` `resolve_sigstore_url(rekor_url)` | DNS lookup, `ssrf.rs:187/265` | b (`if !ctx.offline`) |
| 12 | `pipeline.rs:838` `ocx_oci::ssrf::guard_physical_dial` | `guard_destination` -> `tokio::net::lookup_host` (`ssrf.rs:552-569`); runs only when `physical.registry() != logical.registry()` (mirror/index rewrite) | **c** — not gated by `offline` |
| 13 | `pipeline.rs:1731`, `fetch_rekor_public_key_pem` `pipeline.rs:1783` | HTTPS GET `api/v1/log/publicKey` | b (`None if offline => Err(OfflineNoPinnedRekorKey)`) |
| 14 | `pipeline.rs:1400,1483,2167` `cache_trust_material` | local file write only | no network (also `!offline`) |
| 15 | `ocx_sign/src/verify/trust_resolve.rs:38-40` -> `trust_root.rs:76-81` `TrustRoot::load_embedded` | TUF fetch (`SIGSTORE_TUF_URL`, `sigstore_http_client`) | b (`None if offline => Err(OfflineNoPinnedRekorKey)`; `enforce_offline_rekor_key` `:104`) |
| 16 | `ocx_package_manager/src/tasks/auto_verify.rs:450-459` | `resolve_sigstore_url` + `fetch_rekor_public_key_pem` | b (`!auto_verify.offline`) |
| 17 | `auto_verify.rs:414-430` `resolve_local_trust_root` | file reads only (never network by its own doc) | no network |
| 18 | `tasks/verify.rs:92` -> `tasks/resolve_subject.rs` `verify_resolver` (`index.fetch_manifest`, `index.route`); `auto_verify.rs:149-151` `committed_publishers`/`declared_publishers`; `ocx_index/src/chained_index.rs:857-880` `physical_reference` | index-chain reads; may reach a source (root document, manifest, tolerant DNS guard) | **d** — the overlay does NOT cover it; held only when the PM's index chain is `Frozen`/local-only (`ChainMode::Frozen`, `chained_index.rs:929`). The import PM must be built with such an index; P-VERIFY asserts it with a source that panics on any call. |

Sites 1-9 are the whole transport surface: every one goes through `client.transport()` (`pipeline.rs:413,709,861`) and takes
`&dyn OciTransport`; no verify code calls another `Client` method except `transport_reference` (site 10). So one transport
replacement covers them all.

## `Client::with_transport` (`ocx_oci/src/client.rs:288-299`)

Confirmed `#[cfg(any(test, feature = "__testing"))]`. It builds a **fresh** `Client` and resets: `recipe` to
`TransportRecipe::default()` (auth, plain-HTTP set, DNS resolver, extra CA roots lost), `lock_timeout` to 5 s,
`tag_chunk_size`/`repository_chunk_size` to 100, `progress` to disabled, `mirrors` to empty, `transport_cell` to a new cell.
Unusable for a release overlay.

## What `with_blob_overlay(self, ...)` must keep

Start from `self` (a `Clone` shares the `Arc<OnceLock>` `transport_cell`, so the overlay must NOT reuse the cell):
- keep `recipe`, `lock_timeout`, `tag_chunk_size`, `repository_chunk_size`, `progress`, `mirrors` (clone them).
- `LookupOnly`: `transport_cell = Arc::new(OnceLock::from(Box::new(LookupTransport{..})))` — the recipe is never built, so no
  reqwest client, no DNS resolver, no auth. `ensure_auth` must be `Ok(())`.
- `Live`: `transport_cell = Arc::new(OnceLock::new())` holding a wrapper whose inner is a clone of the original `Client`
  (delegates to `inner.transport()`, built lazily from the kept recipe). Do not call `self.transport()` eagerly in the
  constructor (it would force a build and borrows the Box the Arc owns).
- `LookupTransport` receives the mirror-rewritten reference (`transport_reference`, `client.rs:330`): key the lookup on
  repository + digest, never on host, or a `[mirrors]` rewrite misses.

## DV-16 skip

The one unguarded network call is `guard_physical_dial` (site 12). Put the skip at `pipeline.rs:838` in
`VerifyPipeline::resolve_target`: skip when `client.blob_overlay_mode() == Some(OverlayMode::LookupOnly)`. Derive it from the
client, so `VerifyContext` (constructed at ~9 test sites) is untouched. Rekor/TUF stay held by `offline = true` (11, 13, 15,
16); an import run must set `offline = true`.

## Stub shape for A0

```rust
// ocx_oci::client
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayMode { Live, LookupOnly }
impl Client {
    pub fn with_blob_overlay(self, mode: OverlayMode, home_blobs: PathBuf /* or the blob-store handle */,
        lookup: Arc<dyn BlobLookup>, registry: &str, accept_artifact_types: Vec<String>) -> Self; // release build, keeps mirrors/recipe/lock_timeout/progress/chunks
    pub fn blob_overlay_mode(&self) -> Option<OverlayMode>; // None when no overlay; read by resolve_target (DV-16)
}
// ocx_package_manager::tasks::auto_verify
#[derive(Clone)] pub enum ReferrerSource { Registry, LookupOnly(Arc<dyn BlobLookup>) }
impl AutoVerify { pub fn with_referrer_source(self, source: ReferrerSource) -> Self; } // LookupOnly: one overlay client per logical registry, offline = true
// ocx_sign::verify::pipeline::resolve_target (line ~838):
//   if client.blob_overlay_mode() != Some(OverlayMode::LookupOnly) { guard_physical_dial(..).await.map_err(..)?; }
```
`ReferrerSource` is an enum, not a trait; exact `lookup`/`home_blobs` types follow C-012 and the P-LOOKUP stub.

## Caveats for P-VERIFY

- Offline is `Err(OfflineNoPinnedRekorKey)` whenever no pinned Rekor key exists (`trust_resolve.rs:39`), key-signed packages
  included, as the code reads; the import run needs a cached or pinned trust root or it exits 81. Not tested here.
- Site 18 is the real residual: an unfrozen index in the import PM would leave a network path the overlay cannot see.
