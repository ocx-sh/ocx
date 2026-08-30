# WP6 — measured cosign v3.1.1 behaviour (loop E, 2026-08-29)

Every row below was **run**, not inferred. Stack: `test/docker-compose.yml`
(zot `localhost:5000` = Referrers API present; `registry:2` `localhost:5001` =
absent). cosign image `ghcr.io/sigstore/cosign/cosign:v3.1.1`.
Reproduce with the probe scripts under `~/.cache/ocx-e-tmp/probe*.py`.

## P1 — `cosign verify` has no `--new-bundle-format` and no `--registry-referrers-mode`

Full v3.1.1 flag lists (`cosign {sign,verify} --help`):

* `sign` has `--registry-referrers-mode` (`legacy` | `oci-1-1`); `oci-1-1`
  **requires `COSIGN_EXPERIMENTAL=1`** and errors without it.
* Neither command has `--new-bundle-format`. The design spec's plan to drive
  the four "cosign signs → simplesigning" cells with `--new-bundle-format=false`
  **cannot be executed on the pinned version** — the flag is gone.

## P2 — `cosign sign` always writes a bundle, never a `.sig` sidecar

On zot, both with `--registry-referrers-mode=legacy` and with the mode unset,
`cosign sign` wrote a **Referrers API** entry
(`artifactType: application/vnd.dev.sigstore.bundle.v0.3+json`,
`dev.sigstore.bundle.content: dsse-envelope`) and **no** `sha256-<hex>.sig` tag
(404). `--registry-referrers-mode=legacy` does not select the simplesigning
writer; that writer no longer exists on `cosign sign`.

**Consequence:** a cosign-produced simplesigning sidecar comes only from the
three deprecated commands in sequence — `cosign generate` → `cosign sign-blob`
→ `cosign attach signature`. `golden/generate.py::_capture_simplesigning`
already establishes this route; the matrix reuses it live.

## P3 — `cosign verify` reads the Referrers API, the OCI fallback tag, and the `.sig` sidecar

| Shape | Read by `cosign verify <ref>` |
|---|---|
| Referrers API bundle | yes (rc=0) |
| OCI fallback tag `sha256-<hex>` index (registry:2) | yes (rc=0) |
| `sha256-<hex>.sig` simplesigning sidecar | yes, but needs `--insecure-ignore-tlog=true` when the sidecar carries no `dev.sigstore.cosign/bundle` annotation |

## P4 — cosign ACCEPTS an OCX-created sidecar, empty config and all

OCX's *created* sidecar carries `config.mediaType =
application/vnd.oci.empty.v1+json` (2 bytes); cosign's own carries a 233-byte
`application/vnd.oci.image.config.v1+json`. `cosign verify` returned **rc=0**
against the OCX-created sidecar in **both** key models, on **both** registries.
cosign reads a sidecar's layers, never its config. **No finding** — the create
path is not a cosign-compat break.

Stronger: OCX writes `dev.sigstore.cosign/bundle` on the layer, which cosign's
own `attach signature` cannot (its `--rekor-response` is inert in v3.1.1 —
`REKOR_RESPONSE_GAP` in `golden/generate.py`). So `cosign verify` clears an
**OCX** sidecar with full transparency-log verification
("Existence of the claims in the transparency log was verified offline") and a
**cosign** sidecar only with `--insecure-ignore-tlog`. The OCX artifact is the
stronger one.

## P5 — cosign's own fallback-tag write loses annotations, keeps artifactType

Measured entry in cosign's `sha256-<hex>` fallback index on registry:2:

```json
{"mediaType":"application/vnd.oci.image.manifest.v1+json","size":878,
 "digest":"sha256:2dcb…","artifactType":"application/vnd.dev.sigstore.bundle.v0.3+json"}
```

`artifactType` **survives**; the three annotations
(`dev.sigstore.bundle.content`, `dev.sigstore.bundle.predicateType`,
`org.opencontainers.image.created`) are **dropped**. The design spec's reading
of [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641) —
"loses `artifactType` and annotations" — is half right on this version. The two
"cosign signs × bundle × Referrers-API-absent" cells are therefore **not**
impossible; they are producible, and the annotation loss is the thing to assert.

## P6 — every remaining axis works

* `cosign attach signature` succeeds against registry:2 (no Referrers API).
* `ocx package sign --signature-format simplesigning` succeeds on registry:2 and
  cosign verifies the result.
* `ocx package verify` accepts a cosign-signed bundle and reports
  `signature_format: bundle`, `discovery_method: referrers_api`.

**All 16 cells are producible.** No cell is impossible on this cosign version.

## Fixtures — the key pair already exists

`test/tests/fixtures/golden/keys/cosign.key` + `.pub`, minted by
`cosign generate-key-pair` inside the pinned container, password `ocxtest`
(`OCX_KEY_PASSWORD`). Nothing to generate; regenerating would invalidate
`key_bundle.json` and `simplesigning_key_manifest.json`, which pin this key.
