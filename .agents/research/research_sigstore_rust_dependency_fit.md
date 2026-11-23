# Research: sigstore-rust dependency and build fit

## Metadata

**Date:** 2026-11-23
**Domain:** packaging
**Triggered by:** discussion `.agents/discussions/rekor-v2-sigstore-rust.md`
**Expires:** 2027-02-23 (sigstore-rust at 7fbe058a; three breaking 0.x releases in the prior 30 days)

Method: manifest and lockfile comparison only. No cargo, bazel, build or network call was run. Paths into B are relative to the clone root, prefixed `sigstore-rust:`. B = sigstore/sigstore-rust at 7fbe058a ("chore(deps): update rust crate jiff to v1 (#373)"). X = this repo.
Closure numbers below come from a script (`closure.py`, scratchpad) walking B's `Cargo.lock` from `sigstore-sign` + `sigstore-verify` and diffing against X's `Cargo.lock`. A lock walk over-approximates: it includes optional-feature edges and the dev-deps of B's own workspace crates. Items marked (dev) or (feature) below were removed by hand from the "would enter X" list.

## Direct Answer

Facts only, no recommendation.

- Toolchain fits: B MSRV 1.86 / edition 2021 vs X pinned 1.95.0 / edition 2024.
- B has no `openidconnect`, `oauth2`, `rsa`, `tough`, `p256`, `ecdsa`, `sha2`, `ed25519-dalek`, `sigstore_protobuf_specs` (outside an unpublished test crate), `prost`, `openssl`, `native-tls` in any default feature set. The RUSTSEC-2023-0071 `rsa` exception in `deny.toml:16-28` exists because of `sigstore -> openidconnect -> rsa` (`Cargo.lock` shows `sigstore` as a dependent of `openidconnect`) and would have no consumer under B.
- B's crypto backend is `aws-lc-rs` (already in X's lock, 1.18.2; B locks 1.19.0). Its public key API is its own (`KeyPair`, `SigningKeyPair`, `CryptoProvider`, DER byte slices). `rustls_pki_types::CertificateDer` appears in `sigstore-tsa`'s public builder API; `reqwest::Client` appears in every `with_http_client` setter.
- Conflicts with X: `jiff` 1.0 (X has 0.2.38 via `gix-date`, `serde_with`: two majors, the 0.2 copy stays), `curve25519-dalek` 5.0 (X has 4.1.3), `fiat-crypto` 0.3 (X 0.2.9). All are warn-level under `multiple-versions = "warn"`.
- New crates for X (non-dev, default features): `ambient-id`, `astral-reqwest-middleware`, `cms`, `directories` (its dependency `dirs-sys` 0.5.0 is already in X), `open`, `is-wsl`, plus `jiff-core`, `jiff-static`/`jiff-tzdb*` at 1.0 line, `curve25519-dalek` 5 line.
- X's `Cargo.toml:190-211` RustCrypto pins (`p256`, `x509-cert`, `sha2_oid`, `pki-types`, `ed25519-dalek`) exist so shared types interoperate with sigstore 0.14. B shares none of those types with X's code. Which pins stay depends on what `ocx_sign` itself still uses of `p256`/`x509-cert` (it uses both directly today, see item 3).
- TUF: B ships its own TUF client (`sigstore-tuf`, not `tough`). `TufConfig::with_http_client` is the equivalent of the fork's `new_with_client`. Default cache path is not caller-controlled unless `with_cache_dir` is called.
- No `build.rs` in any B crate; no `[patch]`; no git dependencies.
- Not established: whether all twelve crates are published on crates.io at 0.17.0. The per-crate changelogs show only `sigstore-crypto`, `-rekor`, `-verify`, `-sign` released at 0.17.0 (2026-11-22); the other eight changelogs top out at 0.15.0 to 0.16.0 while every manifest says 0.17.0.

## Findings

### 1. Toolchain

| | X | B |
|---|---|---|
| Pinned compiler | `rust-toolchain.toml:2` channel `1.95.0` | `sigstore-rust:rust-toolchain.toml:2` channel `1.99.1` (dev toolchain, not the MSRV) |
| MSRV | `Cargo.toml:17` `rust-version = "1.95.0"` | `sigstore-rust:Cargo.toml:~30` `[workspace.package] rust-version = "1.86"`; CI job `MSRV (Rust 1.86)` `sigstore-rust:.github/workflows/ci.yml:45-52` runs `cargo +1.86.0 check --workspace --all-features --locked` |
| Edition | `Cargo.toml:14` `2024` | `[workspace.package] edition = "2021"` |
| Bazel | `MODULE.bazel:37-40` `toolchains.toolchain(edition = "2024", version = "1.95.0")` | n/a (no Bazel) |

B's MSRV is below X's compiler; edition 2021 crates build under a 2024 workspace (edition is per crate). Nothing found that needs a compiler newer than 1.95. B's pinned dev toolchain 1.99.1 is newer than X's 1.95.0, but it is not the MSRV, and B's README (`sigstore-rust:README.md:190`) states 1.86.

