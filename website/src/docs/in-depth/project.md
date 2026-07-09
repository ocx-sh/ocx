---
outline: deep
---
# Project Toolchain

A repository's contributors and CI runners need the same tool versions — `cmake 3.28`, `shellcheck 0.11`, `goreleaser 2.0` — without arguing over chat or curl-piping installers. Earlier reproducibility mechanisms ([digest pin][in-depth-versioning-locking], [snapshot pin][in-depth-indices-local], [bundled snapshot][in-depth-indices-bundled]) describe how a *single* invocation freezes its inputs; none of them describe what the *project itself* expects.

A committed [`ocx.toml`][project-toml] plus its sibling [`ocx.lock`][project-lock] closes that gap. The pair makes "the tools this project needs" a piece of source code — reviewable in pull requests, mergeable across branches, reproducible across machines, and resolvable offline once the lock is fetched. The user-facing surface — `ocx init`, `ocx add`, `ocx lock`, `ocx pull`, `ocx run` — lives in the [project section of the user guide][user-project]. This page explains the file formats, the locking contract, the group resolution model, and what reproducibility guarantees actually ship today.

## Declaring tools — `ocx.toml` {#toml}

`ocx.toml` lives at the root of a repository (or anywhere up the directory tree from `cwd`). It is a [TOML][toml] file with a single `[tools]` table mapping local binding names to fully-qualified [OCI identifiers][oci-identifier]:

```toml
[tools]
cmake    = "ocx.sh/cmake:3.28"
ripgrep  = "ocx.sh/ripgrep:14"
mytool   = "ghcr.io/acme/mytool:1.0"
```

Each value is `registry/repo[:tag][@digest]`. Bare-tag forms like `cmake = "3.28"` are rejected at parse time so the file is unambiguous regardless of which registry the user has configured as a default. The binding name on the left is independent of the repository path — `mytool = "ghcr.io/acme/mytool:1.0"` is fine and lets internal projects rename tools without touching the registry.

A registry-qualified entry without a tag — `cmake = "ocx.sh/cmake"` — defaults to `:latest` at parse time, the same convention `docker pull` and OCI tooling apply to bare repository references. Digest-pinned entries (`tool = "ghcr.io/acme/tool@sha256:…"`) keep their canonical pin and never get a tag injected.

The schema is published at [`https://ocx.sh/schemas/project/v1.json`][schema-project] and wired through [taplo][taplo] for editor auto-completion. With `taplo` installed and an `ocx.toml` open in [Helix][helix], [VSCode][vscode-taplo], or [Neovim][neovim-taplo-lsp], unknown fields surface as red squiggles and tool names complete as you type.

::: info Comparable tools, different scope
[mise][mise] and [asdf][asdf] manage what is *installed on a developer's workstation*. `ocx.toml` plus `ocx.lock` cryptographically pins what a *repository requires* — including private OCI registry tools that mise/asdf have no plugin for. The two layers compose: a contributor may use mise for global Node/Python versions and `ocx.toml` for the repo-scoped binaries.
:::

::: warning Avoid global state in `ocx.toml`
The project file describes tools the project needs, not how a contributor's shell prompt should behave. Shell-profile state (`PATH` munging, sentinel env vars, "load on every prompt") stays in the user-tier mechanisms — see [activation](#activation) for how the project tier hooks into a developer's shell without spilling into `ocx.toml`.
:::

## Locking — `ocx.lock` {#lock}

`ocx.toml` declares advisory tags; the registry resolves those tags to immutable digests. To make the project reproducible, [`ocx lock`][cmd-lock] resolves every tag once and writes the result to `ocx.lock` next to `ocx.toml`. Subsequent commands read the lock, never the registry, so two machines running [`ocx pull`][cmd-pull] from the same commit get the same bytes.

The lock carries a `declaration_hash` over the canonicalized `ocx.toml` ([RFC 8785 JCS][rfc-8785]). When you change `ocx.toml`, the hash changes; commands that depend on the lock ([`ocx pull`][cmd-pull], [`ocx run`][cmd-run]) detect the mismatch and refuse to run with stale digests. Re-run [`ocx lock`][cmd-lock] to regenerate the file.

