# Research: Stable signer identity patterns

## Metadata

- Date: 2026-10-25
- Lane: community/practitioner
- Question: how can a verifier express "signed by project X" when the GitHub Actions Fulcio SAN changes with every release tag?
- Scope: facts and evidence for and against each pattern; no recommendation.
- Method: primary sources (Fulcio, cosign, sigstore-go, sigstore-rs, Kyverno, policy-controller, Homebrew and GoReleaser source, GitHub docs) fetched or read as raw files. Certificates of real uv and ruff releases were decoded with `openssl`. Secondary sources (blogs, aggregator pages) are marked **[secondary]**. Statements I could not confirm are marked **[unverified]**.
- Prior art already in the repo: `research_release_signing_prior_art.md` (Homebrew backfill, reusable-workflow fields, `gh attestation verify` flags). This file does not repeat it except where a new fact changes it.

## 0. Facts every pattern rests on

**Certificate fields** (https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md, mapping in https://github.com/sigstore/fulcio/blob/main/config/identity/config.yaml):

| Field | OID `1.3.6.1.4.1.57264.1.` | GitHub source claim | Mutable? |
|---|---|---|---|
| SAN (URI) | n/a | `https://github.com/{job_workflow_ref}` | yes, ref and path change |
| Build Signer URI | `.9` | same as SAN | yes |
| Build Signer Digest | `.10` | `job_workflow_sha` | no |
| Source Repository URI | `.12` | `https://github.com/{repository}` | yes (rename) |
| Source Repository Digest | `.13` | `sha` | no |
| Source Repository Ref | `.14` | `ref` | yes |
| Source Repository Identifier | `.15` | `repository_id` | no |
| Source Repository Owner URI | `.16` | `https://github.com/{repository_owner}` | yes (rename) |
| Source Repository Owner Identifier | `.17` | `repository_owner_id` | no |
| Build Config URI | `.18` | `workflow_ref` (top-level workflow) | yes |
| Build Trigger | `.20` | `event_name` | n/a |
| Run Invocation URI | `.21` | run id and attempt | n/a |
| Deployment Environment | `.23` | `environment` | n/a |
| deprecated `.2` to `.6` | trigger, sha, name, repository, ref | still written by Fulcio today | n/a |

- Fulcio's own config comments label `repository_owner` as "(mutable)" and `repository_owner_id` as the ID. The OID doc says `.15` and `.17` are immutable.
- **Real certificate (decoded 2026-10-25, uv 0.13.5 `uv-x86_64-unknown-linux-gnu.tar.gz`, via `gh api repos/astral-sh/uv/attestations/sha256:f68d291c…`):** SAN and `.9` and `.18` all `https://github.com/astral-sh/uv/.github/workflows/release.yml@refs/heads/main`; `.12` `https://github.com/astral-sh/uv`; `.13` and `.19` the 40-hex commit; `.14` `refs/heads/main`; `.15` `699532645`; `.16` `https://github.com/astral-sh`; `.17` `115962839`; `.20` `workflow_dispatch`; `.21` `…/actions/runs/39703477566/attempts/1`; `.22` `public`. The predicate carries the same `repository_id` and `repository_owner_id`.
- Newer OIDs (`.9` and up) are DER-encoded strings. The `openssl` dump shows the length prefix before the text. Deprecated `.2` to `.6` are raw UTF-8. sigstore-go parses the deprecated ones with `string(e.Value)` and the rest with `ParseDERString` (https://github.com/sigstore/sigstore-go/blob/main/pkg/fulcio/certificate/extensions.go).
- `job_workflow_ref` for a job inside a reusable workflow names the **called** workflow. `repository`, `sha` and `ref` name the **caller**. GitHub docs: "the `github` context is always associated with the caller workflow" (https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations). OIDC claim list: https://docs.github.com/en/actions/reference/security/oidc.
- GitHub recently added "immutable subject claims" (`repo:OWNER@OWNER-ID/REPO@REPO-ID:…`) for repositories created after 2026-07-15. That changes the OIDC `sub`, not the Fulcio SAN, which is built from `job_workflow_ref` (same OIDC doc, Fulcio config above).

**What each verifier can express today** (checked against source on 2026-10-25):

| Verifier | SAN exact | SAN regex | Issuer | Newer extensions (`.9`–`.23`) | Notes |
|---|---|---|---|---|---|
| cosign `verify` flags | `--certificate-identity` | `--certificate-identity-regexp` (Go RE2, **unanchored `MatchString`**) | exact or `-regexp` | **no flag** | Only `--certificate-github-workflow-{trigger,sha,name,repository,ref}`, exact string compare, the deprecated-OID family. Source: https://github.com/sigstore/cosign/blob/main/cmd/cosign/cli/options/certificate.go and `pkg/cosign/verify.go` (`validateCertExtensions`, `CheckCertificatePolicy`). |
| sigstore-go | exact | regex, not anchored by default (code comment: "use ^...$") | exact or regex | **yes**, `CertificateIdentity` embeds `certificate.Extensions` | https://github.com/sigstore/sigstore-go/blob/main/pkg/verify/certificate_identity.go. Several identities are OR-ed (first match wins). The `Extensions` struct has fields for `.9` to `.22`. |
| sigstore-rs (main) | exact (`Identity`, `names.contains`) | **none** | exact (`OIDCIssuer`, `.1`) | **no policy struct for `.9`–`.17`**; only `.1` to `.6` | https://github.com/sigstore/sigstore-rs/blob/main/src/bundle/verify/policy.rs. `SingleX509ExtPolicy` is a trait that compares one OID to one string with `!=`, reading the value as UTF-8 **without DER decoding**, so a policy on `.9`+ would need DER decoding added. `AllOf` and `AnyOf` combinators exist. |
| policy-controller | `subject` | `subjectRegExp` | `issuer` / `issuerRegExp` | **no field** in `ClusterImagePolicy` identity type | https://github.com/sigstore/policy-controller/blob/main/pkg/apis/policy/v1beta1/clusterimagepolicy_types.go. Strict match or regexp, per identity. Multiple policies AND, authorities OR (prior-art file). |
| Kyverno `verifyImages` keyless | `subject` (glob with `*`) | `subjectRegExp` | `issuer` / `issuerRegExp` | `additionalExtensions: map[string]string`; docs show only `githubWorkflowTrigger/Sha/Name/Repository` | https://kyverno.io/docs/policy-types/cluster-policy/verify-images/sigstore/. Whether keys for `.12`–`.17` exist is **[unverified]**. The source marks the attestor-level field `Deprecated` in one struct and not in another (`api/kyverno/v1/image_verification_types.go`). |
| `gh attestation verify` | `--cert-identity` | `--cert-identity-regex` | `--cert-oidc-issuer` (default GitHub Actions) | `--owner` (Source Repository Owner URI), `--repo`, `--signer-workflow`, `--signer-repo`, `--source-ref`, `--source-digest`, `--deny-self-hosted-runners` | https://cli.github.com/manual/gh_attestation_verify |

- Extension pins are easy to lose silently. sigstore-js ≤ 4.1.0 accepted `certificateOIDs` and dropped them before enforcement, so a signature without the extensions passed (CVE-2026-48815, fixed in 4.1.1; **[secondary]** https://corgea.com/research/cve-2026-48815-sigstore-js-certificateoids-verification-bypass).

## 1. Dedicated signing workflow with a constant SAN

**How it works.** A workflow file such as `sign.yml` (or the release workflow itself) runs only from the default branch, so `job_workflow_ref` is `org/repo/.github/workflows/sign.yml@refs/heads/main` for every release. Triggers that run in default-branch context:

- `workflow_dispatch`: `GITHUB_REF` is the branch or tag that received the dispatch; the file must exist on the default branch; it can be dispatched "against any branch or tag via the GitHub API or GitHub CLI".
- `repository_dispatch`, `workflow_run`: `GITHUB_REF` and `GITHUB_SHA` are the default branch.
- `workflow_call`: `GITHUB_REF` and `GITHUB_SHA` are the caller's, so the SAN ref is constant only if the caller writes the called workflow with an explicit ref (`uses: org/repo/.github/workflows/sign.yml@main`). The called workflow's `github` context stays the caller's. Whether a same-repo `uses: ./.github/workflows/sign.yml` pins to the caller's ref (and so keeps the tag in the SAN) is not stated on the docs page I read and was not tested. **[unverified]**
- Source: https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows.

**Identity a verifier pins.** SAN exact `https://github.com/org/repo/.github/workflows/sign.yml@refs/heads/main` plus issuer `https://token.actions.githubusercontent.com`. No regex needed. Optionally add `.20` Build Trigger and `.23` Deployment Environment (needs sigstore-go-style extension support; cosign has no flag).

**Provenance loss.**
- The release **tag is not in the certificate**. `.14` is `refs/heads/main`.
- The repo stays in the SAN, and `.12`, `.13` (commit SHA) and `.15` remain. For a `workflow_dispatch` run the commit is the head of the dispatched branch, not necessarily the commit the tag points at.
- Real data, uv 0.13.5 and ruff 0.17.2 release assets (decoded 2026-10-25): SAN `…/release.yml@refs/heads/main`, `.20` `workflow_dispatch`, `.13` the commit built. The tag is recoverable only from the GitHub release, not from the certificate. Both repos also carry a second, GitHub-signed attestation (SAN `https://dotcom.releases.github.com`), which is GitHub's release attestation (see `gh release verify`: https://cli.github.com/manual/gh_release_verify).
- If the signed payload is a checksum file or an attestation naming the tag, the tag lives in the signed content instead of the certificate.

**Blast radius on compromise.** One repo. Anyone who can merge to the default branch, bypass its protection, or alter `sign.yml` obtains valid signatures for that identity, for any artifact. A compromise of an unrelated repo in the org does not help.

**Signing-oracle risk.** Real, and the main cost of the pattern:
- Any principal allowed to trigger the workflow obtains a signature on whatever the workflow signs. `workflow_dispatch` needs repo write access. `repository_dispatch` needs a token that can call the API. `client_payload` and `inputs` are attacker-controlled data.
- A dispatch on a non-default branch yields `@refs/heads/<branch>`, which does not match a pin on `main`. So a writer cannot sign with a modified workflow unless that change reaches main. Source: dispatch-any-branch doc above.
- GitHub's events doc warns that "running untrusted code on the `workflow_run` trigger may lead to security vulnerabilities", including secret and write-token access (same page).
- PyPI's trusted-publisher guidance, which binds to repo plus workflow filename, lists the mitigations: a dedicated `publish` environment with required reviewers, job-level `permissions`, protected `v*` tags, and a build job separate from the publish job (https://docs.pypi.org/trusted-publishers/security-model/). `.23` puts the environment name into the certificate, so a verifier can in principle require it. This is a certificate field, not something I saw any verifier use.
- GitHub secure-use doc: any user with write access can read repository secrets; `CODEOWNERS` on `.github/workflows` is the suggested control (https://docs.github.com/en/actions/reference/security/secure-use).

**Rotation.** No secret to rotate. The identity changes only when the workflow file is renamed or moved. To rotate, a verifier needs several identities at once during overlap (sigstore-go ORs a list; policy-controller ORs authorities). A client with a single exact-SAN field cannot overlap.

**Real-world users (verified).**
- uv and ruff: release SAN `release.yml@refs/heads/main`, trigger `workflow_dispatch` (decoded above). Docs verify with `--owner`, not with a SAN pin (see pattern 5).
- Chainguard free images: `cosign verify … --certificate-identity=https://github.com/chainguard-images/images/.github/workflows/release.yaml@refs/heads/main --certificate-oidc-issuer=https://token.actions.githubusercontent.com` (https://edu.chainguard.dev/chainguard/containers/security-and-compliance/verifying-chainguard-images-and-metadata-signatures-with-cosign/).
- sigstore/cosign runs `cut-release.yml` on `workflow_dispatch`, authenticating to GCP through workload identity (https://github.com/sigstore/cosign/blob/main/.github/workflows/cut-release.yml). That file only starts the release; the signing identity users verify is the GCP service account in pattern 2 (I did not read the workflow's downstream steps in full).

**For.** Exact string, no regex, no tag knowledge in the verifier, works with every client that has SAN exact match (including sigstore-rs `Identity`). Cheap rotation. Already used by uv, ruff and Chainguard.
**Against.** The tag drops out of the certificate. The workflow's trigger permissions become the signing permissions. The pin is only as strong as branch protection on `main`. It does not give "project X" across repos by itself: each repo has its own SAN.

## 2. Central organisation signing repo or service

Two shapes appear in practice.

**2a. Central reusable workflow called by every repo.**
- SAN is `org/signing/.github/workflows/sign.yml@<ref the caller wrote>`. `.12`, `.13`, `.14` name the **calling** repo, commit and ref. `.18` (Build Config URI) names the caller's top-level workflow. Provenance is preserved.
- The verifier needs two checks: the central workflow in the SAN or `.9`, and the caller identity in `.12` or the repository claim. cosign has no flag for `.12`; the deprecated `--certificate-github-workflow-repository` (`.5`) carries the caller's `owner/repo` name.
- Real users: slsa-github-generator (`generator_generic_slsa3.yml@vX.Y.Z`; it requires a semver tag reference, not a SHA or short tag; verifier uses `--builder-id` for the workflow and `--source-uri` for the caller). The tool is "no longer actively maintained" and points to GitHub attestations (https://github.com/slsa-framework/slsa-verifier/blob/main/README.md; generator README in `internal/builders/generic/README.md`). Homebrew's comment says "switch to a single reusable workflow, which would then be our sole identity", and that GitHub attestations "do not include reusable workflow state by default" (https://github.com/Homebrew/brew/blob/master/Library/Homebrew/attestation.rb).
- An issue in a Linux Foundation release-engineering repo argues the SAN carries the ref the caller pinned (often a SHA), not the project tag, so a tag regexp would never match; it was closed as completed on 2026-10-15 with no visible test result **[secondary, unverified]** (https://github.com/lfreleng-actions/node-workflows/issues/149).

**2b. Central signing service or repo that other repos dispatch to or submit artifacts to.**
- SAN names the service only. The source repository is no longer in the certificate and must be asserted inside the signed payload, which the verifier has to trust the service to have checked.
- Real users:
  - Kubernetes signs with GCP service accounts: binaries and SBOM `krel-staging@k8s-releng-prod.iam.gserviceaccount.com`, images `krel-trust@k8s-releng-prod.iam.gserviceaccount.com`, issuer `https://accounts.google.com`, exact match, no regex (https://kubernetes.io/docs/tasks/administer-cluster/verify-signed-artifacts/). KEP: https://www.kubernetes.dev/resources/keps/3031/.
  - sigstore/cosign: keyless identity `keyless@projectsigstore.iam.gserviceaccount.com`, issuer `https://accounts.google.com`, from `release/cloudbuild.yaml` (https://github.com/sigstore/cosign/blob/main/release/cloudbuild.yaml; install doc https://docs.sigstore.dev/cosign/system_config/installation/).
  - Chainguard private registries: its own OIDC issuer `https://issuer.enforce.dev`, identities `image-syncer` and `custom-image-builder`, verified with `--certificate-identity-regexp="https://issuer.enforce.dev/(${IMAGE_SYNCER}|${CUSTOM_IMAGE_BUILDER})"` (same Chainguard page).
  - Homebrew backfill: a separate repo (`trailofbits/homebrew-brew-verify`) signed old bottles; the verifier trusts it only for attestations dated at or before `2024-03-14` (https://github.com/Homebrew/brew/blob/master/Library/Homebrew/attestation.rb).

**Identity a verifier pins.** One identity for the whole project (2b), or one workflow identity plus a per-caller check (2a).

**Provenance loss.** 2a: none, if the verifier checks `.12`/`.13`. 2b: total at certificate level. Kubernetes and cosign identities say nothing about commit or repo.

**Blast radius.** 2b: compromise of the service or its cloud account signs for every repo in the project. 2a: compromise of the central workflow repo (a malicious change to the called workflow) affects every caller at the pinned ref; callers pinned to a SHA or tag are exposed only on update.

**Signing-oracle risk.** 2b: whoever can submit to the service. The service must authenticate the caller and bind the payload to the caller's repo. 2a: any repo that can call the workflow gets a valid SAN. Homebrew states the same point for GitHub attestations: a SAN-only check cannot tell callers apart.

**Rotation.** 2a: change the ref the callers use. 2b: workload-identity service accounts have no secret; moving to a new service account is an identity change that needs verifier overlap.

**For.** One identity per project across a CLI, mirror tool, SDK generator and product family. Proven at Kubernetes scale. **Against.** Needs infrastructure outside GitHub (2b), or a pinned-ref discipline across all callers (2a). In 2b, repo provenance moves into the payload.

## 3. Match on certificate extensions instead of the SAN

**How it works.** The verifier ignores the tag-bearing SAN and pins stable fields: issuer, Source Repository Owner (URI `.16` or ID `.17`), Source Repository URI (`.12`) or ID (`.15`), Build Signer URI (`.9`), often combined.

**Tool support** (table in section 0):
- cosign: no flags for `.9`–`.17`. Only the deprecated `.2`–`.6` family via `--certificate-github-workflow-*`. `--certificate-github-workflow-repository` is an exact match on the `repository` claim, `owner/repo` string.
- sigstore-go: full support through `CertificateIdentity.Extensions`.
- sigstore-rs: no policy structs for `.9`–`.17`; would need new `SingleX509ExtPolicy` impls with DER decoding.
- policy-controller: not expressible; identities are issuer and subject only.
- Kyverno: `additionalExtensions` map. The documented GitHub example pins `githubWorkflowTrigger`, `githubWorkflowSha`, `githubWorkflowName`, `githubWorkflowRepository` next to a `refs/tags/*` subject. It states the reason: "workflows can be reused", so both the executing and the signing workflow are checked.
- `gh attestation verify --owner` implements the owner check. The uv docs show its output: `Source Repository Owner URI must match: https://github.com/astral-sh` and `Subject Alternative Name must match regex: (?i)^https://github.com/astral-sh/` (https://github.com/astral-sh/uv/blob/main/docs/guides/integration/docker.md).

**Does owner ID protect against renames or resurrection?**
- Yes, in the certificate: `.15` and `.17` are numeric and Fulcio fills them from `repository_id` and `repository_owner_id` (uv cert above: `699532645`, `115962839`). A verifier that pins `.17` rejects a different owner that later takes the same name.
- Not in the common tools: the SAN, `.12`, `.16` and every cosign flag are name-based. `gh attestation verify --owner` matches `.16` (a URI). The gh manual says that with `--bundle` or `--bundle-from-oci` it "cannot detect whether the repository or owner has since been renamed", and that omitting both flags makes it fetch attestations from the GitHub API (https://cli.github.com/manual/gh_attestation_verify).
- Rename behaviour: GitHub keeps redirects for web links and retires an old owner/repo name only in some cases (public repos listed on Marketplace, over 100 clones or Actions uses in the prior week; images over 5,000 downloads). Otherwise a vacated name can be registered again (https://docs.github.com/en/account-and-profile/reference/username-reference).
- A live case of the name-based failure: npm trusted publishing bound trust to `owner/repo` and workflow filename and, until first publish, did not check `repository_id` or `repository_owner_id`. Anyone who re-created a freed name could publish, and the release was "still signed through Sigstore". Disclosed 2026-10-13; npm now expires unvalidated configs after 48 hours **[secondary]** (https://workos.com/blog/npm-trusted-publishing-slugfest). The OpenSSF trusted-publisher spec has a section on resurrection and rename attacks (same source).
- PyPI's security model page does not describe owner-ID handling for GitHub and says GitLab recreated names are still accepted (https://docs.pypi.org/trusted-publishers/security-model/).

**Provenance loss.** None for what is pinned: the repo, owner and ref remain readable. The tag is still absent from the SAN unless `.14` is a tag ref.

**Blast radius.**
- Owner-only pin (`.16`/`.17`): every repo and every workflow in the org can produce a matching signature. That is the largest radius of any pattern here.
- Owner plus repo (`.12`/`.15`): one repo, any workflow in it, any ref, unless `.9`/`.14` also pinned.
- Owner plus Build Signer URI: one workflow file in the central repo, any caller within the owner.

**Signing-oracle risk.** Equal to the radius above: any workflow run in scope that reaches an `id-token: write` job can sign anything.

**Rotation.** No keys. Changing which fields are pinned is a verifier policy change; the new policy must cover signatures already published.

**Real-world users.** uv and ruff docs (`gh attestation verify --owner astral-sh`, `--repo astral-sh/ruff`); Homebrew (`--repo`, workflow optional; repo pin only for homebrew-core); Kyverno 1.13 SigstoreBundle example for GitHub attestations (https://kyverno.io/blog/2024/10/30/announcing-kyverno-release-1.13) **[secondary for the example detail]**.

**For.** Survives ref and tag changes with no regex. Can pin immutable IDs. Provenance stays visible. **Against.** cosign (the most common CLI) cannot express it. sigstore-rs lacks it. Two production-grade verifiers (policy-controller, Kyverno) have gaps or unverified support. A silent drop of the extension check is a documented bug class (CVE-2026-48815). An owner-only pin trusts the whole org.

## 4. KMS or long-lived keys

**How it works.** The release job signs with a key held in KMS or a file key. Verification pins a public key.

- cosign URIs (https://docs.sigstore.dev/cosign/key_management/overview/): `gcpkms://projects/…/cryptoKeys/$KEY/versions/$KEY_VERSION`, `awskms:///$KEYID` (key ID, ARN or alias; alias recommended), `azurekms://[VAULT]/[KEY]`, `hashivault://$keyname`. Only the GCP URI carries a version. The overview page has no rotation guidance.
- minisign: 8-byte key ID inside every signature, trusted comments (version, timestamp, file name) signed with the key; the docs describe no rotation or revocation procedure (https://jedisct1.github.io/minisign/).
- signify (OpenBSD): the man page examples use `/etc/signify/openbsd-81-base.pub`; keys are per release. A secondary write-up says each release embeds the next release's key and that there is no revocation **[secondary]** (https://isopenbsdsecu.re/mitigations/signify/).

**Identity a verifier pins.** A public key (or KMS key reference). The identity never changes per release.

**Provenance loss.** Total. The signature says "someone with the key", not which repo or commit. A trusted comment (minisign) or signed payload can carry a version but not build provenance.

**Blast radius.** Everything the key signs, for as long as the key is trusted. Revocation depends on the client.

**Signing-oracle risk.** Whoever can call the KMS sign operation, which is an IAM question. A CI job with sign permission signs any input.

**Rotation (observed practice).**
- sigstore/cosign signs releases with both a GCP KMS key (`gcpkms://…/versions/{{ .Env.KEY_VERSION }}` in `.goreleaser.yml`, public key `release/release-cosign.pub`) and keyless. The install doc tells users to verify the binary with the artifact key first, and then use identity-based verification for later releases. It also records that "Cosign v3.0.1 is missing artifact key signatures" (https://docs.sigstore.dev/cosign/system_config/installation/).
- Bun: the Docker images fetch `SHASUMS256.txt.asc` and verify it with GPG; the comment says "The signing key is inlined so that a keyserver outage cannot fail the build" (https://github.com/oven-sh/bun/blob/main/dockerhub/debian/Dockerfile).
- Rust: `.asc` signatures exist next to the tarballs and `rust-key.gpg.ascii` is served from `static.rust-lang.org` (HTTP 200 on both, checked 2026-10-25). The Rust Foundation planned a Rust PKI with a delegated signing certificate and rustup verification (post dated 2023-12-21, https://rustfoundation.org/media/improving-supply-chain-security-for-rust-through-artifact-signing). A 2023 search snippet called rustup's signature verification "experimental and incomplete", with no key rotation **[secondary, unverified]**.
- Deno: checksums only (`--checksum` on `deno upgrade`, `.sha256sum` files) **[secondary]** (https://docs.deno.com/runtime/reference/cli/upgrade/).

**For.** Constant identity for every release. Works with any signature verifier. No dependency on CI-provider claims. **Against.** No provenance. Secret management and revocation fall on the project. Rotation is by hand and per-ecosystem. The cosign project itself keeps keyless alongside the key.

## 5. GitHub artifact attestations with org-level verification

**How it works.** `actions/attest-build-provenance` (cargo-dist wraps it: uv and ruff `dist-workspace.toml` has `github-attestations-phase = "announce"`) signs an in-toto statement with Fulcio. Public repos use the Sigstore public-good instance with a public log. Private repos use GitHub's own Sigstore instance with no transparency log (https://docs.github.com/en/actions/concepts/security/artifact-attestations). Consumers verify with `gh attestation verify` using `--owner` or `--repo`. GitHub says attestations alone give SLSA Build Level 2 and recommends a reusable workflow shared across the org for Level 3.

**Identity a verifier pins.** By default: issuer GitHub Actions, Source Repository Owner URI, predicate type, and SAN regex `(?i)^https://github.com/<owner>/` (uv docs output). Tighten with `--repo`, `--signer-repo`, `--signer-workflow`, `--source-ref`, `--source-digest`, `--cert-identity[-regex]`.

**Provenance loss.** None: the attestation records workflow, repo, org, commit, trigger and other OIDC claims. The tag is not in the certificate for dispatch-triggered runs (uv: trigger `workflow_dispatch`, ref `main`); the predicate holds `externalParameters.workflow.ref`.

**Blast radius.** `--owner` alone: every repo in the org, any workflow, as in pattern 3. `--repo` plus `--signer-workflow`: one workflow in one repo.

**Signing-oracle risk.** Any workflow in scope with `attestations: write` and `id-token: write` can produce an attestation for any subject digest.

**Rotation.** No project keys. Trust roots rotate through Sigstore TUF; `--custom-trusted-root` takes a `trusted_root.jsonl` for offline use. Renames: with local or OCI bundles the owner or repo is checked against names at signing time, so renames go undetected (gh manual).

**Real-world users.** uv Docker: `gh attestation verify --owner astral-sh oci://ghcr.io/astral-sh/uv:latest`; ruff: `gh attestation verify <file> --repo astral-sh/ruff` (commit in ruff adding attestations, **[secondary]** via search result). Homebrew core bottles: `gh attestation verify … --repo Homebrew/homebrew-core`. Kyverno 1.13 SigstoreBundle.

**For.** Org-wide "signed by project X" exists today as one flag. No regex. Includes full provenance. **Against.** Requires the `gh` CLI or the GitHub API/bundles; a non-GitHub-aware verifier (such as a Rust client on sigstore-rs) must reimplement the checks. Default trust is the whole owner. Not usable without GitHub-hosted builds. The reusable-workflow state is not in the default attestation (Homebrew comment).

## 6. How notable projects chose their release identity

| Project | Mechanism | Identity a user pins | Source |
|---|---|---|---|
| Kubernetes | keyless, GCP service accounts | `krel-staging@…` (binaries, SBOM), `krel-trust@…` (images), issuer `accounts.google.com`, exact | https://kubernetes.io/docs/tasks/administer-cluster/verify-signed-artifacts/ |
| sigstore/cosign | KMS artifact key plus keyless service account | `release-cosign.pub` for bootstrap, then `keyless@projectsigstore.iam.gserviceaccount.com` | https://docs.sigstore.dev/cosign/system_config/installation/ |
| Chainguard images | keyless: GitHub workflow on main; private registries use own OIDC issuer | `…/chainguard-images/images/.github/workflows/release.yaml@refs/heads/main`; `https://issuer.enforce.dev/(image-syncer\|custom-image-builder)` regexp | Chainguard page above |
| Homebrew | GitHub attestations | repo `Homebrew/homebrew-core`, workflow unpinned (several signing workflows), backfill repo plus date cutoff | `attestation.rb` |
| GoReleaser (own releases) | keyless, tag-triggered | per release: `…/goreleaser/goreleaser/.github/workflows/release.yml@refs/tags/__VERSION__` exact in docs; install script uses `--certificate-identity-regexp "https://github.com/goreleaser/goreleaser.*/.github/workflows/.*.yml@$REF"` | https://github.com/goreleaser/goreleaser/blob/main/www/content/getting-started/install/oss.md, `www/static/run` |
| Flux | keyless, tag-triggered | `--certificate-identity-regexp=^https://github\.com/fluxcd/.*$` (whole org) | https://fluxcd.io/flux/security/ |
| uv, ruff (Astral) | GitHub attestations, cargo-dist, dispatch on main | `gh attestation verify --owner astral-sh`; cert SAN `release.yml@refs/heads/main` | uv docs, decoded certs |
| Rust toolchain | GPG detached `.asc`; PKI plan | `rust-key.gpg.ascii` | HEAD checks; Rust Foundation post |
| Bun | GPG-signed `SHASUMS256.txt.asc` | key inlined in the Docker build | Bun Dockerfile |
| Deno | checksums only **[secondary]** | none | docs.deno.com upgrade |

Observations without interpretation:
- Two of the largest keyless users (Kubernetes, cosign) avoid the GitHub tag-in-SAN problem by signing from a non-GitHub service identity.
- The GitHub-based projects split: constant SAN on `main` (Chainguard, uv, ruff), per-tag SAN with exact or regexp (GoReleaser, Flux), or org-level checks through attestations (uv, ruff, Homebrew).
- Flux and `gh --owner` both resolve to "any workflow in the org".
- Rust, Bun and Deno do not use Sigstore for toolchain releases.

## 7. Pitfalls

**Regex approach.**
- **Unanchored by default.** cosign compiles the identity regexp and calls `MatchString` (`pkg/cosign/verify.go`), so `https://github.com/org/repo` also matches `https://github.com/org/repo-evil/...`. sigstore-go's code comment says the same. A guide gives the anchored form `^https://github\.com/example/widget/\.github/workflows/release\.yml@refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$` **[secondary]** (https://oneuptime.com/blog/post/2026-08-11-cosign-keyless-signing-explained/markdown).
- GoReleaser's install script regexp, as written, has no anchors, an unescaped `.` before `yml`, `goreleaser.*` (any repo name starting with `goreleaser` in that org), `.*.yml` (any workflow) and `@$REF` with no end anchor. This is an observation of the text, not a finding of exploitability.
- **Ref wildcards.** `refs/tags/*` (Kyverno docs example) and `refs/tags/v.*` trust every tag anyone with tag-creation rights can push. Tag protection or rulesets are then part of the identity. `.*` in the ref position on a tag-triggered workflow also admits `refs/tags/v1.0.0-evil`.
- **Org-prefix regexps** (`^https://github\.com/fluxcd/.*$`, gh `--owner`) trust every repo and workflow in the org.
- **No ID, only names.** A regexp on the SAN cannot carry `repository_id`.
- **Empty-policy fail-open.** In cosign's `CheckCertificatePolicy`, an identity with no subject or no issuer "checks out" for that field; the CLI requires one of the flag pairs.
- **RE2 only** (cosign, sigstore-go): no lookahead, so "any tag except X" is hard.
- **Case.** `gh` wraps its SAN regexp in `(?i)`; Fulcio SANs are case-sensitive strings **[secondary]**.
- **Lost extension pins** in verifier code (CVE-2026-48815) show that a test with a deliberately wrong extension is the check that proves the pin is enforced.

**Constant-identity approach.**
- Dispatch rights equal signing rights. `workflow_dispatch` is open to anyone with write; `repository_dispatch` to anyone with a token that can call the API; inputs are unvalidated data.
- The pin is only valid while `main` is protected. A force-push or bypass actor produces valid signatures.
- Branch-selectable triggers: a writer can dispatch from another branch, which gives a non-matching SAN (a safe failure) unless the verifier uses a regexp on the ref.
- `workflow_run` and `repository_dispatch` run in privileged default-branch context; GitHub warns about untrusted code.
- Renaming or moving the workflow file silently changes the identity of all future releases.
- Provenance narrowing: no tag in the certificate, and a dispatch commit may differ from the tag commit.
- Mitigations seen in sources: environments with required reviewers (PyPI doc), `CODEOWNERS` on `.github/workflows`, signing a checksum file or attestation that names the tag, adding `.20`/`.23` to the policy, and a date-bounded second identity during a transition (Homebrew).

## 8. Comparison table

Radius is the set of sources that obtain a valid signature if the pinned identity's controls fail. "Tag in cert" means the release tag is readable from the certificate.

| Pattern | Identity pinned | Tag in cert | Repo/commit in cert | Radius | Oracle | Rotation | Fits a Rust sigstore-rs client today | Seen in |
|---|---|---|---|---|---|---|---|---|
| 1 Dedicated workflow on main | exact SAN `…/sign.yml@refs/heads/main` + issuer | no | repo, commit, branch | one repo's main | anyone who can trigger or edit the workflow | rename = identity change; needs multi-identity overlap | yes (`Identity` exact) | uv, ruff, Chainguard |
| 2a Central reusable workflow | SAN of central workflow + caller field | no | caller repo, sha, ref (`.12`–`.14`) | all callers at pinned ref | any repo that can call it | change pinned ref | partial; caller check needs `.12` | slsa-github-generator, Homebrew (stated goal) |
| 2b Central service or GCP SA | exact SAN (email or URI) + non-GitHub issuer | no | none | whole project | whoever submits to service | new SA = overlap | yes (exact) if issuer allowed | Kubernetes, cosign, Chainguard private |
| 3 Extensions | issuer + `.16`/`.17` and/or `.12`/`.15` and/or `.9` | `.14` only if tag-ref | yes | owner, or repo, or workflow | any workflow in scope | policy change | no (`.9`–`.17` unsupported, no DER decode) | gh `--owner`, Kyverno examples |
| 4 KMS or long-lived key | public key | no | none | all signatures of the key | whoever can call sign | manual, per-ecosystem | yes (key verify) | cosign, Bun, Rust, minisign users |
| 5 GitHub attestations + `--owner` | owner URI + SAN prefix regex + predicate type | in predicate | yes | whole owner (default) | any workflow with attest permission | Sigstore TUF root | no (needs gh or reimplementation) | uv, ruff, Homebrew |
| Per-tag SAN, exact or regexp (baseline) | SAN per release or regexp | yes | repo, workflow, tag | one repo | any tag in the regexp | n/a | exact only (no regex in `Identity`) | GoReleaser, Flux |

## negative

- No source found on how Kyverno's `additionalExtensions` maps keys to `.12`–`.17`, or whether it accepts them.
- cosign docs pages for `verify` did not render the `--certificate-github-workflow-*` flags; they were confirmed from the options source and generated CLI doc instead.
- Not tested: whether a same-repo `uses: ./.github/workflows/x.yml` keeps the caller's ref in `job_workflow_ref`.
- No documented rotation procedure found for minisign or for cosign KMS keys; signify rotation rests on one secondary source.
- Bun and Deno signing was only seen in a Dockerfile and a secondary search snippet; nothing found for Bun or Deno using Sigstore.
- rustup's current signature-verification state was not confirmed.
- A search snippet mentioned a cosign legacy-bundle identity-verification fix (v3.1.3 and v2.6.5) from one blog; I could not confirm it against a cosign advisory.
- No source found stating that GitHub's `gh attestation verify --owner` uses `.17` rather than `.16`; the uv output shows `.16`.

## leads

- sigstore-rs lane: add policy types for `.9`–`.17` with DER decoding and a regex `Identity`, with a red/green test per extension (CVE-2026-48815 as the failure model); check what the OCX fork already carries.
- Empirical lane: publish a throwaway repo and decode certificates for a same-repo `uses: ./` call, a cross-repo `@main` call and a `workflow_dispatch`, to settle what `job_workflow_ref`, `.12`–`.14` and `.20` contain.
- Rulesets lane: GitHub rulesets for `refs/heads/main`, `v*` tag creation and required deployment environments, to measure how far they close the oracle for patterns 1 and 2a.
- Trust-root lane: how `gh attestation` and Kyverno ship rotating trust roots (`--custom-trusted-root`, TUF), as precedent for distributing a project's identity set without a client release.
