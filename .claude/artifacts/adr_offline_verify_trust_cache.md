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

## Amendment (2026-08-19) — six-rung ladder, `--trusted-root`, `[trust.sigstore]`

Supersedes the "`OCX_SIGSTORE_TUF_ROOT` override" and "Trust-material precedence
(verify)" sections above. Everything else in this ADR stands.

**1. `--trust-root` deleted.** Rung 2 loaded a Fulcio PEM into a trust root with an
empty CT-key map, and `verify/pipeline.rs` unconditionally refuses that — so the
flag exited 78 on every invocation that reached it. It could not succeed. Deleted
with `TrustRoot::load_from_pem`, no shim and no alias; `OCX_SIGSTORE_TRUST_ROOT`
went with it. Neither ever shipped (PR #203 was still open), so this is not a CLI
break.

**2. `--tuf-root` renamed `--trusted-root`; `OCX_SIGSTORE_TUF_ROOT` renamed
`OCX_SIGSTORE_TRUSTED_ROOT`.** The old name claimed a TUF fetch the flag never
performed — it reads a static trusted-root JSON. TUF runs only on the
public-good path (`TrustRoot::load_embedded`).

**3. The ladder grows from four rungs to six**, first hit wins:

1. `--trusted-root` flag
2. `OCX_SIGSTORE_TRUSTED_ROOT`
3. `[trust.sigstore]` in the operator `config.toml` — `trusted_root` (path,
   anchored to the declaring file's directory) XOR `trusted_root_json` (inlined)
4. `$OCX_HOME/sigstore/trusted-root.json` — convention path
5. trust-root cache (`state/trust_root/<rekor-slug>.json`, 24h TTL)
6. public-good root over TUF (`load_embedded`; unreachable offline)

Rungs 1–3 are operator-named: a named file that does not exist is an error, never
a fall-through. Rung 4 is a convention: absent falls through, present-and-unreadable
fails. `enforce_offline_rekor_key` still gates every rung, and offline still never
reaches rung 6.

**4. `[trust.sigstore]` is a new sub-table on `[trust]`**, read from the operator
`config.toml` tiers only — the project `ocx.toml` deserializes it and it is never
consulted, because a repository that could name its own Fulcio CA would verify its
own signatures. It locks per-table at system scope (the `[registry]` precedent, not
`[[trust.policy]]`'s pooling — two Fulcio CAs is an ambiguity, not a merge), and the
two trust-root spellings are coupled so a tier switching from path to inline drops
the other rather than leaving a stale pair.

**5. Fleet distribution.** `ocx config push` inlines a path-form `trusted_root` as
`trusted_root_json` at publish time, so a fleet payload names no path on anyone's
disk. Two guards follow: a payload carrying `trusted_root_json` requires a
digest-pinned `[managed] source` (otherwise the trust root arrives over the channel
it exists to verify), and a path-form `trusted_root` arriving from the managed tier
is ignored with a warning.

Docs: `website/src/docs/in-depth/self-hosted-sigstore.md` (new),
`in-depth/signing.md#trust-root`, `reference/configuration.md#keys-trust-sigstore`,
`reference/environment.md#ocx-sigstore-trusted-root`.