### Lock format — per-platform leaf digests {#lock-format}

Each `[[tool]]` entry in `ocx.lock` records the bare registry/repository coordinates (no tag, no digest) and a `[tool.platforms]` table mapping each platform the publisher ships to its per-platform leaf manifest digest:

```toml
[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"           # registry/repo only — no tag, no digest

[tool.platforms]
"linux/amd64"  = "sha256:<leaf-amd64>"
"linux/arm64"  = "sha256:<leaf-arm64>"
"darwin/arm64" = "sha256:<leaf-darwin-arm64>"
# darwin/amd64, windows/amd64 absent — publisher ships no such leaf
```

Only the platforms the publisher ships are recorded: a platform absent from the map means the publisher does not ship it at the locked version. `ocx lock` records every shipped platform at once, regardless of which OS the command runs on, so a lock committed on Linux is already complete for macOS and Windows CI runners — no re-resolution or re-commit is required on a different machine.

At install and run time, OCX looks up the host platform key in `[tool.platforms]`, falling back to the `"any"` key for packages that ship a single platform-independent binary. A host key that is absent — and has no `"any"` fallback — produces a clean pre-network error naming the missing platform and pointing to `ocx update <tool>` to re-lock if the publisher has since added support.

### Adding and dropping platforms {#lock-platforms-lifecycle}

A newly-shipped platform becomes available after an explicit `ocx update <tool>`, which re-resolves the tag and adds the new platform key to the map. A dropped platform disappears as a removed key on the next re-lock. Both changes are visible as a plain-text diff to `ocx.lock` in pull-request review. There is no silent pickup at install time — the lock is the contract.

::: warning `ocx.lock` is machine-generated
Do not hand-edit `ocx.lock`. The format is canonicalized — sort order, whitespace, and the file-level `declaration_hash` are computed by `ocx lock` and may evolve across OCX versions. Manual edits will be overwritten on the next `ocx lock` run and may be rejected by future schema versions.

Tooling that reads `ocx.lock` should validate against the published schema at [`https://ocx.sh/schemas/project-lock/v2.json`][schema-lock]; the schema's top-level `$comment` field carries a machine-readable do-not-edit marker recognizable to JSON Schema processors.
:::

::: tip Commit `ocx.lock` and tame merge conflicts
`ocx.lock` is what makes the project reproducible — without it, every contributor and CI runner re-resolves advisory tags against whatever the registry surfaces today. Commit it alongside `ocx.toml`. To keep merge conflicts manageable on busy projects, add a [`.gitattributes`][gitattributes] entry that lets `git` union sibling lock entries instead of choking on overlapping diff hunks:

```text
ocx.lock merge=union
```

Each `[[tool]]` entry is independent, so concatenation usually produces a syntactically valid lock; running `ocx lock` after the merge normalizes sort order and deduplicates.
:::

### Concurrent writes {#lock-concurrency}

Project-state writes (`ocx lock`, `ocx upgrade`, `ocx add`, `ocx remove`) serialize through an exclusive advisory [flock][flock] taken in-place on `ocx.toml` itself. No sentinel or sidecar file is created — the lock is invisible and leaves no artefact on disk. Concurrent readers ([`ocx pull`][cmd-pull], IDE integrations, `git`) never acquire any lock: they parse `ocx.lock` directly via an atomic read.

## Pin preservation {#pin-preservation}

`ocx add` and `ocx remove` are **partial mutators** — they touch only the binding they name and carry every other lock entry forward unchanged. Neither command re-resolves a surviving tool's live tag. This is the guarantee that adding a new tool or dropping an old one never silently advances the versions of everything else.

The carry-forward has two modes depending on the lock format of the surviving entry. A V2 entry is passed through byte-identical — no registry contact. A V1 (legacy) entry is transcribed using the pinned index digest it already stores: OCX reads the exact same index manifest and extracts its per-platform leaf digests, producing a V2 entry with the identical pins. If the V1 index is no longer retrievable from the registry, the command fails with exit 78 and a message directing you to `ocx upgrade` — it never silently re-resolves against the live tag.

