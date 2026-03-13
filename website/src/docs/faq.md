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

**Tolerant input:** typing `+` works and auto-normalizes to `_`. For example, `ocx install cmake:3.28.1+20260216` installs the tag `3.28.1_20260216`. This normalization happens at the earliest boundary — the identifier parser — so all downstream code sees `_` only.

See [Versioning][ug-versioning] in the user guide for the full tag hierarchy and cascade behavior.

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

Package metadata often exposes tools as shell scripts (`.bat` on Windows). When [`ocx exec`][cmd-exec] runs a command, it needs to find these scripts by consulting the <Tooltip term="PATHEXT">`PATHEXT` is a semicolon-separated list of file extensions (e.g. `.COM;.EXE;.BAT;.CMD`) that Windows uses to resolve bare command names. When you type `hello` in a terminal, the shell appends each extension in turn — `hello.COM`, `hello.EXE`, `hello.BAT` — and searches `PATH` for the first match.</Tooltip> environment variable, just like a Windows shell would.

ocx resolves this automatically using the [which][which-crate] crate: before spawning the child process, it searches `PATH` with `PATHEXT`-aware extension matching. If resolution fails — for example because `PATHEXT` is not set in a stripped-down CI environment — ocx logs a warning and falls back to the bare command name, letting the OS attempt its own lookup.

::: warning PATHEXT must be set
In environments with a minimal set of environment variables (containers, CI runners, custom shells), `PATHEXT` may not be present. Without it, ocx cannot resolve `.bat` or `.cmd` scripts by name. If you see `Could not resolve 'hello' via PATH`, ensure `PATHEXT` is set — the default Windows value is `.COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC`.
:::

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

<!-- commands -->
[cmd-exec]: ./reference/command-line.md#exec

<!-- environment -->
[env-no-codesign]: ./reference/environment.md#ocx-no-codesign

<!-- apple -->
[amfi]: https://support.apple.com/guide/security/app-security-overview-sec35dd877d0/web
[developer-mode]: https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device
