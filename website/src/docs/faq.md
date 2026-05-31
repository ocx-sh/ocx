---
outline: deep
---
# FAQ

## Versioning

### Build Separator {#versioning-build-separator}

[Semantic Versioning][semver] uses `+` to delimit build metadata (e.g., `1.2.3+20260216`). The [OCI Distribution Specification][oci-dist-spec] restricts tags to `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` — the `+` character is not allowed. This is a known ecosystem-wide issue ([distribution-spec#154][oci-dist-issue], open since 2020, unresolved). The `+` also encodes as a space in URL query strings, making it unsafe even if registries accepted it.

ocx uses `_` as the build separator so that every version string is a valid OCI tag by construction:

```
{major}[.{minor}[.{patch}[-{prerelease}][_{build}]]]
```

The grammar is fully unambiguous: `.` separates components, `-` introduces a pre-release, `_` introduces build metadata.

::: info Same convention as Helm
[Helm][helm-oci] adopted the same `+` → `_` normalization when it moved chart distribution to OCI registries. See [helm/helm#10250][helm-issue] for the original discussion.
:::

**Tolerant input:** typing `+` works and auto-normalizes to `_`. For example, `ocx package install cmake:3.28.1+20260216` installs the tag `3.28.1_20260216`. This normalization happens at the earliest boundary — the identifier parser — so all downstream code sees `_` only.

See [Versioning][ug-versioning] in the user guide for the full tag hierarchy and cascade behavior.

## Installation {#installation-faq}

### How do I install a specific ocx version? {#install-specific-version}

Pass a version to [`ocx self setup`][cmd-self-setup] using the downloaded binary. The `VERSION` argument accepts three forms:

```sh
# Install by tag:
/tmp/ocx self setup 0.9.2

# Install by content digest (no tag resolution):
/tmp/ocx self setup sha256:ab12cd34ef56...

# Install by tag and verify the digest (immutability assertion):
/tmp/ocx self setup 0.9.2@sha256:ab12cd34ef56...
```

The `tag@digest` form is the strongest reproducibility guarantee for CI. If the tag ever points to different content, the command exits 65 and names both digests. Note that a digest pin is **platform-specific**: the same tag resolves to a different digest on each OS and architecture, so a digest captured on one runner cannot be shared across platforms. For CI matrices, pin by tag. The digest value for a single platform comes from the JSON output of a prior run:

```sh
digest=$(/tmp/ocx --format json self setup 0.9.2 | jq -r .bootstrap.digest)
# Subsequent runs assert the exact same content:
/tmp/ocx self setup "0.9.2@$digest"
```

That digest round-trips: re-running `/tmp/ocx self setup 0.9.2@$digest` exits 0 with status `already_present` when the same version is already installed and pointed to by `current`. No redundant download occurs.

When the specified version is already installed, `ocx self setup` is a no-op and exits 0 — the shims and profile blocks are only re-written when their content has drifted.

::: tip Offline and frozen environments
A digest-only pin works under `--frozen` when the blobs are cached locally. A tag-only pin under `--frozen` resolves from the local index; the command exits 81 if the tag is absent from the index — run `ocx index update` first.
:::

## Project Toolchain {#project}

### When should I use `ocx package exec` vs `ocx run`? {#exec-vs-run}

**Short rule:** if you have an `ocx.toml`, use [`ocx run`][cmd-run]; if you do not, use [`ocx package exec`][cmd-package-exec].

[`ocx package exec`][cmd-package-exec] is the OCI-tier command — its first argument is an OCI identifier (`node:20`, `ocx.sh/cmake:3.28@sha256:…`). It never reads `ocx.toml` or `ocx.lock`, so it behaves identically regardless of the current directory and regardless of any project file nearby. This makes it the right primitive for embedding in [GitHub Actions][github-actions-docs], [Bazel rules][bazel-rules], and CI scripts that manage their own tool pins.

[`ocx run`][cmd-run] is the project-tier command — its symbols are binding names declared in `ocx.toml` (e.g. `cmake`, `shellcheck`). It resolves those names through `ocx.lock`, auto-installs missing packages, composes the declared environment, and spawns the child. A missing `ocx.toml` is a usage error (exit 64), not a fallback to OCI-tier behavior.

Both commands:

- Auto-install missing packages from the registry
- Compose and forward the [package-declared environment][in-depth-environments]
- Accept `--clean` to strip inherited shell state
- Accept `--self` to expose private-visibility env entries
- Forward the child's exit code byte-for-byte

Neither command is deprecated. They cover complementary use cases — running a tool by its project-assigned name (`run`) vs running it by its registry identity (`package exec`).

See [Project Toolchain In Depth → Running tools][in-depth-project-running] for the full contract, composition order, and exit code table.

## Dependencies {#dependencies}

### No Version Ranges {#dependencies-no-version-ranges}

ocx dependencies are pinned by <Tooltip term="OCI digest">A SHA-256 content fingerprint. The exact bytes of the dependency determine the digest — the same bytes always produce the same fingerprint. No index lookup, no version resolution, no "find me the latest compatible build."</Tooltip>, not by version ranges. There is no "give me any Java >= 21" logic. The publisher records the exact build they tested against, and that is what you get.

This is a deliberate design choice. Version ranges introduce a resolution algorithm — typically [SAT-solving][sat-solving] or [Minimum Version Selection][go-mvs] — that produces different results depending on what is available in the index at resolution time. Two machines with different index states can resolve the same dependency specification to different binaries. This directly contradicts ocx's reproducibility guarantee: the same metadata should always produce the same result.

::: details Why not Minimum Version Selection?
[Go modules][go-modules] and [Bazel's Bzlmod][bazel-bzlmod] use Minimum Version Selection (MVS) — the highest minimum version requested by any dependent wins. MVS is deterministic and polynomial-time, but it still requires an index to enumerate available versions. ocx dependencies work offline with no index at all: the digest is the pin. Future tooling will help publishers discover when their pinned dependencies have newer builds available, keeping the update decision explicit.
:::

### Automatic Security Patches {#dependencies-security-patches}

When a dependency receives a security fix, the publisher must release a new version of their package with the updated digest. ocx does not automatically substitute a newer build — doing so would break the reproducibility guarantee.

This tradeoff matches the [Nix][nix] model: a security patch means rebuilding every affected derivation. The difference is that ocx has no build system — the publisher re-pins the digest and pushes a new tag.

### Shared Dependencies {#dependencies-shared}

When multiple installed packages depend on the same package at the same digest, the dependency is stored once in the [object store][fs-objects] (content-addressed deduplication). Each dependent creates a back-reference, so the shared dependency is not garbage collected until all dependents are removed.

When two packages depend on the same tool at *different* digests, both versions are installed as separate objects. If both would contribute to the same environment — through `ocx package env`, `ocx env`, `ocx package exec`, or `ocx run` — composition fails: a single environment cannot expose two versions of one package, since `PATH` resolves only one. The error names the conflicting repository and the versions involved. Two tags that resolve to the *same* digest are the same version and never conflict. Use [`ocx package deps --flat`][cmd-deps] to see the evaluation order and [`ocx package deps --why`][cmd-deps] to trace the conflicting paths — `deps` reports the conflict as a non-fatal warning so the tree stays inspectable.

See [Dependencies][ug-dependencies] in the user guide for the full picture: automatic installation, environment composition, garbage collection, and inspection commands.

## Linux

### Why won't OCX install a glibc binary on Alpine, even with gcompat? {#linux-gcompat}

OCX selects index entries based on identity, not capability. When OCX probes the host's dynamic linker, it reports what the loader *identifies itself as* — not what it can emulate. On [Alpine Linux][alpine] with [gcompat][gcompat] installed, the linker is still the musl loader (`/lib/ld-musl-x86_64.so.1`), and its `--version` output reads `musl libc`. OCX detects `libc.musl` and selects the `libc.musl` manifest entry.

Forcing a glibc binary onto a musl host via gcompat would work most of the time, but it is fragile. gcompat implements a subset of glibc's ABI. Binaries that call glibc functions not covered by gcompat fail at runtime with a symbol-lookup error — a harder-to-diagnose failure than the install failing cleanly.

The predictable rule — "ld.so tells the truth about what it is" — means every install is either correct or fails with a clear feature mismatch error. It never silently installs a binary that might or might not work.

**If you specifically need the glibc binary on a gcompat host**, pass `--platform` with an explicit `+libc.glibc` feature tag — e.g. `ocx package install mytool:1.0.0 --platform linux/amd64+libc.glibc`. This forces selection of the `libc.glibc`-tagged manifest entry — it is a real override, not a bypass. The resulting binary runs only if your gcompat installation covers the glibc symbols that binary calls. Binaries that call glibc functions not in gcompat's subset fail at runtime with a symbol-lookup error. This is an escape hatch for the cases where you know the specific binary works — for example, cross-fetching to bundle elsewhere — not a general workaround. OCX installs the binary; whether `ld.so` loads it is between the binary and your gcompat version.

**The recommended path** is to publish a fully static build alongside the glibc and musl builds. A static binary carries no `os.features` declaration and matches every Linux host — gcompat or not, musl or glibc. See [libc Differentiation — Static binaries][authoring-libc-static] in the multi-platform authoring guide.

### Does OCX detect libc on NixOS, Gentoo Prefix, and other non-FHS hosts? {#linux-non-fhs}

For most non-FHS hosts — yes. OCX does not look for the dynamic loader at a fixed set of [Filesystem Hierarchy Standard][fhs] paths. It reads the loader path straight out of a present system binary: the [`PT_INTERP`][pt-interp] field every dynamically linked ELF carries names the exact loader the kernel will use, wherever it lives. In a [Gentoo Prefix][gentoo-prefix], under Homebrew-on-Linux, or in a custom sysroot, those probe binaries are dynamically linked and point to the prefix loader — detection works.

**[NixOS][nixos] depends on [nix-ld][nix-ld].** With nix-ld active, it installs an FHS shim at the canonical loader path (`/lib/ld-linux-x86-64.so.2`); OCX picks that up and detection works normally. Without nix-ld, the standard probe binaries on NixOS (`/usr/bin/env`, `/bin/sh`, `/bin/ls`) are statically linked and carry no `PT_INTERP`, the directory scan finds nothing at the FHS paths, and the set comes up empty — OCX logs a debug note when `/nix` is present and degrades to matching only entries with no `os.features`.

In any case where detection finds nothing — a truly minimal container, a static-only image, or NixOS without nix-ld — OCX falls back to matching only entries with no `os.features` (the static / universal builds). It never hard-fails; you can always force a variant with `--platform`.

### Does libc detection follow a binary into a container or distrobox? {#linux-namespace-gap}

No — and this is a deliberate, documented limitation. OCX detects the libc of **the host it runs on**, at install time. If you install on one host and then run the binary in a *different* namespace — a [distrobox][distrobox] / toolbox guest, a bind-mounted container, or a copied `~/.ocx` on another machine — that environment may provide a different libc than the one OCX detected.

For the normal flow this never bites: [`ocx package exec`][cmd-package-exec] and [`ocx run`][cmd-run] launch the binary on the same host and kernel that detection ran against. The gap only appears when the run environment is deliberately decoupled from the install environment.

::: warning Install where you run
When the run environment differs from the install environment (distrobox, a bind-mounted container, a copied store), install *inside* the environment you intend to run in, or pin the variant explicitly with `--platform linux/amd64+libc.glibc`. Exec-time / target-namespace detection is tracked for a future design pass.
:::

## macOS

### Code Signing {#macos-codesign}

macOS requires all executable code to carry a valid <Tooltip term="code signature">A cryptographic seal embedded in the binary's `__LINKEDIT` segment. macOS checks this seal before allowing execution. An ad-hoc signature — created with `codesign --sign -` — is a local integrity check that requires no Apple Developer certificate.</Tooltip>. On Apple Silicon the kernel terminates unsigned binaries immediately (`Killed: 9`). On Intel, <Tooltip term="Gatekeeper">A macOS subsystem that checks downloaded applications for valid signatures and notarization. Since Sequoia 15.1, the ability to bypass Gatekeeper warnings for unsigned apps has been removed entirely.</Tooltip> blocks them when a <Tooltip term="quarantine flag">The `com.apple.quarantine` extended attribute, automatically set by browsers and other download tools on files fetched from the internet.</Tooltip> is present.

When a publisher never signed their binaries before packaging, the extracted files will be unsigned and macOS will refuse to run them. Signatures that were present before packaging survive the tar round-trip — they are part of the binary content, not extended attributes.

ocx handles this automatically: after extracting a package, it recursively walks the content directory, detects <Tooltip term="Mach-O binaries">The native executable format on macOS and iOS. ocx identifies them by reading the first four bytes of each file and checking for known magic numbers (`0xFEEDFACF` for 64-bit, `0xCAFEBABE` for universal, etc.).</Tooltip>, and signs each one individually with an ad-hoc signature. Quarantine flags are stripped. No configuration required.

::: info Same approach as Homebrew
[Homebrew][homebrew] solves the identical problem with the same technique — per-file ad-hoc signing without bundle sealing — see [`codesign_patched_binary`][homebrew-codesign] in their source. ocx applies signatures after extraction rather than after patching, but the `codesign` invocation is equivalent.
:::

::: details What ocx runs under the hood

Quarantine removal (applied to the entire content directory first):

```sh
xattr -dr com.apple.quarantine <content_path>
```

For each Mach-O binary found in the content directory (recursive walk, symlinks not followed):

```sh
codesign --sign - --force --preserve-metadata=entitlements,flags,runtime <binary>
```

`entitlements`, `flags`, and `runtime` are preserved from the original signature.
`requirements` (the original certificate's Team ID constraint) is intentionally dropped —
preserving it would cause dyld "different Team IDs" errors when loading third-party frameworks.
Hardlinked files (same inode) are signed only once.
:::

::: tip For package publishers
Signing binaries before packaging is ideal — the signatures survive tar archiving and ocx will leave them intact. But it is not required: ocx applies ad-hoc signatures automatically for any unsigned binary it encounters.
:::

#### Disabling {#macos-codesign-disable}

Set [`OCX_NO_CODESIGN`][env-no-codesign] to a truthy value to skip automatic signing:

```sh
export OCX_NO_CODESIGN=1
```

#### Manual Signing {#macos-codesign-manual}

If a binary still fails to launch after installation, sign it manually:

::: code-group
```sh [Single binary]
codesign --sign - --force --preserve-metadata=entitlements,flags,runtime /path/to/binary
```

```sh [Remove quarantine]
xattr -dr com.apple.quarantine /path/to/content
```
:::

::: details Can I disable macOS code signing enforcement entirely?
macOS enforces code signatures through <Tooltip term="AMFI">Apple Mobile File Integrity — a kernel-level module that validates code signatures independently of Gatekeeper. It cannot be disabled without booting into Recovery Mode, turning off SIP, and setting `amfi_get_out_of_my_way=1`.</Tooltip>, which runs independently of [Gatekeeper][gatekeeper]. Disabling it requires Recovery Mode, disabling [SIP][sip], and setting a boot argument — a configuration Apple does not support that significantly weakens system security.

The macOS [Developer Mode][developer-mode] setting (since Ventura 13) relaxes restrictions for development tools like Xcode and Instruments, but does **not** exempt unsigned binaries. `Killed: 9` still occurs on Apple Silicon with Developer Mode enabled.

Ad-hoc signing via ocx is the simplest solution — no certificates, no system changes.
:::

## Windows

### Executable Resolution {#windows-exec-resolution}

Windows does not treat scripts the same way Unix does. On Unix, any file with a `#!/bin/sh` shebang and the execute bit set can be launched directly. On Windows, the kernel's [CreateProcessW][createprocessw] API only searches for `.exe` files — it ignores `.bat`, `.cmd`, and other script types entirely.

This applies to the *packaged third-party tool* a command resolves to, not to OCX's own entry-point launchers — those are native `<name>.exe` shims, and `.EXE` is unconditionally in the default Windows `PATHEXT`, so an OCX launcher is always resolvable by bare name with no `PATHEXT` configuration. The concern here is the binary *inside* a package: metadata often exposes tools as scripts (`.bat`/`.cmd` on Windows). When [`ocx exec`][cmd-exec] runs a command, it must find that script by consulting the <Tooltip term="PATHEXT">`PATHEXT` is a semicolon-separated list of file extensions (e.g. `.COM;.EXE;.BAT;.CMD`) that Windows uses to resolve bare command names. When you type `bun` in a terminal, the shell appends each extension in turn — `bun.COM`, `bun.EXE`, `bun.BAT` — and searches `PATH` for the first match.</Tooltip> environment variable, just like a Windows shell would.

ocx resolves this automatically using the [which][which-crate] crate: before spawning the child process, it searches `PATH` with `PATHEXT`-aware extension matching. If resolution fails — for example because `PATHEXT` is not set in a stripped-down CI environment — ocx logs a warning and falls back to the bare command name, letting the OS attempt its own lookup.

::: warning PATHEXT and packaged `.bat`/`.cmd` tools
This caveat is about a packaged tool that ships as a `.bat`/`.cmd` script — *not* about OCX's own launchers, which are `.exe` and always resolvable. In environments with a minimal set of environment variables (containers, CI runners, custom shells), `PATHEXT` may not be present. Without it, `ocx exec` cannot resolve a packaged `.bat`/`.cmd` tool by bare name. If you see `Could not resolve 'bun' via PATH`, ensure `PATHEXT` is set — the default Windows value is `.COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC`.
:::

<!-- linux -->
[alpine]: https://alpinelinux.org/
[gcompat]: https://gitlab.alpinelinux.org/alpine/gcompat
[nixos]: https://nixos.org/
[nix-ld]: https://github.com/nix-community/nix-ld
[gentoo-prefix]: https://wiki.gentoo.org/wiki/Project:Prefix
[distrobox]: https://distrobox.it/
[fhs]: https://refspecs.linuxfoundation.org/fhs.shtml
[pt-interp]: https://refspecs.linuxfoundation.org/elf/elf.pdf

<!-- authoring -->
[authoring-libc-static]: ./authoring/multi-platform.md#libc-static

<!-- dependencies -->
[sat-solving]: https://en.wikipedia.org/wiki/Boolean_satisfiability_problem
[go-mvs]: https://research.swtch.com/vgo-mvs
[go-modules]: https://go.dev/ref/mod
[bazel-bzlmod]: https://bazel.build/external/module
[nix]: https://nixos.org/

<!-- commands -->
[cmd-self-setup]: ./reference/command-line.md#self-setup
[cmd-deps]: ./reference/command-line.md#deps
[cmd-package-exec]: ./reference/command-line.md#package-exec
[cmd-run]: ./reference/command-line.md#run

<!-- in-depth -->
[in-depth-environments]: ./in-depth/environments.md
[in-depth-project-running]: ./in-depth/project.md#running

<!-- external -->
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules

<!-- internal -->
[fs-objects]: ./user-guide.md#file-structure-objects
[ug-dependencies]: ./user-guide.md#dependencies

<!-- versioning -->
[semver]: https://semver.org/
[oci-dist-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md
[oci-dist-issue]: https://github.com/opencontainers/distribution-spec/issues/154
[helm-oci]: https://helm.sh/docs/topics/registries/
[helm-issue]: https://github.com/helm/helm/issues/10250
[ug-versioning]: ./user-guide.md#versioning-tags

<!-- external -->
[mach-o]: https://en.wikipedia.org/wiki/Mach-O
[createprocessw]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw
[which-crate]: https://crates.io/crates/which
[gatekeeper]: https://support.apple.com/guide/security/gatekeeper-and-runtime-protection-sec5599b66df/web
[homebrew]: https://brew.sh/
[homebrew-codesign]: https://github.com/Homebrew/brew/blob/master/Library/Homebrew/extend/os/mac/keg.rb
[sip]: https://support.apple.com/en-us/102149

<!-- environment -->
[env-no-codesign]: ./reference/environment.md#ocx-no-codesign

<!-- apple -->
[amfi]: https://support.apple.com/guide/security/app-security-overview-sec35dd877d0/web
[developer-mode]: https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device
