# Research: OCI layout on-disk conventions (ORAS and peers)

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (owner-requested lane: "what ORAS published for an OCI layout on disk")
**Expires:** 2027-05-08
**Caveat:** the `oras cp` code finding came from a summarizing fetch of `cp.go`; verify before relying on it.

## Direct Answer

ORAS has published no on-disk layout proposal or spec; the only primary sources are oras-go code and issues, and [oras-go#711](https://github.com/oras-project/oras-go/issues/711) (follow image-spec PR #1171) closed "not planned" 2024-11-15. The OCI image-spec layout is the standard; it permits partial layouts in one sentence but defines nothing more, and tools disagree on how referrers live in a layout.

## Image-spec layout ([image-layout.md](https://raw.githubusercontent.com/opencontainers/image-spec/main/image-layout.md))

- Required: `oci-layout` (`imageLayoutVersion`, extra fields allowed), `index.json` (image index, entry point), `blobs/` (may be empty). Blobs at `blobs/<alg>/<encoded>`; unreferenced blobs allowed.
- Partial: referenced blobs "may be absent" and "should be supplied by an external blob store". Nothing marks intentional absence.
- Tags: `org.opencontainers.image.ref.name` meaningful only on `index.json` descriptors; no grammar beyond examples.
- Multiple images: several refs in `index.json`; a ref may point to a nested index (multi-platform).
- Archive: layout may be tar or zip; no tar-specific rules. Unknown extra files allowed and must not cause failure.

## ORAS (oras-go `content/oci`)

- `Tag` requires the blob to exist, loose ref validation, atomic `index.json` write. `Resolve` checks tags then untagged blobs; `Exists`/`Fetch` ignore the index. ([oci.go](https://raw.githubusercontent.com/oras-project/oras-go/main/content/oci/oci.go))
- Referrers: stored as untagged manifests in `index.json`; `Predecessors` from an in-memory graph rebuilt from `subject` fields — discovery reads every manifest.
- GC: `AutoGC` default on; `Delete` removes dangling blobs and referrers, tagged manifests protected; `GC` drops unreachable blobs older than 1 h, no multi-writer coordination; untagged manifest survives only if its subject chain reaches a tagged one.
- Partial: `Tag` fails `ErrNotFound` on an absent blob; `gcReachable` stops silently at a missing manifest.
- CLI: `oras cp --to-oci-layout` / `--from-oci-layout`; `-r` (referrers) Preview; `--force` deep-traverses partially populated destinations. ([oras cp](https://oras.land/docs/commands/oras_cp))

## Referrers in a layout — tools disagree ([PR #1171](https://github.com/opencontainers/image-spec/pull/1171), draft, NACKed 2025-11-12)

| Tool | Method |
|---|---|
| oras, oras-go, containerd | untagged referrer manifests in `index.json` |
| regclient | fallback tag `sha256-…` holding a referrers index |
| olareg | project-specific annotation |
| BuildKit | referrers inside the multi-platform index under an "unknown" platform |
| umoci | no conversion |

Reviewer suggested reusing `vnd.docker.reference.digest` or a `ref.name` fallback tag rather than a new annotation. The PR author: a subject artifact pushed by one tool is "in almost every case not discoverable by other tools".

## Peers

- go-containerregistry `layout` — EXPERIMENTAL; `AppendImage`/`AppendIndex` with annotations/platform; `RemoveBlob` leaves dangling references. ([pkg.go.dev](https://pkg.go.dev/github.com/google/go-containerregistry/pkg/v1/layout))
- containers/image `oci:` — reference matches `ref.name`; `@N` selects untagged by position; no reference → exactly one image; `oci-archive:` = tarred layout; `dir:` non-standard, signatures as files; `--dest-shared-blob-dir`. ([containers-transports](https://raw.githubusercontent.com/containers/image/main/docs/containers-transports.5.md))
- regclient `ocidir://` — copies `--referrers` and `--digest-tags`; `--force-recursive` repairs missing nested content. ([regctl image copy](https://regclient.org/cli/regctl/image/copy/))
- zot — storage is OCI image layout with optional hard-link dedupe (search hits only; directory structure unverified).

## Platform subsetting

- `oras cp`, one `--platform`: tag points at the child manifest, index not copied. Two or more: all kept → original index unchanged; one kept → single-platform path; otherwise a new smaller index with a new digest, source-index referrers not copied (warned); attestations kept via `vnd.docker.reference.digest`; a missing requested platform is an error.
- `skopeo copy --multi-arch <list>`: sparse lists "usually fail" unless the destination supports them; `--strip-removed-platforms` requires removing signatures; `--preserve-digests` fails when impossible.
- `regctl --platform`, crane: index rewriting unconfirmed.

## Signatures

- Cosign tag scheme `<repo>:sha256-<digest>.sig` ([sigstore](https://docs.sigstore.dev/cosign/signing/other_types/)) is just another `ref.name` in a layout — dropped by a copy keeping only the subject tag (regctl `--digest-tags` exists for this).
- The referrers API response is never copied as such; oras-go derives it from `subject`.

## negative

- No ORAS-authored layout design doc; oras-go#711 closed not planned.
- regctl `--platform`, crane/skopeo with `oci:` destinations — need a hands-on test.
- zot hard link vs symlink dedupe conflicting.

## leads

- Empirical diff: copy a signed multi-platform package into a layout with oras, regctl, skopeo; compare `index.json` and kept referrers.
- Sparse layout acceptance by oras-go `Tag`/`Resolve` and zot.
- olareg / BuildKit `vnd.docker.reference.digest` before inventing a marker.
