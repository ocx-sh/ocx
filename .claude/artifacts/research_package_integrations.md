# Research: Vendor-Namespaced Metadata — prior art for package `integrations`

**Date:** 2026-08-09 · **For:** [`ocx-sh/ocx#221`](https://github.com/ocx-sh/ocx/issues/221)
**Question:** How do other ecosystems let a package carry structured configuration for
tools the format owner does not model, and what do they get wrong?

Companion to [`research_interpolation_capability.md`](./research_interpolation_capability.md)
(the `Usage`→`AllowedTokens` gate this feature reuses).

## VERDICT

**The vendor-namespaced open object is the settled answer.** Copy the namespacing
convention; do **not** copy the merge story. Every surveyed format reserves a *name
pattern* and then refuses to validate inside it — the format owner recognizes the shape,
never the content. The one design decision nobody else got right is merge semantics, and
it is exactly the decision OCX cannot punt on, because `ocx.toml` and package metadata are
interface-tier contracts.

Second finding, load-bearing for the visibility design: **no surveyed system propagates a
private dependency's declared attribute to a top-level consumer by default.** OCX's
existing `Visibility` algebra already implements the majority rule, so the feature needs
no new propagation machinery.

## 1. Namespaced-extension mechanisms

| Format | Mechanism | Namespacing | Validation of contents | Notes |
|---|---|---|---|---|
| **devcontainer.json** | `customizations` object | free vendor key, no registry | none — unknown key silently ignored | schema is literally `{"type": "object"}` + a note to pick a unique sub-key |
| **Cargo** | `[package.metadata.<tool>]`, `[workspace.metadata]` | tool name, convention only | none — exempted from the unused-key warning **by name** | `cargo-deb`, `docs.rs`, `wasm-pack`, `dist`, `cargo-binstall` |
| **PEP 621 / 518** | `[tool.<name>]` | PyPI project name | none in `[tool.*]`; `[project]` **forbids** unknown keys | the strict-core + open-annex split, stated normatively |
| **npm** | arbitrary top-level keys | **none** | none | the control group — see failure modes |
| **OCI** | `annotations` map | reverse-DNS; `org.opencontainers.*` reserved | none; consumers **MUST NOT** error on unknown key | shares the manifest size budget |
| **OCI referrers** | separate artifact, `subject` + `artifactType` | media type | per-type | independent size, signing, lifecycle; second round-trip |
| **Kubernetes** | annotations (opaque) vs labels (selectable) | `<reverse-dns>/<name>`, `kubernetes.io/` reserved | none | hard **256 KiB** server-side cap on total annotations |
| **Helm + Artifact Hub** | `Chart.yaml` `annotations`, `artifacthub.io/*` keys | prefix owned by a *third party* | none by Helm | purest consumer-defines-schema case |
| **CycloneDX** | `properties` name/value bag, `namespace:name` | community taxonomy, non-normative | none | flatter than a nested table |
| **Nix** | `meta` (descriptive) / `passthru` (attrs) | none — plain attrset | none | both build-hash-inert |
| **Homebrew** | *(none)* | — | — | outlier: extension requires upstreaming into Homebrew |

**JSON Schema mechanics** underneath all of it: `additionalProperties: false` plus
`patternProperties` on a namespace regex is the literal implementation of OpenAPI's `x-`
convention. Evaluation order (`properties` → `patternProperties` → `additionalProperties`)
is what makes a "strict everywhere except this namespace" schema expressible at all.

### Four recurring patterns

1. **A syntactic escape hatch, not a schema escape hatch.** Reserve a name pattern, refuse
   to validate its contents.
2. **Namespace by the consumer's identity**, not by content type — tool name (Cargo, PEP
   621), reverse-DNS domain (OCI, K8s), or a registered taxonomy string (CycloneDX).
3. **Consumers define the third-party schema, not the format owner.** cargo-binstall,
   Artifact Hub, and docs.rs all specify schemas living inside a file someone else owns.
4. **Pass-through, never error** — stated normatively by OCI, implicit everywhere else.

### Escalation path when "don't validate" stops working

