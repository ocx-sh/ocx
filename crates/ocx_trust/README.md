# ocx_trust

Signer-identity policy: `[[trust.policy]]` model, tiered resolution (`resolve_tiered`), `CompiledPolicy`/`IdentityRule`, `SigstoreTrust` config type.

**Tier:** ecosystem

**May depend on:** `ocx_oci`, `ocx_util`

**Named dependency exceptions:** none

`ocx_lib::trust::X` is `ocx_trust::X` — the module root that was `trust.rs` is
`src/lib.rs`, so the tier's own name appears in no path. `trust/key_ref.rs` is
`key_ref`, still a child of the root rather than a sibling: the `--key` grammar
a `signers` entry speaks belongs to the policy that reads it.

`ocx_oci` is in this crate's allowed set and deliberately unlisted in
`Cargo.toml`. Nothing here names a registry, a digest or a reference — a policy
matches on identity and scope strings — and a manifest row for an edge the code
does not take would claim a coupling that is not there.

The one thing this crate holds that is not a plain value: a compiled `key` or
`key_file` policy *is* a `sigstore::crypto::CosignVerificationKey`, so that type
crosses the public surface and sigstore is a structural dependency rather than
an incidental one. Only the workspace's default feature set — this crate parses
and holds a verification key; it neither signs nor talks to Fulcio or Rekor.

No `ClassifyExitCode` impl lives here. Every error this crate raises is
classified from `ocx_cli`, and each one is consumed by value into a caller's own
kind before it can reach the chain walker — see `CLAUDE.md`.
