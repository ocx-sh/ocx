---
title: "Toolchain Tree"
description: "The rendered toolchain directory: how declared tool names resolve to installed packages through links and launcher trampolines, and why it sits outside garbage collection."
---
<!-- doc_type: explanation -->

This page explains the rendered toolchain tree. It is the directory that maps the tool names a project declares onto packages in the [package store][fs-packages]. The [Storage][storage] page describes the stores around it.

## Layout {#toolchain}

The stores on the Storage page are keyed by digest, registry, or tag. None of them is keyed by the name you actually type. A toolchain is the other direction. It is a set of bindings a project declares. Each binding is a plain name that has to resolve to one package on this host.

[`ocx pull`][cmd-pull] renders that set into a directory. Every path in it is a name you chose, and every leaf points back into the [package store][fs-packages]. Nothing is copied.

The tree lives at `$OCX_HOME/toolchain/` for the global tier and at `<project>/.ocx/toolchain/` for a project.

```text
toolchain/
├── .gitignore            a single * line, kept present by the renderer
├── active                link to shells/default/, so the PATH entry is always active/bin
├── links/
│   └── <group>/          one directory per selected group, default is default/
│       └── <entry>       directory link to a package root (an NTFS junction on Windows)
└── shells/
    └── default/bin/      one launcher trampoline per exposed binary name, default group only
        ├── cmake         POSIX: a five-line /bin/sh trampoline that re-enters ocx
        ├── cmake.exe     Windows: a native shim, paired with the sidecar below
        └── cmake.exec    Windows: one line with the home selector, the project root or "global"
```

Depth 1 is a closed set that the tree owns: `.gitignore`, `active`, `links/` and `shells/`. Every name *you* declare is a component of `links/<group>/<entry>`, one level below all four. No group or binding name you pick can collide with the tree's own structure.

`active` is why the two halves are separate. It is the name a shell, an editor and a CI step hold. It points at the physical directory the renderer actually wrote, so the `PATH` entry stays `<home>/toolchain/active/bin` while what sits underneath is free to move. Ask [`ocx shell state`][cmd-shell-state] for the resolved value rather than joining it yourself. The JSON report's `toolchain_bin` field is that path.

`shells/default/bin/` holds the trampolines that make [`activate = "bin"`][config-activate] work. There is one `PATH` entry, spelled `active/bin`, and each name is resolved by its own launcher at invocation time. The directory covers the default group only, because that is the group `PATH` exposes. On Windows each name is **two** files, an `.exe` and its `.exec` sidecar, and both are tracked independently.

## Links {#toolchain-links}

`links/<group>/<entry>` is the link lane. Each entry is a directory link to a package root, the same target shape that [`candidates/{tag}`][storage-symlinks] uses. A composed environment can therefore name `…/links/default/cmake/content/bin` instead of a digest path. Whether an environment *uses* those links is the [`pinned`][config-pinned] setting. See [Toolchain activation][env-composition-activation].

The lane holds one entry at a time and only for lock entries. A link that is absent, stale, or not a link leaves **that** entry on its digest path. The rest of the composition still follows its links, and nothing is printed. The digest path is the correct path, the same package directory under its other spelling.

A dependency has no link at all. Its `PATH` contributions and every `${deps.<name>.installPath}` name digest paths whatever `pinned` says. Both cases are set out under [`--pinned`][arg-pinned-degrade].

Every composing emitter repairs this lane before it reads it. It creates an absent link and repoints a stale one for the groups it is about to emit, then probes. The repair is best-effort. A read-only tree, a home the symlink guards refuse, or a link lock it cannot take leaves the entry as it found it. That entry then composes on its digest path.

One state the emitter cannot repair is a **directory** sitting where a link belongs. That is the mark of a copy that dereferenced symlinks. [`ocx pull`][cmd-pull] names it and the path to delete. See [what a skipped entry means][cmd-pull-render-skipped].

A toolchain link takes **no** `refs/symlinks/` back-reference. That is the deliberate difference from an install symlink. A back-reference is a [GC root][storage-gc]. A toolchain link that took one would pin its package forever, on every project that ever rendered a tree.

## Where a project's tree lives {#toolchain-location}

By default a project renders into its own checkout, at `<project>/.ocx/toolchain/`. The renderer keeps a `.gitignore` containing `*` in every home it writes, so the default lands in a directory git already ignores.

[`toolchain_dir`][config-toolchain_dir] moves them all out. With it set, a project's tree lands at `<root>/<project-key>/toolchain/`. The `<project-key>` is the same 16-hex key that the `projects/` GC ledger and `state/projects/<key>/` derive from the canonical project directory. The **global** home ignores the key entirely and stays at `$OCX_HOME/toolchain`.

## The render stamp {#toolchain-stamp}

A rendered tree is only usable if it matches the lock it was rendered from. Walking it on every shell prompt to find out is not an option. So each render writes a stamp describing the tree **as it stands on disk**. The stamp lists the trampoline set with each file's identity, and the default group's links. The prompt path compares against that instead.

| Tier | Stamp |
|---|---|
| Global | `$OCX_HOME/state/render_stamp.json` |
| Project | `$OCX_HOME/state/projects/<key>/render_stamp.json` |

The global stamp sits directly under `state/`, not beside the project ones. The directory `state/projects/<key>/` is swept as a unit, and a global stamp filed there would be deleted on every [`ocx clean`][cmd-clean]. An absent, unreadable, or unrecognized stamp reads as *absent*, never as an error. The answer to an absent stamp is "re-render", which is always safe.

## Outside garbage collection {#toolchain-gc}

The rendered tree is **pure derived state, outside the three GC tiers**. [`ocx clean`][cmd-clean] never walks it, it takes no reference-counting back-refs, and nothing in it is a GC root.

Deleting it is safe. The next [`ocx pull`][cmd-pull] rebuilds it from `ocx.toml` and `ocx.lock`. Until then a `bin`-mode prompt withholds the `PATH` entry and prints one line telling you to run exactly that. A superseded tree is litter, accepted by design. The [index][storage-index] takes the same posture.

## See Also

- [Storage][storage]: the stores, packages, layers, index and install symlinks
- [Toolchain activation][env-composition-activation]: the `activate` × `pinned` matrix and what a trampoline composes
- [Project toolchain](./project-toolchain.md): the project file the tree is rendered from
- [`ocx pull`][cmd-pull]: reference for the command that renders the tree

[fs-packages]: ./storage.mdx#packages
[storage]: ./storage.mdx
[storage-symlinks]: ./storage.mdx#symlinks
[storage-gc]: ./storage.mdx#gc
[storage-index]: ./storage.mdx#index
[cmd-pull]: ../reference/command-line/commands.md#pull
[cmd-clean]: ../reference/command-line/commands.md#clean
[cmd-shell-state]: ../reference/command-line/shell.md#shell-state
[cmd-pull-render-skipped]: ./toolchain-render.md#pull-render-skipped
[config-toolchain_dir]: ../reference/configuration.md#keys-toolchain_dir
[config-activate]: toolchain-activation.md#activate
[config-pinned]: toolchain-activation.md#pinned
[env-composition-activation]: ../reference/env-composition.md#toolchain-activation
[arg-pinned-degrade]: toolchain-activation.md
