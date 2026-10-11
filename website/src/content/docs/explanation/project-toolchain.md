---
title: "Project Toolchain"
---
<!-- doc_type: explanation -->

A repository's contributors and CI runners need the same package versions — `cmake 3.28`, `shellcheck 0.11`, `goreleaser 2.0` — without arguing over chat or curl-piping installers. Earlier reproducibility mechanisms ([digest pin][in-depth-versioning-locking], [snapshot pin][in-depth-indices-local], [bundled snapshot][in-depth-indices-bundled]) describe how a *single* invocation freezes its inputs. None of them describe what the *project itself* expects.

A committed [`ocx.toml`][project-toml] plus its sibling [`ocx.lock`][project-lock] closes that gap. The pair makes "the packages this project needs" a piece of source code. It is reviewable in pull requests, mergeable across branches, reproducible across machines, and resolvable offline once the lock is fetched. The user-facing surface — `ocx init`, `ocx add`, `ocx lock`, `ocx pull`, `ocx exec` — lives in the [project section of the user guide][user-project]. This page explains the file formats, the locking contract, the group resolution model, and what reproducibility guarantees actually ship today.

## Declaring bindings — `ocx.toml` {#toml}

`ocx.toml` lives at the root of a repository (or anywhere up the directory tree from `cwd`). It is a [TOML][toml] file with a single `[tools]` table mapping local binding names to fully-qualified [OCI identifiers][oci-identifier]:

```toml
[tools]
cmake    = "ocx.sh/kitware/cmake:3.28"
ripgrep  = "ocx.sh/ripgrep/ripgrep:14"
mytool   = "ghcr.io/acme/mytool:1.0"
```

Each value is `registry/repo[:tag][@digest]`. Bare-tag forms like `cmake = "3.28"` are rejected at parse time so the file is unambiguous regardless of which registry the user has configured as a default. The binding name on the left is independent of the repository path — `mytool = "ghcr.io/acme/mytool:1.0"` is fine and lets internal projects rename packages without touching the registry.

A registry-qualified entry without a tag — `cmake = "ocx.sh/kitware/cmake"` — defaults to `:latest` at parse time, the same convention `docker pull` and OCI tooling apply to bare repository references. Digest-pinned entries (`tool = "ghcr.io/acme/tool@sha256:…"`) keep their canonical pin and never get a tag injected.

A project can also split its tools into named groups. [Project Groups][project-groups] explains how a group scopes a command and why a binding is identified by its group and name.

The schema is published at [`https://ocx.sh/schemas/project/v1.json`][schema-project] and wired through [taplo][taplo] for editor auto-completion. With `taplo` installed and an `ocx.toml` open in [Helix][helix], [VSCode][vscode-taplo], or [Neovim][neovim-taplo-lsp], unknown fields surface as red squiggles and binding names complete as you type.

:::note[Comparable tools, different scope]
[mise][mise] and [asdf][asdf] manage what is *installed on a developer's workstation*. `ocx.toml` plus `ocx.lock` cryptographically pins what a *repository requires* — including private OCI registry packages that mise/asdf have no plugin for. The two layers compose: a contributor may use mise for global Node/Python versions and `ocx.toml` for the repo-scoped binaries.
:::

:::caution[Avoid global state in `ocx.toml`]
The project file describes packages the project needs, not how a contributor's shell prompt should behave. Shell-profile state (`PATH` munging, sentinel env vars, "load on every prompt") stays in the user-tier mechanisms. See [activation](#activation) for how the project tier hooks into a developer's shell without spilling into `ocx.toml`.
:::

## Locking — `ocx.lock` {#lock}

`ocx.toml` declares advisory tags; the registry resolves those tags to immutable digests. To make the project reproducible, [`ocx lock`][cmd-lock] resolves every tag once and writes the result to `ocx.lock` next to `ocx.toml`. Subsequent commands read the lock, never the registry, so two machines running [`ocx pull`][cmd-pull] from the same commit get the same bytes.

The lock carries a `declaration_hash` over the canonicalized `ocx.toml` ([RFC 8785 JCS][rfc-8785]). When you change `ocx.toml`, the hash changes; commands that depend on the lock ([`ocx pull`][cmd-pull], [`ocx exec`][cmd-run]) detect the mismatch and refuse to run with stale digests. Re-run [`ocx lock`][cmd-lock] to regenerate the file.

