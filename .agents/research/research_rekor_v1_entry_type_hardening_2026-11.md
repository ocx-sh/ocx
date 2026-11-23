# Research: Rekor v1 entry-type hardening, 2026-11

## Metadata

**Date:** 2026-11-23
**Domain:** security
**Triggered by:** discussion `.agents/discussions/rekor-v2-sigstore-rust.md` — does the 2026-11-16 hardening of the public-good Rekor v1 instance put a deadline on OCX's `dsse:0.0.1` uploads
**Expires:** 2027-05-23

## Direct Answer

No. The post keeps `hashedrekord`, `dsse`, `intoto` (0.0.1 and 0.0.2), `rekord`
and `helm` enabled on Rekor v1. The two kinds OCX writes (`hashedrekord`,
`dsse:0.0.1`) are unaffected. The quotes below came through a summarizing fetch
tool and are not verified verbatim.

## Findings

1. Post: "Rekor Public-Good Instance Hardening", Sigstore Public Infrastructure
   Maintainers, 2026-11-16 —
   https://blog.sigstore.dev/rekor-public-instance-hardening/
2. It disables upload of "a number of Rekor entry types" that Sigstore client
   tooling does not use (single-digit uploads in 90 days). It names what stays
   enabled, not what is disabled. By exclusion the disabled kinds are `alpine`,
   `rpm`, `rfc3161`, `jar`, `tuf`, `cose` — an inference.
3. "Effective immediately"; no staged rollout or notice period described.
4. Silent on whether existing entries of a disabled type stay verifiable. The
   code stays in the Rekor codebase for private deployments.
5. Client guidance: generate `hashedrekord`; `dsse` is optional on v1 and
   "large attestations are unsupported"; `intoto` is an older revision of
   `dsse`; new integrations should use Rekor v2.
6. Rekor v2, per
   [CLIENTS.md](https://raw.githubusercontent.com/sigstore/rekor-tiles/main/CLIENTS.md):
   `serve` mode supports only `hashedrekord` 0.0.2; "Rekor v2 will not support a
   DSSE entry type" — clients submit the DSSE PAE and signature as a
   `hashedrekord` request. Clients "must fetch an RFC 3161 signed timestamp from
   a trusted timestamp authority and include the signed timestamp in the
   bundle"; `integrated_time` is always `0`.

## negative

- The post does not name the disabled kinds or say anything about existing entries.
- The Rekor v2 GA post (2025-10-10, https://blog.sigstore.dev/rekor-v2-ga/)
  calls `hashedrekord` and `dsse` "the two supported types in Rekor v2", which
  contradicts CLIENTS.md. Not resolved.
- Not checked: sigstore/public-good-instance, sigstore/root-signing, the
  architecture-docs client spec.

## leads

- sigstore/rekor-tiles proto and README settle the v2 `dsse` conflict.
- Raw post markdown (blog.sigstore.dev/index.xml) for verbatim quotes.
