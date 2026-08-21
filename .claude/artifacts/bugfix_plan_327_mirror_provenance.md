# Bugfix Plan — ocx#327 mirror fetch provenance, HTML manifest gate, redirect visibility, digest misclassification

## Status

- **State**: executing
- **Branch**: `fix/327-mirror-fetch-provenance` (fork: `ocx/integration` in `external/rust-oci-client`)
- **Issue**: https://github.com/ocx-sh/ocx/issues/327
- **Progress**: WP0–WP6 done (fork 1f51f32, c8a97d3; ocx cc486d99, e4918a1c, c9260164, 2a578169). WP7 (docs) with the docs agent.
- **Last update**: 2026-08-21

## Context

[ocx#327](https://github.com/ocx-sh/ocx/issues/327) "digest missmatch": a placeholder `[mirrors] "ghcr.io" = "https://company.jfrog.io/ghcr-remote"` (the literal docs example) rerouted GHCR manifest GETs to a dead JFrog tenant → 302 → HTML landing page → digest verification failed with a constant "got" digest across packages. RCA posted ([comment](https://github.com/ocx-sh/ocx/issues/327#issuecomment-5363408568)). One branch/PR fixes the four defects that made this undiagnosable and closes the ticket:

- **D1** — errors never name the physical reference fetched nor the mirror remap applied.
- **D2** — a 200 HTML body flows into digest verification; no Content-Type read exists on any manifest pull path.
- **D3** — manifest error paths use the pre-request URL; the post-redirect URL (`res.url()`) is never surfaced (blob paths already use it).
- **D4** — fork `DigestError` falls into `registry_error`'s catch-all → `ClientError::Registry` → exit 69 (retryable); a digest mismatch is data-integrity → exit 65.

Fork changes land as commits in the `external/rust-oci-client` submodule (ocx-sh/rust-oci-client, branch `ocx/integration`, currently detached HEAD); the ocx branch carries the pointer bump. **Commit locally only — no pushes without owner approval.**

## Design decisions (settled)

1. **D2 allowlist**: content-type essence (lowercased, before `;`) `application/json` or `*+json` accepted; **absent header accepted** (registries omit it; digest check still covers bytes); everything else refused. Dedicated free function `validate_manifest_content_type(headers, url)` — NOT a `validate_registry_response` change (that fn is shared with blob paths where content-type is arbitrary). Runs on manifest **GET** paths only, after status validation, before `validate_digest`. HEAD branch ungated (no body; digest-header-less HEAD already falls through to the gated GET).
2. **D3**: post-hoc `res.url()` capture (matches blob-path convention at fork client.rs:1424/2490) — no custom redirect policy (client-wide; blobs must keep following CDN redirects). Final URL replaces pre-request URL in manifest-path error construction; one `warn!` on cross-origin redirect for manifest GETs via existing `is_same_registry_origin` (fork client.rs:2245).
3. **D1**: new `ClientError::Mirrored { origin, mirror, physical, #[source] source: Box<ClientError> }`; annotation helper reverse-looks the physical host up in the `MirrorMap` (new `upstream_for_host`), so it needs only the `native::Reference`. Classification delegates to `source.classify()`. **Allowlist wrap** — not-found sentinels (`ManifestNotFound`, `BlobNotFound`, `RepositoryNotFound`, `ReferrersUnsupported`) pass through unwrapped (callers `match` on them → `Ok(None)`).
4. **D4**: one guarded arm before the catch-all: `DigestError::VerificationError{expected,actual}` → `ClientError::DigestMismatch` (already DataError/65). Other `DigestError` variants (`UnsupportedAlgorithm` etc.) stay `Registry`/69 — bad answer, not corrupted content. `UnexpectedContentType` → new `ClientError::NotAManifest(#[source] Box<dyn Error…>)` → DataError/65 (no `{0}` in Display — ERR-06; `{err:#}` chain prints the fork detail).
5. **Docs example host**: replace `company.jfrog.io` with `artifactory.example.com` (RFC-2606) in docs + rustdoc; unit-test literals stay.

## Work packages (in order; red-first where a defect is provable)

### WP0 — branches
- `git -C external/rust-oci-client checkout ocx/integration` (currently detached)
- Outer repo (on `main`): `git checkout -b fix/327-mirror-fetch-provenance`
- Copy this plan to `.claude/artifacts/bugfix_plan_327_mirror_provenance.md` with a `## Status` block per Plan Status Protocol.

### WP1 — fork: HTML cannot be a manifest (D2) — red first
Files: `external/rust-oci-client/src/errors.rs`, `src/client.rs`, new `tests/manifest_response.rs`.
- New test file modeled on `tests/referrers_bounds.rs` (axum Router, `TcpListener` port 0, Drop-abort guard, sentence-style names). Routes: `/v2/html/...` 200 text/html portal body; `/v2/untyped/...` 200 no content-type + valid manifest; `/v2/plainjson/...` 200 `application/json` + valid manifest; `/v2/probe/...` HEAD 200 HTML no digest header, GET 200 HTML.
- Tests: `an_html_manifest_response_is_refused_before_the_digest_check` (fails today with DigestError — regression proof), `a_manifest_digest_probe_refuses_html_on_the_get_fallback`, `a_manifest_response_with_no_content_type_is_admitted`, `a_manifest_response_typed_application_json_is_admitted`.
- `errors.rs`: `UnexpectedContentType { content_type: String, url: String }`, `#[error("unexpected content type '{content_type}' for a manifest response from {url}")]` — url = final post-redirect URL.
- `client.rs`: `fn validate_manifest_content_type(headers: &HeaderMap, url: &str) -> Result<()>` (absent header → Ok; essence == `application/json` or `.ends_with("+json")` → Ok; else the new error). Call in `_pull_manifest_raw` (~1241, after `validate_registry_response`, before `validate_digest` ~1244) and in `fetch_manifest_digest`'s GET fallback (~1044/1046).
- Commit: `fix(client): stop an HTML response passing for a manifest`.

### WP2 — fork: manifest errors name the post-redirect URL (D3)
File: `external/rust-oci-client/src/client.rs`.
- In `_pull_manifest_raw` (1222) and `fetch_manifest_digest` GET branch: after `send().await?`, `let final_url = res.url().to_string();`; if `!self.is_same_registry_origin(image, &final_url)` → `warn!(request = %url, redirected_to = %final_url, "manifest request left the registry origin")`. Pass `&final_url` to `validate_registry_response` + `validate_manifest_content_type`. (Note: origin compare includes scheme, so http→https upgrade warns — intended, say so in the doc comment.)
- Test (same new file): `a_redirected_manifest_error_names_the_final_url` — server A 307 → server B `/v2/html/...`; assert `UnexpectedContentType.url` contains B's authority.
- Commit: `fix(client): name the post-redirect URL a manifest error came from`.

### WP3 — ocx: submodule bump + non-manifest response = data error
Files: submodule pointer, `crates/ocx_lib/src/oci/client/error.rs`, `crates/ocx_lib/src/oci/client/native_transport.rs`.
- `ClientError::NotAManifest(#[source] Box<dyn std::error::Error + Send + Sync>)`, `#[error("registry did not answer with a manifest")]`; classify → `DataError`.
- `native_transport.rs` `registry_error` (line 96): arm `UnexpectedContentType { .. } => ClientError::NotAManifest(Box::new(e))` before the catch-all (line 121).
- Unit test `an_html_manifest_response_classifies_as_a_data_error` (variant + `.classify() == DataError`).
- Commit carries the pointer bump: `fix(oci): refuse a non-manifest registry response with the response named`.

### WP4 — ocx: wire digest mismatch = data error (D4) — red first
File: `crates/ocx_lib/src/oci/client/native_transport.rs`.
- Red tests: `a_wire_digest_mismatch_maps_to_digest_mismatch` (→ `ClientError::DigestMismatch`, classify DataError/65 — fails today at 69) and `an_unusable_digest_header_stays_a_registry_failure` (`UnsupportedAlgorithm("md5")` → `Registry`/69, pins the split).
- `registry_error`: `DigestError(WireDigestError::VerificationError { expected, actual }) => ClientError::DigestMismatch { expected: expected.clone(), actual: actual.clone() }` (`use oci_client::errors::DigestError as WireDigestError`). Extend doc comment: 65 = bytes wrong, rerun cannot fix; 69 would tell a retry wrapper the opposite.
- Commit: `fix(oci): classify a wire digest mismatch as a data error`.

### WP5 — ocx: failed fetch names the mirror it was routed through (D1)
Files: `crates/ocx_lib/src/oci/client/mirror_map.rs`, `client/error.rs`, `crates/ocx_lib/src/oci/client.rs`.
- `MirrorMap::upstream_for_host(&self, host: &str) -> Option<&str>` — reverse of `rewrite_repository` (mirror_map.rs:67); ties resolve via `.min()` (deterministic).
- `ClientError::Mirrored { origin, mirror, physical, #[source] source: Box<ClientError> }`, `#[error("fetching '{physical}' via mirror '{mirror}' configured for '{origin}'")]`; classify: `Self::Mirrored { source, .. } => return source.classify()`.
- `Client::via_mirror(&self, image: &native::Reference, error: ClientError) -> ClientError` next to `transport_reference` (client.rs:244): reverse-lookup miss → return untouched; wrap allowlist: `Registry`, `RegistryTransient`, `Authentication`, `NotAManifest`, `DigestMismatch`, `ShortBlobRead`, `InvalidManifest`, `InvalidImageIndex`, `Serialization`; everything else passes through.
- Wrap sites (one-line `.map_err(|e| self.via_mirror(&image, e))`): `fetch_manifest_raw` (~2039, transport call + parse), `fetch_manifest_raw_bytes_addressed` (~1934; make `fetch_manifest_raw_bytes` delegate so the wrap has one home), `fetch_manifest_digest` (~395), `probe_manifest_digest_addressed` (~1877 Err arm), `pull_manifest` (~662, the locally-built DigestMismatch), `pull_blob` (~688), `pull_layer` (~750, wrapping `pull_layer_with_caps` result). Canonical-addressed reads are never annotated (reverse lookup misses) — correct, tested.
- Tests: `upstream_for_host_names_the_origin_a_mirror_stands_in_for` + none-case (mirror_map.rs); `a_mirrored_failure_delegates_its_exit_code_to_the_wrapped_one` (error.rs: DigestMismatch→65, RegistryTransient→75); client.rs mirror test module — add `manifest_fails_hard: bool` to the existing `RecordingTransport` stub (default false), then `a_mirrored_fetch_failure_names_the_physical_reference_and_its_upstream`, `a_mirrored_missing_tag_is_still_a_not_found` (sentinel pass-through — the silent-regression case), `a_canonical_read_failure_is_not_annotated_as_mirrored`.
- Commit: `fix(oci): name the mirror a failed fetch was routed through`.

### WP6 — acceptance: mirror that answers every manifest with HTML
Files: new `test/tests/fake_registry.py`, fixture in `test/conftest.py`, test in `test/tests/test_oci_registry_mirror.py` (has local `write_home_config`:55 / `run_with_env`:103 helpers).
- `HtmlMirror` (stdlib `http.server` + threading + ephemeral port, shape copied from `test/tests/fake_forge.py`): `GET /v2/` → `200 {}` (anonymous auth probe); manifests GET|HEAD → 200 `text/html` portal, no `Docker-Content-Digest`; `tags/list` → valid JSON `{"tags":["1.0.0"]}`; else 404 OCI envelope.
- Fixture `html_mirror` in conftest next to `fake_forge`.
- `test_mirror_serving_html_fails_as_a_data_error_naming_the_mirror`: home config `[mirrors] "<registry>" = "http://<html_mirror>"`, install `<registry>/<repo>:1.0.0`, assert exit 65, stderr contains `via mirror`, the mirror authority, the upstream registry, and `text/html`. No push to upstream needed.
- Commit: `test(oci): cover a mirror that answers every manifest with HTML`.

### WP7 — docs
- `website/src/docs/reference/configuration.md` (#keys-mirrors, ~246–272): host substitution ×3 + short paragraph: mirror-routed failures name the physical ref + upstream; non-manifest responses refused before digest verification, exit 65.
- `website/src/docs/reference/command-line.md` (exit-code 65 row): extend with non-manifest-response case.
- `website/src/docs/user-guide.md` (~759–812), `reference/environment.md` (~351), `in-depth/indices.md` (~436): `company.jfrog.io` → `artifactory.example.com`.
- `website/src/docs/in-depth/storage.md` (digest-verification box ~154): one sentence on the content-type gate.
- Rustdoc: `crates/ocx_lib/src/config/mirror.rs:180`, `crates/ocx_lib/src/env.rs:65` — same substitution.
- Commit: `docs(website): replace the jfrog placeholder mirror host and document the manifest gate`.

## Verification

```sh
# fork (standalone, excluded from workspace)
cd external/rust-oci-client && cargo test && cargo clippy --all-targets -- -D warnings
# ocx
cargo test -p ocx_lib oci::client
cargo clippy --workspace --all-targets -- -D warnings
task verify --force
# acceptance (needs test-registry container; rebuild test/bin/ocx with --features ocx/__testing first if stale)
cd test && uv run pytest tests/test_oci_registry_mirror.py -v
```

Red-first evidence: WP1 and WP4 red tests run and fail BEFORE their fixes; cite both runs in commit bodies (TEST-12).

## Risks

1. Registry serving valid manifests with non-JSON content-type (e.g. `application/octet-stream` from an S3-fronted cache) now refused — mitigated: gate manifest-GET-only, absent header tolerated, error names the exact type; fix is one `||`. Matches containerd behavior.
2. Cross-origin warn fires on http→https upgrade redirects — intended, `warn!` only.
3. `Mirrored` burying a control-flow variant — contained by allowlist + `a_mirrored_missing_tag_is_still_a_not_found`.
4. **Visible exit-code change**: blob/wire digest mismatches move 69 → 65 (WP4). Intended taxonomy; call out in PR body.

## PR

Branch `fix/327-mirror-fetch-provenance` (never PR from the fixed worktree branch; squash-ready). 7 commits: 2 in the submodule on `ocx/integration`, 5 in ocx (WP3 carries the pointer bump). PR body: four defects, RCA one-liner, exit-code change (risk 4), `Closes #327`. Commit locally; ask owner before any push.
