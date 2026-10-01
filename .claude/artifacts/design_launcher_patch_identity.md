# Design: patch identity at the entrypoint launcher

Status: proposed · Scope: `ocx launcher exec` / `ocx launcher shim` patch matching · Tier: high

## Problem

A patch companion composes as part of its target, so its private vars must reach the target's
entrypoint launcher and `--self`. The launcher re-enters through
`PackageManager::install_info_from_package_root` (`crates/ocx_package_manager/src/lib.rs`), which mints
`<default-registry>/file-url-mode/<hex>@digest`. `PatchDescriptor::collect_companions` globs a rule's
`match` against `base` and `base.without_digest()`, so `ocx.sh/plantuml:*` never matches and only `*`
does. `*` then puts the companion's private vars into every launcher on the host. The acceptance test
`test_private_companion_var_reaches_its_targets_launcher_and_self_only` uses `*` to work around this.
`ocx launcher shim` composes with the real pinned identifier. But it then execs the name on PATH,
usually the entrypoint launcher, so its second hop has the same defect.

The launcher alone cannot recover the identity. The package dir is keyed by registry + digest, so one
dir and one set of `entrypoints/` serve every repository that resolves to that digest. Only the process
that composed the launch knows which name it used.

## Decision: the composing process forwards a digest-keyed identity map

### Mechanism

1. **Type.** `ocx_package::launch::LaunchIdentities` holds a `BTreeMap<Digest, BTreeSet<PackageRef>>`.
   Each value is `registry/repo[:tag]`, never a digest: the launcher attaches its own digest, so an
   entry cannot point the bytes at another package. It lives in `ocx_package`, beside `ChildEnv` in
   `metadata/env/apply.rs`, because `ChildEnv` carries it.
   - `from_infos(&[InstallInfo])` takes every package in the composed closure (roots and admitted
     deps), keyed by digest, and calls `identifier.without_digest()`, which keeps the tag.
   - Codecs: `encode() -> String` and `from_env() -> Result<Option<Self>, LaunchIdentityError>`.
   - `identities_for(&Digest) -> &[PackageRef]`.
2. **Wire.** A new reserved key, `OCX_LAUNCH_IDENTITIES`, holds JSON
   `{"sha256:<hex>": {"names": ["ocx.sh/plantuml:1.2024", ...], "no_patches": true}}`, with sorted
   keys and names. `no_patches` is present only for a package the exporting composition opted out, so
   a launcher on an exported `PATH` honours the opt-out exactly as under `ocx exec`. An inherited value
   is merged, this export's entry winning per digest.
   - Add it to `ocx_config::env::keys`. `ForwardedEnvError` already refuses reserved `OCX_*` keys from
     `--env` and `[env]`, so neither a project nor a user can write it.
   - It is a separate key rather than a field of `OCX_PATCHES`. An ambient `OCX_PATCHES` changes config
     resolution (`app/context.rs` falls back to `patches_from_env`), while identity is not config.
   - It is written only when a `[patches]` tier is in effect, so users without patches see no change.
3. **Writers.**
   - `ChildEnv` gains `identities: Option<&LaunchIdentities>`, and `Env::apply_child_env` writes it.
     A composing parent always overwrites the key and never merges it, because it is authoritative for
     its own child tree.
   - P1, spawning composers:
     - `command/toolchain_exec.rs`, from `install_infos` plus the admitted deps.
     - `command/exec.rs` (`ocx package exec`).
     - `command/package_test.rs`, under the `--identifier` the bundle composes as.
     - `command/launcher/shim.rs`, from its pinned identifier, before it execs the PATH name.
     - Bin-mode and session trampolines go through `ocx exec`, so they are covered.
   - P2, env exporters:
     - `toolchain_env.rs` (`ocx env` with `--shell`, `--ci`, structured output).
     - `env.rs` (`ocx package env`) and `direnv_export.rs`.
     - The per-prompt reconciler (`ocx_package_manager::activation`), which emits the key as one more
       owned entry.
     - P2 covers the default `activate = env` path, where launchers run from a bare PATH with no ocx
       parent.
