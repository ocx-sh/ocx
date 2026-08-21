---
outline: deep
---

# Promoting packages between environments {#promoting}

Your organization runs three registries: developers publish to `dev`, QA validates in
`staging`, and only what QA signed off reaches `prod`. The build that was tested is the
one that has to ship.

Without a way to move a published artifact, the only route to production is to build it
again — and a rebuild produces *different bytes*. Compression is not byte-stable, archive
timestamps move, and the platform manifest that names the result gets a new digest. That
digest is load-bearing twice over: a [Sigstore signature][ug-signing]'s subject **is** the
platform manifest digest, and every [`ocx.lock`][lock-format] entry pins it. So a rebuilt
production package silently arrives with the staging signature no longer applying to it
and every downstream lock pointing at an artifact that no longer exists in the pipeline —
all while the release job reports success.

[`ocx package copy`][cmd-package-copy] moves the bytes instead. The platform manifests and
their blobs are transferred verbatim, so the digest at production is the digest dev
published, and everything anchored to it — signatures, SBOMs, attestations — travels with
it.

:::info Analogy: promotion and patches
Both tiers keep an upstream artifact intact rather than forking it. [`[patches]`][ug-patches]
adapts what environment a tool *runs in* without touching the package; promotion moves a
package *between registries* without touching its bytes. Neither rebuilds anything.
:::

## How it works {#promoting-how}

A published package is three different kinds of object, and only one of them is content:

| Object | What a copy does with it |
|---|---|
| The platform manifest and its blobs | Transfers them byte for byte. The digest does not change — that is the point. |
| The tag's [image index][oci-image-index] | Merges one platform at a time. Promoting `linux/amd64` never removes a `darwin/arm64` the target already offers. |
| Rolling tags (`1.4`, `1`, `latest`) | Recomputes them against the **target**'s tag list, never carries the source's over. |

The last two are why a promotion is not `docker pull` + `docker push`. An image index is a
mutable set keyed by platform, so copying one wholesale would delete every platform the
target had and the source did not. And whether `1.4` should point at `1.4.2` depends on
what the *target* publishes: a staging registry that is one release ahead of production
has a different answer, so promoting `1.4.1` into production must not drag `1.4` backwards.

The copy is also written in two phases. Manifests, blobs and referrers land first — all
pure additions, invisible until a tag names them — and only then do the index merges and
the rolling tags move. An interruption partway leaves the target's tags exactly as they
were.

## Promoting a release {#promoting-walkthrough}

<Terminal src="/casts/user-guide/promoting-packages.cast" title="Promoting one build from dev to staging to production" collapsed />

`--to` rewrites only the registry host and keeps the repository path and the tag, which is
the shape a promotion almost always has:

```sh
# dev -> staging
ocx package copy --to staging.example.com --cascade acme/mytool:1.4.2

# staging -> prod, once QA signs off
ocx package copy --to prod.example.com --cascade staging.example.com/acme/mytool:1.4.2
```

Each run reports one row per platform, saying what happened to it:

| Result | Meaning |
|---|---|
| `added` | Production had no entry for this platform. |
| `unchanged` | Production already pointed at this exact digest. |
| `replaced` | Production pointed at a different digest for this platform. |
| `kept (not in source)` | Production offers this platform and the source does not, so the merge left it alone. |

That last row is worth reading rather than skimming. Promoting a subset of platforms into
a registry that offers more is a legitimate thing to do and also a common mistake, and the
row list is the only thing that tells the two apart.

Re-running a finished promotion is idempotent, not free: every row reads `unchanged`, but
each platform's leaf manifest and referrer set are still re-verified against the target
rather than trusted from the index entry, so a retry still re-fetches and re-PUTs the
manifest (and, with signatures attached, the referrer chain). Only blob content is skipped,
via a HEAD against the target. Pipelines can retry the step without special-casing it — see
[`ocx package copy`][cmd-package-copy] for the mechanism.

## What travels, and what does not {#promoting-scope}

**Signatures travel by default.** Everything anchored to a manifest through the
[OCI Referrers API][oci-referrers-spec] — Sigstore bundles, SBOMs, attestations — is copied
along with it, following referrer chains recursively so a signature over an SBOM arrives
too. Verify at the target exactly as you would at the source:

```sh
ocx package verify -p linux/amd64 prod.example.com/acme/mytool:1.4.2
```

This works only because the digest did not move. It is also the reason a target registry
without the Referrers API is refused with exit 84 rather than accepted: such a registry
takes a referrer manifest as an ordinary upload and then never lists it, so the provenance
would be lost silently. Pass `--no-referrers` to promote the package alone, deliberately.

**Descriptions do not travel by default.** The README, logo and catalog annotations on the
`__ocx.desc` tag are repository-level prose rather than part of the version being promoted,
and environments legitimately carry different ones — a staging catalog page that says "not
for production use" should not follow the package to production. Add `--description` to
copy it along, or promote it on its own once it is right:

```sh
ocx package describe --from staging.example.com/acme/mytool prod.example.com/acme/mytool
```

**A copy is not a re-sign.** The signature that travels still names the identity that
signed it in the source environment. If your policy requires a production-specific
attestation, sign again at the target — promotion preserves provenance, it does not
manufacture it.

## Promoting a single platform {#promoting-platform}

`--platform` filters a tag's platforms, so a release that ships four can be promoted one at
a time as each one clears validation:

```sh
ocx package copy --to prod.example.com --platform linux/amd64 staging.example.com/acme/mytool:1.4.2
```

Against a **digest** the same flag means something different: it *declares* the platform,
and exactly one is required. A platform manifest carries no platform of its own — OCX
records that in the index entry, never in the manifest — so there is nothing to read it
from, and guessing would file the package under a platform nobody built it for. A digest
source also needs `--identifier`, because a digest carries no tag for `--to` to preserve:

```sh
ocx package copy \
  --identifier prod.example.com/acme/mytool:1.4.2 \
  --platform linux/amd64 \
  staging.example.com/acme/mytool@sha256:<hex>
```

Naming an *image index* by digest is refused (exit 64). An index digest is a snapshot of a
mutable set, and there is no honest way to merge "the platform list as it was" into a
target that has moved on — name the tag instead.

## Checking before committing {#promoting-dry-run}

`--dry-run` reports the same per-platform rows and writes nothing, so a release job can
show the plan before it acts:

```sh
ocx package copy --to prod.example.com --dry-run staging.example.com/acme/mytool:1.4.2
```

The preview stops at the per-platform disposition, though. With `--cascade`, the rolling
tags that would move at the target are not computed under `--dry-run`, and neither is the
`sha256.<hex>` canonical tag that `--canonical-tag` (the default) would write — both are
decided in the second phase of a copy, which `--dry-run` never runs. A pipeline gating on
`--dry-run --format json` sees empty tag arrays regardless of `--cascade`; that is a
dry-run limitation, not a report that nothing would move.

## In depth {#promoting-in-depth}

- [`ocx package copy`][cmd-package-copy] — every flag, and the full exit-code table.
- [Signing and verification][ug-signing] — why the manifest digest is the signature's subject.
- [Locking][lock-format] — how `ocx.lock` pins a platform manifest digest.
- [Versioning and cascades][ug-versioning] — what the rolling tags mean and when they move.

<!-- commands -->
[cmd-package-copy]: ../reference/command-line.md#package-copy

<!-- in-depth -->
[lock-format]: ../in-depth/project.md#lock-format
[ug-signing]: ../in-depth/signing.md
[ug-versioning]: ../in-depth/versioning.md
[ug-patches]: ./patches.md

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[oci-referrers-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers
