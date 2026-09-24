# ocx_oci

OCI/distribution-spec-generic registry work: references, digests, manifests,
transport, referrers, layer-placement annotations, platform matching, host
capability detection, registry auth, and the SSRF guard.

**Tier:** ecosystem

**May depend on:** `ocx_util`, `ocx_console`, `ocx_exit`

**Named dependency exceptions:** none

The tier is closed around `OciTransport` (sealed — see `client` module docs)
and the `Client` that drives it: `package_ref`, `oci_identifier`, `digest`,
`platform`, `host_capabilities`, `media_type`, `layer_layout`/`layer_ref`,
`repository`, `pinned_package_ref`, `annotations`, `tag`, `referrer`,
`manifest`/`manifest_builder`, `endpoint`, `resolve_target`, `copy` and `ssrf`
are all either inputs to a transport call or types a transport call returns.
`package_ref` and `oci_identifier` are two distinct, unconvertible types — a
`PackageRef` is a logical package identity, never dialled; an `OciIdentifier`
is the physical, dial-able reference `Client` accepts, minted only through
`ocx_index::Index::route*` or `oci_identifier`'s own parse entry points
(ocx#504).
`ocx_console` is linked so the pull/push paths report byte progress through
its bars and the login flow prompts for a secret through it; `ocx_exit` names
the process-outcome vocabulary the transport's retry ladder and `ssrf`'s
refusal classify onto; `ocx_util` is the bottom tier — `archive`,
`compression`, `fs`, `env`, `tls`, and the `FileError`/`SerializationError`
pair this crate's I/O paths raise.

Deliberately **not** here: index management, signing, attestation, and
verification (`oci/index`, `oci/sign`, `oci/verify`, `oci/simplesigning`) all
stay in `ocx_lib::oci` — this crate is the generic distribution-spec client
those subsystems are built on top of, not the subsystems themselves. `trust`
(signer-identity policy) is a separate crate again.

The `__testing` feature gates `pub mod testing`, the transport doubles
(`RecordingTransport`, `SbomTransport`) the signing pipelines drive their unit
tests through. Enabled from `[dev-dependencies]` only — resolver v3 keeps a
dev-dependency feature out of the normal build, so a release binary physically
lacks the module.
