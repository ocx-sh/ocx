---
title: "Sign a release"
description: "Sign a pushed package with keyless Sigstore from your laptop or CI, sweep rolling tags, use a key pair instead, and check the result."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

Sign a package you have pushed so that consumers can check who published it. By the end you have a [Sigstore][sigstore] signature stored next to the package in the registry, and one `ocx package verify` call that proves it.

You need a package already on a registry, as described in [Build and push][publishing-build-and-push]. You also need an identity that Sigstore trusts, such as a GitHub Actions workflow, a GitLab job or an email account. Signing needs network access. With `--offline` the command exits 81.

## Sign a pushed package {#supply-chain-signing}

[`ocx package sign`][cmd-package-sign] publishes a Sigstore bundle as a referrer of the manifest you name. Sign the platform manifest you pushed:

```sh
ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

OCX generates a throwaway key pair and asks [Fulcio][fulcio] for a short-lived certificate bound to your OIDC identity. It then records the signature in the [Rekor][rekor] transparency log. You manage no key. Run the command once and read `Certificate identity` and `Certificate OIDC issuer` off its output. Those two strings are what a verifier has to match.

Add `--sign` to [`ocx package push`][cmd-package-push] to sign each platform manifest in the same step:

```sh
ocx package push --sign -p linux/amd64 -i registry.example.com/acme/mytool:1.0.0 mytool.tar.xz
```

Outside CI, with no token in the environment, `sign` opens a browser for an OAuth sign-in. Pass `--no-tty` on a headless machine so the command fails instead of waiting.

### Where the identity token comes from {#identity-token}

OCX takes the OIDC token from the first source that has one:

1. `--identity-token-file`, a file with mode `0600` or tighter.
2. `--identity-token-stdin`.
3. [`OCX_IDENTITY_TOKEN`][env-identity-token].
4. Ambient CI detection for GitHub Actions, GitLab CI and CircleCI.
5. An interactive browser sign-in, unless `--no-tty` is set.

No flag takes the token itself, because a token on the command line lands in shell history and process listings.

## Sign from CI {#ci}

CI is the case keyless signing exists for. The runner already holds an OIDC identity, so there is no key to store and none to rotate. OCX finds the identity on its own, so the signing step is one line with no credential wiring.

The identity is issued to the pipeline, not chosen by it. Run the step once and copy the two certificate strings from the output into your verifier.

### GitHub Actions {#ci-github}

The job needs the `id-token: write` permission. That permission is what provisions the token OCX detects.

```yaml
permissions:
  contents: read
  id-token: write

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - run: ocx package push -p linux/amd64 -i registry.example.com/acme/mytool:1.0.0 mytool.tar.xz
      - run: ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

The push step needs registry credentials for the target. Set the `OCX_AUTH_<REGISTRY>_*` variables as [Authenticate with a private registry][guides-authenticate] describes. Signing itself needs only the identity token.

OCX detects GitHub Actions through `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, which [`id-token: write`][gha-oidc] sets.

### GitLab CI/CD {#ci-gitlab}

Declare an `id_tokens` entry named `SIGSTORE_ID_TOKEN`. The name matters, because that is the variable OCX reads.

```yaml
publish:
  id_tokens:
    SIGSTORE_ID_TOKEN:
      aud: sigstore
  script:
    - ocx package push -p linux/amd64 -i registry.example.com/acme/mytool:1.0.0 mytool.tar.xz
    - ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

The push step needs registry credentials here too. Set the `OCX_AUTH_<REGISTRY>_*` variables as [Authenticate with a private registry][guides-authenticate] describes.

GitLab hands the token over through [`id_tokens`][gitlab-id-tokens]. CircleCI is detected through `CIRCLE_OIDC_TOKEN_V2`. Any other provider passes the token explicitly with `OCX_IDENTITY_TOKEN` or `--identity-token-file`.

### Credentials from the environment {#ci-credentials}

Three variables carry signing credentials. OCX reads each one directly and strips it from the environment of every subprocess it spawns, plugins (`ocx-<name>`) included. None is forwarded to a child `ocx`.

[`OCX_IDENTITY_TOKEN`][env-identity-token] is the OIDC token for keyless `sign` and `attest`. Both commands share one token resolver. The value must be a short-lived JWT from a supported provider. OCX consumes it once and never logs it or writes it to disk. Tokens typically expire in under ten minutes, so fetch a fresh one just before signing.

[`OCX_SIGNING_KEY`][env-ocx-signing-key] holds the private key itself for `--key env://OCX_SIGNING_KEY`. It carries the PEM, never a path. Use it on a runner with no writable disk. An unset or empty value exits 74 and names the variable. A value over 64 KiB exits 65.

[`OCX_KEY_PASSWORD`][env-ocx-key-password] opens an encrypted private key. It is never a flag, because a password in `argv` is visible to every process on the host. An unset variable reads as the empty password.

