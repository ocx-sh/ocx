# CLAUDE.md — ocx_oci

Agent-specific guidance that has no other home. Read `README.md` first for
what this crate owns; this file states only what a change here must not break.

## Identifier: the one OCX domain fact this crate carries

`adr_crate_split_workspace.md`'s AI-config layout ruling names this crate as
one of four needing a `CLAUDE.md`, for exactly one reason: "the OCX-specific
knowledge that remains here is the default-registry constant and the
`ocx.sh/ocx/cli` self-image in `identifier`; add no *other* OCX domain type."
An earlier draft's blanket "no OCX domain type here" was contradicted by
`identifier` itself — `DEFAULT_REGISTRY`/`OCX_SH_REGISTRY` and
`ocx_cli_identifier()` (the `ocx.sh/ocx/cli` self-image, overridable only
through the `__OCX_SELF_IMAGE` test seam, loopback-only, defense-in-depth
asserted) are a deliberate, narrow exception to this crate's otherwise
distribution-spec-generic surface. Do not widen it: no other module here may
hardcode an OCX-specific registry, repository, or package name. If a second
one shows up, that is a design question for the next ADR, not a quiet second
constant beside `identifier`'s.

## `OciTransport` is sealed — do not "fix" the compile error

`client::OciTransport` carries a private `crate::sealed::Sealed` supertrait
bound, so no downstream crate can implement it — every registry read and
write crosses this trait, and several of its default methods (the SSRF guard,
the retry ladder, mirror routing, the referrers-fallback write) are load
bearing security behaviour a foreign impl could silently skip. `lib.rs` holds
a `compile_fail,E0277` doctest with a *complete* implementation of every
required method; the seal is the only thing that refuses it. If that doctest
stops failing to compile, the seal broke — do not delete or loosen it.
Consumers needing a double take one from `testing`, not a real impl.

## The SSRF guard ratchet: this crate is the pattern, not just a member

`crates/ocx_test_support/tests/fixtures/ssrf_unguarded_baseline.txt` enforces
that every `reqwest::Client`/`ClientBuilder` construction under
`crates/ocx_*/src` either seeds a `GuardedResolver`, is preceded by
`guard_destination` in the same function, or is a named, shrink-only entry in
that file. Adding a new `reqwest::Client` construction anywhere in this crate
(`client/`, `ssrf.rs`, `endpoint.rs`, `auth/`) means resolving one of those
three ways — never adding an unguarded fourth. The allowlist only ever
shrinks: guarding a listed site deletes its line, it never gains one.

## `testing` is the only test-double surface

`#[cfg(any(test, feature = "__testing"))] pub mod testing` is where every
transport double (`RecordingTransport`, `SbomTransport`, and friends) lives.
It is compiled out of a release build by construction — the feature is
enabled from `[dev-dependencies]` only, and resolver v3 keeps a
dev-dependency feature out of the normal build graph. Do not add a second
`#[cfg(test)]` surface elsewhere in the crate for a production type to peek
through; extend `testing` instead.