4. **Reader.** `command/launcher/exec.rs` looks up `LaunchIdentities::from_env()?` by the package
   digest.
   - `install_info_from_package_root(root, identities: &[PackageRef])`: when `identities` is non-empty,
     `InstallInfo.identifier` becomes the first identity in sorted order, with the digest attached and
     the registry taken from the identity, not `default_registry`. When it is empty, the synthetic id
     is unchanged.
   - `InstallInfo` gains `aliases: Vec<PackageRef>`, which holds the rest of the identities.
     `InstallInfo` is internal, so the shape is free to change.
5. **Matching.**
   - `build_site_patch_set` (`tasks/resolve.rs`) calls `collect_companions` once per identity, primary
     plus aliases, and unions the results through the existing `merge_companions`, which dedups by
     companion.
   - Opt-out: a base is opted out when any identity's `registry/repo` is in `no_patches`, because
     opt-out is the project's narrowing intent. `system_required` still overrides it.
   - The target axes do not change: a launcher root still gets the PRIVATE surface, and a dependency
     still gets INTERFACE.
6. **Retire the digest leg of `no_patches`.** Once identity is real, the repo-key check works at the
   launcher, so the leg has no job left.
   - Delete the `forwarded_no_patches` digest loop in `toolchain_exec.rs`.
   - Delete the digest arm of the opt-out check in `resolve.rs` and its comment, at the
     "`file-url-mode/<digest>` identifier has no real repository" comment.
   - Rewrite the `exec.rs` comment to say: "Opt-out keys match the forwarded launch identities; a
     launch with none falls back to the synthetic id, where only `*` rules apply."

### Fallback when no identity reaches the launcher

This covers a launcher run by absolute path (`candidates/<tag>`, `current`) or from a PATH that no P1
or P2 producer composed. The launcher keeps the synthetic id, so only `*` rules match, as today.
- It logs one `debug!`, not a warning. This state is common and benign, and a warning on every
  invocation of an un-composed tool would be noise.
- It never fails closed. Refusing to run would break every directly invoked entrypoint on any host
  with a `[patches]` tier.
- A malformed `OCX_LAUNCH_IDENTITIES` does fail closed, with the same error class as a malformed
  `OCX_PATCHES`. A corrupt envelope must not silently degrade to "no identity".

### Tag semantics

