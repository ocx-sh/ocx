# Research: ocx's command grammar, its two tiers, and its rule on core versus plugin

## Metadata

**Date:** 2026-11-25
**Lane:** codebase read by the orchestrator (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ on main at `f41c6684`: `crates/ocx_schema/tests/golden/cli.json` (walked with a script), `.claude/rules/subsystem-cli.md`, `.claude/rules/subsystem-cli-commands.md`, `.claude/rules/arch-principles.md`, `.claude/rules/subsystem-interface-contract.md`, the decision section of `.claude/artifacts/adr_cli_plugin_pattern.md`, and `scripts/crate_map.toml`. Nothing was run. This discussion's worktree is 42 commits behind main and was not the basis.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Not read: `crates/ocx_cli/src/app/plugin_dispatch.rs` itself, the plugin ADR beyond its decision section, how `ocx package test --output` writes its directory, and the clap source of any command.

## 1. The command tree

Read from `cli.json`. Hidden commands are marked.

- Root verbs: `about`, `add`, `clean`, `env`, `exec`, `export`, `import`, `init`, `inspect`, `lock`, `login`, `logout`, `pull`, `remove`, `status`, `update`, `upgrade`, `version`, and `run` (hidden, deprecated).
- Groups: `config` (`setup`, `update`, `test`, `push`), `direnv` (`init`, `export`), `index` (`catalog`, `list`, `update`, `sync`, `regenerate`), `launcher` (hidden: `exec`, `shim`), `package` (31 entries), `patch` (`freeze`, `sync`, `publish`, `test`, `why`), `shell` (`allow`, `completion`, `revoke`, `state`), `self` (`activate`, `setup`, `update`).
- Nested groups, both under `package`: `cascade` (`check`, `repair`) and `description` (`push`, `pull`).
- Every one of the eight groups is a noun. No command both acts by itself and holds subcommands.
- Six verbs exist in a root form and a `package` form: `env`, `exec`, `export`, `import`, `inspect`, `pull`.

## 2. The two tiers are firm

- `.claude/rules/subsystem-cli-commands.md:16`: "The CLI surface splits into two tiers. The split is firm."
- The toolchain tier takes binding names and consults `ocx.toml`. The OCI tier (`ocx package …`) takes OCI identifiers and never consults `ocx.toml`.
- The same file: "No command silently switches contract based on CWD."
- `.claude/rules/subsystem-cli.md:14`: a missing `ocx.toml` is a usage error (exit 64) for a toolchain-tier command.
- One documented exception exists, for trust policy only (`ocx package verify`).

## 3. A group around one object: the `description` rename

- `ocx package describe` and `ocx package info` became `ocx package description push` and `ocx package description pull` in 0.6. The old spellings are hidden, warn once, and are removed in 0.7.
- The stated reason (`.claude/rules/subsystem-cli-commands.md`, "`description` group notes"): the two commands write and read one registry-side object, so the pair is spelled with the tier's own transport verbs.
- A group is flat in the source: one dispatcher file and one file per verb (`package_description.rs`, `package_description_push.rs`, `package_description_pull.rs`), with no `mod.rs`.

## 4. Output forms selected by an option: `ocx env`

- `ocx env` and `ocx package env` declare two output kinds: a report (`EnvVars`) and a shell stream.
- `--shell[=NAME]` writes export lines for one of ten shells: ash, ksh, dash, bash, elvish, fish, batch, powershell, zsh, nushell.
- `--ci[=NAME]` writes into a CI system's persistence channel: github or gitlab. `--export-file` redirects the GitLab form.
- `ocx direnv export` is a separate command in a group named after the consuming tool.
- `ocx pull` declares two report roots on one command: `WarmedPaths` and `PullDryRun`.

## 5. One report root, several commands

Ten report roots are written by two or more commands:

| Root | Commands |
|---|---|
| `EnvVars` | `env`, `package env` |
| `ExportReport` | `export`, `package export` |
| `ImportReport` | `import`, `package import` |
| `InspectReport` | `inspect`, `package inspect` |
| `Installs` | `package install`, `package select` |
| `LockReport` | `add`, `lock`, `remove` |
| `PackageDescriptions` | `package description pull`, `package info` |
| `PushReport` | `config push`, `package push` |
| `Removed` | `package deselect`, `package uninstall` |
| `ScriptRunReport` | `package test`, `patch test` |

A report root carries its own `schema_version` (IC-02). A command declares its output modes and its own version (IC-21).

## 6. Plugin dispatch, and the rule on core versus plugin

- `cli.json` marks the root command with `external_subcommands: true`, and no other command.
- `.claude/rules/subsystem-cli.md`, "Cross-Cutting: Plugin Dispatch": an unknown subcommand name is routed to a program `ocx-<name>` found on `PATH`, "matching the cargo / git / kubectl plugin pattern". Built-ins always shadow plugins. A plugin gets the execution state and the binary pin, and the credential variables are removed. A missing plugin exits 64.
- `.claude/artifacts/adr_cli_plugin_pattern.md`, decision: discovery by `PATH` scan only; the subcommand name is dropped from the arguments, so the plugin is an ordinary CLI; no plugin list in `--help`; no shell completion for plugins. The ADR's header still says "Proposed", while the rule file says the dispatch has shipped.
- The ADR's reason against building the mirror tool in: it would bring that tool's dependencies into every `ocx` install and force one release for both.
- `.claude/rules/arch-principles.md`, "Core vs Plugin Boundary (owner doctrine, 2026-07-16)":
  - "`ocx` core is self-contained: complete primitive set (`ocx package *`, toolchain tier) stays in the one binary. No verb extraction to slim the binary."
  - "Add-ons are `ocx-<name>` plugins (git/cargo-style dispatch, shipped — `app/plugin_dispatch.rs`). No plugin ABI, ever."
  - "Boundary is behavioral, not link-level: package/store/registry *operations* go through the CLI surface … CLI = the stable contract."
  - The satellite linking rule: a satellite links the ecosystem-tier crates and drives every operation through the CLI. Linking an internal-tier crate is named as drift. The internal crates it lists: `ocx_store`, `ocx_project`, `ocx_package_manager`, `ocx_announce`, `ocx_shell`, `ocx_script`, `ocx_setup`.
- The one program that follows the rule today is the mirror tool, in its own repository.
- `.claude/artifacts/adr_cli_repo_split_template.md` (Accepted, 2026-06-12): "More satellite CLIs are planned (`ocx-mcp`, `ocx-dist`)." The record gives the split of the mirror tool as the template for them. It says nothing on what `ocx-dist` does. Read after the owner named `ocx dist` on 2026-11-25.

## 7. The word "materialize" in help text

15 help strings use it. All mean writing a package's content to disk.

- `--pull` on `env`, `add`, `direnv export`, `lock` and `update`: "Materialize resolved packages into the object store, installing on a local miss". `--no-pull` on the same five: "Skip materialization".
- `ocx launcher shim`: "Materialize a deferred package and run one of its declared names".
- `ocx package test`: "Materialize a package locally (no registry round-trip) and run a command in its env". Its `--output`: "Materialize into DIR instead of an auto-managed temp dir". Its `--identifier` uses the word once more.

Other words that are in use: "bundle" is the archive that `ocx package create` writes, and also a signature format (`--signature-format bundle`). "export" is the transfer directory. "image" belongs to OCI.

The word "metadata": 14 help strings in 9 commands use it. It names the publisher's `metadata.json` or a package's description metadata. `package create`, `package push` and `package test` each have a `--metadata` flag, and `ocx inspect --resolve` is documented as "emit its metadata plus the OCI resolution chain".

Leaves that are nouns and print a report: `env`, `package env`, `package deps`, `package receipt`, `package sbom`, `shell state`, `status`, `about`, `version`.

## 8. Contract items that bind a new command and its document

From `.claude/rules/subsystem-interface-contract.md`:

- IC-02: every report root is an object led by `schema_version`.
- IC-09: a dry run is a status value of a report.
- IC-12: the output destination is `--output` / `-o`.
- IC-15: enum values and union variants are open; adding one is additive.
- IC-18: an output schema never sets `additionalProperties: false`.
- IC-21: every command declares its output modes and its version.
- The semantic-break checklist names a renamed or removed flag, variable, slug or field as a break. It has no line on an added field.

## What this shows

- The house spelling for several operations on one object is a noun group with verbs.
- A command that takes binding names and a command that takes package identifiers are two commands.
- Two commands can write one report root, so a second command needs no second document.
- The house also selects an output form with an option, on `ocx env`.
- A second executable is an existing, shipped mechanism, and an owner rule says what belongs in it: add-ons, never primitives.
- A program outside `ocx` may not link the crates that hold the construction code and the shell emitters.

## What this does not show

- Which noun is free and fits. No list of free nouns was made beyond the four words in section 7.
- Whether a plugin gets everything it needs from the environment that dispatch passes on. The dispatch source was not read.
- Whether an added field counts as additive for the compat gate. The rule file has no line on it.
