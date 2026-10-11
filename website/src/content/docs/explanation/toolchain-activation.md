---
title: "Toolchain Activation"
---
<!-- doc_type: explanation -->

A toolchain reaches a shell in one of three ways, and its environment names package content in one of two ways. Two keys in `ocx.toml`, `activate` and `pinned`, choose among them, and each can also be set from an environment variable. This page explains what the keys decide and why the environment variable is the weakest place to decide it.

The two keys are read by both toolchain tiers: a project's own `ocx.toml` decides for that project, `$OCX_HOME/ocx.toml` for the global toolchain. The `activate` × `pinned` matrix and what each cell puts on `PATH` is in [Toolchain activation][env-composition-activation].

## Activate: how a toolchain reaches a shell {#activate}

A shell that recomposes a whole toolchain environment on every prompt is doing work you may not want it to do. Reasons include a large toolchain, a slow filesystem, or a preference for a `PATH` that does not change under you. `activate` decides how a rendered toolchain reaches a shell at all.

```toml
activate = "bin"
```

| Value | What reaches the shell |
|---|---|
| `"env"` *(default)* | The toolchain environment is composed on every prompt: each package's own `PATH` entries and declared variables land in the shell. |
| `"bin"` | Only `<home>/toolchain/active/bin` goes on `PATH`. Nothing else is composed; a binary is resolved by its [launcher trampoline][env-composition-activation] when it runs, and the trampoline composes the environment at that moment. |
| `"none"` | Neither. The reconciler withdraws whatever it owns and adds nothing. |

An unrecognized value in `ocx.toml` is a parse error, exit 78, for every command that loads the file; `lazy-mode` gets the same treatment. [`OCX_TOOLCHAIN_ACTIVATE`][env-ocx-toolchain-activate] is deliberately not symmetric. An unrecognized value there warns on stderr and falls through to the next tier, exit 0:

```text
Environment variable 'OCX_TOOLCHAIN_ACTIVATE' ignored: invalid activate mode 'shim' (expected 'env', 'bin' or 'none')
```

A file you own may fail loudly; an inherited variable may not break every prompt in every project.

### The hook fails open {#activate-fail-open}

The per-prompt hook is the one exception. A malformed `ocx.toml` must not break every prompt on the machine, so the hook reads the file leniently. The whole file's keys go absent, and `activate` falls through to `OCX_TOOLCHAIN_ACTIVATE` and then to `env`. That is the most-composing mode, not the safest-looking one.

The hook also discards ocx's stderr, so nothing warns you that a typo like `activate = "nnone"` left your restriction unapplied. [`ocx shell state`][cmd-shell-state] reports it: it reads the same file, resolves the same ladder, and prints a `note:` naming the manifest that would not parse. Run it whenever a mode you set does not seem to be in effect.

### No flag, by design {#activate-no-flag}

There is no `--activate` flag, because the choice belongs in the file that a whole team reads. [`ocx self setup --toolchain-activate MODE`][cmd-self-setup] writes the key into `$OCX_HOME/ocx.toml` rather than overriding it for one invocation, creating that file carrying only this key if it does not exist yet:

```sh
ocx self setup --toolchain-activate bin
```

That targets the ocx home's own `ocx.toml`, never the project in effect: `--project` and [`OCX_PROJECT`][env-ocx-project] name a different toolchain and this flag does not redirect onto it. A project's own `ocx.toml` still decides for that project.

### What the global toolchain does {#activate-global}

`activate` governs how a toolchain reaches a shell on its own, never what a command you typed prints. [`ocx --global env`][cmd-env-root] and [`ocx --global exec`][cmd-run] are explicit requests and compose the global tier in full, whatever the key says.

Two moments put a global environment into a shell, and both read this key. One is the login stream [`ocx self activate`][cmd-self-activate] emits at shell start. The other is the per-prompt reconciler that runs at every prompt after that. In `env` mode the login stream carries an `ocx --global env` eval.

Under `bin` and `none` it does not, and the trampoline directory it always prepends is what resolves the binaries instead. That matters most where no prompt ever runs: a script, an `ssh host cmd`, a git hook, a `sh` that registers no hook at all.

`$OCX_HOME/toolchain/active/bin` is a session-level directory. [`ocx self setup`][cmd-self-setup] registers it on `PATH` once, a shell start prepends it again, and a prompt never withdraws it. So a global `activate = "bin"` and a global `activate = "none"` both leave the global toolchain reachable through its trampolines and compose nothing else: the same `PATH`, by the same route. The two values part company only for a project's toolchain, whose `active/bin` directory a prompt does add and remove.

## Pinned: which paths the environment names {#pinned}

A rendered toolchain carries `links/<group>/<entry>` links pointing at package roots, and a composed environment can name either those links or the digest paths directly. `pinned` chooses.

```toml
pinned = true
```

| Value | What the composed environment names |
|---|---|
| `false` *(default)* | The rendered `links/<group>/<entry>` links are followed, so a later [`ocx update`][cmd-update] moves a root's paths with no re-render. |
| `true` | Digest paths, exactly what `ocx.lock` pins right now, consulting no link and reading no tree. |

