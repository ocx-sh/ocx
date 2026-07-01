---
outline: deep
---

# Installation {#installation}

One command installs ocx and puts it on your `PATH`. Pick your shell, paste, done:

::: code-group
```sh [Shell]
curl -fsSL https://setup.ocx.sh/sh | sh
```

```powershell [PowerShell]
Invoke-RestMethod 'https://setup.ocx.sh/pwsh' | Invoke-Expression
```

```nushell [Nushell]
curl -fsSL https://setup.ocx.sh/nu | nu
```

```fish [fish]
curl -fsSL https://setup.ocx.sh/fish | fish
```

```sh [Elvish]
curl -fsSL https://setup.ocx.sh/elvish | elvish
```
:::

Open a new terminal and verify with [`ocx about`][cmd-about], which prints the installed version, supported platforms, and detected shell:

```sh
ocx about
```

Prefer not to pipe a script into your shell? Jump to [Manual Installation][manual] to download the binary and run [`ocx self setup`][cmd-self-setup] instead.

## With Script {#with-script}

The one-liners above are thin per-shell bootstraps served by [setup.ocx.sh][setup-home]. Each detects your platform, resolves the latest release from the [`dist.json`][setup-dist] manifest (no GitHub API call in the install path), downloads the archive and verifies its SHA-256 against the manifest, then hands off to [`ocx self setup`][cmd-self-setup] — which performs the [content-addressed][fs-objects] self-install, writes the per-shell `$OCX_HOME/env.*` shims, and adds a managed activation block to your shell profile.

There is one installer per shell because a bash-only one-liner either fails to parse in [fish][fish], [Nushell][nushell], [Elvish][elvish], or [PowerShell][powershell], or leaves the profile unwired. Each script is written in its own shell's dialect, so the one-liner parses natively and the managed block lands in the right profile file.

::: warning Old wget (<1.17) is rejected by the POSIX installer
The [`setup.ocx.sh/sh`][setup-sh] installer fails closed when only an older `wget` is available — the strict TLS flags it relies on were added in wget 1.17 (2015), and downgrading would silently weaken the download integrity check. Alpine edge and some minimal Docker base images still ship older builds. If you hit this, either install a newer `wget` (`apk add --upgrade wget` on Alpine) or rely on `curl`, which the installer prefers whenever it is present.
:::

### Options {#script-options}

The installer takes a few knobs, passed as flags after `-s --` (POSIX `sh`) or as environment variables that work across every shell's argument-passing quirks.

**Skip shell profile modification.** If you manage your `PATH` yourself — in a CI environment or a dotfile framework — pass `--no-modify-path` so the installer never touches your shell profile:

```sh
curl -fsSL https://setup.ocx.sh/sh | sh -s -- --no-modify-path
```

You can also set [`OCX_NO_MODIFY_PATH=1`][env-no-modify-path]. Either way, add `~/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin` to your `PATH` yourself.

**Pin a version.** The `OCX_INSTALL_VERSION` env knob is the portable way to install an exact release — `<VERSION>` is the semver string with no leading `v`:

```sh
OCX_INSTALL_VERSION=0.5.0 curl -fsSL https://setup.ocx.sh/sh | sh
```

For CI, the pinned, immutable URL is the most explicit form:

```sh
curl -fsSL https://setup.ocx.sh/sh/0.5.0 | sh
```

PowerShell needs the script compiled to a scriptblock so `-Version` binds to its parameter:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod 'https://setup.ocx.sh/pwsh'))) -Version 0.5.0
```

**Track prereleases.** To follow the latest prerelease instead of stable, use the `next` channel on any per-shell prefix — `https://setup.ocx.sh/sh/next`, `https://setup.ocx.sh/pwsh/next`, and so on.

### Supported Shells {#shells}

[setup.ocx.sh][setup-home] serves a separate installer per shell. Pipe each URL to its matching interpreter — `| sh`, `| nu`, `| fish`, `| elvish`, or `irm … | iex` for [PowerShell][powershell]. The POSIX `sh` installer covers every Bourne-family shell with one script.

| Shell | Installer URL | Platforms |
|---|---|---|
| POSIX `sh` (bash, zsh, ash, ksh, dash) | [`setup.ocx.sh/sh`][setup-sh] | <PlatformIcons mode="os" :platforms="['linux','darwin']" /> |
| [PowerShell][powershell] | `setup.ocx.sh/pwsh` | <PlatformIcons mode="os" :platforms="['windows','linux','darwin']" /> |
| [Nushell][nushell] | `setup.ocx.sh/nu` | <PlatformIcons mode="os" :platforms="['linux','darwin','windows']" /> |
| [fish][fish] | `setup.ocx.sh/fish` | <PlatformIcons mode="os" :platforms="['linux','darwin']" /> |
| [Elvish][elvish] | `setup.ocx.sh/elvish` | <PlatformIcons mode="os" :platforms="['linux','darwin','windows']" /> |

