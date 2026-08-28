---
paths:
  - crates/ocx_cli/src/command/**
---

# OCX CLI Commands — Quick Reference

> **Authority:** `.claude/artifacts/handshake_toolchain_cli.md` (signed 2026-05-16). The taxonomy below reflects the signed handshake. Commands listed in the **Deleted Commands (exit 64)** section do NOT exist; any description of them in older ADRs or rules is superseded. Do not implement deleted commands.

Concise index of all `ocx` CLI commands. User-facing per-command docs live in `website/src/docs/reference/command-line.md`. Implementation under `crates/ocx_cli/src/command/`.

---

## Layering: Toolchain-Tier vs OCI-Tier

The CLI surface splits into two tiers. The split is firm.

| Tier | Commands | Input | Consults `ocx.toml`? |
|------|----------|-------|----------------------|
| **Toolchain-tier** | `add`, `remove`, `lock`, `update`, `run`, `env`, `status`, `inspect` | Binding names / OCI id (add) | **Yes** (or `$OCX_HOME/ocx.toml` under `--global`) |
| **OCI-tier** (`ocx package`) | `install`, `uninstall`, `select`, `deselect`, `exec`, `env`, `which`, `deps` | OCI identifiers | **Never** |
| **Bootstrap / mixed** | `init`, `direnv init`, `direnv export`, `about`, `version`, `shell completion`, `shell state`, `shell allow`, `shell revoke` | Varies | — |
| **Low-level registry** | `package pull`, `package push`, `package copy`, `package describe`, `package info`, `package create`, `index update/sync/list/catalog`, `login`, `logout` | OCI identifiers | Never |
| **Low-level local store** | `clean`, `launcher exec` | OCI identifiers | Never |

**Layer-purity rule:** `ocx run` is toolchain-tier (binding-name semantics); `ocx package exec` is OCI-tier (identifier semantics). `ocx env` is toolchain-tier; `ocx package env` is OCI-tier. No command silently switches contract based on CWD.

---

## Global Flags (all commands)

`--global` is a root flag — it must appear **before** the subcommand name (peer of `--project`), not after it.

| Flag | Env Var | Default | Purpose |
|------|---------|---------|---------|
| `--color auto\|always\|never` | `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` | auto | ANSI color output control |
| `--remote` | `OCX_REMOTE` | false | Route mutable lookups to remote registry |
| `--offline` | `OCX_OFFLINE` | false | Disable all network access |
| `--format plain\|json` | — | plain (all commands, no exceptions) | Root-only output format; no subcommand-level `--format` |
| `--index PATH` | `OCX_INDEX` | — | Override local index directory |
| `-l/--log-level` | — | — | Tracing level |
| `--global` | `OCX_GLOBAL` | false | Select `$OCX_HOME/ocx.toml` toolchain tier; affects toolchain-tier commands `add`/`remove`/`lock`/`update`/`pull`/`run`/`env` (plus `patch freeze`, which reads `context.global()`); mutually exclusive with `--project` |

## Toolchain-Tier: `--global` vs Project

`--global` is a **root flag** (before the subcommand), defined once on `ContextOptions` (peer of
`--project`). It re-targets the project file to `$OCX_HOME/ocx.toml` for the toolchain-tier
commands: `add`, `remove`, `lock`, `update`, `pull`, `run`, `env`. Canonical form:
`ocx --global <subcommand>` — e.g. `ocx --global add ripgrep:14`.

Project-tier commands resolve their project file in strict precedence order: `--global` (explicit) → `--project`/`OCX_PROJECT` (explicit) → CWD walk → None.

Mutually exclusive with `--project` — combining both is a clap conflict (exit 64 — ocx maps clap usage errors → EX_USAGE 64). No implicit discovery of `$OCX_HOME/ocx.toml`. `OCX_GLOBAL` is the env equivalent.

**`ocx package install --global` → clap unknown-flag error (exit 64 — ocx maps clap usage errors → EX_USAGE 64).** `--global` is NOT on any `ocx package` subcommand.

---

## Command Summary

### Toolchain-Tier Commands

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `add IDENTIFIER...` | Append one or more bindings to `ocx.toml`, update lock, install (staged atomically) | `-g/--group`, `--pull/--no-pull` |
| `init` | Create minimal `ocx.toml` in current directory | — |
| `remove IDENTIFIER...` | Drop one or more bindings from `ocx.toml`, rewrite lock (fail-fast, all-or-nothing) | — |
| `lock` | Resolve tags to digests, write `ocx.lock` | `-g/--group`, `--pull/--no-pull` |
| `update [-g GROUP]... [NAME...]` | Re-resolve advisory tags in lock against the LIVE registry by default (update-family verb: writes `ocx.lock` only, never tag pointers; `--remote` redundant-but-accepted; `--frozen` caps at snapshot, unknown tag exit 81); whole file (no args) or a scoped subset by name/group (reuses `resolve_lock_touched`: named bindings re-resolve, rest carried forward verbatim; scoped needs a predecessor lock, exit 78 if absent; refuses drifted `ocx.toml`, exit 65; unknown group/name, exit 64). ADR `adr_toolchain_update_family.md` | `-g/--group`, `--check`, `--pull/--no-pull` |
| `run [-g GROUP]... [NAME...] -- ARGV...` | Spawn child with composed toolchain env. No `--self`: the self view is package vocabulary and drops a package's own `entrypoints/` from `PATH`, so it composes a strictly worse toolchain | `-g/--group`, `--clean`, `--env` |
| `env [--shell[=NAME]] [--ci[=PROVIDER]]` | Composed toolchain env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe; `--ci` = CI sink (later-step); installs on miss by default (`--no-pull` opts out → offline local probe; missing tool → stderr warn + omit, exit 0); JSON also carries `binaries`/`entrypoints`/`integrations` admitted-claim attribution arrays — `integrations` is the payload-carrying `{namespace, package, payload}` shape, one row per (package, namespace) pair, never merged (never in `--shell`/`--ci` output) | `-g/--group`, `--env`, `--shell[=NAME]`, `--ci[=PROVIDER]`, `--export-file`, `--pull/--no-pull` |
| `pull` | Pre-warm package store from `ocx.lock`; re-saves lock to advance mtime for direnv re-fire (skipped under `--dry-run`) | `--dry-run` |
| `status` | Report what `ocx.toml` + `ocx.lock` declare; no resolution, no network, no flock, no staleness gate. Missing / stale / unparseable lock are all payload with exit 0 — it is the command that answers on a project the others refuse. Full per-platform digest map (no host-leaf selection), per-scope `[env]` verbatim (relative `path` values NOT anchored), `[package.*]`, both declaration hashes. Absence-as-signal per binding: no `platforms` = unlocked, no `declared` = orphaned. NO selectors by design | — |
| `inspect [-g GROUP]... [NAME...]` | Toolchain-tier `ocx package inspect`: identical envelope + `--resolve`/`--closure`/`--env`, keyed by binding, plus the project's composed `env` array in application order. Read-only. **Default mode resolves nothing** — each binding lists the platform candidates `ocx.lock` pins for it (no registry, no host-leaf selection, `-p` inert, no entry-level `pinned_*`, no candidate `media_type`/`size`); `--resolve` selects the host leaf exactly as on the OCI tier. `identifier` is the `ocx.toml` declaration verbatim, tag included — the lock's bare `repository` would drop it. Needs a current lock (78 / 65) — a moving tag would make the answer depend on the moment. `--closure` reports cross-tool collisions pre-install; non-empty conflicts exit 65 | `-g/--group`, `-p/--platform`, `--resolve`, `--closure`, `--env` |

### OCI-Tier Commands (`ocx package`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `package announce` | Observe an owner-curated tag set and publish the rebuilt entry into the index — write locally (`--out`), or open/update a pull request (GitHub) or merge request (GitLab) from a fork (`--fork`) or from a branch on the index repo itself (neither flag; needs push access, verified up front, exit 80 naming repo + permission). Coordinates are `[HOST/]NAMESPACE/PROJECT` — host selects a self-hosted instance (GHES `/api/v3`, GitLab `/api/v4`), namespace may nest (GitLab groups; GitHub refuses nesting). Forge inferred for github.com/gitlab.com/no-host, `--forge` REQUIRED for a self-hosted host (never probed — a wrong guess sends the credential to the wrong API), exit 64 | `--package`, `--tags`/`--tags-from-file`/`--tags-from-registry`/`--refresh`, `--out`/`--fork` (both optional), `--index-repo`, `--forge github\|gitlab`, `--yank`/`--unyank`/`--yank-reason` |
| `package cascade check <id>...` | Diff each identifier's observed registry tag graph (plus, for a logical `ocx.sh/…` identifier, the live public index root) against the fold-expected cascade state computed from every published concrete version; read-only, Pull-only auth, never writes. Exit 0 clean, 65 on any finding (including index staleness), 64 on a usage error (digest-pinned identifier, a non-version scope tag) | — |
| `package cascade repair [--dry-run] [--announce-tags PATH] <id>...` | Recompute and PUT the whole alias index for every tag the fold disagrees with — batched, concurrent writes, preflighted against missing child manifests. `--dry-run` previews the same plan with zero registry writes. Exit 0 once every attempted registry write succeeded (remaining index staleness is `announce`'s job, not a failure here); 65 when a finding remains after the run (including an alias refused for lacking new content); 64 usage | `--dry-run`, `--announce-tags` |
| `package install PKGS...` | Download and install packages (no `ocx.toml` touched) | `-s/--select`, `-p/--platform` |
| `package uninstall PKGS...` | Remove candidate symlink | `-d/--deselect`, `--purge` |
| `package select PKGS...` | Set `current` symlink | `-p` |
| `package deselect PKGS...` | Remove `current` symlink | — |
| `package exec PKGS... -- CMD` | Run command with package env (hermetic) | `--clean`, `-p`, `--self`, `--env` |
| `package env PKGS... [--shell[=NAME]] [--ci[=PROVIDER]]` | Per-package composed env; output via root `--format` (default plain); `--shell[=NAME]` = eval-safe; `--ci` = CI sink (later-step); JSON also carries `binaries`/`entrypoints`/`integrations` admitted-claim attribution arrays — `binaries`/`entrypoints` are `{name, package}`, `integrations` is `{namespace, package, payload}` (one row per (package, namespace) pair, never merged, always `[]` under `--self`); `package` = canonical resolved identifier, possibly tagless digest-pinned; never in `--shell`/`--ci` output; plain mode gets a hint line | `--shell[=NAME]`, `--ci[=PROVIDER]`, `--export-file`, `--self`, `--env` |
| `package pull PKGS...` | Download to object store only | `-p` |
| `package create PATH` | Bundle directory into archive; `--bin-scan`/`--no-bin-scan` fill or verify the `binaries` claim | `-o`, `-m`, `-l`, `-j`, `--force`, `--bin-scan`/`--no-bin-scan` |
| `package push -i ID LAYERS...` | Publish archive to registry | `-i`, `-c`, `-n`, `-m`, `-p`, `--build-timestamp`, `--canonical-tag/--no-canonical-tag` (default on — pushes `sha256.<hex>` per platform manifest, registry-side deletion safety net; `index.ocx.sh` ignores it), `--announce-file` (append pushed + cascade tags to a scratch file `package announce --tags-from-file` can consume) |
| `package copy SOURCE` | Promote a published package to another registry/repository. Leaf manifests + blobs + referrers copied verbatim (digest preserved, so signatures and lock pins survive); the target index is merged **per platform**, never byte-copied; rolling tags recomputed against the **target**. Per-platform report rows: `added`/`unchanged`/`replaced`/`kept (not in source)` | `--to` (host rewrite) \| `-i/--identifier` (mutually exclusive; required for a digest source), `-p/--platform` (filter for a tag source, **required declaration** for a digest source), `-c/--cascade`, `--canonical-tag`/`--no-canonical-tag` (default on), `--referrers`/`--no-referrers` (default on; exit 84 without the Referrers API), `--description`, `--annotation`, `--dry-run` |
| `package describe ID` | Push description metadata; `--from SOURCE` copies another repository's description wholesale (replace, not merge; mutually exclusive with the field flags) | `--from`, `--readme`, `--logo`, `--title`, `--description`, `--keywords` |
| `package inspect PKGS...` | Inspect each reference (candidates / metadata+layers / resolution); `--closure` adds a metadata-only dependency closure OBJECT without installing — `closure.deps` (transitive dependencies in transitive-closure order, root excluded, each with `effective_visibility` + tri-state `binaries` + `entrypoints` + declared `integrations` namespace keys (`Vec<String>`, no payload) + own `dependencies`), `closure.surface.{interface,private}` (the two symmetric projections: binaries/entrypoints `{name, package}` + env `{key, type, package}` value-omitted + integrations `{name, package}` (`name` = namespace, no payload; interface surface only — `private.integrations` is always `[]`) + `binaries_complete`; public entries cross both axes), and `closure.conflicts`. Read-only inspect never grows the local index (writes content to the GC-able blob cache only). Keyed object for multiple | `--resolve`, `--closure`, `-p` |
| `package info PKGS...` | Display description metadata; keyed object for multiple | `--save-readme`, `--save-logo` (single package only) |
| `package sign IDENTIFIER` | Keyless Sigstore sign via OCI Referrers | `-p/--platform` (required), `--fulcio-url`, `--rekor-url`, `--identity-token-file`, `--identity-token-stdin`, `--no-tty`, `--no-cache` |
| `package verify IDENTIFIER` | Keyless Sigstore verify via OCI Referrers | `-p/--platform` (required), `--certificate-identity` / `--certificate-oidc-issuer` (optional-when-a-`[trust.policy]`-matches; **both-or-neither**, one alone → exit 64), `--sigstore-trusted-root`, `--rekor-url`, `--offline`, `--no-cache` |
| `package attest --predicate FILE --type TYPE IDENTIFIER` | Attach a signed in-toto attestation (SBOM/provenance) as a cosign-bundle referrer | `--predicate` (required), `--type` (required; cyclonedx/spdx/spdxjson/slsaprovenance1/URI), `-p/--platform`, `--fulcio-url`, `--rekor-url`, `--identity-token-file`, `--identity-token-stdin`, `--no-tty`, `--no-cache` |
| `package sbom IDENTIFIER` | List or extract SBOM attestations under one of two verification modes. **Demand** (`--verify`, or the default when identity flags or a matching `[[trust.policy]]` resolve): full crypto, and a raw unsigned attachment is refused (`unsigned_rejected_by_policy`, exit 77) rather than listed. **Permissive** (`--no-verify`, or the default when no identity source resolves): no cryptography runs and no trust root is resolved — raw attachments and bundle payloads alike are listed `verified: false` with no signer fields. `summary.verification` (`verified`/`unverified`) names the mode that ran. `verify --attestation` is untouched: its scan still filters bundles only, so an unsigned referrer is never a verification candidate | `--verify`/`--no-verify` (`overrides_with`, last-wins like every paired toggle; `--no-verify` conflicts with the certificate flags → 64; `--verify` with no identity source → 64), `--type`, `--output PATH\|-` (verbatim bytes; TTY refused), `--summary` (CycloneDX, both modes), `-p/--platform`, verify flags inherited (`--certificate-identity`/`--certificate-oidc-issuer`, `--sigstore-trusted-root`, `--rekor-url`, `--offline`, `--no-cache`) |
| `package test -i ID LAYERS... -- CMD` | Materialise + exec locally (no registry) | `-i`, `-p`, `-m`, `--keep`, `-o`, `--self`, `--clean`, `--env` |
| `package which PKGS...` | Resolve installed packages to paths (package-root or stable symlink anchor) | `--candidate`, `--current`, `-p` |
| `package deps PKGS...` | Show dependency tree/flat/why | `--flat`, `--why`, `--depth`, `--self`, `-p` |

**`cascade` group notes:**
- Files: `command/package_cascade.rs` (dispatcher) + `package_cascade_{check,repair}.rs` (one leaf per subcommand) — same flat, no-`mod.rs` shape as `patch.rs` + `patch_{...}.rs`, one level deeper under `package.rs`'s own `Cascade` variant (see `subsystem-cli.md` "Command Module Structure").
- Both subcommands take one or more identifiers, logical (`ocx.sh/<ns>/<pkg>[:tag]`, resolved via `physical_reference` the same way `install` does) or physical (bare registry path); a digest-pinned identifier is a usage error (exit 64) — there is no tag graph to diff against a fixed digest.
- The identifier's own tag selects scope: tagless = every variant track (`WholeGraph`); an explicit `:latest` = the default variant track only; a rolling tag (`:3.28`) = its subtree plus the path up to its own root; a fully build-tagged leaf = the path to root only (never a write target). Multiple identifiers for the same package union their scopes into one report.
- A logical identifier gets a third finding layer (`index_findings`) from the live public index root; a physical identifier skips it — no reverse mapping from registry path back to a logical name.
- `repair --announce-tags <PATH>` writes the tags this run re-pointed or created (newline-separated, `parse_tags_file`-compatible; empty file when nothing changed) — the follow-up publish step is `package announce --tags-from-file <PATH>` (union semantics, so it also picks up an alias that was never committed at all).

### Installation Management Commands (`ocx self`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `self setup [VERSION]` | Bootstrap specified or latest ocx, write env shims, add managed activation block to shell profiles | `--no-modify-path`, `--profile PATH`, `--dry-run`, `--force` |
| `self activate` | Emit eval-safe PATH prepend + completions + global env eval for the detected shell | `--shell[=NAME]` |
| `self update` | Check and install the latest released ocx version | — |
| `self update --check` | Query registry for newer version; no install | `--check` |

**`self setup` notes:**
- **`VERSION` positional (optional)**: three forms accepted — `1.2.3` (tag), `sha256:<64hex>` (digest, written bare without `@`), `1.2.3@sha256:<64hex>` (tag+digest immutability assertion). Malformed input exits 64. `tag@digest` mismatch exits 65 (fail-closed; message names both digests; under `--frozen` adds stale-index hint). Pin satisfied ⟺ `current` already points at the pinned digest — no re-download. Downgrade (pinned tag semver-older than installed) warns on stderr and proceeds. Literal `latest` = ordinary tag lookup; omit `VERSION` for latest-release semantics.
- Resolved digest surfaces in JSON output (`bootstrap.digest`; omitted when unpinned). Round-trips as a pin: `ocx self setup 0.9.2@<digest>`.
- ChainMode applies to pin resolution: `--frozen`/`--offline` + uncached tag → exit 81; digest-only pin works frozen when blobs cached.
- Exit 74 (`IoError`) — writing an env shim or shell profile failed (disk full, permission denied, etc.).
- `--no-modify-path` (or `OCX_NO_MODIFY_PATH` truthy) — writes env shims only, skips profile modification. `OCX_NO_MODIFY_PATH` is read through `ocx_lib::env::flag` + `BooleanString`: truthy = `1`/`y`/`yes`/`on`/`true`; falsy = `0`/`n`/`no`/`off`/`false`; unrecognised non-empty value → WARN + default (`false`). The opt-out is not remembered between runs.
- `--profile PATH` — override auto-detected profiles; repeatable. Explicit targets use POSIX-fence semantics regardless of file name.
- `--dry-run` — resolve but write nothing; reports `WouldPull` with resolved digest. Never returns exit 82.
- `--force` — overwrite a managed block that carries user edits (the dirty state).
- Exit 82 (`DirtyRcBlock`) — at least one profile contained a managed block with user edits and `--force` was not passed. Scripts: `case $? in 82)`.
- All `Self_` variants (including `self setup`) are in the `should_check_for_update` skip list.

**`self activate` notes:**
- `--shell` absent or bare → autodetect from `$SHELL`/parent process; exit 64 if undetectable. Differs from `ocx env --shell` where absent means "structured report path".
- Completions are emitted **inline** in the activation stream (no file): emitted first so PowerShell's `using namespace` leads the stream, which `Invoke-Expression` requires; zsh self-loads `compinit` before clap's trailing `compdef`. Gated by `options::Completion` (`crates/ocx_cli/src/options/completion.rs`): paired `--completion`/`--no-completion` flags (`overrides_with`, last-wins), then `OCX_NO_COMPLETIONS`, then `[shell] completions`, then an interactivity auto fallback. The accessor is `Completion::enabled(interactive: bool, configured: Option<bool>)`.
- Interactivity is a separate hidden flag pair, `--interactive`/`--no-interactive` (`options::Interactive`, `overrides_with`, last-wins), not the completion flags. Every `env.*` shim states it explicitly from the test its own shell language provides (`$-` on POSIX, `status is-interactive` on fish, `[Console]::IsInputRedirected` on pwsh, `test -t 0` on elvish) and passes it through, so the gate never depends on the binary probing a stderr every shim already redirects — and stdin is no better, since `ssh -t host 'bash -lc …'` hands a terminal to stdin for a shell that never renders a prompt. The auto fallback (`stdin.is_terminal() || stderr.is_terminal()`) serves only a direct `ocx self activate` with neither flag, or a shim from an unrefreshed `self setup`. The shim also selects the real sourcing shell (`bash`/`zsh`, never `sh` → `Dash`, which has no completion backend) so the correct extension is generated.
- `OCX_NO_COMPLETIONS=1` → suppress completion injection.
- `Self_` variants are in the `should_check_for_update` skip list — `self activate` runs on every shell start and must not trigger the background update-check.

**`self update` / `self update --check` notes:**
- Both always bypass the auto-check throttle (explicit user intent).
- Both list the newest published release live through the **configured index chain** (`TagProbe::Remote` → `Index::remote_view()`) — self update exists to reach the freshest upstream ocx, matching the `ocx self setup` bootstrap and the background auto-check (`app/update_check.rs`), *not* the local index a stale `ocx index update` snapshot would echo. Through the chain, never a bare registry tags API: `ocx.sh/ocx/cli` is a logical name the published index routes to a physical repository. `--offline` (no client) short-circuits to `Skipped(Offline)`; `--remote` is redundant (already the default) but still accepted.
- `--check` calls `self_check_update(Some(Duration::ZERO), TagProbe::Remote)` and reports without installing.
- Without `--check` calls `self_update()` which routes the install through `install_all`.
- A `sha256:` digest pin in `self setup` selects a platform-specific package digest; the same tag resolves to a different digest per OS/arch. For CI matrices, pin by tag (each runner resolves its own platform digest) rather than sharing a single digest across platforms.

### Patch Commands (`ocx patch`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `patch freeze` | Write `patches.snapshot.json` pinning companion + descriptor digests beside `ocx.lock` (or `$OCX_HOME/ocx.lock` under `--global`) | — |
| `patch sync [OPTIONS]` | Re-fetch every patch descriptor for all installed packages, install newly-referenced companions | `-p/--platform` |
| `patch publish --descriptor <FILE> [--global \| <BASE-ID>]` | Push a patch descriptor to the configured (or `--registry`) `[patches]` registry | `--descriptor`, `--global`, `--registry` |
| `patch test --descriptor <FILE> [OPTIONS] <BASE-ID> [-- CMD]` | Compose a descriptor onto a base locally without publishing (maintainer preview) | `--descriptor`, `--companion-archive`, `-p/--platform`, `--script`, `--registry`, `--env` |
| `patch why <BASE-ID>` | List which companion, and which descriptor rule, contributes each patched env var to a base | — |

**`patch` group notes:**
- Files: `crates/ocx_cli/src/command/patch.rs` (dispatcher) + `patch_{freeze,sync,publish,test,why}.rs` (one leaf per subcommand).
- Only `freeze` reads the root `context.global()` flag — without it, the snapshot lands beside the project's `ocx.lock`; with `--global` (root flag, before the subcommand), beside `$OCX_HOME/ocx.lock`. `sync`, `publish`, `test`, `why` never call `context.global()`.
- `publish`'s own `--global` (declared on `PatchPublishArgs`) is unrelated to the root toolchain flag: a subcommand-local selector for the reserved `global` descriptor repository, mutually exclusive with a `<BASE-ID>` positional.
- `publish`, `test`, `why` are registry/maintainer commands against the configured `[patches]` tier; none consult `ocx.toml`. `sync` re-checks whatever is installed locally, not scoped to one project.
- `publish` and `test` accept `--registry <HOST/PATH>` (via shared `patch_common::effective_patches`) to override — or stand in for a missing — `[patches]` tier, so a maintainer can bootstrap a brand-new patch registry without a config block. No configured tier and no `--registry` → usage error (exit 64).

### Managed-Config Commands (`ocx config`)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `config setup` | Adopt (or clear) the `[managed]` tier — config-only counterpart to `self setup --managed-config`; shares `ocx_lib::setup::apply_managed_config` + the CLI precedence seam `command/config_setup.rs::resolve_managed_config_arg` (flag > `OCX_MANAGED_CONFIG` > seed). Nothing resolved → exit 64 (unlike `self setup`'s no-op); dirty fence → exit 82 | `--managed-config REF`, `--dry-run`, `--force` |
| `config update [VERSION]` | Fetch + persist the managed-config snapshot (throttle-bypassing); `--check` probes only; `--pause`/`--resume` gate the background tick | `--check`, `--pause`, `--resume` |
| `config push -i ID CONFIG` | Operator-side publish of a `config.toml` as a managed-config package | `-i`, `-c/--cascade`, `-p` |
| `config test CONFIG` | Validate a managed-config payload locally — reports what a fleet would adopt (default registry, registries, mirrors, `[patches]`) plus this machine's `[managed]` posture. Unknown keys are warnings, never failures; keys inside a `[mirrors]` entry are not checked. Nothing is published, adopted or written; exit 78 when the file is not a publishable payload | — |

All `ConfigGroup` variants are exempt from the required-snapshot gate; `config setup`, `config update`, and `self setup` are the three onboarding commands that get a managed-fetch client with no seed present (`app.rs::is_managed_config_onboarding_command`).

**`config` group notes:**
- Files: `crates/ocx_cli/src/command/config.rs` (dispatcher) + `config_{setup,update,push}.rs` (one leaf per subcommand).
- The managed-config tier (`[managed]`) is fetched as an ordinary OCX package; `config push` is the operator-side publish, `config setup`/`config update` the consumer-side onboard/sync. See `adr_managed_config_tier.md`.

### Other Commands

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `login [REGISTRY]` | Authenticate to a registry (verifies credentials against the registry before storing; `--no-verify` opts out) | `-u/--username`, `--password-stdin`, `--verify`/`--no-verify`, `--allow-insecure-store` |
| `logout [REGISTRY]` | Remove stored credentials | — |
| `clean` | GC unreferenced objects | `--dry-run`, `--force` |
| `shell completion` | Generate shell completions | `--shell` |
| `shell allow [PATH]` | Write the consent stamp for the project governing PATH (default: cwd) — the explicit form of the grant the six mutating commands write as a side effect. Resolution reuses the CWD walk with PATH substituting for the CWD, so `allow` consents to exactly the project a prompt activates; `--project` / `--global` still take precedence. Exit 64 for no project at PATH, and exit 64 for `$OCX_HOME` (A-44 — the ocx home is always consented and never carries a stamp; the guard lives in `consent::record`, the CLI only reads its `Recorded` answer) | — |
| `shell revoke [PATH]` | Delete that stamp. Idempotent — revoking an absent stamp is exit 0 with a `nothing to revoke` line, never an error. Does not touch `[shell.consent] paths` / `namespaces`, which live in `config.toml` | — |
| `shell state` | Read-only, never-eval-able diagnostics for the per-prompt reconciler. Default text = the answer only (`$OCX_HOME`, project, active/inert verdict, enumerated reason, one-line `fix:`); `-v/--verbose` adds the evidence (decoded ledger + carrier bytes, fingerprint watch set, project key + stamp, hook ladder). `--verbose` is a rendering tier only — root `--format json` emits the complete report at either verbosity. Never installs, never mutates, never stamps consent (`adr_shell_env_overhaul.md` Decision 10, C-050) | `-v/--verbose` |
| `direnv init` | Write `.envrc` wiring `ocx direnv export` | `--force` |
| `direnv export` | Stateless bash export generator for direnv `.envrc`; installs on miss by default (best-effort — never fails the prompt), `--no-pull` stays strictly offline. `-g` selects groups (hand-edit the generated `.envrc` line); an unknown group or malformed `--env` exits 64 — argv faults fail loudly, toolchain-state faults do not | `-g/--group`, `--env`, `--pull/--no-pull` |
| `index catalog` | List known repositories | `--tags` |
| `index list PKGS...` | List tags for packages | `--platforms`, `--variants` |
| `index update PKGS...` | Merge the named packages' remote roots into the local index via `LocalIndex::refresh_tags` — per-tag dispatch object plus root document (never a leaf manifest, A3), so a version choice resolves fully offline afterwards. Tagged identifier = that tag only; bare = every remote tag plus package-level fields (routing). Never deletes a locally-known tag, never fetches anything about a package you did not name. At least one PACKAGE required (exit 64); aggregates any per-package failure to a single nonzero exit (first failure in input order, deterministic) | — |
| `index sync REGISTRIES...` | The whole-registry form of `index update`: each registry's own catalog (published) or repository listing (derived) names the packages, read live from the source, then refreshed through the *same* loop with a bare identifier each. At least one REGISTRY required (exit 64). Every registry is enumerated before any is refused, so one unreachable source does not cost the others their snapshot; an enumeration failure outranks a refresh failure in deciding the exit. `--dry-run` prints the set and refreshes nothing | `--dry-run` |
| `index regenerate REGISTRIES...` | Re-derive a published source's `c/index.json` from the `p/` walk — the one writer that can drop a catalog entry whose root document is gone. Consults no source. Refuses a derived (plain-OCI) namespace, which has no catalog document by grammar. Skips symlinked roots and directories, so a symlink-deduplicated layout loses every package reached through a link | `--dry-run` |
| `version` | Print version | — |
| `about` | Print version + registry + platform + shell + home | — |

### Deleted Commands (exit 64 if invoked)

These commands **do not exist** in the current model. Any invocation returns exit 64 (ocx maps clap usage errors → EX_USAGE 64; see `app.rs:112-119`):

| Deleted command | Replacement |
|-----------------|-------------|
| `ocx install` | `ocx package install` |
| `ocx uninstall` | `ocx package uninstall` |
| `ocx select` | `ocx package select` |
| `ocx deselect` | `ocx package deselect` |
| `ocx exec` | `ocx package exec` |
| `ocx which` | `ocx package which` |
| `ocx deps` | `ocx package deps` |
| `ocx ci` | Removed as a command; CI export is the `--ci[=PROVIDER]` flag on `ocx env` / `ocx package env` |
| `ocx shell hook` | Removed (login-shell activation via `$OCX_HOME/env.sh` + `ocx --global env --shell=sh`; per-prompt reconciliation for a consenting project now rides the emitted hook body's hidden `ocx self activate --reconcile` arm — `adr_shell_env_overhaul.md` Decision 3/5 — never a resurrected `ocx shell hook`) |
| `ocx shell init` | Removed (`ocx self setup` owns profile modification) |
| `ocx shell env` | `ocx env` (toolchain) or `ocx package env` (per-package) |

---

## `ocx env` vs `ocx package env`

| Want | Invocation | Output |
|------|------------|--------|
| Toolchain env, default | `ocx [--global] env` | plain table (context default) |
| Toolchain env, machine-readable | `ocx --format json [--global] env` | JSON |
| Toolchain env, eval-safe | `ocx [--global] env --shell[=NAME]` | Shell export lines |
| Per-package env, default | `ocx package env <ids...>` | plain table (context default) |
| Per-package env, machine-readable | `ocx --format json package env <ids...>` | JSON |
| Per-package env, eval-safe | `ocx package env <ids...> --shell[=NAME]` | Shell export lines |
| Either, CI sink (later-step) | `ocx [--global] env --ci=github` / `ocx package env <ids...> --ci=gitlab [--export-file PATH]` | GitHub two-file sink / GitLab JSON-lines |

Rules:
- `--env KEY[:TYPE]=VALUE` is on **both** tiers — it is a per-invocation override, not project configuration, so adding it to an OCI-tier command does not make that command read `ocx.toml`. `-g` stays project-tier only (no groups without a project file). `ocx env --env X` composes exactly what `ocx run --env X` executes with; same pairing for `package env` / `package exec`.
- `--shell` is the **only eval-safe form**. Plain/JSON are NOT sourceable.
- `eval "$(ocx env)"` is a user error. `eval "$(ocx env --shell=bash)"` is correct.
- `--shell=sh` ≡ `--shell=dash` (POSIX strict; `sh` is a `PossibleValue` alias on `Shell::Dash` — no new enum variant).
- `ocx package env` reuses `env.rs::execute` which auto-installs via `find_or_install_all` — deliberate (handshake §2).
- `--ci` writes to a CI persistence channel for **later** steps (vs `--shell` = current step). `--ci` ⟂ `--shell` (exit 64). `--ci=github` infers `$GITHUB_ENV`/`$GITHUB_PATH` (rejects `--export-file`, exit 64); `--ci=gitlab` writes JSON-lines to `--export-file` or stdout. Bare `--ci` autodetects (`$GITHUB_ACTIONS`/`$GITLAB_CI`); exit 64 if undetected. Explicit `--ci=github` outside GitHub Actions → exit 78. ADR `adr_ci_env_export_flag.md`.

---

## Task Method Quick Reference

| Manager Method | Auto-Install | Symlink | Use In |
|----------------|-------------|---------|--------|
| `find_all()` | No | No | `package which`, `package deps` |
| `resolver().build_graph()` | No | No | `package deps` |
| `find_symlink_all(kind)` | No | Yes (candidate/current) | `package which --candidate` |
| `find_or_install_all()` | **Yes** | No | `package env`, `package exec` |
| `install_all(candidate=true)` | N/A | Creates candidate | `package install` |
| `install_all(candidate=false)` | N/A | No | `package pull` |
| `deselect_all()` | No | Removes current | `package deselect` |
| `uninstall_all()` | No | Removes candidate | `package uninstall` |
| `inspect_all()` | No | No | `package inspect` |
| `select_all()` | No | Sets current | `package select` |
| `clean()` | No | — | `clean` |

---

## Semantics & Gotchas

- **`ocx run` semantics** — `--` mandatory, exit 64 if missing (ocx maps clap usage errors → EX_USAGE 64); default scope = `[tools]` only; `ocx --global run` = compose global toolchain env for child only, never mutates parent; `ocx run` (no `--global`) never reads `$OCX_HOME/ocx.toml`; missing `ocx.toml` → exit 64; missing `ocx.lock` → exit 78.
- **`ocx run NAME` scopes host-leaf resolution** — `-g` selects the *namespace* for name resolution, not a mandate that every tool in it be available. The phases split selection from resolution: `select_tool_set` (resolution-free) runs whole-scope duplicate-across-groups validation; `filter_by_names` narrows to the requested NAMEs; `resolve_selected_tools` resolves host leaves for the named subset ONLY. A `NoHostLeaf` (exit 78) on an unrelated, unnamed sibling no longer aborts a narrowly-named run; an unnamed run (`ocx run -- …`) still resolves the whole scope. Duplicate-across-groups validation stays whole-scope regardless of what is named.
- **`ocx env` output format is a context-only concern** — root `--format` (default plain, same as every command); no subcommand `--format`; no env-specific JSON default (handshake §3 amended 2026-05-19, reversing the original backend-first JSON default). JSON via `ocx --format json env`. Plain and JSON are both NOT sourceable; `--shell[=NAME]` only eval-safe channel.
- **`package env` auto-installs** — `ocx package env` uses `find_or_install_all` (unlike the deleted `shell env` which used `find_all`). Do NOT assert no-download semantics against `ocx package env`. **`ocx shell state` is the one command family exception** — read-only diagnostics only, it never installs, never mutates, never stamps consent, and is a named non-member of the six-writer `state/projects/<key>/` allowlist (`adr_shell_env_overhaul.md` C-050).
- **Root `ocx env` auto-installs on miss by default** — the project tier runs the batched `find_or_install_all` (a present lock-pinned tool resolves locally with no network; only a genuine miss pulls). `--no-pull` opts out: it probes the store through an offline `PackageManager` clone (`offline_view` + `find`), warns on stderr (`run \`ocx pull\``) + omits a not-materialised tool, exit 0, never touching the registry (shared `options::Pull` flatten, **eager default — same as `add`/`lock`/`update`**; `direnv export` shares the same pair, best-effort pull so a prompt never fails). The global tier never installs regardless.
- **`login`/`logout` registry optional** — falls back to `OCX_DEFAULT_REGISTRY` (default `ocx.sh`).
- **`logout` is always exit 0** — matches `docker`/`oras`/`helm`; CI cleanup must not fail.
- **`--password VALUE` does not exist** — argv-visible secrets leak via `ps`. Use `--password-stdin`.
- **`index list <pkg>@<digest>` rejected** — tag-only identifiers still work.
- **`index update` / `index sync` aggregation** — per-package tag refreshes run concurrently through one shared bounded fan-out (`command/index_common.rs`, `buffer_unordered(INDEX_REFRESH_CONCURRENCY)`; the earlier `JoinSet` was unbounded); on any failure the command returns the failure with the lowest input index (sorted, not completion order) as the process error, so the exit code is deterministic across repeated runs. A failing tag does not discard the rest of its own package's work: only that tag — and any other tag sharing its content digest — is withheld from the commit, every other tag in the package's root is still pinned, and a package with no failing tag keeps every tag untouched. `index sync` adds one rule on top: an **enumeration** failure outranks a refresh failure, since that registry contributed no work at all.
- **`shell hook` vs `direnv export` vs the emitted per-prompt hook** — the `ocx shell hook` *command* is deleted and stays deleted; `direnv export` is the stateless bash export generator for direnv `.envrc` (still alive, untouched). Per-prompt reconciliation for a consenting project is a **different** mechanism: `ocx self setup` emits a hook body into the shell profile that invokes `ocx self activate`'s hidden `--reconcile` arm — never `ocx shell hook`, never a new command (`adr_shell_env_overhaul.md` Decision 3/5). `ocx shell state` is the read-only diagnostic for that mechanism, not the hook itself.
- **`package test` tempdir lifecycle** — without `--keep` or `--output`, temp dir deleted on any exit. `--keep` + `--output` are mutually exclusive.
- **`launcher exec` internal subcommand** — hidden from `--help`. Wire ABI: `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Forces `self_view=true`. Resolves `${installPath}` in baked entrypoint `args` and prepends them before user args (wire ABI unchanged).
- **`package verify` trust policy (#98)** — when the `--certificate-identity`/`--certificate-oidc-issuer` flags are omitted, verify resolves an identity from `[[trust.policy]]` under **cross-tier precedence** (`trust::resolve_tiered`): the operator `config.toml` set (system/user/`$OCX_HOME`, array-appended) is authoritative — if any operator policy matches the target, the project `ocx.toml` is ignored for it; the `ocx.toml` only ADDS trust for scopes no operator policy governs and can never override an operator pin. Within the chosen tier: most-specific scope wins, ANY-of among equal. Flags override policy when both are given; one flag alone → exit 64. No matching policy and no flags → exit 64 (`NoIdentityProvided`); a matched-but-malformed policy → exit 78 (`TrustPolicyInvalid`). Reading `ocx.toml` here is the **one documented OCI-tier carve-out**, scoped SOLELY to `[[trust.policy]]` — never package resolution (ADR `adr_trust_policy.md`).