Kubernetes is the only surveyed ecosystem to have hit that wall and answered
architecturally: **CRDs** — a first-class, OpenAPI-validated resource type — rather than
stricter annotations. The lesson for OCX is the mirror of #90/#91: a *typed* field is the
right answer only for data OCX itself acts on.

## 2. Observed failure modes

- **Key squatting / silent collision** — npm's `types` and `exports`, fought over by
  competing tool generations, is the direct consequence of having no container at all.
- **Unvalidated blobs bloating the primary artifact** — OCI annotations share the manifest
  budget, which is *why* the referrers API exists; Kubernetes hit it harder and imposed a
  hard server-side cap after `last-applied-configuration` routinely blew past informal
  limits. **A cap belongs in v1, not a follow-up.**
- **Schema drift with no update path** — consumer-owned schemas validate at *consumption*
  time, so errors surface downstream and late; the format owner cannot tell an author their
  metadata is stale.
- **Two overlapping mechanisms for one job** — SPDX carries both `ExternalRef` and a
  CycloneDX-compatible `properties` bag; OCI carries both annotations and referrers.
  Neither pairing is deprecated, so every implementer must support both.
- **Punted merge semantics** — the devcontainer spec says outright: *"For `customizations`…
  merging is left to the tools."* Two conformant implementors may therefore disagree about
  the same namespace. This is the one thing OCX must not inherit.

## 3. Propagation: what a dependency's attribute does to its consumer

The motivating case: a private transitive dependency (a C compiler) declaring "set me as
the IDE's compiler" must not reach the top-level consumer.

| System | Re-export mechanism | Enforcement |
|---|---|---|
| **CMake** | `PUBLIC` / `INTERFACE` append to the target's own `INTERFACE_*` properties; `PRIVATE` does not | structural |
| **Gradle** | `api` reaches the consumer's compile classpath; `implementation` does not | structural (compile error) |
| **Bazel** | `exports` republishes a dep one hop further; `deps` alone does not | structural (`strict_java_deps`, `layering_check`) |
| **Nix** | `propagatedBuildInputs` runs a setup hook in every downstream build; `buildInputs` does not | structural |
| **pkg-config** | `Requires:` always folds in; `Requires.private:` only under `--static` | mode-dependent |
| **npm** | `dependencies` install transitively; `devDependencies` never do; `peerDependencies` invert (declared by dep, satisfied by consumer) | resolution-level |
| **Cargo** | `public = true` (unstable, RFC 3516) enables `exported_private_dependencies` | **advisory only** — lint, never a build error |

**Unanimous rule:** every mature system separates *"I use this"* from *"I re-expose this,"*
and the re-export edge requires an explicit second declaration at each hop — never inferred
from use. Two enforcement tiers exist; OCX's composer is in the structural tier.

**Direct consequence for #221:** a private dependency's `integrations` reaching the
consumer by default would be unprecedented. OCX's `Visibility` (explicitly modelled on
CMake, `metadata/visibility.rs:63`) with `through_edge` / `merge` already gives the
majority behaviour with no new machinery.

## 4. Conflict resolution when N sources write one key

| System | Rule | Errors on conflict? |
|---|---|---|
| devcontainer Features | scalars/objects replace by install order; arrays union | no — silent, order-driven |
| VS Code settings | scope chain (default < user < remote < workspace < folder < language < policy); objects deep-merge per key, arrays/scalars overwrite | no |
| Kustomize / Helm | ordered overlay; `null` deletes a key | no |
| systemd drop-ins | multi-value directives **accumulate**; assign-empty resets, then re-set | no |
| EditorConfig | nearest file wins per key, inherits the rest | no |
| Bazel action conflicts | two targets producing one output path | **yes** — names both targets |
| Nix `buildEnv` | file-path collision | **yes** — `priority` attr is the escape hatch |
| Homebrew `brew link` | path already owned | **yes** — `--overwrite` required |

Design space, cheapest first: last-write-wins by deterministic order · type-aware merge ·
explicit reset idiom · hard error with no default winner. The hard-error bucket is correct
only when silently picking one value would be *wrong* rather than merely surprising.