### Lock format — per-platform leaf digests {#lock-format}

`lock_version = 3` is the only lock format this build reads or writes — see [Platforms][reference-platforms] for the canonical grammar its keys use. A `[metadata]` header carries the format version, a `declaration_hash` over the canonicalized `ocx.toml`, and provenance (`generated_by`, `generated_at`). Each `[[tool]]` entry records the bare registry/repository coordinates (no tag, no digest). A `[tool.platforms]` table maps each platform the publisher ships to its per-platform leaf manifest digest:

```toml
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:<hex>"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/kitware/cmake"           # registry/repo only — no tag, no digest

[tool.platforms]
"linux/amd64"  = "sha256:<leaf-amd64>"
"linux/arm64"  = "sha256:<leaf-arm64>"
"darwin/arm64" = "sha256:<leaf-darwin-arm64>"
# darwin/amd64, windows/amd64 absent — publisher ships no such leaf
```

Only the platforms the publisher ships are recorded: a platform absent from the map means the publisher does not ship it at the locked version. `ocx lock` records every shipped platform at once, regardless of which OS the command runs on. A lock committed on Linux is already complete for macOS and Windows CI runners. No re-resolution or re-commit is required on a different machine.

Every key in `[tool.platforms]` must be the canonical grammar spelling of the platform it parses to (`+a,a` or unsorted `+b,a` are rejected, not silently normalized). No two keys may parse to the same platform. Both are checked on load and on every write, so the file's bytes stay the unique canonical form. Two distinct keys pointing at the *same* digest is fine and expected (see [Shared Digests][reference-platforms-shared-digests]); only key-level duplication is rejected.

At install and run time, OCX resolves the host against `[tool.platforms]` through the same [directed compatibility relation][reference-platforms-compatibility] `Index::select` uses for a fresh install. A bare `linux/amd64` host can match a `linux/amd64+libc.glibc` entry, and a package that ships only `"any"` matches every host. A host with no compatible entry at all produces a clean pre-network error naming the missing platform. It points to `ocx update <name>` to re-lock if the publisher has since added support.

