# Research: what tools call "write a self-contained directory" and "describe it without writing"

## Metadata

**Date:** 2026-11-25
**Lane:** prior-art web scan, plus a count over ocx's own help text (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ where a row says so: a subagent fetched the page. MEMORY where a row says so. The ocx counts were RUN by the orchestrator, with a script over `crates/ocx_schema/tests/golden/cli.json` on main at `f41c6684` (548 help strings). The orchestrator did not re-read the web pages. One RUN on 2026-11-25 with Bazel 9.2.0, described in the section "The manifest as a file in the output directory". The counts and help texts of 2026-11-26 were RUN on this host.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. The brief named candidate tools and no leaning. Not fetched: guix pack, `uv venv --relocatable`, the bare `spack view` command, kubectl, the AWS CLI, `docker manifest inspect`, `npm view`, `nix flake show`, `nix copy`, `conda create --prefix`. A count of a word in help text says how often ocx uses the word, not what it means there.

## The question

- Operation A: the command that writes a self-contained directory of resolved packages, usable without the tool that made it.
- Operation B: the command that prints a machine-readable description of what was resolved, or of what operation A would write, and writes no files.

## Operation A in other tools

| Spelling | Meaning, in the documentation's words | Basis | Note |
|---|---|---|---|
| `pnpm deploy` | copies one workspace package to a target directory and installs its dependencies there | READ, https://pnpm.io/cli/deploy | reuses the file selection of `pnpm pack` |
| `conan install --deployer=full_deploy` | deployers are "a mechanism to facilitate copying files from one folder, usually the Conan cache, to user folders" | READ, https://docs.conan.io/2/reference/extensions/deployers.html | the built-in deployers are "in preview" |
| `vcpkg export` (`--raw`, `--zip`, `--7zip`, `--nuget`) | "Exports built packages ... into a standalone developer SDK" that works "on another machine without separately acquiring vcpkg" | READ, https://learn.microsoft.com/en-us/vcpkg/commands/export | the IFW, Chocolatey and Prefab formats "have been removed" |
| `go mod vendor` | copies the packages needed to build and test into a `vendor` directory | READ, https://go.dev/ref/mod | sources for a build |
| `cargo vendor` | "Vendor all dependencies locally" | READ, https://doc.rust-lang.org/cargo/commands/cargo-vendor.html | sources for a build |
| `bazel vendor` | "a local copy of external dependencies", for offline builds | READ, https://bazel.build/external/vendor | rewrites symlinks so that the copy survives a move |
| `conda pack` | "creating archives of conda environments that can be installed on other systems and locations" | READ, https://conda.github.io/conda-pack/ | an archive; `conda-unpack` fixes prefixes afterwards |
| `npm pack` | "Create a tarball from a package" | READ, https://docs.npmjs.com/cli/v10/commands/npm-pack | an archive of one package |
| `spack env view` | a view is "a directory with a more traditional structure" | READ, https://spack.readthedocs.io/en/latest/environments.html | links files by default, so it is not self-contained |
| Gradle `installDist` | "Assembles the distribution content and installs it on the current machine" | READ, https://docs.gradle.org/current/userguide/distribution_plugin.html | writes into `build/install` |
| `jlink --output` | writes a "custom runtime image" | READ, https://docs.oracle.com/en/java/javase/21/docs/specs/man/jlink.html | |
| `jpackage --type app-image` | a "tool for packaging self-contained Java applications" | READ, https://docs.oracle.com/en/java/javase/21/docs/specs/man/jpackage.html | |
| `dotnet publish --self-contained` | compiles and writes the application "along with its dependencies ... for distribution" | READ, https://learn.microsoft.com/en-us/dotnet/core/deploying/ | "publish" for the act, "deployment" for the result |
| `nix bundle` | packages an application "so it can run outside the Nix store" | READ, https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-bundle.html | experimental; one self-extracting executable by default |
| `python -m venv` | "lightweight 'virtual environments'" | READ, https://docs.python.org/3/library/venv.html | "Not considered as movable or copyable" |
| `conda create --prefix`, `guix pack`, `nix copy`, `uv venv --relocatable` | not fetched | MEMORY | |

## Operation B in other tools

