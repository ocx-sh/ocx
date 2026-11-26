# Research: SBOM generator contracts

## Metadata

**Date:** 2026-11-25
**Domain:** packaging
**Triggered by:** `.agents/discussions/signing-dogfood-mirror.md`, the contract shape for SBOM generation in release mirroring (a named tool as a first-class variant, a free-form command hook, a typed generator with a default, or a built-in minimal generator only), and what a content scanner adds for Python wheels beyond a lock-derived list
**Expires:** 2027-05-25

Returned by one researcher (sonnet). The prompt named the four options and asked for evidence on each, with no ranking. It builds on `.claude/artifacts/research_signing_dogfood_prior_art.md` and does not repeat it. The fetch tool summarises pages through a small model, so "the page does not say" means not found on that page.

## Direct Answer

All three non-trivial shapes exist in shipped tools. GoReleaser has a generic command whose default is syft and leaves installing it to the user. BuildKit has a typed protocol (an image reference in, SPDX JSON at a fixed path out) whose default is a syft scanner. dist, JReleaser and `anchore/sbom-action` name the tool. For Python, no source shows syft detecting the shared libraries bundled in a wheel, and no tool is confirmed to read `.dist-info/sboms/`; the embedded documents themselves are now written by maturin and by auditwheel from 6.5.0.

## Findings

### Configuration surface per tool

- **GoReleaser `sboms`**: a generic command with a named default. Fields `id`, `artifacts` (default `archive`), `ids`, `documents`, `cmd` (default `syft`), `args`, `env`, `disable` (since v2.10). Default args are `["$artifact","--output","spdx-json=$document","--enrich","all"]`. The format is declared inside `args`, not as a field, and the `documents` filename is separate, so the two can disagree; the docs' own example pairs `cyclonedx-json` with a `.spdx.json` name. The page does not say who installs syft. <https://goreleaser.com/customization/sbom/>
- **Docker BuildKit**: a pluggable protocol with a named default. `--attest type=sbom` or `--sbom=true`, plus `generator=<image>`; the default is `docker/buildkit-syft-scanner`. The scanner runs with the build root filesystem mounted read-only, takes `BUILDKIT_SCAN_SOURCE` and `BUILDKIT_SCAN_DESTINATION` (and optionally `BUILDKIT_SCAN_SOURCE_EXTRAS`), and writes `$BUILDKIT_SCAN_DESTINATION/<scan>.spdx.json`. "Only SPDX JSON is currently supported." The result is attached as an in-toto attestation with predicate `https://spdx.dev/Document`. Neither page covers pinning the generator image, and the only listed implementation is the syft scanner. <https://docs.docker.com/build/metadata/attestations/sbom/>, <https://github.com/moby/buildkit/blob/master/docs/attestations/sbom-protocol.md>
- **dist (cargo-dist)**: named tools as boolean toggles, `cargo-auditable = true` and `cargo-cyclonedx = true`, both since 0.26.0, default `false`. No format or version field. <https://axodotdev.github.io/cargo-dist/book/reference/config.html>
- **JReleaser**: named tools as separate blocks. `catalog.sbom.syft` has `active` (default `NEVER`), `version` (default `1.42.1`), `formats` (default `SPDX_JSON`; also `CYCLONEDX_JSON`, `SYFT_JSON`), `distributions`, `files`, `pack`. A separate `cyclonedx` block exists. The page does not say how JReleaser obtains the syft binary. <https://jreleaser.org/guide/latest/reference/catalog/sbom/syft.html>
- **ko**: built in. Its `--sbom` page returned 404. The GoReleaser deprecations page says ko dropped upstream `cyclonedx` and `go.version-m` support, leaving `spdx` or `none`, since ko v2.2. <https://goreleaser.com/deprecations/>
- **anchore/sbom-action**: a named tool. Inputs `syft-version` (no default listed), `format` (`spdx-json` default), `output-file`, `path`, `image`, `upload-artifact`. <https://github.com/anchore/sbom-action>

### Breakage and maintenance cost

- JReleaser pins syft at a default of 1.42.1 inside its own configuration, so a syft bump is a JReleaser release.
- An opentelemetry-collector-releases issue reports GoReleaser-generated SBOMs listing only the archive for some darwin tarballs; the thread blames the syft version (empty on 1.1.0, fine on 1.1.1). Reported 2024-02-28, closed as inactive 2025-10-22, no confirmed fix shown. Older than 18 months. <https://github.com/open-telemetry/opentelemetry-collector-releases/issues/488>
- ko removed two SBOM formats (above).
- No documented harm from a generic hook was found; silence is not evidence of absence. The mismatched format and filename in GoReleaser's own example is the nearest case.
- No source documents a syft flag rename breaking GoReleaser.