The identity carries the advisory tag the composer had.
- An `ocx.toml` binding has the declared tag, through the `ProjectLock::bind` join from
  [f402b39a0](https://github.com/ocx-sh/ocx/commit/f402b39a0).
- A CLI identifier has the tag the user typed.
- The shim has the tag in its pinned identifier.
- A digest-only composition forwards a tagless identity. `repo:*` does not match it, while `repo` and
  `repo@*`-style rules do. These are the same glob semantics as every other path, with no
  launcher-specific special case. Document this in the patches user guide.

### `ocx launcher shim`

The shim has the tagged identity already, so it adds no lookup, only a P1 write. It runs its own
composition unchanged. Before `exec`, it writes the map for its pinned package and that package's
closure. The entrypoint launcher it reaches then matches the same rules the shim did. Without this, a
deferred tool loses targeted companions on its second hop.

### Execution record

A launcher frame today names `file-url-mode/<hex>`, and `record/purl.rs::has_logical_identity` drops the
purl for it. With a forwarded identity, the frame names the real `registry/repo@digest` and emits a
valid `pkg:oci` purl. The frame stays tagless: it records the primary identity through
`without_tag()`, keeping the f402b39a0 contract. The two records still join on the content digest.
Without a forwarded identity, the frame is unchanged.
- Update `website/src/docs/reference/execution-records.md`, whose example shows the placeholder name.
- Keep `PLACEHOLDER_REPOSITORY_PREFIX`, because the fallback still mints the placeholder.

### Security and trust direction

- The map only selects among rules the operator configured in `[patches]` descriptors. It cannot
  introduce a companion, a descriptor or a registry.
- A spoofed map can steer a configured companion onto another launcher. That requires control of the
  launcher's environment, and that control already grants `OCX_PATCHES`, `PATH` and `LD_PRELOAD`, so
  this opens no new boundary.
- Projects and users cannot set the key, because it is reserved, so the identity is always derived by
  ocx from the lock or the argv.
- A union of identities widens operator-scoped matching only over names this one composition actually
  used. The host-wide history of names is never consulted, which is why origins are rejected below.

## Tests

### Unit

- `LaunchIdentities`:
  - The codec round-trips.
  - An absent key gives `None`, and malformed JSON or a malformed ref gives a typed error.
  - Lookup of an unknown digest gives an empty result.
  - `from_infos` keeps the tag, strips the digest, includes deps and is order-independent.
- `install_info_from_package_root`:
  - With identities, the identifier and registry are real.
  - Without identities, the synthetic id is byte-identical to today's.
- `build_site_patch_set`:
  - Two identities, one rule each, give the union of companions with no duplicate.
  - A tagless identity does not match `repo:*`.
  - Opt-out on either identity's repo suppresses the companion, and `system_required` still wins.
- `record`: a launcher frame with an identity emits a purl with no `tag` qualifier. Without an
  identity, the frame has no purl.

### Acceptance (`test/tests/test_patches.py`)

- Rewrite the `*` workaround test to use `{"match": "<registry>/<repo>:*"}` on package A, with an
  entrypoint and a private companion var, and a second package B with an entrypoint and no matching
  rule.
  - `ocx package exec A:<tag> -- <A-entry>` sees the var.
  - `ocx package exec B:<tag> -- <B-entry>` does not.
  - `--self` agrees with both.
- Variants:
  - Project `ocx exec`, where the tag comes from the `ocx.toml` binding.
  - A deferred tool under `--lazy-mode always`, which goes through the shim to the launcher.
  - P2: `eval "$(ocx env --shell=sh)"`, then run A's entrypoint by name.
  - Fallback: run A's `candidates` entrypoint by absolute path. The targeted rule must not apply,
    while a `*` rule does.
- Red proof: delete the `apply_child_env` identity write and the targeted cases go red. Restore it,
  confirm the restored text is present, and they go green.

## Rejected

- **`refs/origins` as the identity source.** Origins carry no tag, so `repo:*` still fails. They are
  written only on a genuine registry fetch, so they are absent after `pull_local`, after a second
  repository hits an installed digest, and on older installs. They also hold host history, not this
  launch's name.
- **Baking identity into the launcher body or the `.shim` sidecar.** The launcher lives in the
  per-digest package dir, so one launcher serves N repositories. Baking would also change the wire ABI
  in `body.rs` and `ocx_shim::core`, and every installed launcher would lack it.
- **Looking up the project lock from the CWD.** The launcher's CWD is not the project, so the answer
  would depend on where the tool runs.
- **Back-references through `refs/symlinks`.** They are absent for lock-pinned tools and for toolchain
  links, and like origins they are history.
- **Carrying identity inside `OCX_PATCHES`.** Exporting it ambiently turns the patch config ambient
  through `patches_from_env`.
- **Failing closed when no identity is present.** That breaks every directly invoked entrypoint on a
  patched host.
- **Removing the synthetic id now** (the rootless-`InstallInfo` item in
  `plan_issue_batches_2026-09-04.md`). That is orthogonal and ADR-class, and the fallback still needs
  an id.

## Interface impact and commits

These are not breaking for published packages: no metadata, manifest, lock or launcher ABI change.
Three changes are interface-visible:
- A new reserved environment variable, which is additive and goes in `reference/environment.md`.
- Launcher record frames gain a real name and purl, which are values and not schema.
- Targeted rules now reach launchers, which is the fix itself.

`OCX_PATCHES` no longer carries digest opt-out keys. A launcher from an older ocx nested under a newer
parent loses that leg. Accepted pre-1.0, and stated in the P1 commit body.

- P1: `fix(patches): repository- and tag-scoped patch rules now reach a package's entrypoint launchers`
- P2: `feat(env): shell, direnv and CI exports carry launch identities, so patch rules reach entrypoints run from PATH`
