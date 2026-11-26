# Research: caching proxies in front of a TUF repository

## Metadata

**Date:** 2026-11-25
**Domain:** infrastructure
**Triggered by:** `.agents/discussions/signing-dogfood-mirror.md`, the owner's question how long a JFrog Artifactory Generic remote repository serves a cached file, asked for a mirror of the Sigstore TUF repository configured under `[mirrors]`
**Expires:** 2027-05-25

Returned by one researcher (sonnet). The prompt asked for evidence only, with no recommendation. Nothing here was measured against a running Artifactory or Nexus. Claims that rest on a search-result summary are marked.

## Direct Answer

A JFrog knowledge-base article says a Generic remote repository treats a cached file as immutable and does not fetch it again when the upstream file changes. Its remedies are to delete the cached file or to turn off "Store Artifacts Locally". The documented re-check timer, Metadata Retrieval Cache Period (default 7200 seconds), is described for metadata files of package types, and no page says it applies to Generic. The Sigstore repository uses consistent snapshots, so `timestamp.json` is the file whose name stays while its content changes; on 2026-11-25 it expired six days later. No account of mirroring that repository through Artifactory, Nexus or another caching proxy was found.

## Findings

### Artifactory remote repository settings

Settings table, <https://docs.jfrog.com/artifactory/docs/remote-repositories> (read; no product version or date shown):

- Metadata Retrieval Cache Period, in seconds. Default 7200, and 21600 for Docker, OCI and Helm OCI. Minimum 30. The examples are `maven-metadata.xml`, `package.json` and `manifest.json`; Generic is not mentioned.
- Missed Retrieval Cache Period, in seconds. Default 1800. It caches "not found" responses; 0 means a miss is never cached.
- Assumed Offline Period, in seconds. Default 300. It is the wait after a connection error before the remote is asked again.
- Unused Artifacts Cleanup Period. No unit or default on the page. Empty keeps cached artifacts forever.
- Store Artifacts Locally. No default stated. When off, files stream to the client uncached. It applies to every package type except Docker, Ansible, Helm OCI, OCI and Hugging Face, so it covers Generic.
- Synchronize Properties. It concerns artifacts fetched from another Artifactory instance.
- The same page: "Immutable binaries are not invalidated" by a cache zap. It does not say how an upstream change is detected.

JSON keys, from the Terraform provider schema at <https://pkg.go.dev/github.com/jfrog/terraform-provider-artifactory/v6@v6.7.1/pkg/artifactory/resource/repository/remote> (v6.7.1, a third-party source, probably older than 18 months): `retrievalCachePeriodSecs` (default 7200), `missedRetrievalCachePeriodSecs`, `assumedOfflinePeriodSecs` (default 300), `unusedArtifactsCleanupPeriodHours`, `storeArtifactsLocally`, `synchronizeProperties`.

The JFrog article on the metadata period, <https://jfrog.com/help/r/how-remote-repository-metadata-works/metadata-retrieval-cache-period> (read): seconds to cache metadata files before checking the remote for newer versions; 0 means no caching. Its examples are Maven, PyPI and YUM. It says a Generic remote works by the client making a direct GET for a package, and does not say that this setting governs Generic.

### Fixed-name files behind a Generic remote

- <https://jfrog.com/help/r/artifactory-why-isn-t-the-generic-remote-repository-cache-automatically-updated> (read; first published 2023-05-03, last modified 2025-07-20): Artifactory assumes artifacts are immutable, so a Generic remote does not fetch or cache a changed upstream file even when the source was modified.
- The same article gives two remedies: delete the cached file from the remote-cache repository and request it again, or disable "Store Artifacts Locally" so files stream from the upstream uncached.
- Search-result summaries only, pages not read: the cache-zap reference says zapping invalidates cached metadata artifacts; a knowledge-base article on APT staleness recommends a lower Metadata Retrieval Cache Period for an APT remote, which is not Generic.

### Sonatype Nexus raw proxy, for comparison

The Sonatype caching article returned 403, and the Nexus Repository 3 help pages tried returned 404. From search-result summaries only: Maximum metadata age defaults to 1440 minutes, with -1 for never and 0 for always; Maximum component age is cited as 1440 minutes for Docker proxies and the raw default is not confirmed; Not Found Cache TTL applies only with negative caching on.

### The Sigstore TUF repository

- <https://tuf-repo-cdn.sigstore.dev/timestamp.json>, fetched 2026-11-25: version 843, `expires` 2026-12-01T07:21:08Z. It names `snapshot.json` at version 166.
- `snapshot.json` returned 404; `166.snapshot.json` returned version 166, `expires` 2036-10-04T08:51:19Z.
- `14.root.json`: `consistent_snapshot` true. It is an older root and has expired; the current root was not fetched.
- <https://github.com/sigstore/root-signing> (read through a summary): "online signing happens at least every three days because of online signature expiry". It states no expiry durations.
- The expiry values are single observations.

### Mirroring through a caching proxy

- `cosign initialize --mirror` takes a GCS bucket, an HTTP(S) base address or a `file:///` path (search summary of <https://github.com/sigstore/cosign/blob/main/doc/cosign_initialize.md>).
- A custom TUF repository needs its initial `root.json` plus a mirror address (search summary of a Sigstore blog post).
- Red Hat Trusted Artifact Signer's offline guides use a `file://` mirror and a `1.root.json` (search summary).
- One blog post, read as a search summary only, reports that with consistent snapshots the client puts the hash into target paths, so a mirror that rewrites or caches paths has to keep that layout.

negative: no JFrog page states whether Metadata Retrieval Cache Period applies to Generic remotes.
negative: no JFrog page names HEAD, Last-Modified, ETag or checksum headers for re-validating a cached Generic file.
negative: no account of mirroring `tuf-repo-cdn.sigstore.dev` through Artifactory, Nexus or another caching proxy, and no reported problems apart from the single blog post.
negative: no primary Nexus document was read for raw proxy defaults.
negative: the root-signing README states no timestamp or snapshot expiry durations; the current `root.json` was not fetched.

leads: run a Generic remote against the Sigstore address, repeat `timestamp.json` requests across an upstream change and record the requests sent upstream — a measurement where the documentation is silent.
leads: read the JFrog REST repository-configuration page for the Generic remote keys and defaults — the key table above rests on the Terraform schema.
leads: read the Nexus Repository 3 raw proxy help page directly — settles the raw defaults.
leads: check sigstore/root-signing for a policy document or workflow that gives the timestamp expiry in days.
