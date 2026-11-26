# Research: mirror signature practice

## Metadata

**Date:** 2026-11-25
**Domain:** security
**Triggered by:** `.agents/discussions/signing-dogfood-mirror.md`, the question of what a mirror signs with its own identity (each package after verifying upstream, each package unverified, its index only, or nothing)
**Expires:** 2027-05-25

Returned by one researcher (sonnet). The prompt named the four options and asked for evidence on each, with no ranking. OCI vendor mirrors are not repeated here; they are in `.claude/artifacts/research_signing_dogfood_competitive.md` § 4.

## Direct Answer

Tools that republish a repository's metadata sign the new metadata with the operator's key (aptly, reprepro, Pulp). No source read shows a mirror re-signing individual packages. Go, PEP 458 and OpenShift keep the origin as the signer and treat the mirror as untrusted. A Sigstore keyless bundle verifies without network calls when the trusted root is supplied out of band, with a staleness limit at each root rotation.

## Findings

### apt

- aptly verifies the upstream `Release` signature when it creates or updates a mirror, against `~/.gnupg/trustedkeys.gpg`; `-ignore-signatures` or `gpgDisableVerify` turn that off. <https://www.aptly.info/doc/aptly/mirror/create/>
- aptly publishing signs the new `Release` with a key the operator chooses (`-gpg-key`, or the gpg default); `-skip-signing` publishes unsigned. An aptly snapshot or subset therefore carries the mirror's signature, never the upstream one. <https://www.aptly.info/doc/aptly/publish/snapshot/>
- reprepro: `SignWith` in `conf/distributions` produces a `Release.gpg` with the operator's key. Upstream is checked per update pattern with `VerifyRelease` (a key fingerprint, several allowed with `|`). `FilterList` selects a subset. <https://manpages.debian.org/unstable/reprepro/reprepro.1.en.html> (first 100k of 110k characters read)
- debmirror checks `Release` and `Release.gpg` with gpgv by default; `--ignore-release-gpg` downgrades failures. The man page does not say whether upstream signature files are copied byte for byte, and is silent on re-signing. <https://manpages.debian.org/unstable/debmirror/debmirror.1.en.html>
- apt-mirror: `Release.gpg` and `InRelease` are only added to the download list; `gpg` and `gpgv` are never called. <https://github.com/apt-mirror/apt-mirror/blob/master/apt-mirror> (fetch truncated)
- apt has two layers. Tools that republish regenerate the repository-level `Release` signature. A per-package signature is not covered by these sources.

### rpm

- Pulp RPM signs `repomd.xml` through an administrator-supplied signing service. Each publication gets a detached signature and a public key (`repodata/repomd.xml.key`); clients need `repo_gpgcheck=1` and `gpgkey`. The page does not say whose key it is. <https://pulpproject.org/pulp_rpm/docs/user/guides/metadata_signing/>
- Satellite: a Red Hat solution says Satellite does not support signing repository metadata; a Bugzilla comment says Pulp has the metadata-signing information but not the package signing keys. Both seen only in search summaries, Satellite 6.x era, older than 18 months. <https://access.redhat.com/node/6826681>, <https://bugzilla.redhat.com/show_bug.cgi?id=1410638>
- `dnf reposync --gpgcheck` deletes downloaded packages that fail the per-package signature check and exits 1 if any were removed. It does nothing for repositories with `gpgcheck=0` and says nothing about metadata signatures. <https://dnf-plugins-core.readthedocs.io/en/latest/reposync.html>
- No primary source found on Pulp verifying upstream signatures at sync, on Satellite content-view publish behaviour, or on createrepo.

### Other ecosystems

- Nix: with `require-sigs = true` (default), a path that is not content addressed needs a signature from a key in `trusted-public-keys`, which can hold several keys. `secret-key-files` signs locally built paths. <https://nix.dev/manual/nix/stable/command-ref/conf-file.html> `nix store sign` signs with a local key; the docs read do not say whether it adds to existing signatures. <https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-store-sign.html>
- Go: the proxy is not a trust point. Hashes are checked against `go.sum` and the checksum database, which "makes untrusted proxies possible". `GOPRIVATE` and `GONOSUMDB` skip the database, and the first hash is then accepted unchecked. <https://go.dev/ref/mod>
- PEP 458: PyPI signs, and mirrors and CDNs "will not have any of the keys required to sign for projects". A bad mirror is a threat to be contained, not a signer. <https://peps.python.org/pep-0458/>