The freshness gate runs before any carry-forward. If `ocx.toml` drifted from `ocx.lock` since the lock was last written (the `declaration_hash` does not match), the mutator fails with exit 65 before touching anything. The fix is a single `ocx lock` to reconcile the file, after which the add or remove succeeds.

The two commands that intentionally advance version pins are:

| Command | When it re-resolves |
|---------|---------------------|
| `ocx lock` | Only when `ocx.toml` drifted (whole-file reconcile; a moving tag may advance) |
| `ocx upgrade` | Whole file by default; `-g GROUP` / `NAME` scopes it to a named subset (those advance, the rest stay frozen) |

Groups are primarily a **composition concern** — they scope which tools `ocx run`, `ocx env`, and `ocx pull` see. `ocx lock` ignores them and always reconciles the whole file. `ocx upgrade` is the exception: passing `-g GROUP` or a binding `NAME` advances only that subset and carries every other pin forward verbatim, just like `ocx add` and `ocx remove` do for the bindings they touch.

## Pulling and executing {#pull-exec}

Once `ocx.lock` exists, two commands cover the bulk of day-to-day use. [`ocx pull`][cmd-pull] pre-warms the [package store][in-depth-storage-packages] from the lock without creating install symlinks — ideal for CI matrix builds and developer machines that already have a [direnv][direnv] hook in place. [`ocx run`][cmd-run] spawns a child with the project's resolved environment, treating the lock as the source of truth (project-tier counterpart to OCI-tier [`ocx exec`][cmd-exec]).

Both gate on the lock's `declaration_hash`: if `ocx.toml` has changed since the lock was generated, the command exits with a structured error pointing at [`ocx lock`][cmd-lock]. There is no implicit re-resolution — the project file is the input, the lock file is the contract, and registry round-trips happen only when you ask for them.

## Running tools {#running}

Once `ocx.lock` is current, [`ocx run`][cmd-run] spawns a child process whose environment is composed from the lock's resolved tool set. It is the project-tier counterpart to the OCI-tier [`ocx exec`][cmd-exec]: the same child-spawn mechanics, but symbols are binding names from `ocx.toml` rather than OCI identifiers.

### Argument shape {#running-shape}

```shell
ocx run [-g GROUP[,GROUP,...]]... [NAME...] -- ARGV...
```

`--` is mandatory. At least one token after `--` is required. A user typing `ocx run cmake` (no `--`) receives exit 64. A user typing `ocx run -- echo hi` (no NAME) composes every binding in the default scope and executes `echo hi` in the resulting environment.

### Scope semantics {#running-scope}

The scope controls which groups contribute to the composed environment.

| Invocation | Scope |
|---|---|
| `ocx run -- CMD` | Default group (`[tools]`) only — matches `ocx pull` precedent |
| `ocx run -g ci -- CMD` | `[group.ci]` only |
| `ocx run -g ci,release -- CMD` | `[group.ci]` and `[group.release]` |
| `ocx run -g all -- CMD` | `[tools]` + every declared `[group.*]` |
| `ocx run cmake -- CMD` | Default scope, then filter to the `cmake` binding only |

`-g all` is the "everything" form. Omitting `-g` does not imply everything — it means the default group. The `all` keyword is reserved: `[group.all]` in `ocx.toml` is rejected at parse time (exit 78); `ocx add --group all` is rejected at mutate time (exit 64).

### Composition order rule {#running-composition-order}

> First by group-selection order (the order of `-g` flags after `all` expansion, deduplicated); then alphabetical by binding name within each group.

This rule determines iteration order through the resolved tool set. The composer applies env entries by **prepending**, so the **last tool walked** has its PATH entries placed **first** on the resolved PATH at runtime. In other words: in `-g` argument order, **groups listed later win** PATH lookup. This matches the load-bearing prepend invariant in `composer.rs` ([source][composer-source]) — entries pushed later in iteration land first on PATH.