`true` is the setting for an environment that must not shift underneath a long-running process or a captured export. The paths name content, and content-addressed paths never change meaning.

The key names a lane, and two things qualify what lands in it. A link that is absent, stale, or not a link degrades that one entry to its digest path, silently. Its siblings still compose through their links. The digest path is the correct path, the same package directory under its other spelling.

Only roots have links: a dependency's `PATH` contributions and every `${deps.<name>.installPath}` are digest paths under `false` as well as `true`.

`pinned` reaches five emitters, not the two that carry the flag: [`ocx env`][cmd-env-root], [`ocx exec`][cmd-run], [`ocx direnv export`][cmd-direnv-export], the `env`-mode shell hook, and the `env`-mode login exporter. For the three that declare no flag, this key and [`OCX_TOOLCHAIN_PINNED`][env-ocx-toolchain-pinned] are the only tiers that answer. The last two compose only when the global tier's [`activate`](#activate) is `env`; under `bin` or `none` neither runs, so no lane is chosen for them at all.

## The `bin` render gate {#bin-gate}

The `<home>/toolchain/active/bin` entry of a `bin` toolchain is added only when the tree's render stamp matches what is on disk for this project and this lock. The stamp fails to match when the tree was never rendered or the lock has moved since. Then nothing is added and the prompt prints one line instead:

```text
ocx: /home/dana/work/api: its toolchain has not been rendered for this lock; run `ocx pull` here
```

Run `ocx pull` there to render the tree for the current lock.

## Order on PATH {#path-order}

The entries land front-to-back as: the project's composed entries, the project's `<home>/toolchain/active/bin`, `$OCX_HOME/toolchain/active/bin`, OCX's own install directory, then the global tier's composed entries. A globally installed package therefore never shadows the project's own, which is the [strict isolation][env-strict-isolation] boundary enforced by ordering. `ocx` is no exception: the install directory is last, so a toolchain that pins `ocx` is the one that answers for the name.

The prompt's own `PATH` takes the `pinned` lane too. In `env` mode the per-prompt reconciler is itself a composing emitter. It composes the project's default group and, with `pinned` false, follows that group's `links/<group>/<entry>` links. So the project entries on your `PATH` are link paths. It carries no flag of its own, which is why `ocx.toml` and [`OCX_TOOLCHAIN_PINNED`][env-ocx-toolchain-pinned] are the only tiers that answer for it.

Turning `pinned` on never deletes a tree. The link pass is suppressed and not reconciled to empty, so switching the key on and back off takes effect with no re-render.

## A trampoline composes at call time {#trampolines}

The surprising part of `bin` mode is that a trampoline in `<home>/toolchain/active/bin/` carries no frozen environment. It re-enters `ocx exec` against its own home. The environment is composed at the moment the binary runs, from `ocx.toml` and `ocx.lock`. It includes the project [`[env]`][env-composition-project-env] and sibling packages, not just the one package the binary came from.

:::info[The same two positions mise and rustup take]
[mise][mise-shims] ships both shapes and names them the same way round. `mise activate` recomposes the environment on every prompt, and `mise activate --shims` puts one shim directory on `PATH` instead. By contrast, [rustup][rustup-proxies] only ever ships the second, where `~/.cargo/bin/cargo` is a proxy that forwards to whichever toolchain is active. `env` and `bin` are those two positions, chosen per toolchain rather than once per install.
:::

The whole body is five lines, and what it bakes is the entire story:

```sh
#!/bin/sh
# ocx-toolchain-trampoline
unset OCX_GLOBAL OCX_PROJECT
__ocx_binary='/home/dana/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx'
exec "${OCX_BINARY_PIN:-$__ocx_binary}" --project '/home/dana/work/api' exec -- "${0##*/}" "$@"
```

Two values are baked and nothing else: the home selector (`--project <root>`, or a bare `--global` for the global tier) and the absolute path to `ocx`. No tool path, no digest, no environment. On Windows the same selector rides a one-line `<name>.exec` sidecar beside a native `<name>.exe`, carrying either the absolute project root or the literal `global`.

Two consequences follow directly. A config edit takes effect with no re-render: change `[env]`, add a binding, flip [`pinned`][config-pinned], and the next invocation composes against the new file. The trampoline body is byte-identical either way. And there is no baked value that can drift. The lock decides at call time which package a name resolves to, not what the renderer saw.

The absolute `ocx` path is baked rather than left as a bare `ocx` for a specific reason. In `bin` mode the interactive shell has `toolchain/active/bin` prepended, and a project may pin its own `ocx`. A bare-name fallback would make `/bin/sh` re-resolve the trampoline as itself.

That is an infinite loop before any ocx process starts, and no in-process guard can see it. The [asdf project hit exactly this][asdf-shim-loop]: a shim of `asdf` itself on `PATH`, re-resolving to the shim, hundreds of frames deep and hanging every command. [`OCX_BINARY_PIN`][env-ocx-binary-pin] still overrides the baked path.

## Name collisions {#collisions}