**#221 sits outside this table entirely.** Because OCX never learns a payload's schema, it
cannot rank contributors and therefore cannot detect a conflict — so it emits an ordered,
attributed list and the consuming application adjudicates. Note this also *removes* a
conflict gate that would otherwise have looked natural next to the existing
`closure.conflicts` machinery for entrypoint-name collisions.

## 5. IDE / dev-environment landscape (2026)

Two lineages, no single standard:

- **`devcontainer.json` + `customizations.<vendor>`** — the de-facto answer for "which
  container, which tools", read by **seven** independent implementors: VS Code, GitHub
  Codespaces, JetBrains Gateway, Zed, Coder, DevPod, Daytona. Joining costs nothing: pick a
  key, document it (JetBrains simply started writing `integrations.jetbrains`).
- **`devfile.yaml`** (devfile.io, LF, AWS/IBM/JetBrains/Red Hat) — used by AWS CodeCatalyst
  and JetBrains **Space**. JetBrains backs both specs for different products, so vendor
  backing is not a signal.
- **Outside both:** Gitpod/Ona (`.gitpod.yml`), Google Cloud Workstations (Terraform).

Known namespace payloads: `vscode` → `extensions[]`, `settings{}` · `jetbrains` →
`backend`, `plugins[]`, `settings{}` · `codespaces` → `repositories`, `openFiles`,
`disableAutomaticConfiguration` · `zed` → `extensions[]` · `coder` → `ignore`, `autoStart`,
`displayApps`, `apps[]`.

**No convergence at the editor-UI layer.** Theme, keybindings, and per-language-server
settings stay vendor-local everywhere; EditorConfig is the only genuinely cross-editor
convention and covers whitespace/encoding only. A package therefore cannot emit one
portable IDE block — per-vendor namespaces are the shape of the world, not a shortcut.

Distribution precedent worth noting: devcontainer Features ship as OCI artifacts whose
metadata rides a **manifest annotation** (`dev.containers.metadata`, escaped JSON), config
media type `application/vnd.devcontainers`, layer `application/vnd.devcontainers.layer.v1+tar`.
That sidesteps registering a config-blob schema at the cost of no validation anywhere.
OCX already carries typed metadata in its config blob and needs no such workaround.

## 6. Shell completions

**No package-metadata standard exists in any ecosystem.** Not npm, not Cargo, not PyPI.
Debian's `debian/package.bash-completion` (consumed by `dh_bash-completion` /
`dh_shell_completions`) is the closest, and it is per-distro build tooling rather than a
manifest field. Everything else is convention-by-path. A package declaring its own
completion paths would be filling a gap, not converging on prior art.

Activation mechanics split cleanly, and the split explains every packager's policy:

| Shell | Mechanism | Cost |
|---|---|---|
| bash (bash-completion v2) | file-drop under `XDG_DATA_DIRS/bash-completion/completions/<cmd>`; `complete -D` fallback loads on **first Tab** | zero until used |
| fish | file-drop `~/.config/fish/completions/<cmd>.fish`, autoloaded on demand | zero until used |
| zsh | `fpath` + `compinit`; eager scan + `.zcompdump` parse | **16–640 ms** measured; `zcompile` + once-a-day regen are the standard fixes |
| PowerShell | `Register-ArgumentCompleter` — a runtime call, per session | eager, no lazy path |
| elvish | `$edit:completion:arg-completer[…]` at rc time | eager |
| nushell | exactly **one** global external-completer slot | architectural — no per-tool file |

zsh additionally runs `compaudit` inside `compinit` and refuses group/world-writable `fpath`
entries — the only built-in trust check in any shell. Nix passes it by construction
(root-owned immutable store paths); a `$OCX_HOME`-owned tree would too.

**Auto-activation and lazy-loading are the same decision.** Homebrew explicitly refuses to
touch rc files ("startup files and plugin managers vary"); Nix relies on home-manager to
wire `fpath`. Both are eager-cost shells' problem, not bash/fish's.

