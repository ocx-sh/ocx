# Research: What a distro/OS package renderer needs from an exported package

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (distro-packaging lane; apt/Chocolatey renderers deferred, interface-compatibility constraint only)
**Expires:** 2027-05-08
**Gaps:** FHS `/opt` page blocked; Chocolatey signing page 404; no primary RPM docs; Debian profile.d/alternatives conventions without primary source.

## Direct Answer

The union a renderer needs: name, version, platform (os/arch), description, homepage, license (SPDX), maintainer/publisher, file tree with modes, entrypoint list (relative binary paths, optional alias), env entries (set, path-prepend as relative dir), dependencies `{name, version-range}`, content digest. Maintainer/publisher and license are not in the OCX model today. URL-based targets (Scoop, winget, Homebrew, Chocolatey community) want a stable HTTP URL + SHA-256 rather than embedded content. nfpm renders deb/rpm/apk/archlinux/ipk/msix from one YAML.

## Per target

- **Debian .deb** — mandatory `Package`, `Version`, `Architecture`, `Maintainer`, `Description` ([Policy ch.5](https://www.debian.org/doc/debian-policy/ch-controlfields.html)). Version `[epoch:]upstream[-revision]`; semver `-rc.1` → `~rc.1`; `+build` has no meaning; GitHub turns `~` into `.` ([nfpm](https://nfpm.goreleaser.com/docs/configuration/), [GoReleaser nfpm](https://goreleaser.com/customization/nfpm/)). One arch name per package: amd64, arm64, armhf, riscv64, ppc64el, s390x ([wiki](https://wiki.debian.org/SupportedArchitectures)); OCI `arm/v6` and `arm/v7` both → `armhf`. No two packages may ship the same `/usr/bin` name ([Policy ch.10](https://www.debian.org/doc/debian-policy/ch-files.html)). No env-var field — profile.d conffile convention. Four maintainer scripts. Signing repo-level (`InRelease`/`Release.gpg`, [repo format](https://wiki.debian.org/DebianRepository/Format)).
- **RPM** (via nfpm) — same model plus `release`, `epoch`; different dependency syntax; package-level signing.
- **Alpine apk** (via nfpm) — RSA PEM signing; own arch override.
- **Chocolatey .nupkg** — nuspec `id`, `version`, `description`, `authors` ([nuspec](https://learn.microsoft.com/en-us/nuget/reference/nuspec)); community validator adds Title, Tags, ProjectUrl ([rules](https://docs.chocolatey.org/en-us/community-repository/moderation/package-validator/rules/)). NuGet SemVer 2, optional 4th segment for package fixes, `+build` dropped, pre-release case-insensitive ([versioning](https://learn.microsoft.com/en-us/nuget/concepts/package-versioning)). Auto-shims every `.exe` in the package dir (`.ignore`/`.gui` markers). Env via `Install-ChocolateyEnvironmentVariable` / `Install-ChocolateyPath` from generated `chocolateyInstall.ps1` ([docs](https://docs.chocolatey.org/en-us/create/functions/install-chocolateyenvironmentvariable/)). Embedding allowed with distribution rights; community repo mostly requires runtime download with checksums ([host-packages](https://docs.chocolatey.org/en-us/features/host-packages/)).
- **Scoop** — `version`, `homepage`, `license`; per-arch `url`/`hash`/`bin`; `env_add_path` (relative), `env_set`, `depends` ([wiki](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests)). URL-based — closest native fit to OCX's model.
- **winget** — `PackageIdentifier`, `PackageVersion`, `Publisher`, `PackageName`, `License`, `ShortDescription`, `Installers[]` with URL + SHA-256 ([manifest](https://learn.microsoft.com/en-us/windows/package-manager/package/manifest)); no env/PATH field on the fetched page.
- **Homebrew** — `homepage`, `desc`, `license`, `url`, `sha256`, `bin.install`, `depends_on`, per-OS/arch blocks ([Cookbook](https://docs.brew.sh/Formula-Cookbook)).

## Common minimal model (nfpm / GoReleaser)

nfpm requires `name`, `arch` (Go names, translated per packager), `version`, `maintainer`; `contents[]` (`src`, `dst`, `type`, `file_info`, `packager`), `scripts`, per-format `overrides`. GoReleaser chocolatey/scoop/winget add download URL template + checksums, publisher/owner, `project_url`, license, per-arch archives, `{id, minimum_version}` dependencies.

## negative

- Embed vs download: URL-based targets need a stable HTTP URL + SHA-256; an export carrying only OCI blobs cannot satisfy them alone.
- Env mapping inconsistent: deb/rpm none; Chocolatey/Scoop persistent; Scoop path relative, Chocolatey absolute — install-path placeholders need per-target substitution.
- Maintainer/publisher missing from the OCX model; Debian and nfpm (future) require it.
- Version gaps: `+build`, NuGet case-folding, `~` mangling, no pre-release in Chocolatey Id.
- Arch gaps: `arm/v6`/`arm/v7` collide on `armhf`; Scoop only 32bit/64bit/arm64; OCI variants (`amd64v3`) only via nfpm `deb.arch_variant`.
- Dependency names: OCX names won't match distro names — render the closure or map names (inference).
- Duplicate `/usr/bin` names across registries forbidden by Debian policy.

## leads

- nfpm as back end (Go library vs CLI).
- `update-alternatives(1)` and profile.d guidance.
- Chocolatey signing / `choco pack`.
- Scoop and winget zip/portable installers (`NestedInstallerFiles`, `PortableCommandAlias`).
- Sigstore/cosign attestations beside OS-native signatures.
