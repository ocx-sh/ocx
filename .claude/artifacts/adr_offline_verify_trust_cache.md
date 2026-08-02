# ADR — Offline / air-gapped verify + trust-root cache (#196)

Status: Accepted (2026-07-09). Depends on #194. Gates #99.

## Context

`ocx package verify` is online-only today: `--offline`/`OCX_OFFLINE` fails at
`Context::online_context()` → exit 81 before any work. Two #194 weaknesses block
offline verify:

1. The Rekor public key is **TOFU-fetched** from `--rekor-url/api/v1/log/publicKey`
   at verify time — a network dependency AND a trust-on-first-use hole.
2. The embedded TUF trust root is stubbed (`load_embedded` → `TrustRootUnavailable`),
   so trust material only ever comes from a supplied `--trust-root` PEM.

Product principle #2 is offline-first; auto-verify on install (#99) collides
verify's online-only stance with install's offline-first stance. This ADR
resolves that contradiction.

## Decision

### What "offline" means for verify (the contradiction, resolved)

For `ocx package verify`, `--offline`/`OCX_OFFLINE` governs the **Sigstore
trust-services network** (the Rekor-public-key fetch and any TUF fetch/refresh) —
NOT the artifact registry. Verifying an artifact inherently means reading it, and
its signature referrer, from the registry where it lives; in an air-gapped
deployment that registry is a local mirror the operator runs. So offline verify:

- still fetches the referrer + bundle from the configured registry (a live client
  is available in every mode — see "Registry client in all modes");
- MUST NOT contact Sigstore trust services — the Fulcio CA **and** the Rekor
  public key must come from a supplied override or the fresh trust-root cache;
- FAILS with an actionable error when trust material is absent/stale — never
  silently skips verification.

`sign` stays online-only, unchanged (it needs Fulcio + Rekor round-trips).

The bundle-is-local-too concern (true no-registry air-gap) is #99's install-time
job: install already downloads the artifact, and the reusable offline-trust
decision below lets install-time auto-verify make the same fail-vs-verify call.

### Trust-root cache (`$OCX_HOME/state/trust_root/<rekor-authority-slug>.json`)

Mirrors the referrers capability cache (`oci/referrer/capability.rs`): atomic
tempfile+rename write, TTL-gated fail-open read, host-scoped key. Caches the
trust MATERIAL needed for offline verify:

- Fulcio CA certificate(s) — DER (the certs the online verify chained against);
- the Rekor public key PEM (whether pinned from a trust root or TOFU-fetched).

Populated on a **successful online verify**. Read on a later verify when no
explicit override is supplied. TTL = 24h (`TTL_SECS`); honoring real TUF metadata
expiry is deferred with the real TUF client. Keyed by the Rekor URL authority so
public and private Sigstore instances never collide. The cache is per-`OCX_HOME`.

### `OCX_SIGSTORE_TUF_ROOT` override (+ `--tuf-root` flag)

Points verify at a Sigstore `TrustedRoot` JSON (a file, or a directory containing
`trusted_root.json`). Parsed leniently (serde_json walk) to extract Fulcio CA
certs (`certificateAuthorities[].certChain.certificates[].rawBytes`) and Rekor
public keys (`tlogs[].publicKey.rawBytes`, DER SPKI → PEM). No TUF **network**
fetch/refresh — that stays deferred; this is the air-gapped local-mirror seam.

### Rekor key pinning (security fix for #194 weakness 1)

`verify_rekor_set` now prefers the Rekor key from the trust root (supplied via TUF
root, or cached) when present — no network, and it closes the TOFU hole. It falls
back to the online `--rekor-url` fetch ONLY when no trust-root Rekor key exists
AND the run is online. Offline + no pinned Rekor key → actionable failure.

### Registry client in all modes

`Context` now builds the registry client unconditionally (cheap; no network on
build) and exposes it to verify via `verify_context()`. `remote_client()` /
`online_context()` keep their offline gating (sign etc. unchanged); only verify
reads the always-present client, because verify's offline semantics scope to
trust services, not the registry.

### Trust-material precedence (verify)

1. `--tuf-root` / `OCX_SIGSTORE_TUF_ROOT` (Fulcio + pinned Rekor key)
2. `--trust-root` / `OCX_SIGSTORE_TRUST_ROOT` PEM (Fulcio only; Rekor via cache/TOFU)
3. fresh trust-root cache (Fulcio + Rekor key)
4. embedded root (stubbed → exit 78)

Offline additionally requires the resolved material to carry a Rekor key (only
1 and 3 do); offline + only a bare PEM, or offline + empty cache → exit 78 with a
remedy naming `--tuf-root` / "run an online verify first".

## Reusable seam for #99

The offline decision is a library primitive: `TrustRootCache::from_cache(...)` →
`filter(is_fresh)` → `into_trust_root()` (has a Rekor key ⟺ offline-verifiable).
`#99`'s install-time auto-verify composes the same primitive: fresh cached
material ⇒ verify offline; none ⇒ the documented fail-vs-skip policy.

## Consequences

- Offline verify is genuinely no-Sigstore-network (proved by the acceptance suite
  returning 503 from fake Rekor and still passing offline).
- The TOFU Rekor-key hole is closed whenever trust material provides the key.
- Real TUF fetch/refresh + bundle-local-CAS air-gap remain honestly deferred.