Two packages in one closure can claim the same binary name. OCX resolves it and moves on. The last one walked wins, matching composed-`PATH` order. So the trampoline name and the name that wins a `PATH` lookup are decided by one rule and not two.

There is no warning and no refusal. The collision is logged at debug level, and the losing claim is recorded against the winning name so it can be reported rather than lost. A refusal would be wrong on its face. A project may legitimately pin its own `ocx` or its own `python`.

Shadowing that name for itself is the declared intent, not an accident. A package that claims one name on both its `binaries` and its `entrypoints` axes is making one claim, not two, so it can never shadow itself.

## Resolution order {#ladders}

Each key resolves down a ladder, strongest tier first:

| Key | Ladder |
|---|---|
| `activate` | `ocx.toml` ▸ [`OCX_TOOLCHAIN_ACTIVATE`][env-ocx-toolchain-activate] ▸ `env` |
| `pinned` | [`--pinned` / `--no-pinned`][arg-pinned] ▸ `ocx.toml` ▸ [`OCX_TOOLCHAIN_PINNED`][env-ocx-toolchain-pinned] ▸ follow the links |

The environment tier is the weakest, not an override. An exported `OCX_TOOLCHAIN_ACTIVATE=none` loses to a file that states `activate = "env"`; the variable decides only where no file states the key. This is the opposite of the usual "environment beats config" reflex, and it is what makes the key mean something a collaborator can rely on. The same rule holds for `pinned`, for the same reason.

Both variables parse leniently, on purpose. `OCX_TOOLCHAIN_ACTIVATE` is case-insensitive (`Bin`, `BIN` and `bin` are equivalent) but does not trim whitespace, so `" bin"` is an unrecognized value, not `bin`. `OCX_TOOLCHAIN_PINNED` takes the truthy and falsy spellings.

For both, an empty value reads as unset, and an unparseable one warns and falls through to the next tier with exit 0. The `ocx.toml` keys are stricter: lowercase only, and an unrecognized value is a parse error. A typo in a file you committed should stop the build. A typo in a variable a CI image exported should not take every project on that runner down with it.

"Unset" and "explicitly false" are different answers for `pinned`. `OCX_TOOLCHAIN_PINNED=false` *states* the link lane and beats the ladder's floor, while an unset variable states nothing and lets a lower tier answer.

`OCX_TOOLCHAIN_UPDATE` is not on these ladders: it overrides `[update] toolchain` in `config.toml` and, like `OCX_SELF_UPDATE`, beats the file. It sets what the background drift check does, as described under [Update OCX][ug-update-ocx]. `OCX_TOOLCHAIN_ACTIVATE`, `OCX_TOOLCHAIN_PINNED` and `OCX_TOOLCHAIN_UPDATE` are not forwarded to child `ocx` processes, because they change how a toolchain reaches `PATH` and never which digest resolves.

`--no-pinned` exists precisely so a project that declares `pinned = true` can still be composed through the links for one invocation; see [`--pinned`][arg-pinned] for the flag pair's own semantics.

The ladder is resolved per tier, over that tier's own file: a global `activate = "bin"` never reaches a project, and a project's never reaches the global toolchain.

```toml
# <project>/ocx.toml
pinned = true

[tools]
cmake      = "ocx.sh/kitware/cmake:3.28"
shellcheck = "ocx.sh/shellcheck:0.11"
```

Both keys are resolve-time policy, not bindings, so both are excluded from `declaration_hash`: editing either does not invalidate `ocx.lock`.

<!-- external -->
[mise-shims]: https://mise.jdx.dev/dev-tools/shims.html
[rustup-proxies]: https://rust-lang.github.io/rustup/concepts/proxies.html
[asdf-shim-loop]: https://github.com/asdf-vm/asdf/issues/2166

<!-- commands -->
[cmd-update]: ../reference/command-line/commands.md#update
[cmd-run]: ../reference/command-line/commands.md#exec
[cmd-env-root]: ../reference/command-line/commands.md#env-root
[cmd-direnv-export]: ../reference/command-line/direnv.md#direnv-export
[cmd-self-setup]: ../reference/command-line/self.md#self-setup
[cmd-self-activate]: ../reference/command-line/self.md#self-activate
[cmd-shell-state]: ../reference/command-line/shell.md#shell-state
[arg-pinned]: ../reference/project-file.md#project-config-pinned

<!-- environment -->
[env-ocx-toolchain-activate]: ../reference/environment.md#ocx-toolchain-activate
[env-ocx-toolchain-pinned]: ../reference/environment.md#ocx-toolchain-pinned
[env-ocx-project]: ../reference/environment.md#ocx-project
[env-ocx-binary-pin]: ../reference/environment.md#ocx-binary-pin

<!-- internal -->
[ug-update-ocx]: ../guides/update-ocx.mdx#update-ocx
[env-strict-isolation]: environments.mdx#strict-isolation
[env-composition-project-env]: ../reference/env-composition.md#project-env
[config-pinned]: toolchain-activation.md#pinned
[env-composition-activation]: ../reference/env-composition.md#toolchain-activation