| Spelling | Meaning | Basis | Note |
|---|---|---|---|
| `cargo metadata` | "Output JSON to stdout containing information about the workspace members and resolved dependencies" | READ, https://doc.rust-lang.org/cargo/commands/cargo-metadata.html | `--format-version=1`, and a `version` field in the JSON |
| `nix flake metadata` | "show flake metadata"; `--json` exists | READ, https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake-metadata.html | |
| `terraform show -json` | renders a state or plan file; `-json` for machines | READ, https://developer.hashicorp.com/terraform/cli/commands/show | shows an existing file, not a preview |
| `conan graph info` | builds the dependency graph and "doesn't try to install or build the binaries" | READ, https://docs.conan.io/2/reference/commands/graph/info.html | |
| `docker buildx bake --print` | prints "the resolved options for the requested targets as JSON", "without starting a build" | READ, https://docs.docker.com/reference/cli/docker/buildx/bake/ | a flag |
| `pip install --dry-run --report` | writes a JSON report of what pip would have installed | READ, https://pip.pypa.io/en/stable/reference/installation-report/ | two flags |
| `vcpkg export --dry-run` | "Do not perform the export, only print the export plan" | READ, https://learn.microsoft.com/en-us/vcpkg/commands/export | a flag on operation A |
| `npm pack --dry-run` | "only report what it would have done" | READ, https://docs.npmjs.com/cli/v10/commands/npm-pack | a flag on operation A |
| `helm template` | "Render chart templates locally and display the output" | READ, https://helm.sh/docs/helm/helm_template/ | |
| `nix path-info --json` | details about store paths, "suitable for consumption by another program" | READ, https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-path-info.html | |
| `cargo tree`, `uv tree` | a dependency tree for people | READ, https://doc.rust-lang.org/cargo/commands/cargo-tree.html and https://docs.astral.sh/uv/reference/cli/ | text, not a document |
| `uv export` | "Export the project's lockfile to an alternate format" | READ, https://docs.astral.sh/uv/reference/cli/ | "export" means a lock format here |
| `go list -m` | lists modules | READ, https://go.dev/ref/mod | the `-json` flag was not confirmed on that page |
| `kubectl describe`, AWS `describe-*`, `docker manifest inspect`, `npm view`, `nix flake show`, `helm show` | not fetched | MEMORY | |

## Tally

Counts are over the rows above, the unfetched ones included.

