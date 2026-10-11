---
title: "Use ocx in dev containers and editors"
description: "Give a dev container, VS Code or any other editor the pinned toolchain of your project."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to give an editor and a [dev container][devcontainer] the same pinned toolchain that your shell and your CI use. It covers the VS Code extension, a dev container recipe, and the one query any other editor needs.

An editor does not source your shell profile, so a tool that works in your terminal can be "not found" in the editor's run configuration. The fix is the same everywhere: give the editor the directory that ocx resolved, and never build that path by hand.

## Use the VS Code extension {#editors-vscode}

[OCX for VS Code][vscode-ocx] is the maintained route for VS Code. Install `ocx-sh.ocx` from the extension marketplace. It needs the `ocx` CLI on `PATH`, or `ocx.path.executable` set.

Open a folder that holds an `ocx.toml`. The extension composes the project toolchain with the CLI and injects the result into the extension host. Language servers and other extensions then find the tools your project declares. Editing `ocx.toml` or `ocx.lock` reloads the environment.

Three settings decide the rest:

| Setting | Default | Effect |
|---|---|---|
| `ocx.env.applyToTerminals` | `false` | Puts the toolchain `PATH` on newly opened terminals and tasks |
| `ocx.groups` | `[]` | Composes the named [tool groups][guide-groups] instead of only the default group |
| `ocx.restart.automatic` | `false` | Restarts the extension host without asking when the environment changes |

The environment changes `PATH`, so it loads only in a trusted workspace. The extension also provides palette commands for `ocx lock`, `ocx pull`, `ocx upgrade` and `ocx clean`. Its README lists the commands, the settings and how to wire them into `tasks.json`.

## Point any editor at the toolchain {#editors-home}

An editor without an extension needs one directory on its `PATH`. Ask ocx for it:

```sh
ocx pull
ocx --format json shell state | jq -r .toolchain_bin
```

`toolchain_bin` is the directory that holds a launcher for every tool of the project in effect. Paste it into the editor's setting. In VS Code that is `terminal.integrated.env.<platform>` in `.vscode/settings.json`. Put the printed directory in front of the inherited `PATH`:

```jsonc
{
  "terminal.integrated.env.linux": {
    "PATH": "/home/dev/.ocx/toolchain/active/bin:${env:PATH}"
  }
}
```

The directory above stands in for the value the query printed. Use `terminal.integrated.env.osx` or `terminal.integrated.env.windows` on those platforms.

Run [`ocx pull`][cmd-pull] first. The query reports the directory even if nothing was rendered into it. A setting that points at an empty directory looks the same as a broken one. The [shell state report][explanation-shell-state] describes the other fields, and [Scripts and JSON][p-scripts-and-json] covers the format.

:::caution[Read the field, do not build the path]
`toolchain_bin` is not `toolchain_home` plus a fixed suffix. A hand-joined path succeeds, the editor accepts any string, and the failure surfaces later as `cmake: command not found` with no ocx process in the trace.
:::

A package can also publish editor configuration of its own. The [`integrations` field][reference-integrations] carries a vendor-keyed block such as `com.microsoft.vscode`, and `ocx env --format json` returns every block of the composition with its declaring package. A tool that wants those blocks reads the `integrations` array. ocx never interprets or merges the payloads, as [Integrations][explanation-integrations] explains.

## Use ocx in a dev container {#devcontainer}

ocx has no dev container feature. Install the binary in the image, pull at creation time and put the toolchain on the shell's `PATH`.

Start from the `Dockerfile`. [Copy the binary][docker-copy-from] from the official image, which works on any base:

```dockerfile
FROM mcr.microsoft.com/devcontainers/base:ubuntu
COPY --from=ghcr.io/ocx-sh/ocx:0.9.2-alpine /usr/local/bin/ocx /usr/local/bin/ocx
```

Then let `devcontainer.json` pull the lock and wire the shell. The extension id installs the editor integration of the previous section inside the container:

```json
{
  "build": { "dockerfile": "Dockerfile" },
  "postCreateCommand": "ocx pull && { grep -q 'ocx --offline env' ~/.bashrc || echo 'eval \"$(ocx --offline env --shell=bash)\"' >> ~/.bashrc; }",
  "customizations": { "vscode": { "extensions": ["ocx-sh.ocx"] } }
}
```

[`ocx pull`][cmd-pull] downloads every digest-pinned package once. `ocx --offline env --shell=bash` then prints export lines from the lock with no network access, so each new shell opens fast. The `grep` guard keeps a rebuild from appending the line twice. For a zsh login, pass `--shell=zsh` and append the line to `~/.zshrc` instead, since a zsh login never reads `~/.bashrc`. Bake the pull into the image with the [Docker recipe][p-docker-project-toolchain] when the container must start without a registry.

Private packages need credentials in the container. [Registry authentication recipes][p-istry-auth-recipes-auth-method] lists the variables. Put them in `remoteEnv` from the host's environment and never in the `Dockerfile`.

<!-- external -->
[devcontainer]: https://containers.dev/
[vscode-ocx]: https://github.com/ocx-sh/vscode-ocx

<!-- commands -->
[cmd-pull]: ../reference/command-line/commands.md#pull

<!-- reference -->
[reference-integrations]: ../reference/metadata/integrations.md#integrations

<!-- guides -->
[guide-groups]: ../explanation/project-toolchain.md
[docker-copy-from]: ./docker.md#copy-from
[explanation-shell-state]: ../explanation/shell-integration.mdx
[explanation-integrations]: ../explanation/integrations.md

<!-- pages -->
[p-scripts-and-json]: ./scripts-and-json.md
[p-docker-project-toolchain]: ./docker.md#project-toolchain
[p-istry-auth-recipes-auth-method]: ./registry-auth-recipes.md#auth-method
