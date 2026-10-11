---
title: "Toolchain Render"
description: "What ocx pull writes into the project toolchain home, which entries a render skips and why, and how pull --dry-run previews a pull."
---
<!-- doc_type: explanation -->

This page explains what [`ocx pull`][cmd-pull] renders into a project's toolchain home, why a render can skip an entry, and how `ocx pull --dry-run` previews a pull.

## Rendering the toolchain {#pull-render}

A warm object store is not yet a usable toolchain: something has to point at those packages under names a shell can resolve. `ocx pull` does both in one run. After the roots this invocation selected have resolved, it renders the toolchain home. So the tree that the [shell integration][in-depth-shell-integration] and [`ocx exec`][cmd-run] read is current when the command returns. [Toolchain Tree][in-depth-storage-toolchain] describes what each path in the tree is.

The render is a reconcile, not an append. The computed name set is written and every name no longer in it is pruned. A group that leaves the lock is pruned at `links/<group>`, not at the tree root. A depth-1 directory named after a locked group is therefore an orphan of the tree's own namespace, and the render prunes it as one.

`-g` narrows the render. A bare `ocx pull` covers every group the lock declares plus the default group unconditionally, and `-g ci,lint` covers exactly those. The trampoline directory belongs to the default group and to nothing else. A run that narrows the default group away leaves `shells/default/bin/` entirely untouched rather than emptied. Reconciling it would force the default group's metadata to resolve, which adds a network dependency nobody asked for.

The repair of a stale or absent link is not the pull's alone. Every composing emitter repairs the groups it is about to emit before it probes a link. `--dry-run` performs no repair, so a poisoned or stale link is still there afterwards. Every write step the repair needs is a step a dry run does not take.

A render failure never fails the pull. The command's product is a warmed object store, and that work is finished before the render starts. A read-only checkout, a foreign-owned `.ocx/`, a lock timeout, or a metadata closure an offline invocation cannot walk all leave the warming intact. Whatever the render could not do is a warning on stderr, and `ocx pull` still exits 0. Script against the warmed store, not against the tree.

The render stamp that records this run's entry set is written last, after the final entry and the final prune. It lives under `$OCX_HOME/state/`, per project at `state/projects/<key>/render_stamp.json` or directly at `state/render_stamp.json` for the global toolchain. It sits outside the tree so it survives a `toolchain_dir` relocation and a checkout wipe of `<home>/toolchain/` alike.

### When an entry is skipped {#pull-render-skipped}

Each thing the render could not write is one `warning:` line on stderr naming the artifact, the path and the reason. The reason is what you act on, because the exit code says nothing. Files a file manager drops into any folder you open (`.DS_Store` and `._*` on macOS, `desktop.ini` and `Thumbs.db` on Windows, `.directory` from KDE) are not skipped entries. The render leaves them in place and does not report them, except inside `shells/default/bin/`, which is on `PATH` and keeps only what ocx wrote.

The reason most people meet is a directory sitting where a link belongs. `cp -rL`, `rsync` without `-l`, Docker `COPY` and most zip extractors dereference symlinks. A project copied by one of them arrives with `links/<group>/<entry>` as a full directory copy of the package instead of a link:

```text
$ ocx pull
warning: Toolchain render skipped link "cmake" in group "default" at "/work/copy/.ocx/toolchain/links/default/cmake": "a directory occupies this link's name — the mark of a symlink-dereferencing copy (`cp -rL`, `unzip`, `rsync` without `-l`, Docker `COPY`); it is left in place rather than deleted recursively. Remove '/work/copy/.ocx/toolchain/links/default/cmake' and run `ocx pull` again"
```

The remedy is the message's last clause: delete that one path, then pull again. The directory is left byte-for-byte intact rather than removed recursively. Both of its name components come from `ocx.lock`, and this command never runs a recursive delete driven by a file's contents. Until it is deleted the entry is skipped on every render. It composes on its digest path, which is correct but not moved by a later [`ocx update`][cmd-update].

The `active` link needs no such action. A copy that turned it into a real directory is repaired silently on the next render. No name in that path comes from a lock or a config file.

## Previewing a pull {#pull-dry-run}

`ocx pull --dry-run` resolves each locked package through the local index, cache-first like the real pull, and reports whether it is already in the store. The store is never modified. Combine it with `--offline` to forbid the cache-miss network probe entirely.

```shell
$ ocx pull --dry-run
Package                                     Status
localhost:5000/cmake@sha256:1f4a9c2e7b03    cached
localhost:5000/ripgrep@sha256:8d2b60fa1c95  would_fetch
```

Plain output shortens each locked leaf to a 12-hex digest. The full pin rides out under `--format json` as one entry per locked package in an `items` array, each with a `status` and a `path`. The `path` is the package root, the parent of `content/` and `entrypoints/`, and it is present only on `cached` rows. The staleness gate fires ahead of the dry-run branch, so a stale lock still exits 65: the preview is not a way to bypass `declaration_hash` validation.

[cmd-pull]: ../reference/command-line/commands.md#pull
[cmd-update]: ../reference/command-line/commands.md#update
[cmd-run]: ../reference/command-line/commands.md#exec
[in-depth-storage-toolchain]: ./toolchain-tree.md#toolchain
[in-depth-shell-integration]: ./shell-integration.mdx
