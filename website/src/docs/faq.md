---
outline: deep
---
# FAQ

## macOS 

### Code Signing {#macos-codesign}

macOS requires all executable code to carry a valid <Tooltip term="code signature">A cryptographic seal embedded in the binary's `__LINKEDIT` segment. macOS checks this seal before allowing execution. An ad-hoc signature — created with `codesign --sign -` — is a local integrity check that requires no Apple Developer certificate.</Tooltip>. On Apple Silicon the kernel terminates unsigned binaries immediately (`Killed: 9`). On Intel, <Tooltip term="Gatekeeper">A macOS subsystem that checks downloaded applications for valid signatures and notarization. Since Sequoia 15.1, the ability to bypass Gatekeeper warnings for unsigned apps has been removed entirely.</Tooltip> blocks them when a <Tooltip term="quarantine flag">The `com.apple.quarantine` extended attribute, automatically set by browsers and other download tools on files fetched from the internet.</Tooltip> is present.

When a publisher never signed their binaries before packaging, the extracted files will be unsigned and macOS will refuse to run them. Signatures that were present before packaging survive the tar round-trip — they are part of the binary content, not extended attributes.

ocx handles this automatically: after extracting a package, it detects <Tooltip term="Mach-O binaries">The native executable format on macOS and iOS. ocx identifies them by reading the first four bytes of each file and checking for known magic numbers (`0xFEEDFACF` for 64-bit, `0xCAFEBABE` for universal, etc.).</Tooltip> and `.app` bundles in the content directory, applies ad-hoc signatures, and strips quarantine flags. No configuration required.

::: info Same approach as Homebrew
[Homebrew][homebrew] solves the identical problem with the same technique — see [`codesign_patched_binary`][homebrew-codesign] in their source. ocx applies the signatures after extraction rather than after patching, but the `codesign` invocation is equivalent.
:::

::: details What ocx runs under the hood

For individual Mach-O binaries:

```sh
codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime <binary>
```

For `.app` bundles (signs nested frameworks, helpers, and plugins):

```sh
codesign --sign - --force --deep <bundle.app>
```

Quarantine removal (applied to the entire content directory):

```sh
xattr -dr com.apple.quarantine <content_path>
```
:::

::: tip For package publishers
Signing binaries before packaging is ideal — the signatures survive tar archiving and ocx will leave them intact. But it is not required: ocx applies ad-hoc signatures automatically for any unsigned binary it encounters.
:::

#### Disabling {#macos-codesign-disable}

Set [`OCX_DISABLE_CODESIGN`][env-disable-codesign] to a truthy value to skip automatic signing:

```sh
export OCX_DISABLE_CODESIGN=1
```

#### Manual Signing {#macos-codesign-manual}

If a binary still fails to launch after installation, sign it manually:

::: code-group
```sh [Single binary]
codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime /path/to/binary
```

```sh [.app bundle]
codesign --sign - --force --deep /path/to/App.app
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

<!-- external -->
[mach-o]: https://en.wikipedia.org/wiki/Mach-O
[gatekeeper]: https://support.apple.com/guide/security/gatekeeper-and-runtime-protection-sec5599b66df/web
[homebrew]: https://brew.sh/
[homebrew-codesign]: https://github.com/Homebrew/brew/blob/master/Library/Homebrew/extend/os/mac/keg.rb
[sip]: https://support.apple.com/en-us/102149

<!-- environment -->
[env-disable-codesign]: ./reference/environment.md#ocx-disable-codesign

<!-- apple -->
[amfi]: https://support.apple.com/guide/security/app-security-overview-sec35dd877d0/web
[developer-mode]: https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device