Trend: **dynamic** completion is displacing static generation — Cobra's hidden `__complete`
subcommand and clap's `CompleteEnv` (`COMPLETE=<shell>`, feature `unstable-dynamic`) both
make one `eval` line always current with the installed binary. That matters double for a
package manager, which would otherwise regenerate every installed tool's completions on
every version bump (cf. [`#262`](https://github.com/ocx-sh/ocx/issues/262)).
[carapace-bin](https://carapace.sh/) is the escape hatch for nushell, which `clap_complete`
does not cover.

## Sources

Dev Containers — [JSON reference](https://containers.dev/implementors/json_reference/) ·
[Features](https://containers.dev/implementors/features/) ·
[Features distribution](https://containers.dev/implementors/features-distribution/) ·
[spec](https://containers.dev/implementors/spec/) ·
[supporting-tools.md](https://github.com/devcontainers/spec/blob/main/docs/specs/supporting-tools.md) ·
[base schema](https://github.com/devcontainers/spec/blob/main/schemas/devContainer.base.schema.json)

Namespacing — [Cargo manifest `metadata`](https://doc.rust-lang.org/cargo/reference/manifest.html#the-metadata-table) ·
[cargo-binstall SUPPORT.md](https://github.com/cargo-bins/cargo-binstall/blob/main/SUPPORT.md) ·
[PEP 621](https://peps.python.org/pep-0621/) ·
[OCI annotations](https://github.com/opencontainers/image-spec/blob/main/annotations.md) ·
[OCI distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) ·
[OCI 1.1 referrers](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) ·
[k8s labels/annotations](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/) ·
[k8s 256 KiB cap](https://github.com/kubernetes/kubernetes/pull/16068) ·
[Artifact Hub Helm annotations](https://artifacthub.io/docs/topics/annotations/helm/) ·
[CycloneDX properties](https://cyclonedx.org/use-cases/cyclonedx-properties/) ·
[nixpkgs meta](https://ryantm.github.io/nixpkgs/functions/library/meta/)

Propagation — [CMake `target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html) ·
[Gradle Java Library plugin](https://docs.gradle.org/current/userguide/java_library_plugin.html) ·
[pkg-config guide](https://people.freedesktop.org/~dbn/pkg-config-guide.html) ·
[Rust RFC 3516](https://rust-lang.github.io/rfcs/3516-public-private-dependencies.html) ·
[Nix Pills — dependencies and hooks](https://nixos.org/guides/nix-pills/20-basic-dependencies-and-hooks.html) ·
[Bazel Java rules](https://bazel.build/reference/be/java) ·
[Strict Java Deps](https://blog.bazel.build/2017/06/28/sjd-unused_deps.html)

Precedence — [VS Code settings](https://code.visualstudio.com/docs/configure/settings) ·
[systemd-system.conf(5)](https://www.man7.org/linux/man-pages/man5/systemd-system.conf.5.html) ·
[EditorConfig spec](https://spec.editorconfig.org/index.html) ·
[Helm values](https://helm.sh/docs/chart_template_guide/values_files/)

IDEs — [IntelliJ devcontainer integration](https://www.jetbrains.com/help/idea/customizing-devcontainer-json-file.html) ·
[Zed Dev Containers](https://zed.dev/docs/dev-containers) ·
[Coder integrations](https://coder.com/docs/user-guides/devcontainers/customizing-dev-containers) ·
[devfile.io](https://devfile.io/docs/2.3.0/what-is-a-devfile) ·
[GitHub Codespaces dev containers](https://docs.github.com/en/codespaces/setting-up-your-project-for-codespaces/configuring-dev-containers)

Completions — [bash-completion README](https://github.com/scop/bash-completion/blob/main/README.md) ·
[fish interactive docs](https://fishshell.com/docs/current/interactive.html) ·
[Homebrew Shell Completion](https://docs.brew.sh/Shell-Completion) ·
[nixpkgs installShellFiles](https://ryantm.github.io/nixpkgs/hooks/installShellFiles/) ·
[clap_complete `CompleteEnv`](https://docs.rs/clap_complete/latest/clap_complete/env/struct.CompleteEnv.html) ·
[Cobra completion system](https://deepwiki.com/spf13/cobra/3-shell-completion-system) ·
[zsh compaudit issue](https://github.com/zsh-users/zsh-completions/issues/433) ·
[carapace-bin](https://github.com/carapace-sh/carapace-bin)