### 2. Direct dependencies of the twelve crates

Version requirements come from `sigstore-rust:Cargo.toml` `[workspace.dependencies]` (all member crates use `workspace = true`). Lock versions are B's `Cargo.lock`; X versions are X's `Cargo.lock`. "gate" = the feature that enables it.

| Dependency (req) | B crates using it | Gate | B lock | X lock | Status |
|---|---|---|---|---|---|
| `serde` 1.0 | types, merkle(dev), rekor, tuf, trust-root, oidc, fulcio, verify | always | 1.0.229 | 1.0.229 | same |
| `serde_json` 1.0 | types, rekor, tuf, trust-root, oidc, fulcio, verify, sign | always | 1.0.154 | 1.0.154 | same |
| `base64` 0.23 | types, rekor, oidc, verify | always | 0.23.1 | 0.21.7, 0.22.1, 0.23.1 | same |
| `hex` 0.4 | types, rekor, tuf, tsa, verify | always | 0.4.3 | 0.4.3 | same |
| `jiff` 1.0 | types, crypto, bundle, rekor, tuf, trust-root, tsa (`std`), oidc, verify | always | 1.0.2 | 0.2.38 | **conflict**: new major, 0.2 stays (`gix-date`, `serde_with` use it) |
| `thiserror` 2.0 | all | always | 2.0.21 | 1.0.69, 2.0.21 | same |
| `pem` 4.0 | types | always | 4.0.0 | 3.0.6, 4.0.0 | same |
| `aws-lc-rs` 1.18 | crypto | `aws-lc-rs` (default) | 1.19.0 | 1.18.2 | same major (patch bump of X's lock) |
| `aws-lc-sys` (indirect) | | | 0.47.0 | 0.46.0 | same major; has a build script (see 8) |
| `der` 0.7 (`alloc`,`derive`,`oid`) | crypto, tsa | always | 0.7.10 | 0.7.10 | same |
| `spki` 0.7 (`alloc`) | crypto | always | 0.7.3 | 0.7.3 | same |
| `const-oid` 0.9 | crypto, tsa (`db`), verify | always | 0.9.6 | 0.9.6, 0.10.2 | same |
| `x509-cert` 0.2 | crypto, tsa, verify (`sct`) | always | 0.2.5 | 0.2.5 | same |
| `curve25519-dalek` 5.0 (no default) | crypto | always | 5.0.0 | 4.1.3 | **conflict**: new major (`ed25519-dalek` 2.2.0 in X pulls 4.1.3 via sigstore) |
| `fiat-crypto` (indirect via dalek 5) | | | 0.3.0 | 0.2.9 | conflict (follows dalek) |
| `rustls-pki-types` 1.14 (`alloc`) | crypto, tsa, verify | always | 1.16.0 | 1.15.1 | same major |
| `rustls-webpki` 0.103 (`alloc`,`std`) | tsa, verify | always | 0.103.17 | 0.103.15 | same minor line |
| `cms` 0.2 (no default) | tsa | always | 0.2.3 | absent | **new** (deps: const-oid, der, spki, x509-cert only) |
| `tls_codec` 0.4 (`derive`) | verify | always | 0.4.2 | 0.4.2 | same |
| `futures-io` 0.3 | crypto, verify, sign | always | 0.3.34 | 0.3.34 | same |
| `serde_json_canonicalizer` 0.4 | verify | always | 0.4.1 | 0.3.2, 0.4.1 | same |
| `tracing` 0.1 | tsa, tuf, verify | always | 0.1.44 | 0.1.44 | same |
| `reqwest` 0.13 (no default; `json`,`form`) | tsa, rekor, tuf, fulcio, oidc | optional `client` in tsa/rekor/tuf; always in fulcio/oidc; enabled by `rustls`/`native-tls`/`rustls-no-provider` | 0.13.5 | 0.13.5 | same (X: `Cargo.toml:143`) |
| `url` 2.5 | tuf (optional `client`), oidc | `client` / always | 2.5.9 | 2.5.8 | same |
| `rand` 0.10 | tsa (optional `client`), oidc | `client` / always | 0.10.4 | 0.10.3 (plus 0.8.8, 0.9.5) | same |
| `tokio` 1.47 | oidc (`net`,`io-util`,`time`), trust-root (`fs`, optional `tuf`) | always / `tuf` | 1.54.1 | 1.53.3 | same |
| `directories` 6.0 | trust-root | `tuf` (default) | 6.0.0 | absent (X has `dirs-sys` 0.5.0, which it needs) | **new** |
| `tempfile` 3.27.0 | tuf | always | 3.27.0 | 3.27.0 | same |
| `glob` 0.3 | tuf | always | 0.3.4 | 0.3.4 | same |
| `zeroize` 1 | oidc | always | 1.9.2 | 1.9.1 | same |
| `ambient-id` 0.0.11 (no default; `reqwest-middleware`) | oidc | always; `rustls`/`native-tls` re-enabled via `sigstore-oidc` features | 0.0.11 | absent | **new**; pulls `astral-reqwest-middleware` 0.5.1 (new; deps anyhow, async-trait, http, reqwest, serde, thiserror, tower-service) and `secrecy` (X has 0.10.3) |
| `open` 5.3 | oidc | `browser` (default; sign default includes it) | 5.4.5 | absent | **new**; pulls `is-wsl` 0.4.0, `libc`, `is-docker` 0.2.0 |
| `sigstore-*` | internal path+version deps | | 0.17.0 | n/a | new (they are the library) |

Dev-only in B (not entering X's lock for a registry dependency): `insta`, `rstest`, `regex`, `futures`, and `boring` (`sigstore-rust:crates/sigstore-crypto/Cargo.toml:36-37`, gated `cfg(sigstore_boring_test)`; its lock presence makes B's own `Cargo.lock` carry `boring`, `boring-sys`, `bindgen`, `fslock`, `foreign-types`).

Per-crate gating summary (all from the crates' `[features]`):
- `sigstore-types`: no features. Deps: serde, serde_json, base64, hex, jiff, thiserror, pem. (`sigstore-rust:crates/sigstore-types/Cargo.toml:10-17`)
- `sigstore-crypto`: `default = ["aws-lc-rs"]`; `aws-lc-rs` gates `dep:aws-lc-rs` and the `aws_lc` module (`sigstore-rust:crates/sigstore-crypto/src/lib.rs:7-8`).
- `sigstore-merkle`: `default = ["aws-lc-rs"]`; deps only types, crypto, thiserror.
- `sigstore-tsa`: `default = ["rustls","aws-lc-rs"]`; `client` gates reqwest + rand; `rustls`/`rustls-no-provider`/`native-tls` select TLS.
- `sigstore-tuf`: `default = ["rustls","aws-lc-rs"]`; `client` gates reqwest + url.
- `sigstore-trust-root`: `default = ["tuf","rustls","aws-lc-rs"]`; `tuf` gates `sigstore-tuf`, tokio (`fs`), directories.
- `sigstore-bundle`: no features; deps types, jiff, thiserror (`sigstore-rust:crates/sigstore-bundle/Cargo.toml:10-13`).
- `sigstore-rekor`: `default = ["rustls","aws-lc-rs"]`; `client` gates reqwest.
- `sigstore-fulcio`: `default = ["rustls","aws-lc-rs"]`; reqwest always on.
- `sigstore-oidc`: `default = ["rustls","browser","aws-lc-rs"]`; `browser` gates `open`.
- `sigstore-verify`: `default = ["rustls","aws-lc-rs"]`; optional `tuf` forwards to trust-root (trust-root's own default already enables `tuf`, and verify depends on it with default features not disabled at `sigstore-rust:crates/sigstore-verify/Cargo.toml:14`, workspace dependency sets `default-features = false` at workspace level, so `tuf` is off unless requested).
- `sigstore-sign`: `default = ["rustls","browser","tuf","aws-lc-rs"]`.

Net count for a default `sigstore-sign` + `sigstore-verify` build, by the lock walk after removing dev/feature-only items: about 22 crates differ or are new; brand-new to X: `ambient-id`, `astral-reqwest-middleware`, `cms`, `directories`, `open`, `is-wsl`, `is-docker`, and the jiff 1.0 family (`jiff`, `jiff-core`, `jiff-static`, `jiff-tzdb`, `jiff-tzdb-platform`).

### 3. Crypto backend and RustCrypto pins

Backend: `aws-lc-rs` is the default and only built-in provider (`sigstore-rust:crates/sigstore-crypto/Cargo.toml:11-12`, `sigstore-rust:README.md:156`). `CryptoProvider` is a trait (`sigstore-rust:crates/sigstore-crypto/src/provider.rs:46`) with `install_default_provider` (`:152`); `README.md:168-186` documents disabling `aws-lc-rs` and installing another provider. B's lock has no `p256`, `p384`, `ed25519-dalek`, `rsa`, `ecdsa`, `sha2` entries. RustCrypto crates B does use: `der`, `spki`, `const-oid`, `x509-cert`, `cms`, `tls_codec` (certificate and ASN.1 parsing only, no signature algorithms). `ring` is in B's lock only through `rustls-webpki`/`quinn-proto`; X's lock has `ring` 0.17.14 as well.

Public API types: signing and verification use B's own types: `KeyPair` wraps `Box<dyn SigningKeyPair>` (`sigstore-rust:crates/sigstore-crypto/src/signing.rs:211`), `public_key_der()` returns a DER wrapper (`:294`), `from_pkcs8_der(&[u8])` (`:237`), `VerificationKey`, `KeyAlgorithm`, `SigningScheme`. `x509_cert` is used inside `sigstore-crypto/src/x509.rs:9-10` and not in a `pub fn` signature (grep for `pub fn` mentioning `x509_cert` returned nothing). `rustls_pki_types::CertificateDer` is public in `sigstore-tsa` (`sigstore-rust:crates/sigstore-tsa/src/verify.rs:73,79,85`). `reqwest::Client` is public in all `with_http_client` setters (item 4). Bundle types are B's own serde types (`sigstore-rust:crates/sigstore-types/src/bundle.rs:101`).

X pins (`Cargo.toml:190-211`), one by one:
- `sigstore = 0.14.0` comment says the RustCrypto crates are pinned so shared types (`x509_cert::CertReq`, `pki_types::CertificateDer`, the protobuf `Bundle`) interoperate with sigstore's Fulcio client and `ManualTrustRoot`. B exposes none of `CertReq` or protobuf `Bundle` in its API; B's TSA verifier takes `CertificateDer` (same `rustls-pki-types` 1.x as X's `pki-types`).
- `pki-types` (`rustls-pki-types` 1): still the type in B's TSA API. X's own use is `ocx_sign` `trust_root.rs:204` (sigstore `TrustRoot::fulcio_certs` impl) and `ocx_util/src/tls.rs`; the first use disappears with the sigstore trait, the second does not.
- `p256` 0.13.2: X's `ocx_sign/src/sign/signer.rs:7-10` signs with `p256::ecdsa::SigningKey` directly, and `ocx_package_manager/Cargo.toml:43`, `ocx_test_support/Cargo.toml:24` use it. Those direct uses do not depend on the sigstore version; only the type-sharing rationale in the comment would.
- `x509-cert` 0.2.5: B uses the same 0.2 line (lock 0.2.5), so no second copy. X uses it in `ocx_sign/src/verify/tlog.rs:15-16`, `ocx_config`, `ocx_util`.
- `sha2_oid` (`sha2` 0.10 with `oid`): only exists for `x509-cert`'s `CertificateBuilder` in `ocx_test_support`; `p256` 0.13 still pulls `sha2` 0.10, so B changes nothing about it.
- `ed25519-dalek = "3.0.0"` (`Cargo.toml:211`): declared in `[workspace.dependencies]`; no member `Cargo.toml` references it (grep of `crates/*/Cargo.toml` shows no match). The lock has `ed25519-dalek` 2.2.0 only, pulled from sigstore (`Cargo.lock:4827`). B's `curve25519-dalek` 5.0 is a different major from the 4.1.3 that `ed25519-dalek` 2.2.0 uses.

What B would change: it removes the sigstore-0.14-specific reason for the pins (type sharing with sigstore's API) and the `sigstore` -> `ed25519-dalek` 2.2.0 / `curve25519-dalek` 4.1.3 edge; X's own direct use of `p256` and `x509-cert` remains. Conflicts with the pins: none found. B's `x509-cert` 0.2 and `der`/`spki` 0.7 match X's.

### 4. TLS and HTTP

- `reqwest = "0.13"`, `default-features = false`, `features = ["json","form"]` (`sigstore-rust:Cargo.toml` workspace deps). X: `reqwest = "0.13"` with `["json","rustls","charset","http2","system-proxy"]` (`Cargo.toml:143`); both resolve to 0.13.5. `reqwest::Client` types therefore unify (item 11).
- TLS selection is by feature, per crate: `rustls` (default) -> `reqwest/rustls`; `rustls-no-provider` -> `reqwest/rustls-no-provider`; `native-tls` -> `reqwest/native-tls` (e.g. `sigstore-rust:crates/sigstore-tsa/Cargo.toml:11-16`, `sigstore-sign/Cargo.toml:31-51`). The forwarding chain from `sigstore-sign` to rekor/fulcio/oidc/tsa/trust-root is explicit.
- `openssl`, `openssl-sys`, `native-tls`: none under any default feature. They appear in B's own `Cargo.lock` (`native-tls` 0.2.18 <- `hyper-tls`, `reqwest`, `tokio-native-tls`; `openssl` <- `native-tls`) only because B's CI resolves `--all-features`. A consumer that does not enable `native-tls` gets neither. Selecting `sigstore-sign = { default-features = false, features = ["rustls", ...] }` keeps them out. Whether Cargo would still record them in X's `Cargo.lock` was not tested; Cargo writes only features-reachable optional edges for registry deps, so they should not appear, but that was not run.
- Ambient-ID constraint: `ambient-id` supports only `rustls` or `native-tls`, so `rustls-no-provider` loses ambient CI credential detection (`sigstore-rust:README.md:186`, comment at `sigstore-oidc/Cargo.toml:13`). X's `ocx_sign/src/sign/oidc_ambient_inline.rs` is X's own implementation, not `ambient-id`.
- Provider: default `rustls` uses `aws-lc-rs` provider; `aws-lc-sys` is already in X's lock (0.46.0, B 0.47.0), so no second C crypto library.
- `hyper` 1.12.0, `hyper-util` 0.1.21, `rustls` 0.23.46, `tokio-rustls` 0.26.6, `rustls-native-certs` 0.8.5: identical in both locks (X `hyper-util` lines `Cargo.toml:~149-153`). `rustls-platform-verifier` 0.7.1 in B's lock; X's lock has it at line 5958.
- B has no direct `hyper` dependency.

### 5. deny.toml (read in full, 85 lines)

Advisory ignores in `deny.toml:5-29`:
- `RUSTSEC-2024-0388` (derivative), `-2024-0436` (paste), `RUSTSEC-2025-0057` (fxhash), `RUSTSEC-2023-0089` (atomic-polyfill): all attributed to the starlark family. Not sigstore related. B does not change them.
- `RUSTSEC-2024-0449` (unsafe-libyaml, via `serde_yaml_ng`): not sigstore related.
- `RUSTSEC-2023-0071` (rsa Marvin timing sidechannel, `deny.toml:16-28`): the only exception whose stated cause is sigstore. X's `Cargo.lock` shows `sigstore` depends on `openidconnect` and `openidconnect` on `rsa` (0.9.10). The comment gives the removal condition `cargo tree -e normal -i rsa` is empty. B's lock contains no `rsa`, `openidconnect`, `oauth2`, `tough` entries (checked by reverse-dependency scan of `Cargo.lock`). Whether another X crate pulls `rsa` was not checked (command was not run, per instructions); the `Cargo.lock` reverse scan of X shows only `openidconnect` listing `rsa` as a dependency.

Other sigstore-only items that may go away with the `sigstore` crate (not in `deny.toml`): `tough` 0.22.0 (listed in `ocx_sign/Cargo.toml:31-33` comment as pulled by `sigstore-trust-root`), `openidconnect` 4.0.1, `oauth2` 5.0.0, `rsa` 0.9.10, `ed25519-dalek` 2.2.0, `p384` 0.13.1, `ecdsa`. Whether each is removed entirely depends on other X dependents; not walked.

Licences, deny list and bans for B: `[bans] deny` (`deny.toml:72-81`) lists `openssl`, `openssl-sys`, `native-tls`, `structopt`, `async-std`, `error-chain`, `failure`. B's default graph reaches none (item 4). `multiple-versions = "warn"` (`deny.toml:65`): new warnings from `jiff` 0.2/1.0, `curve25519-dalek` 4/5, `fiat-crypto` 0.2/0.3, `jiff-core`/`jiff-tzdb*`. `unknown-registry = "deny"` and `unknown-git = "allow"` (`:84-85`): B has no git dependencies, all registry. New advisory exceptions needed: none identified; advisory status of the new crates was not queried (no network, no cargo-deny run).

### 6. Licences

- B's twelve crates: `license = "Apache-2.0"` from `[workspace.package]`; no crate overrides it (grep of `crates/*/Cargo.toml` for non-workspace `license` found none; `sigstore-rust:LICENSE` present). Apache-2.0 is in X's allow-list (`deny.toml:33`).
- X's `.licenserc.toml` (`:1-11`) only checks file headers on `crates/**/*.rs`, `excludes = ["external/**"]`; it is a header check and does not list licences. B's sources, when consumed from crates.io, are not under `crates/`.
- New dependency licences: verified from the local cargo registry only for crates already present: `jiff` 0.2.38 `Unlicense OR MIT`, `x509-cert` `Apache-2.0 OR MIT`, `secrecy` `Apache-2.0 OR MIT`, `fiat-crypto` 0.2.9 `MIT OR Apache-2.0 OR BSD-1-Clause`, `curve25519-dalek` 4.1.3 `BSD-3-Clause` (in allow-list), `dirs-sys` `MIT OR Apache-2.0`. Not verifiable offline (crate sources absent from `~/.cargo/registry`): `cms`, `directories`, `open`, `is-wsl`, `is-docker`, `ambient-id`, `astral-reqwest-middleware`, `jiff` 1.0, `curve25519-dalek` 5.0, `fiat-crypto` 0.3. From memory only (unverified): `cms` Apache-2.0 OR MIT (RustCrypto), `directories` MIT OR Apache-2.0, `open` MIT. `ambient-id` and `astral-reqwest-middleware` licences are not known to me.
- `Unlicense` is not in X's allow list (`deny.toml:32-61`); `jiff` already passes through the `MIT` arm of its `OR` expression.

### 7. TUF

- B has its own TUF client: `sigstore-tuf`, "a pure-Rust implementation of TUF ... inspired by `tough`" (`sigstore-rust:crates/sigstore-tuf/src/lib.rs:1-5`). It says why it does not use `tough` (`tough` recomputes key IDs and cannot load GitHub's TUF root, `lib.rs:9-22`). It ships its own canonical JSON (securesystemslib/OLPC style, `lib.rs:24-28`, file `canonical_json.rs`). No `tough` or `tuf` crate dependency (`sigstore-tuf/Cargo.toml:21-47`).
- Embedded roots: `include_bytes!("../repository/tuf_root.json")` and `tuf_staging_root.json` (`sigstore-rust:crates/sigstore-trust-root/src/tuf.rs:59,62`); GitHub root presumably the same pattern (`TufConfig::github()` `:158`). Embedded trusted roots `trusted_root.json`, `trusted_root_staging.json`, `trusted_root_github.json` (`trusted_root.rs:359-369`); embedded signing configs (`signing_config.rs:43,47,56`).
- Bootstrap trust is explicit: `TufBootstrap::TrustedRoot(Vec<u8>)` or `TufBootstrap::UnsafeCachedRoot` (`tuf.rs:100-112`), `TufConfig::custom_from_file` (`:199`).
- Cache location: `TufConfig::with_cache_dir(PathBuf)` (`tuf.rs:208`) makes it caller-controlled. Default when unset: `directories::ProjectDirs::from("dev","sigstore","sigstore-rust")`, `cache_dir().join("tuf").join(<url namespace>)` (`tuf.rs:398-410`), so the default is the platform cache directory (not under `OCX_HOME`). `without_cache()` (`:224`) and `offline()` (`:246`) exist. Offline mode re-verifies the cache against the bootstrap root (`:230-246`).
- `FileStore` and a `MetadataStore` trait (`sigstore-tuf/src/cache.rs`) back the cache; writes go through `tempfile` (`sigstore-tuf/Cargo.toml` comment "Atomic on-disk cache writes").

### 8. Protobuf and build scripts

- Protobuf: B does not use `sigstore_protobuf_specs`, `prost` or `protoc` in any published crate. Its bundle, rekor entry and trust-root types are hand-written serde types (`sigstore-rust:crates/sigstore-types/src/bundle.rs:101`). The only reference is a dev-dependency `sigstore_protobuf_specs = "0.5.1"` in the unpublished `sigstore-protobuf-compat` crate (wire compatibility tests, `publish = false`). X uses `sigstore_protobuf_specs` 0.5.2 (`Cargo.toml:~190`), and `ocx_sign` constructs protobuf `Bundle`/`TransparencyLogEntry` values (`crates/ocx_sign/src/sign/bundle.rs:6-9`).
- Build scripts: `find` over the whole B clone for `build.rs` and `*.proto` returned nothing. No B crate has a build script. Embedded data is `include_bytes!`/`include_str!` of checked-in JSON.
- Transitive build scripts for X's Bazel graph: `aws-lc-sys` (already annotated at `MODULE.bazel:104-107` `gen_build_script = "on"`), `curve25519-dalek` (uses `rustc_version` as a build dependency in both 4.1.3 and 5.0.0 locks; whether 5.0 has a `build.rs` was not read), `libc`, `jiff-static` (proc macro). Build scripts of `jiff` 1.0, `cms`, `open`, `directories`, `ambient-id`, `astral-reqwest-middleware`: not established (crate sources not available offline). `rules_rs` handles `from_cargo` from `Cargo.lock`; whether each new crate needs a `crate.annotation` was not established.

### 9. Async runtime and features

- Runtime: `tokio` 1.47 requirement; non-dev features used: `net`, `io-util`, `time` (`sigstore-oidc/Cargo.toml:28`, for the local redirect listener, `oauth.rs:21-22`) and `fs` (`sigstore-trust-root/Cargo.toml` `tuf` feature, `tuf.rs:351`). `macros`/`rt-multi-thread` appear only in dev-dependencies.
- No `reqwest::blocking`, no `block_on` outside tests (`crates/sigstore-crypto/src/hash.rs:254-448` are test uses of `futures::executor::block_on`), no `spawn_blocking` found by grep. TUF and signing APIs are `async`. `TufConfig::custom_from_file` is a sync `std::fs::read` (`tuf.rs:199-205`).
- Hashing API accepts `futures-io` readers (`hash_async_reader`), not tokio readers.

### 10. Publication

- Workspace version `0.17.0` for all twelve (`sigstore-rust:Cargo.toml` `[workspace.package]`). Internal deps use `path` + `version = "0.17.0"` (comment says "path + version for publishing").
- `publish = false` only on `sigstore-conformance` and `sigstore-protobuf-compat` (`crates/*/Cargo.toml:9`). The other twelve have no `publish` key.
- `[patch]` sections: none (`grep patch Cargo.toml crates/*/Cargo.toml` found none). Git dependencies: none (`grep 'git ='` found none).
- Release evidence is mixed (not proof): `CHANGELOG.md` heads show `0.17.0` dated 2026-11-22 for `sigstore-crypto`, `-rekor`, `-verify`, `-sign`; `sigstore-trust-root` at 0.16.0 (2026-11-09); `sigstore-types`, `-tuf`, `-bundle`, `-fulcio`, `-oidc` at 0.15.0 (2026-10-13); `-merkle`, `-tsa` at 0.15.1 (2026-10-23). Every manifest still says 0.17.0 and requires its siblings at `0.17.0`. The release tool (`release-plz.toml`, `git_release_enable = true`, `changelog_update = true`) writes per-crate changelogs. Whether `sigstore-types 0.17.0` and the other eight exist on crates.io was not checked (no network). A mismatch between workspace version and last released version could be a release-plz lag or a lockstep publish that does not write changelog entries for untouched crates; the files do not say which.
- Breaking-release cadence from `CHANGELOG.md`: trust-root 0.13.0 (09-23), 0.14.0 (09-29), 0.15.0 (10-13), 0.16.0 (11-09); `sigstore-verify` CHANGELOG lists `[**breaking**]` entries (`sigstore-rust:crates/sigstore-verify/CHANGELOG.md:24,27,30,32`) for "collapse verification entry points", "categorized, matchable verification errors", "final API naming polish before 1.0". 0.17.0 landed 2026-11-22. README example versions say "0.14" (`README.md:182-183`), so README is stale against 0.17.0.

### 11. The fork and the one recorded delta

- X's fork: `external/sigstore-rs` -> `https://github.com/ocx-sh/sigstore-rs.git`, branch `ocx/integration`, `ignore = untracked` (`.gitmodules`, third stanza). `git submodule status`: `-2361ffe07303acc4718b41874f4455f0a3fa865b external/sigstore-rs` (the `-` prefix means not checked out in this worktree; the directory is empty). Wired by `Cargo.toml:26` `[patch.crates-io] sigstore = { path = "external/sigstore-rs" }` and excluded from the workspace at `Cargo.toml:10`.
- Recorded in `.claude/rules/subsystem-deps.md:117`: `SigstoreTrustRoot::new_with_client`, "the TUF fetch takes the shared Sigstore `reqwest::Client` so extra CA roots, proxy and timeouts reach it (ocx#470). Based on tag `v0.14.0`; one upstreamable, DCO-signed commit per change". Last repo commit touching it: `d80c2e8e build(deps): vendor sigstore-rs as external/sigstore-rs with a TUF client seam`. The fork's own diff was not read (submodule not checked out); this is the only delta recorded in the repo docs.
- B equivalent: `TufConfig::with_http_client(self, client: sigstore_tuf::reqwest::Client)` (`sigstore-rust:crates/sigstore-trust-root/src/tuf.rs:218`), documented as "Fetch through a caller-configured HTTP client (timeouts, proxies, TLS roots, user agent)"; `sigstore_tuf` re-exports `reqwest` (`sigstore-tuf/src/lib.rs:101`). Lower-level: `HttpRepository`/`Updater` builder `with_http_client` (`sigstore-tuf/src/client.rs:572`). B also takes a caller client for Rekor (`sigstore-rekor/src/client.rs:334,376`), Fulcio (`sigstore-fulcio/src/client.rs:206`), TSA (`sigstore-tsa/src/client.rs:33`), OIDC (`sigstore-oidc/src/oauth.rs:234`) and `sigstore-sign` (`sigstore-sign/src/sign.rs:472`). Because both sides resolve `reqwest` 0.13.5 the types unify. B's default client for TUF is built in `client.rs:552`, with what timeouts was not read.
- Not established: whether B's `with_http_client` setters cover every request B makes (for example the Fulcio/Rekor/OIDC discovery calls in `sigstore-sign`), and whether X's extra-CA-roots plumbing (`ocx_util::tls::install_sigstore_roots`, `crates/ocx_cli/src/app/context.rs:327`) could be handed to each builder.

### 12. Satellite (ocx-mirror) and ripple

- The `ocx-mirror` checkout is not on this machine (`../ocx-mirror` absent), so what it imports from `ocx_sign`/`ocx_trust` could not be read.
- X side: `scripts/crate_map.toml:29-30` `ocx_trust = ["ocx_oci","ocx_util","ocx_env"]`, `ocx_sign = ["ocx_trust","ocx_oci","ocx_util","ocx_exit","ocx_env"]`; both listed as ecosystem tier in `AGENTS.md`. `taskfiles/satellite.taskfile.yml` header: "build the lockstep satellite ocx-mirror against this workspace" (`:1-12`), runs from `OCX_MIRROR_DIR` default `../ocx-mirror` (`:20`), refuses links to the operations crates `ocx_store ocx_shell ocx_project ocx_package_manager ocx_setup ocx_announce ocx_script ocx_test_support ocx_cli` (`:34-36`). `crate_map.toml:36` says `ocx_python`'s consumer is ocx-mirror; `ocx_sign` and `ocx_trust` are not named there as mirror dependencies. `adr_crate_split_workspace.md:42` records that `grimoire`'s manifest has no `sigstore`/`sign` entries at the time of writing; the ADR's `ocx_trust` row (`:216`) says "grimoire needs the policy engine".
- Public types of `ocx_trust` that name sigstore types: `SigstoreTrust` is OCX's own config type (`crates/ocx_trust/src/lib.rs:33-40`). `CosignVerificationKey` (a `sigstore` type) is held inside compiled policy and the crate's `Cargo.toml` comment says it "crosses this crate's public surface" (`crates/ocx_trust/Cargo.toml`, `sigstore.workspace = true`); used at `publishers.rs:197-199` with `SigstoreError::PublicKeyUnsupportedAlgorithmError`. A library swap would change the type that `ocx_trust`'s public `CompiledPolicy`/identity-rule surface exposes.
- Public types of `ocx_sign`: `SignedBundle` carries only bytes and strings (`crates/ocx_sign/src/sign/bundle.rs:33-50`), no sigstore types. `ocx_sign::verify::trust_root::TrustRoot` (`trust_root.rs:56`) implements `sigstore::trust::TrustRoot` and holds `SigstoreTrustRoot` (`:13-14`). `ocx_sign` imports from `sigstore`: `bundle::verify::Verifier` (pipeline.rs, 2 uses), `trust::*`, `rekor::models::*`, `crypto::verification_key::CosignVerificationKey`, `crypto::{Signature, SigningScheme}`; and from `sigstore_protobuf_specs`: about 40 `use` lines (counts from grep) across `bundle.rs`, `attestation_sidecar.rs`, `dsse.rs`, `simplesigning_read.rs`, `tlog.rs`, `verify/pipeline.rs` (25 matches). `sigstore_protobuf_specs` has no uses outside `crates/ocx_sign` (grep of `crates` returned no other file).
- Outside `ocx_sign`/`ocx_trust`: `ocx_config/src/tls.rs` and `ocx_cli/src/app/context.rs` use `sigstore` only in names (`sigstore_extra_roots`, `install_sigstore_roots`), not the crate.
- Where X already avoids sigstore's own clients: `Cargo.toml:~138` comment says Fulcio/Rekor calls in `ocx_sign` are hand-rolled over `reqwest` because "sigstore's own clients require an SCT + a different wire shape than the offline fake stack".

## negative

Searched and not found:
- No `build.rs`, `.proto`, `[patch]`, `git =` dependency in B (`find`/`grep` over the whole clone).
- No `openidconnect`, `oauth2`, `rsa`, `tough`, `p256`, `p384`, `ecdsa`, `ed25519-dalek`, `sha2`, `prost` in B's `Cargo.lock` outside what is listed above (`sha2`, `rsa`, `p256` reverse scans returned empty).
- No `openssl`/`native-tls` under a B default feature set.
- No `rust-toolchain` reference above 1.99.1 and no use of unstable features found; not exhaustively searched.
- `ocx-mirror` source: not on this machine; nothing found about its use of `ocx_sign`/`ocx_trust`.
- The fork's actual diff against 0.14.0: submodule not checked out, so only the docs record of the delta was read.

Contradicting or surprising evidence:
- `Cargo.toml:211` declares `ed25519-dalek = "3.0.0"` but no member uses it and the lock resolves 2.2.0 only. The workspace entry is dead weight regardless of the library choice.
- B's `README.md:182-183` shows `version = "0.14"`; the manifest is 0.17.0.
- B's own `Cargo.lock` contains `openssl`, `boring`, `native-tls`; none of those is a default-feature edge. Reading B's lock as "what a consumer gets" overstates the graph.
- The brief's "three breaking 0.x releases in 30 days": trust-root changelog shows 0.16.0 on 2026-11-09 and 0.17.0 on 2026-11-22 (for four crates) inside the window, i.e. at least two; the third was not identified.

Could not establish:
- Whether all twelve crates are on crates.io at 0.17.0 (no network; changelog evidence above says only four have a 0.17.0 release entry).
- Licences of `cms`, `directories`, `open`, `is-wsl`, `is-docker`, `ambient-id`, `astral-reqwest-middleware`, `jiff` 1.0, `curve25519-dalek` 5.0, `fiat-crypto` 0.3 (sources not in the local registry).
- Advisory status of any B-new crate (no `cargo deny`).
- Whether Cargo would add `native-tls`/`openssl` to X's `Cargo.lock` under B's default features (not run).
- Whether new crates need `rules_rs` build-script annotations (`MODULE.bazel:104-111` has them for `aws-lc-sys` and `zstd-sys` only).
- What `ocx-mirror` imports from `ocx_sign`/`ocx_trust`.
- B's default TUF HTTP client timeouts and whether every B network call honours a caller-supplied client.
- Whether the X-side `rsa` exception has any dependent besides `openidconnect`.

## leads

- Run `cargo tree -i rsa`, `cargo tree -i tough`, `cargo tree -i openidconnect` in X after a trial swap to learn which of `deny.toml:16-28` and the `tough`/`oauth2`/`ed25519-dalek` edges actually disappear.
- Check crates.io (or `cargo info`) for each of the twelve crate names at 0.17.0 and for `yanked` flags; compare against the changelog mismatch in item 10.
- Read the `ocx-mirror` repository's manifest and `use ocx_sign::`/`use ocx_trust::` lines to bound the ripple from `CosignVerificationKey` and `TrustRoot`.
- Read `sigstore-rust:crates/sigstore-trust-root/src/tuf.rs:260-420` and `sigstore-tuf/src/client.rs:520-600` for the default client's timeouts and redirect policy against X's SSRF guard.
- Read `sigstore-rust:crates/sigstore-fulcio/src/client.rs` and `sigstore-rekor/src/client.rs` against X's hand-rolled Fulcio/Rekor code (the `Cargo.toml:~138` comment) to see whether B's wire shape fits the offline fake stack the acceptance tests use (`test/fixtures/sigstore_stack.py`).
- Read the fork's single commit (`external/sigstore-rs`, rev 2361ffe) to confirm it is only the client seam.
- Look at `curve25519-dalek` 5.0 and `jiff` 1.0 for `build.rs` before judging Bazel impact.