## Manual Installation {#manual}

Security-conscious environments often forbid piping a network script into an interpreter. The downloaded binary is the answer: pre-built binaries for every supported platform are published to [GitHub Releases][releases]. Download the archive for your platform, extract it, and run [`ocx self setup`][cmd-self-setup] — you reach exactly the same managed state the install script produces (the content-addressed self-install, the `$OCX_HOME/env.*` shims, and the managed shell-profile block), with no shell script piped from the network.

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

```sh
# Download the archive and checksums
curl -LO https://github.com/ocx-sh/ocx/releases/latest/download/ocx-x86_64-unknown-linux-gnu.tar.xz
curl -LO https://github.com/ocx-sh/ocx/releases/latest/download/sha256sum.txt

# Verify checksum
sha256sum --check --ignore-missing sha256sum.txt

# Extract and run the managed setup
tar xf ocx-x86_64-unknown-linux-gnu.tar.xz
./ocx-x86_64-unknown-linux-gnu/ocx self setup
```

See [Install without `curl | sh`][ug-bare-binary] in the user guide for the full bare-binary flow, including the Windows and pinned-version variants.

::: info The musl builds are fully statically linked
The `musl` variants have no runtime dependency on glibc. Use them in Alpine containers, minimal Docker images, or any environment where libc compatibility is uncertain.
:::

### Pinned versions {#manual-pinned}

When a job needs the same ocx build on every run — regardless of when it runs or who triggers it — pass a version to [`ocx self setup`][cmd-self-setup]. The version can be a tag, a content digest, or both:

```sh
# Pin by tag (resolves once, installs that release):
ocx self setup 0.9.2

# Pin by tag and assert the exact content (immutability assertion):
ocx self setup 0.9.2@sha256:ab12cd34ef56...
```

The `tag@digest` form is the strongest guarantee: if the tag ever resolves to different content — for example because a digest was republished — the command fails with exit 65 and names both digests. No silent drift.

::: warning Digest pins are platform-specific
A `sha256:` digest pin selects the platform-specific package digest — the same tag resolves to a different digest on each OS and architecture. For CI matrices, pin by tag (each runner resolves its own platform digest), or supply a per-platform digest map; never share one digest value across different platforms.
:::

To get the digest for a version you trust, capture it from a prior run's JSON output:

```sh
digest=$(ocx --format json self setup 0.9.2 | jq -r .bootstrap.digest)
# Then write the full pin into your CI config:
echo "ocx self setup 0.9.2@$digest"
```

That digest round-trips: the next `ocx self setup 0.9.2@<digest>` call succeeds immediately (`already_present`) if the same version is already installed, or downloads and verifies the exact content otherwise. Under `--frozen`, a tag-only pin resolves from the local index (exit 81 if the tag is absent) and a digest-only pin works when the blobs are already cached — both suitable for hermetic CI where the index is pre-populated.

## Updating {#updating}

Once ocx is installed, it updates itself — no index refresh, reinstall, and reselect dance. [`ocx self update`][cmd-self-update] resolves the latest release through your [local index][fs-index], installs it, and swaps the `current` symlink, then heals the `$OCX_HOME/env.*` shims and the managed profile block so the refreshed binary stays wired into your shells:

```sh
ocx self update
```

To report whether a newer version exists without installing anything, add `--check`:

```sh
ocx self update --check
```

Version discovery honors `--offline` and `--frozen` because it runs through the [local index][fs-index] like every other command; pass [`--remote`][arg-remote] to force a live probe of the registry for the newest published release.

::: tip `self update` is not `upgrade`
[`ocx self update`][cmd-self-update] upgrades the ocx binary itself. It is distinct from `ocx upgrade`, which bumps the versions pinned in a project's `ocx.lock` — different verb, different target. Don't reach for one expecting the other.
:::

Alternatively, re-run the installer — it always fetches the latest release:

::: code-group
```sh [Shell]
curl -fsSL https://setup.ocx.sh/sh | sh
```

```powershell [PowerShell]
Invoke-RestMethod 'https://setup.ocx.sh/pwsh' | Invoke-Expression
```
:::

## Uninstalling {#uninstalling}

To completely remove ocx and all installed packages, delete the data directory (default [`~/.ocx`][env-home], or whatever [`OCX_HOME`][env-home] points to):

::: code-group
```sh [Shell]
rm -rf "${OCX_HOME:-$HOME/.ocx}"
```