`all` expansion inserts groups alphabetically by group name in place of `all` in the `-g` argument list, after the default group. So `ocx run -g ci,all,release` expands to `[ci, default, ci_alpha_ordered_named_groups..., release]` and then `compose_tool_set` deduplicates.

### PATH precedence consequence {#running-path-precedence}

Two groups may declare different bindings whose installed packages happen to ship a binary with the same filename. The group listed **last** in `-g` order controls which binary appears first in PATH.

Concrete example: `[tools]` declares `cmake = "ocx.sh/cmake:3.28"` (ships `cmake`). `[group.ci]` declares `toolchain = "ocx.sh/some-toolchain:1"` (also ships a `cmake` binary). Running:

```shell
ocx run -g default,ci -- cmake --version
```

resolves `cmake` from `[group.ci]`'s `toolchain` — `ci` is iterated last, so its PATH entries land first on the child's PATH. Running `-g ci,default` flips the order: `default`'s `cmake` lands first instead. There is no error for this case: the two bindings are different names (`cmake` vs `toolchain`), so `compose_tool_set` composes them both; the group listed **later** in `-g` wins PATH lookup.

For direct binding-name conflicts — same `(group, name)` key with different identifiers — `compose_tool_set` returns `DuplicateToolAcrossSelectedGroups` (exit 64) before any spawn occurs.

### Exit codes {#running-exit-codes}

| Code | Condition |
|------|-----------|
| *(child)* | Child process ran; its exit code is forwarded byte-for-byte |
| 0 | Child exited 0 |
| 1 | Child spawn failed (binary not found, exec errno) |
| 64 | `--` missing; empty argv; empty `-g` segment; no `ocx.toml` found; unknown `-g` group; unknown NAME; ambiguous NAME across groups |
| 65 | `ocx.lock` is stale (run `ocx lock`) |
| 69 | Registry unreachable during auto-install |
| 78 | `ocx.lock` missing (run `ocx lock`); or `ocx.toml` parse error |
| 79 | Package not found in registry during auto-install |
| 80 | Authentication failure during auto-install |

Exit codes 64 and 78 for clap-level failures: OCX remaps clap's default exit 2 to 64 (`UsageError`) for consistency with all other project-tier usage errors.

### Layer purity {#running-layer-purity}

`ocx run` never falls back to OCI-tier behavior. If `ocx.toml` is absent, it exits 64 rather than re-parsing the NAME arguments as OCI identifiers. This makes the behavior stable across directory changes and prevents embedding scripts from silently switching contracts.

`ocx exec` remains unchanged — it never consults `ocx.toml` even when one is present.

## Groups {#groups}

Not every contributor needs every tool. CI needs `shellcheck` and `shfmt`; the release pipeline needs `goreleaser`; daily development needs neither. Named groups let `ocx.toml` describe these subsets without forcing every workstation to download release tooling on first checkout:

```toml
[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci]
shellcheck = "ocx.sh/shellcheck:0.11"
shfmt      = "ocx.sh/shfmt:3.7"

[group.release]
goreleaser = "ocx.sh/goreleaser:2.0"
```

The top-level `[tools]` table is the implicit `default` group; named `[group.<name>]` tables add to it. `[group.default]` is reserved and produces a parse error — there is no ambiguity between "implicit default" and "named default."

Pass `--group` (repeatable, comma-separated) to scope a command:

```shell
ocx pull -g ci,lint               # CI runner — only what's needed for lint jobs
ocx pull -g release               # release runner — only release tools
ocx lock                          # workstation — every group resolved
```

### Per-group binding identity {#groups-binding-identity}

The same binding name may appear in the default `[tools]` table and in any named `[group.*]` table simultaneously — the identity of a binding is `(group, name)`, not `name` alone. This lets a project pin one version of a tool for daily workstation use and a different version in `ci` without conflict:

```toml
[tools]
shfmt = "ocx.sh/shfmt:3.7"       # workstation default

[group.ci]
shfmt = "ocx.sh/shfmt:3.13"      # CI: pinned to a newer build
```