### Sigstore verification

- A bundle holds the Fulcio leaf certificate, transparency log entries (inclusion proof, checkpoint, SET) and optional RFC 3161 timestamps. It must carry at least one SET or RFC 3161 timestamp, because certificates are short-lived. <https://docs.sigstore.dev/about/bundle/>
- Fulcio certificates are valid for 10 minutes. <https://docs.sigstore.dev/certificate_authority/overview/>
- The client specification lists no network calls for verification. Inputs are the artifact, the bundle and root material distributed out of band. Rekor v1 online checks are optional; Rekor v2 clients "SHOULD not offer online verification". <https://github.com/sigstore/architecture-docs/blob/main/client-spec.md>
- cosign: `--trusted-root` takes a TrustedRoot JSON file; when omitted, cosign fetches the root from TUF. <https://github.com/sigstore/cosign/blob/main/doc/cosign_verify-blob.md> The official docs do not describe `--offline`.
- GitHub's offline guide: a trusted root has no built-in expiry, so material signed before it was generated keeps verifying. Material signed later verifies only until the next key rotation, "a few times per year". Revocation is not visible offline. <https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline>
- root-signing: `trusted_root.json` changes go through signing events; online signing happens "at least every three days". No TUF expiry durations are given. <https://github.com/sigstore/root-signing>
- An air-gap recipe from a third-party article of 2026-08 (not official): `cosign initialize` and `cosign save` on the connected side, `cosign verify --local-image --trusted-root` on the other. Limits it lists: trust roots go stale on rotation; Rekor v2 has no integrated time, so signed timestamps are needed; image indexes are affected by cosign issue 4937. <https://oneuptime.com/blog/post/2026-08-11-verify-cosign-air-gapped-sigstore-bundles/view>
- Private Fulcio, Rekor, TSA and CT log: vendor docs only (SUSE, GitLab). SUSE uses `cosign trusted-root create` and warns that anyone who can write the trust ConfigMap can swap the root. <https://documentation.suse.com/cloudnative/admission-controller/1.39/en/howtos/policy-servers/05-private-sigstore.html>, <https://docs.gitlab.com/ci/yaml/signing_self_hosted/>

### A mirror as trust point through re-signing

- OpenShift generates a `ClusterImagePolicy` with `remapIdentity` per mirrored repository, which verifies the original signature against the mirror's location. <https://developers.redhat.com/articles/2025/08/27/how-verify-container-signatures-disconnected-openshift>
- Docker Hardened Images docs say to copy the image and its signed attestations (`regctl --referrers`) and do not mention re-signing. <https://docs.docker.com/dhi/how-to/mirror/>
- A Red Hat Jira request (CLID-309) reportedly argues that re-signing breaks the vendor's chain of custody, that keyless signatures cannot be recreated and that signing keys are not available in an air gap. Seen only in a search summary; a feature request, not policy.
- No incident report and no design discussion was found that weighs re-signing against preserving signatures, in any ecosystem.

negative: Pulp's moved docs redirect to a landing page, so no claim about Pulp verifying upstream at sync is sourced.
negative: the Satellite docs page returned 403; the cosign_initialize page returned 404; the Quay source is paywalled.
negative: cosign's official docs do not list `--offline`.
negative: not checked: Nix signature coverage, the PEP 480 text, the two Jira items beyond their search summaries.

leads: sigstore-go `verify` options — they decide whether Rekor v2 bundles need signed timestamps, which affects mirrored keyless bundles.
leads: Notation and ORAS referrers — they copy detached signatures and would show how verifiers treat the same signature at a different registry host.
leads: Fedora and RPM `repo_gpgcheck` practice — a primary source would settle whether metadata signing is normal for republished repositories.