```powershell [PowerShell]
Remove-Item -Recurse -Force $(if ($env:OCX_HOME) { $env:OCX_HOME } else { "$env:USERPROFILE\.ocx" })
```
:::

Then remove the managed activation that [`ocx self setup`][cmd-self-setup] added to your shell. POSIX shells, Elvish, and PowerShell get a fenced block that opens with `# >>> ocx v1 <hash> >>>` and closes with `# <<< ocx <<<`; fish and Nushell get a dedicated file. Remove whichever applies:

- **bash / zsh / sh / dash / ksh**: delete the `# >>> ocx v1 … >>>` … `# <<< ocx <<<` block from `~/.profile`, `~/.bash_profile`, `~/.bashrc`, `~/.zprofile`, and `~/.zshrc` (whichever exist)
- **fish**: delete `~/.config/fish/conf.d/ocx.fish`
- **Nushell**: delete `~/.local/share/nushell/vendor/autoload/ocx.nu`
- **Elvish**: delete the `# >>> ocx v1 … >>>` … `# <<< ocx <<<` block from `~/.config/elvish/rc.elv`
- **PowerShell**: delete the `# >>> ocx v1 … >>>` … `# <<< ocx <<<` block from your `$PROFILE`

::: warning This deletes everything
This removes all installed packages, the local index cache, and ocx itself. If you only want to remove specific packages, use [`ocx uninstall --purge`][cmd-uninstall] and [`ocx clean`][cmd-clean] instead.
:::

## CI Setup {#ci}

Most CI jobs should install ocx through the [`ocx-sh/setup-ocx`][setup-ocx] GitHub Action rather than a raw one-liner. A single `uses:` step installs ocx, and in project mode it pulls the toolchain pinned in `ocx.lock`, replays [`ocx env`][cmd-env] into the job environment, and caches on the lockfile hash. Pin the action to a commit SHA:

```yaml
- uses: ocx-sh/setup-ocx@<sha> # v1.2.2
```

On runners without the action — other CI systems, or a bare shell step — install with a pinned form so every run gets the same build: the [version-pinned installer URL](#script-options) or [`ocx self setup <tag>`](#manual-pinned). See [CI Integration][ci-indepth] for caching, private-registry auth, and the full project-mode flow.

## Dev Builds {#dev-builds}

Binaries published from the `deploy-dev` workflow carry a `channel: dev` field and a version string with a build-metadata suffix, such as `0.3.2-dev+20260528143045`. These are pre-release snapshots, not stable releases.

To identify whether your installed binary is a dev build, run:

```sh
ocx version --verbose
```

The `channel: dev` line appears in the verbose output when present. For scripting use:

```sh
ocx --format json version | jq .channel
```

This returns `"dev"` for dev builds and `null` (field absent) for stable releases. Include this output when filing bug reports — it helps distinguish issues introduced between stable releases.

## Next Steps {#next-steps}

- **[Getting Started][getting-started]** — install your first package, switch versions, and set up shell integration.
- **[User Guide][user-guide]** — the [three-store architecture][fs-objects], versioning, locking, and [authentication][auth] for private registries.
- **[Environment Reference][env-ref]** — all environment variables ocx reads, including [`OCX_HOME`][env-home] for custom data directories.

<!-- external -->
[setup-home]: https://setup.ocx.sh
[setup-sh]: https://setup.ocx.sh/sh
[setup-dist]: https://setup.ocx.sh/dist
[setup-ocx]: https://github.com/ocx-sh/setup-ocx
[releases]: https://github.com/ocx-sh/ocx/releases/latest
[powershell]: https://learn.microsoft.com/en-us/powershell/
[nushell]: https://www.nushell.sh/
[fish]: https://fishshell.com/
[elvish]: https://elv.sh/

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
[ci-indepth]: ./in-depth/ci.md

<!-- commands -->
[cmd-about]: ./reference/command-line.md#about
[cmd-env]: ./reference/command-line.md#env-root
[cmd-self-setup]: ./reference/command-line.md#self-setup
[cmd-self-update]: ./reference/command-line.md#self-update
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-clean]: ./reference/command-line.md#clean
[arg-remote]: ./reference/command-line.md#arg-remote

<!-- environment -->
[env-home]: ./reference/environment.md#ocx-home
[env-no-modify-path]: ./reference/environment.md#ocx-no-modify-path

<!-- internal -->
[manual]: #manual

<!-- in depth -->
[fs-objects]: ./in-depth/storage.md#stores
[fs-index]: ./in-depth/indices.md#local

<!-- user guide sections -->
[auth]: ./user-guide.md#authentication
[ug-bare-binary]: ./user-guide.md#install-bare-binary
