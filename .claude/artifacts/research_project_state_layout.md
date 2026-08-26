# Research: Per-project state layout & project-key derivation

**Date:** 2026-08-24
**Axis:** 1 of 4 — env overhaul ADR (`brief_env_overhaul.md` scope item 5)
**Consumed by:** `adr_shell_env_overhaul.md`

## Grounding: OCX's existing scheme (already correct — extend it)

`crates/ocx_lib/src/project/registry.rs` (`$OCX_HOME/projects/`) already implements the
pattern the industry converged on: flat dir, one symlink per project, **name = first-16-hex
SHA-256 of the canonical absolute project dir**, target = the project dir itself. Liveness =
symlink resolves + `<target>/ocx.lock` exists (three-state probe Live/Dead/Unknown, treating
transient I/O errors as Unknown so a live project is never GC'd). `ReferenceManager::name_for_path`
is the single hash source, shared with `refs/symlinks/` back-refs.

**This is the ledger to extend, not a third scheme to add.** A consent stamp should derive its
key the same way, and live *inside* the per-project entry rather than as a sibling flat dir.

## Per-tool findings

**pnpm** — `{storeDir}/v11/projects/`: one symlink per registered project, added specifically to
support `pnpm store prune`'s mark-and-sweep GC (scan registered-project symlinks → walk transitive
deps → mark reachable → delete unmarked). Shipped in the 10.x line. Exact naming scheme not stated
in primary docs, but the shape — flat registry dir as GC root set — is structurally identical to
OCX's `projects/`. [store prune](https://pnpm.io/cli/store) · [10.27 notes](https://pnpm.io/blog/releases/10.27)

**Nix indirect gcroots** — `/nix/var/nix/gcroots/auto/<hash>`, one per `nix-build --indirect`;
**hash is of the indirect root's own path** (not content, not target). When the project dir is
deleted the entry dangles and GC silently ignores it — **but never deletes the stale entry**;
it accumulates forever absent a manual sweep. The negative lesson: OCX's active three-state
liveness probe is strictly better than Nix's laissez-faire model.
[Nix Pills — GC](https://nixos.org/guides/nix-pills/11-garbage-collector.html) · [nix-store --gc](https://nix.dev/manual/nix/stable/command-ref/nix-store/gc)

**direnv** — two-part identity, exactly the "bind path AND content" shape:
`$XDG_DATA_HOME/direnv/allow/<pathHash>` gates on canonicalized path hash; a `fileHash` of
`.envrc` content is tracked separately. The older shell implementation used a combined
`sha256(path + "\n" + content)`. Either way **moving the project OR editing `.envrc` invalidates
trust**. Confirms consent must key on both axes, not path alone.
[cmd_allow.go](https://github.com/direnv/direnv/blob/master/internal/cmd/cmd_allow.go)

**mise** — `$MISE_STATE_DIR` (`~/.local/state/mise`, XDG state, explicitly *not* synced across
machines — direct precedent for "state ≠ config"). Trust file naming is **not a bare hash**:
`<path-hash>-<truncated-parent-dirname>-<truncated-filename>`, so `ls` on the trust dir stays
human-legible while the key stays a hash. Paranoid mode stores a companion `.hash` file via a
**custom append-extension helper**, deliberately avoiding `PathBuf::with_extension` (which
truncates at the last dot and corrupts/collides filenames already containing dots) — a real
footgun worth citing in implementation notes.
[trust.rs](https://github.com/jdx/mise/blob/main/src/cli/trust.rs) · [directories](https://mise.jdx.dev/directories.html)

**VS Code workspace storage** — `workspaceStorage/<hash>/` keyed by hashing the **raw path string
as opened**, not the canonical path. Confirmed footgun: opening the same physical folder via a
symlink vs its resolved path creates two unrelated buckets — duplicate state, silent divergence.
Direct argument for canonicalizing before hashing, which OCX's `name_for_path` already does.
[microsoft/vscode#313681](https://github.com/microsoft/vscode/issues/313681)

**cargo / uv (contrast)** — neither keys state by consuming-project identity at all. Cargo's
registry cache and `~/.cache/uv` are **content-addressed** (crate/wheel hash, resolved VCS
revision, URL), shared across every project depending on the same artifact. Right model for
*package content* (OCX already does this in `blobs/`, `layers/`, `packages/`); wrong model for
*session/consent/activation state*, which is project-identity-scoped, not content-scoped.
[uv caching](https://docs.astral.sh/uv/concepts/cache/) · [Cargo Home](https://doc.rust-lang.org/cargo/guide/cargo-home.html)

## Comparison

| Tool | Key | In-home vs in-project | Moved-project behavior | GC |
|---|---|---|---|---|
| OCX `projects/` (existing) | SHA-256(canonical abs path), 16 hex | in-home | new hash, old entry orphaned → pruned by liveness probe | active, 3-state probe |
| pnpm `projects/` | project registry symlink (naming unconfirmed) | in-home | — | mark-and-sweep on `store prune` |
| Nix `gcroots/auto/` | hash of indirect-root's own path | in-home root + in-project symlink | dangles forever until manual sweep | passive; never cleans the root file |
| direnv `allow/` | path hash (dir) + content hash (file) | in-home | invalidated — must re-`allow` | none (grows until user prunes) |
| mise state/trust | path hash + readable dir/file suffix | in-home (`MISE_STATE_DIR`, not synced) | invalidated | none documented |
| VS Code `workspaceStorage/` | hash of raw (uncanonicalized) opened path | in-home | **bug** — symlink vs real path fork into two buckets | manual only |
| cargo/uv cache | content hash (not project) | in-home | n/a | mark-and-sweep / LRU |

## Recommendation

Extend `ReferenceManager::name_for_path` and the existing `$OCX_HOME/projects/<hash>` ledger as
the **one** project key and **one** per-project state root:

1. **Key** — keep SHA-256(canonicalized abs project dir), 16 hex; already correct, already shared
   with `refs/symlinks/`. Canonicalize before hashing (VS Code's bug is what skipping this causes).
2. **Shape** — make `$OCX_HOME/projects/<hash>` a **directory**, not a bare symlink: the existing
   pointer plus feature subdirs (`activation-consent/`, future per-project state) inside it.
   Collapses three schemes into one. `state/<feature>/` flat dirs stay for subsystem-scoped
   (not project-scoped) state — a genuinely different axis that should not be merged in.
3. **GC** — no new liveness signal; reuse the existing three-state probe in `registry.rs`.
   Strictly better than Nix's passive model, matches pnpm's 10.x direction.
4. **Consent stamp** — key on path hash *and* store a content/identity fingerprint alongside
   (direnv's two-axis model), e.g. a hash of `ocx.lock`'s source set, so source-set edits
   re-trigger consent without a path change.
5. **Accepted failure modes** — 64-bit truncated-SHA-256 collision risk is negligible at realistic
   scale and already consciously accepted (ARCH-1a in the code). Path reuse (delete a repo, clone
   a different one at the same path) is indistinguishable from a legitimate re-clone under any pure
   path-hash scheme; direnv, mise and Nix all accept this. To close it, the consent-stamp content
   fingerprint is the seam — not the ledger key.
