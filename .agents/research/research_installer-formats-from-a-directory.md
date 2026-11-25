# Research: what installer formats need beyond a prepared directory of files

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, web (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ for Chocolatey: a subagent fetched the official documentation pages named in the table. MEMORY for every other format: nothing was fetched for them in this run. Nothing was run. The orchestrator did not re-read the pages.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. The reference page of the pack command returned 404 twice, so the options of `choco pack` and its offline behaviour are not verified. Also not verified: path-length limits, size limits, and how a shim forwards arguments and exit codes. The brief described the directory and named no design leaning.

## The question

The starting point is a prepared directory for one platform. It holds several programs with their files, one small native launcher per exposed program, and one machine-readable document that lists the files, the launchers and the environment variables. A launcher sets the environment for its process and then starts the real program. The directory works at any path. What does each installer format need beyond that directory?

## Chocolatey

Every row is READ unless it says MEMORY.

| Question | Finding | Source |
|---|---|---|
| Required contents | The nuspec is the only required element of a package. The page gives no full list of mandatory fields and points to the NuGet documentation. It names `id`, `title`, `description`, `releaseNotes`, `projectUrl`, `iconUrl` and `dependencies`. MEMORY (NuGet base): `id`, `version`, `authors` and `description` are mandatory. | https://docs.chocolatey.org/en-us/create/create-packages/ |
| Community repository extras | Required: a description of 30 to 4000 characters, a project URL, tags, a copyright of at least 4 characters, and no forced licence acceptance. Guidelines: icon URL, licence URL, package source URL, release notes, summary, title, versioned dependencies. The rules list a missing licence URL once as a requirement (CPMR0007) and once as a guideline (CPMR0039); the two entries were not reconciled. | https://docs.chocolatey.org/en-us/community-repository/moderation/package-validator/rules/ |
| `tools/` and scripts | `tools/` holds `chocolateyInstall.ps1`, `chocolateyUninstall.ps1` and an optional `chocolateyBeforeModify.ps1`. All scripts are optional. Binaries may be in the package or downloaded into the package folder. The page says to avoid a folder named `content`. | same page |
| Which files get a shim | Every `.exe` in the package folder, automatically, unless it is ignored. A file that is not an `.exe` gets none. `Install-BinFile` makes one by hand, and such a shim is not removed automatically. | https://docs.chocolatey.org/en-us/features/shim/ and https://docs.chocolatey.org/en-us/create/functions/install-binfile/ |
| Suppressing a shim, GUI marker | An empty `<exe>.ignore` file suppresses the shim. An empty `<exe>.gui` file marks a GUI program, whose shim exits right after the launch. | shim page |
| Where shims go | `$env:ChocolateyInstall\bin`, usually `C:\ProgramData\chocolatey\bin`. Chocolatey keeps that folder on `PATH`. | shim page |
| What a shim does | It runs the target at its original location. The working directory stays the caller's. It waits for a console program. The page says nothing on environment variables, argument forwarding or exit codes. MEMORY: a shim forwards arguments and returns the child's exit code. | shim page |
| Environment variables | `Install-ChocolateyEnvironmentVariable` sets a variable for the process, the user (default) or the machine. Machine scope needs administrator rights. User and machine scope persist. No mechanism for one program's environment was found other than a wrapper. This is an absence in the pages read, not a documented statement. | https://docs.chocolatey.org/en-us/create/functions/install-chocolateyenvironmentvariable/ |
| Embedded binaries | The community repository requires a licence file (CPMR0005) and a verification file (CPMR0006) when a package includes binaries. Embedding is called "the most reliable and deterministic" way, and the packager needs the right to distribute. No size limit was found on the pages read. MEMORY: a size cap exists. | https://docs.chocolatey.org/en-us/community-repository/moderation/package-validator/rules/cpmr0006/ and `…/cpmr0005/` |
| Private feed | The validator and moderation pages are written for the community repository only. No statement was found that a private feed is exempt. | https://docs.chocolatey.org/en-us/community-repository/moderation/package-verifier/ |
| Building locally | `choco pack` runs in the directory that holds the nuspec. A package is tested with `choco install <id> --source .`. MEMORY: pack reads the nuspec and the files beside it and needs no network. | create-packages page |
| Where files land | `%ChocolateyInstall%\lib\<package>`, per machine. The default root is `C:\ProgramData\chocolatey`, which only administrators can write. A non-administrator install needs a user-writable root and can set user variables only. Nothing was found on path length. | https://docs.chocolatey.org/en-us/information/security/ |
| One package or several | If a package for a required tool exists, depend on it. Split optional modules into packages of their own. The page gives no guidance on one package that bundles several tools. | create-packages page |

## Other formats

Every cell is MEMORY. Nothing was fetched for these formats.

| Format | Files or pointer | Minimum metadata | How a program reaches `PATH` | Environment variables | Build tool |
|---|---|---|---|---|---|
| Scoop | A JSON manifest with URL and hash; the archive is fetched | `version`, `description`, `homepage`, `license`, `url`, `hash` | `bin` entries become shims in `scoop\shims` | `env_set` and `env_add_path`, user scope | hand-written JSON |
| winget | A YAML manifest with installer URL and SHA256 | identifier, version, locale, publisher, name, licence, short description, installer type, URL, hash | the `portable` type creates a link; otherwise the installer decides | none | `wingetcreate` |
| Homebrew | A formula or cask is Ruby with a URL and a checksum; a bottle is a prebuilt archive | description, home page, URL, checksum, licence | symbolic links into the prefix's `bin` | build time only; run time needs a wrapper script | `brew create`, `brew bottle` |
| Debian `.deb` | Holds the files | `Package`, `Version`, `Architecture`, `Maintainer`, `Description` | files in `/usr/bin`, or a link the package creates | none; a wrapper is the usual way | `dpkg-deb --build`, `fpm`, `nfpm` |
| RPM | Holds the files | `Name`, `Version`, `Release`, `Summary`, `License`, a file list | files in `/usr/bin`, or links | none; a wrapper or a profile file | `rpmbuild`, `fpm`, `nfpm` |
| macOS `.pkg` | Holds the files | identifier, version, install location | nothing automatic; a post-install script links into `/usr/local/bin` | none; a wrapper | `pkgbuild`, `productbuild` |

## What this shows

- Chocolatey needs one file beyond the directory: the nuspec. Its scripts are optional.
- Every format read or recalled needs an identity (a name and a version) and a description. A directory of files carries neither.
- Chocolatey makes a shim for every `.exe` and the shim starts the target where it lies. So a launcher is started at its own location. An executable that must not reach `PATH` needs an `.ignore` file beside it.
- Chocolatey documents no environment for one program. A program that needs its own variables needs a wrapper.
- A Chocolatey package lands in a directory that only administrators can write by default.
- Formats differ in kind: Chocolatey, `.deb`, RPM and `.pkg` hold files; Scoop, winget and Homebrew point at an archive with a checksum (from memory).

## What this does not show

- Anything verified about a format other than Chocolatey.
- How large a package a feed accepts, and whether a deep directory hits a path-length limit on Windows.
- How a Chocolatey shim treats arguments, exit codes and the environment it inherits.
- Whether a private feed applies the community repository's rules.
- Any package that was built. Nothing was run.