:::caution[`ocx.lock` version 1 and 2 are rejected outright]
A lock written by an older OCX release fails to load with `unsupported ocx.lock version N;
regenerate with \`ocx lock\`` (exit 78). There is no bridged read and no partial support. Run
[`ocx lock`](#lock) to regenerate it against the current registry state.
:::

### Adding and dropping platforms {#lock-platforms-lifecycle}

A newly-shipped platform becomes available after an explicit `ocx update <name>`, which re-resolves the tag and adds the new platform key to the map. A dropped platform disappears as a removed key on the next re-lock. Both changes are visible as a plain-text diff to `ocx.lock` in pull-request review. There is no silent pickup at install time — the lock is the contract.

:::caution[`ocx.lock` is machine-generated]
Do not hand-edit `ocx.lock`. The format is canonicalized — sort order, whitespace, and the file-level `declaration_hash` are computed by `ocx lock` and may evolve across OCX versions. Manual edits will be overwritten on the next `ocx lock` run and may be rejected by future schema versions.

Tooling that reads `ocx.lock` should validate against the published schema at [`https://ocx.sh/schemas/project-lock/v3.json`][schema-lock]. The schema's top-level `$comment` field carries a machine-readable do-not-edit marker recognizable to JSON Schema processors.
:::

:::tip[Commit `ocx.lock` and tame merge conflicts]
`ocx.lock` is what makes the project reproducible — without it, every contributor and CI runner re-resolves advisory tags against whatever the registry surfaces today. Commit it alongside `ocx.toml`. To keep merge conflicts manageable on busy projects, add a [`.gitattributes`][gitattributes] entry that lets `git` union sibling lock entries instead of choking on overlapping diff hunks:

```text
ocx.lock merge=union
```

Each `[[tool]]` entry is independent, so concatenation usually produces a syntactically valid lock; running `ocx lock` after the merge normalizes sort order and deduplicates.
:::

### Concurrent writes {#lock-concurrency}

`ocx.toml` is published by atomic rename: every write lands in a tempfile beside it, then replaces it with one `rename(2)`, so its inode rotates on every `ocx add`, `ocx remove`, `ocx lock`, or `ocx update`. A reader that already has the file open keeps reading the inode it opened and always sees one whole document. Readers include `ocx status`, the per-prompt shell reconciler, an editor and `git`. A `kill -9` mid-mutation leaves the file intact and parseable, never a torn or spliced write. Unix permissions carry across the rename — a `0644` `ocx.toml` stays `0644`.

Because rename rotates the inode, a lock held *on* `ocx.toml` would strand on the inode the rename just orphaned. So the mutex is not on the data file at all. Project-state writes (`ocx lock`, `ocx update`, `ocx add`, `ocx remove`) serialize through a content-keyed lock entry under [`$OCX_HOME/locks`][in-depth-storage-locks], scope `project-mutate`, keyed on the config file's parent directory and file name. There is never an `ocx.toml.lock` sidecar in the project itself. A lock held on `ocx.toml` by an editor or a bare `flock(1)` no longer blocks `ocx add`. A second `ocx` process contending for the mutation lock still exits 75 ("`ocx.toml` is locked by another process") once it waits past the contention budget. Concurrent readers ([`ocx pull`][cmd-pull], IDE integrations, `git`) never acquire any lock. They parse `ocx.lock` directly via an atomic read. `ocx.toml` itself is a plain bounded read guarded only by a symlink refusal.

## Pin preservation {#pin-preservation}

`ocx add` and `ocx remove` are **partial mutators** — they touch only the binding they name and carry every other lock entry forward unchanged. Neither command re-resolves a surviving binding's live tag. This is the guarantee that adding a new binding or dropping an old one never silently advances the versions of everything else.

`ocx add` is also **idempotent** for a binding the target group already declares with the exact same identifier. The manifest is left byte-identical, `ocx.lock` is re-locked only when it has no entry yet, and an already-pinned binding is never re-resolved. Re-running `ocx add` after an interrupted download therefore just finishes the install — it never restates a version. Adding the same name with a *different* identifier still fails (exit 64): `ocx add` never moves a pin, only [`ocx update`][cmd-update] does.

Every surviving entry is passed through byte-identical — no registry contact. Because `ocx.lock` is always the current V3 format (an older lock fails to load outright, per the warning above), there is nothing to transcribe. `ocx add` and `ocx remove` never touch the network for pins they are not adding or removing.

The freshness gate runs before any carry-forward. If `ocx.toml` drifted from `ocx.lock` since the lock was last written (the `declaration_hash` does not match), the mutator fails with exit 65 before touching anything. The fix is a single `ocx lock` to reconcile the file, after which the add or remove succeeds.

The two commands that intentionally advance version pins are:

| Command | When it re-resolves |
|---------|---------------------|
| `ocx lock` | Only when `ocx.toml` drifted (whole-file reconcile; a moving tag may advance) |
| `ocx update` | Whole file by default; `-g GROUP` / `NAME` scopes it to a named subset (those advance, the rest stay frozen) |

Groups are primarily a **composition concern** — they scope which bindings `ocx exec`, `ocx env`, and `ocx pull` see. `ocx lock` ignores them and always reconciles the whole file. `ocx update` is the exception: passing `-g GROUP` or a binding `NAME` advances only that subset and carries every other pin forward verbatim. `ocx add` and `ocx remove` do the same for the bindings they touch.

`ocx update` is the only command that holds both the predecessor `ocx.lock` and the candidate at once. So only this command can answer what moved, rather than what is pinned now. It reports exactly that: a table of the bindings whose pull identifier changed. The pins that held still are summarized behind a hint line (`-v`/`--verbose` lists them too). `--check` runs the same diff without writing anything, exiting 65 when it would move something. See [`ocx update`][cmd-update] for the full report shape.

## Pulling and executing {#pull-exec}

Once `ocx.lock` exists, two commands cover the bulk of day-to-day use. [`ocx pull`][cmd-pull] pre-warms the [package store][in-depth-storage-packages] from the lock without creating install symlinks. That suits CI matrix builds and developer machines that already have a [direnv][direnv] hook in place. [`ocx exec`][cmd-run] spawns a child with the project's resolved environment, treating the lock as the source of truth (project-tier counterpart to OCI-tier [`ocx package exec`][cmd-exec]).

Both gate on the lock's `declaration_hash`: if `ocx.toml` has changed since the lock was generated, the command exits with a structured error pointing at [`ocx lock`][cmd-lock]. There is no implicit re-resolution. The project file is the input, the lock file is the contract, and registry round-trips happen only when you ask for them.

`ocx pull` also renders the project's toolchain home, which the shell hook and `ocx exec` read. [Toolchain Render][project-render] covers the render, the entries it skips and `--dry-run`.

## Running binaries {#running}

Once `ocx.lock` is current, [`ocx exec`][cmd-run] spawns a child process whose environment is composed from the lock's resolved package set. It is the project-tier counterpart to the OCI-tier [`ocx package exec`][cmd-exec]: the same child-spawn mechanics, but symbols are binding names from `ocx.toml` rather than OCI identifiers. The argument shape, the `-g` scope forms and the exit codes are in [Run project tools][guide-run-tools] and the [`ocx exec` reference][cmd-run].

### Project and binary resolution {#running-resolution}

The project is the nearest `ocx.toml` above the current directory, the same walk [`--project`][cmd-arg-project] describes, and only that file's lock is composed. Inside a project nested in another one, the enclosing project's packages are not part of the child environment. Name the enclosing project with `ocx --project <dir> exec` to run against it instead. See [Nested projects switch, they do not stack][in-depth-shell-integration-nested].

A binding missing from the local object store is auto-installed as part of composition. Because it auto-installs, a binding covered by a [`[[trust.policy]]`][config-trust] is signature-verified first, the same gate as [`package install`][cmd-package-install].

The first token after `--` is resolved once, before the child starts. `exec` searches only the composed environment's `PATH` contributions, the resolved packages' own directories, never the ambient `PATH` and never a working-directory fallback. It also refuses a match that is itself an ocx-generated launcher trampoline, rather than let the child re-enter `ocx exec` through it. A command that does not resolve this way, and a trampoline match, both exit 65. Exit 1 is reserved for a spawn that fails after resolution succeeded, such as the resolved binary vanishing before `execvp`.

### Composition order rule {#running-composition-order}

> First by group-selection order (the order of `-g` flags after `all` expansion, deduplicated); then alphabetical by binding name within each group.

This rule determines iteration order through the resolved package set. The composer applies env entries by **prepending**, so the **last package walked** has its PATH entries placed **first** on the resolved PATH at runtime. In other words: in `-g` argument order, **groups listed later win** PATH lookup. This matches the load-bearing prepend invariant in `composer.rs` ([source][composer-source]) — entries pushed later in iteration land first on PATH.

`all` expansion inserts groups alphabetically by group name in place of `all` in the `-g` argument list, after the default group. So `ocx exec -g ci,all,release` expands to `[ci, default, ci_alpha_ordered_named_groups..., release]` and then `compose_tool_set` deduplicates.

### Project and group `[env]` {#running-project-env}

The rule above governs package-composed env — stage 2 of a longer pipeline. [Project Environment][env-composition-project-env] in the Environment Composition reference is the full six-stage table: ambient, packages, patches, project `[env]`, group `[env]`, then `--env`. The `--env` flag accepts the same `constant`/`path`/[`list`][metadata-env-list] typing as the file form (see [`--env`][cmd-run]). Project and group `[env]` entries compose after every package's own entries. A `constant` or `path` value there always wins a same-key collision with a package default. A `list` value there is appended last, so a last-wins consumer sees it take effect over a package's own contribution to the same key.

Worked example, continuing the `[tools]` / `[group.ci]` declaration from [Project Groups][project-groups]:

```toml
[env]
SOURCE_DATE_EPOCH = "0"

[group.ci.env]
SOURCE_DATE_EPOCH = "1700000000"
```

`ocx exec -g ci -- CMD` composes the project's own `[env]` (stage 4) and then `[group.ci.env]` (stage 5) — the later stage wins, so the child sees `SOURCE_DATE_EPOCH=1700000000`. This is the same later-wins rule the group-selection order above already applies to `-g ci,release`. What "wins" depends on the type:

- `constant`: whichever `[env]` stage runs later replaces an earlier one for the same key.
- `path`: the later stage prepends ahead of the earlier one.
- [`list`][metadata-env-list]: the later stage appends after the earlier one, replacing and prepending nothing.

See [Append Semantics][env-composition-list] for the fold direction and the per-key separator agreement.

The project file is the second channel, not the first. Before this stage existed, the only way to set a variable for one run was the ambient shell (`FOO=bar ocx exec -- …`). That fails outright on Windows, where neither PowerShell nor `cmd.exe` has a per-invocation variable prefix and both mutate session state that persists after the command. It also fails for any caller that builds an argv array rather than a shell command line. That is the shape a [GitHub Action][github-actions-docs], a [Bazel rule][bazel-rules] or a Python subprocess call all use.

`--env` is the per-invocation form. It is not project configuration, so every command that composes an environment accepts it, on both tiers. The package tier composes only what the caller typed, because there is no project file to declare stages 4 and 5. `ocx exec` never prints, since it replaces itself with the child process. To see a composed environment, run the command that emits one. `ocx env --env X` composes stages 1 to 6 exactly as `ocx exec --env X` does. On the package tier, [`ocx package env`][cmd-package-env] pairs with [`ocx package exec`][cmd-exec] the same way.

`--clean` controls only stage 1, what the child inherits from the parent shell. It does not make the package-composed set reproducible: package env values are computed from `ocx.lock` and the resolved digests alone, identically with or without `--clean`. Project `[env]` is the opposite case by design. It is the user's own file, deliberately allowed to read ambient state, and for that reason it is excluded from the lock's `declaration_hash`. `--clean` strips what the shell contributed and never what ocx itself decided; [OCX Configuration Forwarding][in-depth-env-forwarding] lists what ocx writes onto the child in either mode. Project and group `[env]` entries have no visibility axis. A project is never a dependency of anything, so there is no interface/private edge to gate.

### PATH precedence consequence {#running-path-precedence}

Two groups may declare different bindings whose installed packages happen to ship a binary with the same filename. The group listed **last** in `-g` order controls which binary appears first in PATH.

For example, if `[tools]` and `[group.ci]` each ship a `cmake` binary under different binding names, `ocx exec -g default,ci -- cmake --version` resolves the one from `ci`, and `-g ci,default` flips the order. No error is raised, because `compose_tool_set` composes both bindings.

For direct binding-name conflicts — same `(group, name)` key with different identifiers — `compose_tool_set` returns `DuplicateToolAcrossSelectedGroups` (exit 64) before any spawn occurs.

### Layer purity {#running-layer-purity}

`ocx exec` never falls back to OCI-tier behavior. If `ocx.toml` is absent, it exits 64 rather than re-parsing the NAME arguments as OCI identifiers. This makes the behavior stable across directory changes and prevents embedding scripts from silently switching contracts.

The reverse holds too. `ocx package exec` and the other OCI-tier `package` commands never consult `ocx.toml`, even when one is present.

## Activation {#activation}

A project's binaries should be on `PATH` whenever you `cd` into the project. Nothing needs `eval`-ing on every shell startup, and nothing leaks into whatever else that shell does afterward.

On bash, zsh, fish, PowerShell, and elvish, the per-prompt shell hook does exactly that. Once the project is [consented][in-depth-shell-integration-consent], `cd` in and its locked binaries land on `PATH` at the next prompt. `cd` back out and they revert. Elvish's guard is narrower than the other four's. See [Shell Integration][in-depth-shell-integration] for the mechanism, the full per-shell coverage table, and the consent model that gates it.

Nushell and the shells with no append-safe prompt-hook point need one of two explicit entry points instead. These shells are the strict-POSIX family (`ash`, `dash`, `ksh`) and Windows Batch. Neither installs missing packages, contacts the registry, or mutates the [package store][in-depth-storage-packages]; run [`ocx pull`][cmd-pull] first to materialize anything `ocx.lock` requires.

[`ocx direnv export`][cmd-direnv-export] is the [direnv][direnv] entry point. It is stateless — it emits a fresh export block on every invocation. The cache layer comes from [direnv][direnv], with one re-evaluation per `cd` and re-triggering on watched files. So the export stays simple. Run [`ocx direnv init`][cmd-direnv-init] in a project directory to drop a ready-made `.envrc`, then `direnv allow`.

For scripted environments and CI, call [`ocx exec`][cmd-run] directly. It composes the project toolchain env and spawns the target command without any persistent shell state. It works on every shell, including the ones the per-prompt hook already covers.

:::tip[Learn more]
[Shell Integration][in-depth-shell-integration] — the per-prompt hook, the consent grants that gate it, and [`ocx shell state`][cmd-shell-state] for diagnosing an inert shell.
:::

## Global toolchain {#global-toolchain}

A user-wide `ocx.toml` at [`$OCX_HOME`][env-ocx-home]`/ocx.toml` (default `~/.ocx/ocx.toml`) holds packages that should be available in every shell — `ripgrep`, `cmake`, `shellcheck` — without being part of any specific project. This is the global toolchain tier, activated explicitly via the [`--global`][cmd-global-flag] flag or the [`OCX_GLOBAL`][env-ocx-global] environment variable.

The global file uses the same [schema][schema-project] and lock semantics as a project file. The lock lives at `$OCX_HOME/ocx.lock`. The global toolchain is **never discovered implicitly** — the CWD walk does not activate it. Pass `--global` or set `OCX_GLOBAL`.

A working directory that is itself under `$OCX_HOME` is project-free, not global. The CWD walk explicitly skips an `ocx.toml` candidate sitting at `$OCX_HOME`'s own directory rather than treating it as the nearest project file. It continues walking upward, so a `$OCX_HOME` nested inside a real project does not hide that project from a deeper working directory. A project-tier command run directly inside `$OCX_HOME` exits 64 ("no `ocx.toml` found") rather than silently adopting the global manifest; reach it only through `--global`/`OCX_GLOBAL` as above. The comparison is lexical: a `$OCX_HOME` reached through a symlink is not recognised — the same limitation the CWD walk's other boundary checks share.

:::caution[Global and project binaries are isolated by PATH precedence]
`ocx exec` and `ocx package exec` are always hermetic: the global toolchain is never consulted during project-tier resolution. Global binaries remain on `PATH` (there is no strip). Project-declared binaries are **prepended** by the active hook, so they shadow any same-named global binaries. See [Strict isolation][env-composition-strict-isolation] for the full model.
:::

For managing global packages day-to-day, see [Keep everyday packages available everywhere][user-guide-global] in the user guide. To opt out of project-tier discovery entirely for a single invocation, set [`OCX_NO_PROJECT=1`][env-no-project].

## Multi-project retention {#multi-project-retention}

When multiple projects on the same machine pin different package versions, [`ocx clean`][cmd-clean] retains every package referenced by *any* registered project's lockfile — not just the active project. A developer with project A and project B can run `ocx clean` from project B without losing packages that only project A's `ocx.lock` pins.

OCX tracks registered projects automatically in a flat symlink ledger at `$OCX_HOME/projects/`. It holds one symlink per project, named after the SHA-256 hash of the project's canonical absolute path, pointing at the project directory. The ledger is updated whenever [`ocx lock`][cmd-lock] runs in a project directory. You should not edit it manually. It is browsable with `ls -l $OCX_HOME/projects/`.

To collect packages held only by other projects' lockfiles, for example after removing a project from your machine, pass `--force` to bypass the registry: `ocx clean --force`. Live install symlinks are always honoured regardless of `--force`.

## Reproducibility and SLSA {#reproducibility}

OCX v1 ships digest-pinning reproducibility. Every package a project resolves is identified by its OCI manifest digest, and the lock file commits that digest under a hash of the source `ocx.toml`. Any consumer with the lock can verify they are pulling exactly the bytes the project committed to — no tag races, no silent registry rewrites.

What is not yet shipped is a signed build attestation describing how each package was produced. That capability — the kind of [SLSA build provenance][slsa-l1] producers can generate via [Sigstore][sigstore] or similar — is deferred to v2. Treat OCX v1 as solid input integrity, not as compliance with any [SLSA level][slsa-attestation].

## See Also

- [User guide → Pin a project's packages][user-project] — task-driven overview.
- [User guide → Run binaries from your project][user-run] — quick-start examples for `ocx exec`.
- [User guide → Keep everyday packages available everywhere][user-guide-global] — global toolchain use-case narrative.
- [Environment Composition reference][env-composition-strict-isolation] — reference-level statement of the strict isolation rule.
- [Indices In Depth][in-depth-indices] — how `ocx pull` reads the lock and where the registry round-trips happen.
- [Storage In Depth → Garbage collection][in-depth-storage-gc] — how project-lock back-references protect packages.
- [Configuration In Depth][in-depth-configuration] — discovery tier rationale, `OCX_NO_PROJECT` kill switch.

[toml]: https://toml.io/
[oci-identifier]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[taplo]: https://taplo.tamasfe.dev/
[helix]: https://helix-editor.com/
[vscode-taplo]: https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml
[neovim-taplo-lsp]: https://github.com/neovim/nvim-lspconfig/blob/master/lua/lspconfig/configs/taplo.lua
[direnv]: https://direnv.net/
[gitattributes]: https://git-scm.com/docs/gitattributes
[rfc-8785]: https://www.rfc-editor.org/rfc/rfc8785
[mise]: https://mise.jdx.dev/
[asdf]: https://asdf-vm.com/
[slsa-l1]: https://slsa.dev/spec/v1.0/levels#build-l1
[slsa-attestation]: https://slsa.dev/spec/v1.0/attestation-model
[sigstore]: https://www.sigstore.dev/
[schema-project]: https://ocx.sh/schemas/project/v1.json
[schema-lock]: https://ocx.sh/schemas/project-lock/v3.json
[composer-source]: https://github.com/ocx-sh/ocx/blob/main/crates/ocx_package_manager/src/composer.rs
[cmd-clean]: ../reference/command-line/commands.md#clean
[cmd-exec]: ../reference/command-line/package-consuming.md#package-exec
[cmd-package-env]: ../reference/command-line/package-consuming.md#package-env
[cmd-global-flag]: ../reference/command-line/index.md#global-flag
[cmd-lock]: ../reference/command-line/commands.md#lock
[cmd-pull]: ../reference/command-line/commands.md#pull
[cmd-update]: ../reference/command-line/commands.md#update
[cmd-run]: ../reference/command-line/commands.md#exec
[cmd-direnv-export]: ../reference/command-line/direnv.md#direnv-export
[cmd-direnv-init]: ../reference/command-line/direnv.md#direnv-init
[cmd-shell-state]: ../reference/command-line/shell.md#shell-state
[cmd-arg-project]: ../reference/command-line/index.md#arg-project
[cmd-package-install]: ../reference/command-line/package-consuming.md#package-install
[config-trust]: ../reference/configuration.md#keys-trust
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-global]: ../reference/environment.md#ocx-global
[env-no-project]: ../reference/environment.md#ocx-no-project
[project-toml]: #toml
[project-lock]: #lock
[reference-platforms]: ../reference/platforms.md
[reference-platforms-compatibility]: ../reference/platforms.md#compatibility
[reference-platforms-shared-digests]: storage.mdx#shared-digests
[metadata-env-list]: ../reference/metadata/env.md#env-list
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[user-project]: ../guides/project-toolchain.mdx#project
[user-run]: ../guides/run-project-tools.mdx#exec
[user-guide-global]: ../guides/global-toolchain.mdx
[env-composition-strict-isolation]: ../reference/env-composition.md#strict-isolation
[env-composition-project-env]: ../reference/env-composition.md#project-env
[env-composition-list]: ../reference/env-composition.md#composition-order-list
[in-depth-env-forwarding]: ./launcher-forwarding.md#ocx-forwarding
[in-depth-versioning-locking]: ./versioning.mdx#locking
[in-depth-indices]: ./indices.md
[in-depth-indices-local]: ./local-index.md#local
[in-depth-indices-bundled]: ./indices.md#bundled
[in-depth-storage-packages]: ./storage.mdx#packages
[in-depth-storage-gc]: ./storage.mdx#gc
[in-depth-storage-locks]: ./storage.mdx#stores
[in-depth-shell-integration]: ./shell-integration.mdx
[in-depth-shell-integration-consent]: ./shell-integration.mdx#consent
[in-depth-shell-integration-nested]: ./shell-integration.mdx#activation-nested-projects
[in-depth-configuration]: ./configuration.mdx
[project-render]: ./toolchain-render.md
[guide-run-tools]: ../guides/run-project-tools.mdx
[project-groups]: ./project-groups.md