:::caution[Any other variable name is inherited]
`env://` accepts any variable name, and only `OCX_SIGNING_KEY` is on the credential list. A key held in `MY_KEY` reaches every plugin and generated launcher that OCX starts. Use `OCX_SIGNING_KEY` unless you have a specific reason not to.
:::

## Sign every tag of a release {#sign-tags}

A multi-platform tag is an image index, and an index digest is final only after the last platform has been pushed. Sign the finished index with a tag sweep.

Record the tags while you push, then sign what each tag resolves to:

```sh
ocx package push --cascade --tags-file tags.txt -p linux/amd64 -i registry.example.com/acme/mytool:1.0.0 mytool-amd64.tar.xz
ocx package push --cascade --tags-file tags.txt -p linux/arm64 -i registry.example.com/acme/mytool:1.0.0 mytool-arm64.tar.xz
ocx package sign --tags-file tags.txt registry.example.com/acme/mytool
```

`--tags` takes the same list inline. The sweep refuses `--platform`. A tag that resolves to a single manifest is skipped with a warning. The sweep continues past a failed tag and exits non-zero at the end with every failure listed.

A signature belongs to a digest, not a tag. The cascade aliases of one release share one signature, and later tags that name the same index are reported as `covered`.

## Sign with a key pair {#sign-with-a-key}

Reach for a key when no OIDC step exists to detect, or when you want signatures independent of any identity provider. `--key` takes a path, a `file://` URL or an `env://VAR` reference:

```sh
export OCX_SIGNING_KEY="$(cat cosign.key)"
export OCX_KEY_PASSWORD='<password>'
ocx package sign --key env://OCX_SIGNING_KEY -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

OCX does not generate key pairs. Create `cosign.key` and `cosign.pub` with [`cosign generate-key-pair`][cosign].

Keyless signatures always go to Rekor, and `--no-rekor-upload` without `--key` is a usage error. A key signature skips Rekor unless you pass `--rekor-upload`. Consumers verify a key signature with `ocx package verify --key <public key>`. [Signing in depth][in-depth-signing] explains the choice.

## Check the signature {#verify-the-signature}

Verify the manifest you just signed against the identity from the signing output:

```sh
ocx package verify \
  -p linux/amd64 \
  --certificate-identity https://github.com/acme/mytool/.github/workflows/release.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  registry.example.com/acme/mytool:1.0.0
```

A match exits 0 and prints a `Field` and `Value` table. The table holds the subject digest, the referrer digest, the certificate identity and issuer, and the signing time. An identity or issuer that differs from the one you passed exits 77. The [exit codes reference][exit-codes] lists every code.

Consumers should not have to pass those flags on every install. Tell them to [pin your signer identity][guide-pin-signer], and every `ocx package install` then checks it. To check installs by hand, see [Verify what you install][guide-verify-installs]. A private Sigstore stack needs its own trust root, covered in [Self-hosted Sigstore][guide-self-hosted-sigstore].

## Attach an SBOM or provenance {#sbom-and-provenance}

A signature says who published the package. An SBOM says what is inside it. Attach one with [`ocx package push --sbom`][cmd-package-push] or [`ocx package attest`][cmd-package-attest]. [Attest and SBOM][publishing-attest-and-sbom] walks through both. [Publisher CI][integration-publisher-ci] wires signing, SBOM and provenance into one release pipeline.

## See also {#see-also}

- [`ocx package sign` reference][cmd-package-sign] for every flag and exit code.
- [`ocx package verify` reference][cmd-package-verify] for the checks verify runs.
- [Signing in depth][in-depth-signing] for the trust root, identity matching and the cosign interop.

[sigstore]: https://www.sigstore.dev/
[guides-authenticate]: ../guides/authenticate.mdx
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[cosign]: https://github.com/sigstore/cosign
[gha-oidc]: https://docs.github.com/en/actions/deployment/security-hardening-your-deployments/about-security-hardening-with-openid-connect
[gitlab-id-tokens]: https://docs.gitlab.com/ee/ci/yaml/#id_tokens

[cmd-package-sign]: ../reference/command-line/package-publishing.md#package-sign
[cmd-package-verify]: ../reference/command-line/package-consuming.md#package-verify
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push
[cmd-package-attest]: ../reference/command-line/package-publishing.md#package-attest
[env-identity-token]: ../reference/environment.md#ocx-identity-token
[env-ocx-signing-key]: ../reference/environment.md#ocx-signing-key
[env-ocx-key-password]: ../reference/environment.md#ocx-key-password

[in-depth-signing]: ../explanation/signing.mdx
[publishing-build-and-push]: ./build-and-push.mdx
[publishing-attest-and-sbom]: ./attest-and-sbom.mdx
[integration-publisher-ci]: ../integration/publisher-ci.md
[guide-pin-signer]: ../guides/pin-signer-identity.mdx
[guide-verify-installs]: ../guides/verify-installs.mdx
[guide-self-hosted-sigstore]: ../guides/self-hosted-sigstore.md
[exit-codes]: ../reference/exit-codes.md