When a binding name is unambiguous (present in exactly one group), [`ocx remove shfmt`][cmd-remove] finds it automatically. When the same name exists in multiple groups, pass `--group` to disambiguate:

```shell
ocx remove shfmt                  # ok — unambiguous (only in [tools])
ocx remove --group ci shfmt       # ok — removes from [group.ci] only
ocx remove shfmt                  # error — ambiguous (exists in [tools] and [group.ci])
```

Without `--group`, an ambiguous remove exits with code 64 and names every group that holds the binding.

## Activation {#activation}

A project's tools should be on `PATH` whenever you `cd` into the project — without `eval`-ing anything on every shell startup. OCX ships two entry points for project activation.

The hooks only export variables — they never install missing tools, never contact the registry, and never mutate the [package store][in-depth-storage-packages]. Run [`ocx pull`][cmd-pull] first to materialize anything `ocx.lock` requires.

[`ocx direnv export`][cmd-direnv-export] is the [direnv][direnv] entry point. It is stateless — it emits a fresh export block on every invocation. [direnv][direnv] supplies the cache layer (one re-evaluation per `cd`, watched files re-trigger), so the hook stays simple. Run [`ocx direnv init`][cmd-direnv-init] in a project directory to drop a ready-made `.envrc`, then `direnv allow`.

For scripted environments and CI, call [`ocx run`][cmd-run] directly — it composes the project toolchain env and spawns the target command without any persistent shell state.

::: tip One entry point per workflow
[direnv][direnv] users want `ocx direnv`. Scripted environments and CI use `ocx run` or `ocx pull` + `ocx package env`. There is no per-prompt shell hook — global toolchain activation uses `$OCX_HOME/env.sh` (written by the installer), not a prompt hook.
:::

## Global toolchain {#global-toolchain}

A user-wide `ocx.toml` at [`$OCX_HOME`][env-ocx-home]`/ocx.toml` (default `~/.ocx/ocx.toml`) holds tools that should be available in every shell — `ripgrep`, `cmake`, `shellcheck` — without being part of any specific project. This is the global toolchain tier, activated explicitly via the [`--global`][cmd-global-flag] flag or the [`OCX_GLOBAL`][env-ocx-global] environment variable.

The global file uses the same [schema][schema-project] and lock semantics as a project file. The lock lives at `$OCX_HOME/ocx.lock`. Unlike the old home-tier fallback, the global toolchain is **never discovered implicitly** — the CWD walk does not activate it. You must pass `--global` or set `OCX_GLOBAL`.

::: warning Global and project tools are isolated by PATH precedence
`ocx run` and `ocx exec` are always hermetic: the global toolchain is never consulted during project-tier resolution. Global tools remain on `PATH` (there is no strip), but project-declared tools are **prepended** by the active hook, so they shadow any same-named global tools. See [Strict isolation][env-composition-strict-isolation] for the full model.
:::

For managing global tools day-to-day, see [Keep everyday tools available everywhere][user-guide-global] in the user guide. To opt out of project-tier discovery entirely for a single invocation, set [`OCX_NO_PROJECT=1`][env-no-project].

## Multi-project retention {#multi-project-retention}

When multiple projects on the same machine pin different package versions, [`ocx clean`][cmd-clean] retains every package referenced by *any* registered project's lockfile — not just the active project. A developer with project A and project B can run `ocx clean` from project B without losing packages that only project A's `ocx.lock` pins.

OCX tracks registered projects automatically in a flat symlink ledger at `$OCX_HOME/projects/` — one symlink per project, named after the SHA-256 hash of the project's canonical absolute path, pointing at the project directory. The ledger is updated whenever [`ocx lock`][cmd-lock] runs in a project directory. You should not edit it manually. It is browsable with `ls -l $OCX_HOME/projects/`.

If you intentionally want to collect packages held only by other projects' lockfiles — for example, after removing a project from your machine — pass `--force` to bypass the registry: `ocx clean --force`. Live install symlinks are always honoured regardless of `--force`.

## Reproducibility and SLSA {#reproducibility}

