# Research: where command-line tools keep the renderers of their output forms

## Metadata

**Date:** 2026-11-25
**Lane:** prior-art web scan (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ where a row says so: a subagent fetched the page. MEMORY where a row says so: four searches returned snippets only, and some fetches failed. Nothing was run. The orchestrator did not re-read the pages.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Not verified: fpm, Tauri, conda-pack, cargo-dist, and how Briefcase plugs in a backend. No page stated a version promise for a contract between a core and a renderer, except `cargo metadata`. The brief named three shapes and no leaning.

## The question

Some tools produce one intermediate artifact (a directory, a resolution, or a machine-readable document) and derive several output forms from it: installers, operating-system packages, shell scripts, build-system files, container images. Where do the output forms live, and how are they spelled?

- Shape A: subcommands of the main binary.
- Shape B: one command with an option that selects the output type.
- Shape C: separate executables, either plugins dispatched as subcommands or independent tools that read a documented interface.

## Tools

| Tool | Shape | Spelling | Where the code lives | Interface between core and renderer | Stated reason | Basis |
|---|---|---|---|---|---|---|
| cargo metadata | source for C | `cargo metadata --format-version=1` | core writes JSON | a `version` field and a required flag; inside a version, fields and enum values may be added | none | READ, https://doc.rust-lang.org/cargo/commands/cargo-metadata.html |
| cargo-deb, cargo-generate-rpm | C | `cargo deb`, `cargo generate-rpm` | separate crates, found on `PATH` as `cargo-<name>` | each reads its own table in `Cargo.toml`; no versioned contract seen | none | READ, https://github.com/kornelski/cargo-deb |
| Poetry | C, plugin | `poetry export -f requirements.txt` | `poetry-plugin-export`, own repository and releases | none documented | export "was never considered part of the core functionality"; the move was to "accelerate development of the main Poetry project" | READ, https://python-poetry.org/blog/announcing-poetry-1.2.0/ |
| uv | B | `uv export --format requirements.txt\|pylock.toml\|cyclonedx1.5` | core | only CycloneDX is marked unstable | none | READ, https://docs.astral.sh/uv/concepts/projects/export/ |
| conda constructor | C | `constructor <dir>` with `construct.yaml`; writes sh, pkg, exe, msi | separate project | its own YAML schema | none | READ, https://github.com/conda/constructor |
| mise | A | `mise generate config\|devcontainer\|git-pre-commit\|github-action\|install-script\|task-docs\|task-stubs\|tool-stub` | core | none needed | none | READ, https://mise.jdx.dev/cli/generate.html |
| docker buildx | B | `--output type=image\|registry\|local\|tar\|oci\|docker\|cacheonly` | buildx | not covered | "Each exporter type suits different use cases" | READ, https://docs.docker.com/build/exporters/ |
| Docker Compose | C, plugin | `docker compose` | a Docker CLI plugin | shared configuration (MEMORY) | see Moves | READ, https://www.docker.com/blog/announcing-compose-v2-general-availability/ |
| Nix | C | `nix bundle [-B bundler]`; a bundler is a function from a derivation to a derivation | flakes of third parties | a plain Nix function; the command is "experimental and its interface is subject to change" | none | READ, https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-bundle |
| GoReleaser | A, as configuration sections | sections for nfpm, dmg, pkg, msi, nsis, snap, flatpak, chocolatey, docker, and publishers for homebrew, scoop, winget | one binary (MEMORY) | one configuration schema | none | READ, https://goreleaser.com/customization/ |
| nfpm | C | a YAML file, then deb, rpm, apk, ipk, arch, msi, msix | own project | its own YAML schema | "No Ruby, no tar, no external dependencies" | READ, https://nfpm.goreleaser.com/ |
| pixi-pack | C | `pixi-pack --environment prod --platform linux-64 pixi.toml`, and `pixi-unpack` | separate binaries | reads `pixi.lock` and `pixi.toml`; no version promise stated | the unpack side must run without pixi | READ, https://pixi.prefix.dev/latest/deployment/pixi_pack/ |
| git | C | `git foo` runs `git-foo` | separate executables | the file name and the arguments | none | READ, https://git-scm.com/docs/git |
| kubectl | C | `kubectl-foo-bar` runs as `kubectl foo bar` | separate executables | none versioned; a plugin cannot override a built-in | none | READ, https://kubernetes.io/docs/tasks/extend-kubectl/kubectl-plugins/ |
| Homebrew | A for bundle, C for external commands | `brew bundle …`; `brew-<name>` on `PATH` | bundle is in the core | none needed | none | READ, https://docs.brew.sh/External-Commands |
| jpackage | B | `--type app-image\|exe\|msi\|rpm\|deb\|pkg\|dmg`; `--app-image <dir>` takes a prepared directory as input | the JDK | the intermediate is a directory | each type must be built on its own platform | READ, https://docs.oracle.com/en/java/javase/21/docs/specs/man/jpackage.html |
| CPack | B | `cpack -G <generators>` | CMake | a configuration file that CMake writes | none | READ, https://cmake.org/cmake/help/latest/manual/cpack.1.html |
| Briefcase | A and B | `briefcase create`, `build`, `package <platform> [-p format]` | core | none needed | formats are "platform dependent" | READ, https://briefcase.beeware.org/en/stable/reference/commands/package.html |
| rules_pkg | A, as rules | rules for tar, zip, deb, rpm | a separate ruleset | providers, not confirmed | Bazel's built-in `pkg_tar` is frozen; new features go to rules_pkg | READ, https://github.com/bazelbuild/rules_pkg |

## Evidence by shape

**A, subcommands of the main binary.**

- For: mise lists eight generators in one help tree, so a user finds them.
- For: GoReleaser configures many package types in one file with one schema.
- For: Briefcase splits one lifecycle into `create`, `build` and `package`.
- Against: only the core team adds a renderer. Poetry judged export not to be core.
- Against: every format ships in the core binary and in its release. GoReleaser lists more than 15 package types and about 25 publishers.

**B, one command with a type option.**

- For: uv has one verb and a list of formats.
- For: jpackage takes a prepared directory as input and selects the installer type with one option.
- For: buildx selects among seven exporters with one option.
- Against: uv marks one format unstable, so stable and unstable formats share a flag.
- Against: jpackage types are tied to the host platform, one type per run.

**C, separate executables or plugins.**

- For: anyone can write a `cargo-*`, `git-*` or `kubectl-*` program. The contract is the file name and the arguments.
- For: `cargo metadata` carries a version field and allows additions inside a version.
- For: Poetry's plugin has its own tracker and releases. nfpm needs no other tool.
- For: the unpack side of pixi-pack runs without pixi.
- Against: git and kubectl specify nothing beyond the file name and the arguments. cargo-deb and cargo-generate-rpm each read a table of their own.
- Against: a kubectl plugin cannot override a built-in command.
- Against: cargo-generate-rpm needs a build to have run first.
- Against: Nix bundlers sit behind an experimental flag.

## Moves between shapes

- Poetry moved `export` from the core to a plugin in 1.2, with the reasons quoted above. Poetry 2 declares the plugin as a requirement. READ.
- Docker Compose moved from a standalone Python program to a plugin of the Docker CLI. Reasons given: shared Go code with the CLI, the same behaviour for `run` and `exec`, static binaries, speed, and updates that ship with the CLI. READ.
- Homebrew folded its bundle tap into the core. No reason was found. MEMORY, from a search snippet.
- Bazel froze its built-in `pkg_tar`; new capabilities go to rules_pkg. READ.
- Not found: a move of a renderer into a core for which the stated reason was the machine interface.

## What this shows

- All three shapes are in use by tools of the same kind.
- One versioned document between a core and outside renderers was found: `cargo metadata`.
- Where a reason for a move out of a core is given, it is that the renderer is not core work and slows the core's releases.
- A prepared directory as the input of an installer step has a precedent in jpackage.

## What this does not show

- Any measurement of binary size or release cost.
- A stated stability promise for a renderer's input in any tool but cargo.
- fpm, Tauri, conda-pack and cargo-dist: their pages were not read.
