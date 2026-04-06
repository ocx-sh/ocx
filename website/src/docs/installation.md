---
outline: deep
---

# Installation {#installation}

## Quick Install {#quick-install}

The install scripts download the latest ocx binary from [GitHub Releases][releases], verify its SHA-256 checksum, and place it under [`~/.ocx`][env-home]. They also configure your shell so that `ocx` is available on `PATH` in new terminals.

::: code-group
```sh [Shell]
curl -fsSL https://ocx.sh/install.sh | sh
```

```ps1 [PowerShell]
irm https://ocx.sh/install.ps1 | iex
```
:::

After installation, open a new terminal and verify:

```sh
ocx info
```

::: tip Skip shell profile modification
If you manage your `PATH` manually — for example, in a CI environment or a dotfile framework — pass `--no-modify-path` to prevent the installer from touching your shell profile:

```sh
curl -fsSL https://ocx.sh/install.sh | sh -s -- --no-modify-path
```

You can also set [`OCX_NO_MODIFY_PATH=1`][env-no-modify-path] as an environment variable. Either way, add `~/.ocx/symlinks/ocx.sh/ocx/current/bin` to your `PATH` yourself.
:::

## GitHub Releases {#github-releases}

Pre-built binaries for all supported platforms are published to [GitHub Releases][releases]. Download the archive for your platform, extract it, and place the `ocx` binary somewhere on your `PATH`.

| Platform | Architecture | Archive |
|---|---|---|
| Linux | x86_64 | [`ocx-x86_64-unknown-linux-gnu.tar.xz`][dl-linux-x64] |
| Linux | aarch64 | [`ocx-aarch64-unknown-linux-gnu.tar.xz`][dl-linux-arm64] |
| Linux (static) | x86_64 | [`ocx-x86_64-unknown-linux-musl.tar.xz`][dl-linux-musl-x64] |
| Linux (static) | aarch64 | [`ocx-aarch64-unknown-linux-musl.tar.xz`][dl-linux-musl-arm64] |
| macOS | Apple Silicon | [`ocx-aarch64-apple-darwin.tar.xz`][dl-macos-arm64] |
| macOS | Intel | [`ocx-x86_64-apple-darwin.tar.xz`][dl-macos-x64] |
| Windows | x86_64 | [`ocx-x86_64-pc-windows-msvc.zip`][dl-win-x64] |
| Windows | ARM64 | [`ocx-aarch64-pc-windows-msvc.zip`][dl-win-arm64] |

Each release also includes a `sha256sum.txt` file for checksum verification.

::: details Manual install example (Linux x86_64)
```sh
# Download the archive and checksums
curl -LO https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-unknown-linux-gnu.tar.xz
curl -LO https://github.com/ocx-sh/ocx/releases/latest/download/sha256sum.txt

# Verify checksum
sha256sum --check --ignore-missing sha256sum.txt

# Extract and move into place
tar xf ocx-x86_64-unknown-linux-gnu.tar.xz
install -m 755 ocx-x86_64-unknown-linux-gnu/ocx ~/.local/bin/ocx
```
:::

::: info The musl builds are fully statically linked
The `musl` variants have no runtime dependency on glibc. Use them in Alpine containers, minimal Docker images, or any environment where libc compatibility is uncertain.
:::

## Updating {#updating}

Once ocx is installed, it can manage its own updates. Refresh the local index to discover new versions, then install and select the latest:

```sh
ocx index update ocx
ocx install --select ocx
```

Alternatively, re-run the install script — it always fetches the latest release:

::: code-group
```sh [Shell]
curl -fsSL https://ocx.sh/install.sh | sh
```

```ps1 [PowerShell]
irm https://ocx.sh/install.ps1 | iex
```
:::

::: tip Pin a specific version
To install a specific version instead of the latest, pass `--version` (Shell) or set `$Version` (PowerShell) — see the examples below.
:::

::: code-group
```sh [Shell]
curl -fsSL https://ocx.sh/install.sh | sh -s -- --version 0.5.0
```

```ps1 [PowerShell]
& { $Version = '0.5.0'; irm https://ocx.sh/install.ps1 | iex }
```
:::

## Verify Installation {#verify}

[`ocx info`][cmd-info] prints the installed version, supported platforms, and detected shell:

```sh
ocx info
```

## Uninstalling {#uninstalling}

To completely remove ocx and all installed packages, delete the data directory (default [`~/.ocx`][env-home], or whatever [`OCX_HOME`][env-home] points to):

::: code-group
```sh [Shell]
rm -rf "${OCX_HOME:-$HOME/.ocx}"
```

```ps1 [PowerShell]
Remove-Item -Recurse -Force $(if ($env:OCX_HOME) { $env:OCX_HOME } else { "$env:USERPROFILE\.ocx" })
```
:::

Then remove the shell integration line from your profile:

- **bash**: remove `. "$HOME/.ocx/env"` from `~/.bash_profile` or `~/.profile`
- **zsh**: remove `. "$HOME/.ocx/env"` from `~/.zshenv`
- **fish**: delete `~/.config/fish/conf.d/ocx.fish`
- **PowerShell**: remove the `. "...\.ocx\env.ps1"` line from your `$PROFILE`

::: warning This deletes everything
This removes all installed packages, the local index cache, and ocx itself. If you only want to remove specific packages, use [`ocx uninstall --purge`][cmd-uninstall] and [`ocx clean`][cmd-clean] instead.
:::

## Next Steps {#next-steps}

- **[Getting Started][getting-started]** — install your first package, switch versions, and set up shell integration.
- **[User Guide][user-guide]** — the [three-store architecture][fs-objects], versioning, locking, and [authentication][auth] for private registries.
- **[Environment Reference][env-ref]** — all environment variables ocx reads, including [`OCX_HOME`][env-home] for custom data directories.

<!-- external -->
[releases]: https://github.com/ocx-sh/ocx/releases/latest

<!-- downloads -->
[dl-linux-x64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-unknown-linux-gnu.tar.xz
[dl-linux-arm64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-aarch64-unknown-linux-gnu.tar.xz
[dl-linux-musl-x64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-unknown-linux-musl.tar.xz
[dl-linux-musl-arm64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-aarch64-unknown-linux-musl.tar.xz
[dl-macos-arm64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-aarch64-apple-darwin.tar.xz
[dl-macos-x64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-apple-darwin.tar.xz
[dl-win-x64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-pc-windows-msvc.zip
[dl-win-arm64]: https://github.com/ocx-sh/ocx/releases/latest/download/ocx-aarch64-pc-windows-msvc.zip

<!-- pages -->
[getting-started]: ./getting-started.md
[user-guide]: ./user-guide.md
[env-ref]: ./reference/environment.md

<!-- commands -->
[cmd-info]: ./reference/command-line.md#info
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-clean]: ./reference/command-line.md#clean

<!-- environment -->
[env-home]: ./reference/environment.md#ocx-home
[env-no-modify-path]: ./reference/environment.md#ocx-no-modify-path

<!-- user guide sections -->
[fs-objects]: ./user-guide.md#file-structure-objects
[auth]: ./user-guide.md#authentication