- **Operation A, verb of the command:** vendor 3, pack 3, export 1, deploy 1 (2 with Conan's deployers), publish 1, bundle 1, view 1, copy 1.
- **Operation A, noun for the output:** image 2 (jlink, jpackage), distribution or deployment 3 (Gradle, dotnet, Conan), SDK 1 (vcpkg, "standalone"), environment 2 (venv, uv).
- **Operation B, command name:** show 3, metadata 2, tree 2, describe 2, info 1, inspect 1, list 1, view 1, template 1.
- **Operation B, as a flag:** `--dry-run` 3, `--print` 1, `--report` 1.

## Words with a second meaning

All READ.

- **export:** a lock format in uv, a standalone SDK in vcpkg.
- **view:** a linked directory in Spack; `npm view` prints package data.
- **pack:** a tarball of one package in npm, an archive of an environment in conda.
- **publish:** `dotnet publish` writes a local folder, which is the opposite of the registry meaning.
- **deploy:** in pnpm and Conan it copies files locally, not to a server.
- **show:** `terraform show` renders an existing state or plan.
- **vendor:** in go, cargo and Bazel it holds sources for a build.

## The same words in ocx

RUN on main at `f41c6684`. "Help strings" counts a command summary or a flag's help text that holds the word in any inflection.

| Word | A command today | A flag today | Help strings |
|---|---|---|---|
| materialize | no | no | 15 (10 as a verb, 5 as "materialization") |
| closure | no | yes (`ocx inspect --closure`) | 3 |
| deploy | no | no | 1 |
| standalone | no | no | 0 |
| vendor, portable, relocatable, sysroot | no | no | 0 |
| prefix | no | no | 2 |
| view | no | no | 3 |
| stage | no | no | 1 |
| metadata | no | yes (`--metadata` on `package create`, `push`, `test`) | 14 |
| manifest | no | no | 11 |
| show | no | yes | 9 |
| plan | no | no | 1 |
| layout, spec, facts, contents | no | no | 0 |
| describe, info | yes, hidden and removed in 0.7 | no | 0 |
| report | no | yes | 17 |
| print | no | no | 12 |
| list | yes (`index list`) | no | 22 |
| export | yes | yes | 19 |
| bundle | no | no | 13 |
| image | no | no | 13 |
| dist, sdk | no | no | 0 |

## Candidates for the family word

Sent to the owner on 2026-11-25, after the owner asked for about 20 names and named `portable` and `standalone` as leanings. "In ocx" was RUN on main at `f41c6684`: commands, flags and 548 help strings from `cli.json`, the files under `website/src/content/docs`, and the Rust sources under `crates/`. "Seen in" is READ where the tables above hold the tool, and MEMORY where it is marked with an asterisk.

A word that is not a verb needs a verb below it: `ocx <word> create -o DIR` and `ocx <word> manifest`. A verb takes what it writes: `ocx <word> files -o DIR` and `ocx <word> manifest`. ocx has no hyphenated command name.

| Word | Kind | In ocx | Seen in | Catch |
|---|---|---|---|---|
| standalone | adjective | no command, flag or help string. The docs use it in the package name `python-build-standalone` and for "standalone executables such as `uv`" | vcpkg "standalone developer SDK" | an adjective used as a noun |
| portable | adjective | no command, flag or help string. The docs use "portable install" for a zipped `$OCX_HOME` that is carried to another machine (anchor `offline-portable`), 8 lines | winget's "portable" installer type* | the docs' meaning is another thing: a home that still needs `ocx` |
| relocatable | adjective | 1 docs line ("Relocatable symlinks", planned) | uv `--relocatable`* | long; says that it moves and nothing else |
| self-contained | adjective | not possible as written: no hyphenated commands, and a group `self` exists | dotnet "self-contained deployment" | |
| hermetic | adjective | 15 docs lines and 160 source lines, for a run that reads no ambient configuration | Bazel* | already a property of a run in ocx |
| detached | adjective | unused | git* | sounds broken off |
| sealed | adjective | no help string; 8 docs files | none | implies read-only or signed |
| offline | adjective | the flag `--offline`, 5 help strings | none | taken by the flag |
| kit | noun | unused everywhere: 0 help strings, 0 docs files, 0 source lines | "software development kit"* | says what it holds, not that it moves |
| toolset | noun | unused everywhere | MSVC "platform toolset"* | matches this discussion's word "set" |
| toolkit | noun | unused everywhere | none | a longer `kit` |
| capsule | noun | unused everywhere | Capsule, for JVM applications* | vague |
| closure | noun | the flag `--closure` on `ocx inspect`, 3 help strings | Nix* | names the set, not the directory |
| prefix | noun | 2 help strings, 33 docs files | conda `--prefix`* | the docs use it for other things |
| stage | noun or verb | 1 help string | snapcraft's stage step* | reads as a step of a pipeline |
| payload | noun | no help string; 33 docs files | installer payload* | the docs already use the word |
| materialize | verb | 15 help strings; no command or flag | Bazel's word for outputs written to disk* | the first verb group in ocx |
| deploy | verb | 1 help string | pnpm deploy, Conan deployers | heard as "to a server" |
| vendor | verb | no help string; 12 docs files | go, cargo, bazel | means sources there; rules_ocx users have `bazel vendor` |
| eject | verb | unused everywhere | Create React App* | implies leaving the tool for good |
| provision | verb | no help string; 4 docs files | Ansible, Vagrant* | implies machines |
| unpack | verb | no help string; 3 docs files | pixi-unpack; `dpkg --unpack`* | hides the launchers and the environment |
| realize | verb | unused everywhere | `nix-store --realise`* | obscure |
| render | verb | 3 help strings, 23 docs files | helm template | the owner's word for installers later |

Taken, and so not listed: export, bundle, image, snapshot, dist, toolchain, home.

## A verb as the family: subcommands or a flag

RUN on main at `f41c6684`, over `cli.json` and the user documentation, after the owner asked on 2026-11-25 whether a verb needs a subcommand.

- The root has 19 flat commands and 8 groups. Every group is a noun (`config`, `direnv`, `index`, `launcher`, `package`, `patch`, `shell`, `self`). No verb holds subcommands.
- `--dry-run` exists on 8 commands: `clean`, `config setup`, `index sync`, `package cascade repair`, `package copy`, `package prune`, `pull`, `self setup`. `ocx pull` declares two report roots, and one of them is for the dry run (`PullDryRun`).
- Of those 8, 7 declare one report root for a real run and for a dry run: `Clean`, `ConfigSetupData`, `CatalogPreview`, `PackageCascadeRepair`, `CopyReport`, `PackagePrune`, `SelfSetupData`. Only `pull` declares a second root for the dry run.
- Their help texts say the same: "Compute and print the same plan without writing anything" (`package cascade repair`), "Report the intended actions without writing anything" (`config setup`, `self setup`), and "Report what would be copied and write nothing. The cascade and keep-tag moves are not computed" (`package copy`), which is a dry run that leaves parts out.
- `.claude/rules/subsystem-interface-contract.md`, IC-09: "`status` is a named open enum; a dry run is a status value". The `Clean` report carries a `dry_run` field, and `CopyReport` says "Whether this run copied or only planned".
- `--no-pull` exists on 5 commands: `env`, `add`, `direnv export`, `lock`, `update`.
- No flag ends in `-only`, and none starts with `print`.
- 8 commands declare two or more report roots: `update`, `upgrade`, `package deps`, `patch test`, `pull`, `shell state`, `self update`, `version`.
- "vendor" in ocx's user documentation, 19 lines: as a noun for the publisher of a tool ("vendor-namespaced" and "vendor-specific" for the `integrations` field, "vendor archive", "vendor documentation"), as "vendored" for something copied into a build (2 lines), and in nushell's `vendor/autoload` path.
- "vendor" in rules_ocx's documentation, 5 lines: "the vendored `dist.json` snapshot".

## The manifest as a file in the output directory

After the owner's message of 2026-11-25 that the JSON is part of the outputs. READ on main at `f41c6684`, and one RUN.

- `ocx package create` declares an empty output in `cli.json`. It writes a build receipt beside the bundle: `package.tar.gz` gives `package-receipt.json` (`crates/ocx_cli/src/conventions.rs:42`). `ocx package receipt` prints that file as the report root `PackageReceipt`.
- The receipt's module says: "A build artifact: never published, no JSON Schema" (`crates/ocx_cli/src/build_receipt.rs:5`). The precedent covers a file beside an output and a command that prints it. It does not cover a published format.
- `ocx export` writes a directory and prints `ExportReport`: the directory, the packages, a blob count and a size.
- `ocx export` and `ocx package export` have `--referrers` and `--no-referrers`. The second reads: "Leave them out; a trust policy then cannot verify the import offline".
- 17 flags have the form `--no-…`. `--no-pull` reads: "Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit".
- RUN with Bazel 9.2.0, with `--ignore_all_rc_files` and an output root in the scratch directory. A module extension ran `sh -c` to write `out/manifest.json`, and read it with `module_ctx.read`. The repository it then declared held the file's text. The command's working directory was `<output base>/modextwd/+probe`. Control: the same extension read `out/missing.json` and failed with exit code 7 and `java.io.FileNotFoundException`.

## Where the word "only" goes, and whether the output directory is required

After the owner's question of 2026-11-26. RUN on this host on that day, and READ where a line says so.

**The two forms, counted.** A script took every long option from two sets of files and sorted the names by shape. `--read-only` and `--readonly` were left out, because they name a state.

| Set | Files | Names ending in `-only` | Files with one | Names starting with `only-` | Files with one |
|---|---|---|---|---|---|
| zsh completion functions | 992 | 78 | 42 | 21 | 17 |
| man pages, sections 1 and 8 | 2599 | 78 | 101 | 18 | 28 |

- Most frequent with the word last: `--name-only` (16 man pages), `--token-only` (11), `--list-only` (5), `--download-only` (3 completion functions), `--ff-only` (4 man pages).
- Most frequent with the word first: `--only-matching` (5 man pages), `--only-plugins` (4), `--only-keep-debug` (4).
- Names with the word last and no hyphen, counted apart: 9 in the completion functions (`--downloadonly`, `--cacheonly`) and 4 in the man pages.

**The nearest by meaning,** each RUN as the tool's own help on this host:

- `pnpm install --lockfile-only`: "Dependencies are not downloaded" (pnpm 11.22.0).
- `npm install --package-lock-only`.
- `ctr content fetch --metadata-only`: "Pull all metadata including manifests and configs".
- uv, the other way round: `--only-dev` and `--only-group` on `uv sync`, `--only-binary` on `uv pip install`.
- Bazel 9.2.0: `--build_tests_only` and `--experimental_extra_action_top_level_only`. No flag starts with `only`.

**The house.**

- ocx's `cli.json` on main at `f41c6684` has no flag with "only" in its name.
- The mirror tool has `ocx-mirror package pipeline patch --metadata-only`: "Required. Republish the metadata blob and nothing else — currently the only mode." READ in that repository, `docs/reference/cli.md:310` and `src/command/package/pipeline/patch.rs:77`.

**The output option.** READ from `cli.json`: seven commands have `--output` with the short form `-o`.

| Command | Required | What it takes |
|---|---|---|
| `export`, `package export` | yes | "Directory to write the export to; created when absent" |
| `package create` | no | a file or a directory; the file name is inferred |
| `package test` | no | a directory; a temporary one by default |
| `package sbom` | no | a path, and `-` for stdout |
| `package announce`, `package claim` | no | a directory, which selects another mode |

- `cargo vendor [path]`: "Where to vendor crates (`vendor` by default)". RUN, its help.
- `pnpm deploy <target directory>`: the directory is required. RUN, its help.
- `bazel vendor` with no `--vendor_dir`: exit code 2 and "ERROR: You cannot run the vendor command without specifying --vendor_dir". RUN on Bazel 9.2.0, in a scratch workspace with `--ignore_all_rc_files`.

## A default for the output option, and the document on stdout

After the owner's message of 2026-11-26. READ on main at `f41c6684` unless a line says RUN or MEMORY.

- `cli.json` has six output kinds: `empty`, `passthrough`, `raw_document`, `report`, `report_then_fail`, `shell_stream`.
- `ocx package sbom` declares three: a report (`SbomListingReport`), `empty` and `raw_document`. Its `--output`: "Write the SBOM document to PATH ("-" for stdout)". The option conflicts with `--summary`.
- The same command refuses a terminal as the destination, with exit code 64: the bytes are "publisher-authored and unsanitized" (`crates/ocx_cli/src/command/package_sbom.rs:418`).
- `ocx package test --output`: "Materialize into DIR instead of an auto-managed temp dir. DIR must not exist (created by OCX) or be empty. Implies keep - the dir is never deleted by OCX."
- `ocx export`: "Exporting into an existing export directory adds to it" (`website/src/content/docs/reference/command-line/package-publishing.md:453`).
- `ocx package create --force`: "Force overwrite of output file if it already exists".
- The project toolchain is rendered "into `<project>/.ocx/toolchain/` - or under the `toolchain_dir` root, when one is configured" (`website/src/content/docs/reference/command-line/commands.md:36`). The key is `toolchain_dir`, with the variable `OCX_TOOLCHAIN_DIR` as its fallback (`website/src/content/docs/reference/configuration.md:520`).
- This repository's `.gitignore` ignores `.ocx/`: "Rendered project toolchain home … it is a cache, never a source."
- RUN, the tools' own help on this host: `cargo vendor [path]` defaults to `vendor` and has `--no-delete` ("Don't delete older crates in the vendor directory"). `docker save -o`: "Write to a file, instead of STDOUT". `pnpm pack` has `--out <path>` and `--pack-destination <dir>`.
- MEMORY, not checked: `go mod vendor` writes `vendor`; `git clone` refuses a directory that is not empty; `oras pull --output` defaults to the current directory; `pip download --dest` defaults to the current directory.

## What this shows

- ocx can declare a document on stdout: the output kind `raw_document` exists, with `-` as the value of `--output`.
- A project has a default directory for what ocx renders from its lock: `<project>/.ocx/toolchain`.
- The one command that materialises into a directory the caller names requires it to be absent or empty.
- The form with the word last is the more frequent one on this host, by about four to one in distinct names.
- Both ocx commands whose product is a directory require `-o`. The house sends a document to stdout with `-o -`.
- A JSON file that a command writes with its output, and the report that the command prints, are two things in ocx.
- A Bazel module extension can take a document from a file as well as from stdout.
- For a verb, ocx's own grammar is a flat command whose flags switch between acting and reporting.
- "vendor" has a noun meaning in ocx's documentation: the publisher of a tool.
- Of the owner's two leanings, `standalone` has no other meaning in ocx, and `portable` has one: the docs' "portable install".
- No word dominates operation A. The two most frequent, `vendor` and `pack`, each mean something else than the operation asked about: sources for a build, and an archive.
- Two tools use one word for exactly this operation: `pnpm deploy` and Conan's deployers. vcpkg uses `export` and calls the result a standalone SDK.
- For operation B, `metadata` and `show` each have several users. Three tools spell it as a dry-run flag on operation A.
- In ocx no help string uses `standalone`, `vendor`, `portable`, `layout`, `dist`, `spec` or `facts`. `metadata`, `manifest`, `show` and `materialize` are in use.

## What this does not show

- Whether anything in ocx prunes `<project>/.ocx/` beyond `toolchain/`. Not read.
- How often each tool is used. The counts are of option names in the files installed on this host, and a man page can name another tool's option.
- Whether the gate with the contract ledger can cover a file. Not read.
- Whether `ocx` can write the manifest without layers. Nothing was built; the Bazel run used a stand-in command.
- Which word reads best to ocx's users. No user was asked.
- What the unfetched tools document.
- What each ocx help string means by the word. Only the count was taken, except for `metadata` and `materialize`, which were read on 2026-11-25 (`.agents/research/research_ocx-command-grammar-and-plugin-rule.md`).