### Python

- Syft `PythonPackage` fields: `name`, `version`, `author`, `authorEmail`, `platform`, `files`, `sitePackagesRootPath`, `topLevelPackages`, `directUrlOrigin`, `requiresPython`, `requiresDist`, `providesExtra`. `python-installed-package-cataloger` reads site-packages; `python-package-cataloger` reads requirements.txt, setup.py and Pipfile on directory scans only. <https://oss.anchore.com/docs/reference/syft/json/16/>, <https://oss.anchore.com/docs/guides/sbom/catalogers/> (schema page read to character 100000 of 118125)
- Syft has lock types for PDM, Pipfile, Poetry and uv; the page shows no PEP 751 type.
- Shared libraries bundled in wheels: Syft and OSV-Scanner reportedly did not detect them in a virtual environment (Alpha-Omega paper, August 2025; search summary only, the PDF could not be read). <https://alpha-omega.dev/wp-content/uploads/sites/22/2025/08/Python-White-Paper-for-AO-3.pdf> bocpy says libraries bundled by auditwheel, delocate or delvewheel are not enumerated in the components list (search summary only). <https://microsoft.github.io/bocpy/sphinx/sbom.html> No syft cataloger is documented for `.libs/` directories.
- Reading `.dist-info/sboms/` when generating: no tool confirmed. Seth Larson says Syft can "use embedded SBOMs inside of wheels", but the thread never names the path. <https://discuss.python.org/t/sboms-for-python-packages-project/70261>
- PEP 770 adoption: maturin writes `auditwheel.cdx.json` (CycloneDX 1.4, OS packages found through dpkg, rpm or apk) and `<crate>.cyclonedx.json` (CycloneDX 1.5) into `.dist-info/sboms/`. <https://www.maturin.rs/sbom> Upstream auditwheel 6.5.0 (2025-11-02) added SBOM generation for repaired libraries; 6.8.2 (2026-08-30) made component order deterministic; the changelog never names PEP 770 or the path. <https://github.com/pypa/auditwheel/blob/main/CHANGELOG.md> Hatchling support is claimed by a third-party plugin only; nothing was found for setuptools, meson-python or delocate. The Pillow wheel's embedded SBOM uses `pkg:rpm/almalinux/...` package URLs for vendored libraries.

### What a PEP 751 `pylock.toml` records per package

- `name` (required), `version`, `marker`, `requires-python`, `index`, `dependencies` (informational), `attestation-identities`, `tool`.
- Exactly one source form: `wheels[]`, `sdist`, `archive`, `vcs` or `directory`. Wheel, sdist and archive entries carry `name`, `url` or `path`, `upload-time`, `size` and `hashes`.
- Not recorded: the contents of a wheel, bundled libraries, licences, authors, `Requires-Dist`, file lists. <https://packaging.python.org/en/latest/specifications/pylock-toml/>

### Comparisons

- arXiv 2409.06390 (2024, older than 18 months, source-level Python projects): cdxgen best on vulnerability assessment (Jaccard about 50 percent, Syft about 26, Trivy about 24). <https://arxiv.org/pdf/2409.06390>
- arXiv 2606.02442 (v3 2026-09-29): cdxgen 12.1.5, Syft v1.42.4, Trivy v0.69.3, ORT, sbom-tool. Syft and Trivy failed all Python tests except pinned versions, and no tool detected the native libc or C test components. Inputs are small custom packages, not wheels or prebuilt binaries; second half unread. <https://arxiv.org/html/2606.02442>
- No benchmark on wheels with bundled libraries or on prebuilt release binaries was found.

negative: the ko SBOM page returned 404, so ko's configuration surface is unread.
negative: the JReleaser sbom index and CycloneDX pages were not fetched; only the syft page was read.
negative: no page covers pinning of BuildKit generator images or the default syft version of `anchore/sbom-action`.
negative: no upstream auditwheel changelog entry names PEP 770; no delocate, setuptools or meson-python support was found.
negative: the Alpha-Omega and bocpy claims rest on search summaries only.

leads: run `syft` on a manylinux wheel with a `.libs/` directory — measured behaviour where the docs are silent.
leads: read BuildKit `frontend/attestations/sbom/sbom.go` — what the generator default is and whether it is pinned by digest.
leads: read the PyPA binary-distribution specification for the `sboms` entry — the canonical text that replaced the PEP.
leads: read GoReleaser's changelog for the history of the syft default args — the actual breakage record.
leads: check cdxgen and Trivy for `.dist-info/sboms` handling — neither was checked.