OCX v1 ships digest-pinning reproducibility: every tool a project resolves is identified by its OCI manifest digest, and the lock file commits that digest under a hash of the source `ocx.toml`. Any consumer with the lock can verify they are pulling exactly the bytes the project committed to — no tag races, no silent registry rewrites.

What is not yet shipped is a signed build attestation describing how each tool was produced. That capability — the kind of [SLSA build provenance][slsa-l1] producers can generate via [Sigstore][sigstore] or similar — is deferred to v2. Treat OCX v1 as solid input integrity, not as compliance with any [SLSA level][slsa-attestation].

In practice, the v1 contract is sufficient for the most common reproducibility needs: locking a CI matrix to known-good binaries, surviving registry mutability incidents, and ensuring contributors review tool upgrades the same way they review code changes. v2 will add the cryptographic chain that links published digests to verifiable build pipelines.

## See Also

- [User guide → Pin a project's tools][user-project] — task-driven overview.
- [User guide → Run tools from your project][user-run] — quick-start examples for `ocx run`.
- [User guide → Keep everyday tools available everywhere][user-guide-global] — global toolchain use-case narrative.
- [Environment Composition reference][env-composition-strict-isolation] — reference-level statement of the strict isolation rule.
- [Indices In Depth][in-depth-indices] — how `ocx pull` reads the lock and where the registry round-trips happen.
- [Storage In Depth → Garbage collection][in-depth-storage-gc] — how project-lock back-references protect packages.
- [Configuration In Depth][in-depth-configuration] — discovery tier rationale, `OCX_NO_PROJECT` kill switch.

<!-- external -->
[toml]: https://toml.io/
[oci-identifier]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[taplo]: https://taplo.tamasfe.dev/
[helix]: https://helix-editor.com/
[vscode-taplo]: https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml
[neovim-taplo-lsp]: https://github.com/neovim/nvim-lspconfig/blob/master/lua/lspconfig/configs/taplo.lua
[direnv]: https://direnv.net/
[gitattributes]: https://git-scm.com/docs/gitattributes
[rfc-8785]: https://www.rfc-editor.org/rfc/rfc8785
[flock]: https://man7.org/linux/man-pages/man2/flock.2.html
[mise]: https://mise.jdx.dev/
[asdf]: https://asdf-vm.com/
[slsa-l1]: https://slsa.dev/spec/v1.0/levels#build-l1
[slsa-attestation]: https://slsa.dev/spec/v1.0/attestation-model
[sigstore]: https://www.sigstore.dev/
[schema-project]: https://ocx.sh/schemas/project/v1.json
[schema-lock]: https://ocx.sh/schemas/project-lock/v2.json
[composer-source]: https://github.com/ocx-sh/ocx/blob/main/crates/ocx_lib/src/package_manager/composer.rs

<!-- commands -->
[cmd-clean]: ../reference/command-line.md#clean
[cmd-exec]: ../reference/command-line.md#exec
[cmd-global-flag]: ../reference/command-line.md#global-flag
[cmd-lock]: ../reference/command-line.md#lock
[cmd-pull]: ../reference/command-line.md#pull
[cmd-remove]: ../reference/command-line.md#remove
[cmd-run]: ../reference/command-line.md#run
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[cmd-direnv-init]: ../reference/command-line.md#direnv-init

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-global]: ../reference/environment.md#ocx-global
[env-no-project]: ../reference/environment.md#ocx-no-project

<!-- internal anchors -->
[project-toml]: #toml
[project-lock]: #lock

<!-- cross-page -->
[user-project]: ../user-guide.md#project
[user-run]: ../user-guide.md#run
[user-guide-global]: ../user-guide.md#global-toolchain
[env-composition-strict-isolation]: ../reference/env-composition.md#strict-isolation
[in-depth-versioning-locking]: ./versioning.md#locking
[in-depth-indices]: ./indices.md
[in-depth-indices-local]: ./indices.md#local
[in-depth-indices-bundled]: ./indices.md#bundled
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-storage-gc]: ./storage.md#gc
[in-depth-configuration]: ./configuration.md
