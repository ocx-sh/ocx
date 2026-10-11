# Friction log: T01 install and use a first package

## Context

- Persona: A backend developer who has never used ocx or any OCI-based binary package manager, on a Linux workstation, following only the published documentation pages.
- This log was written by a subagent restricted to the published docs snapshot (Markdown copies of the docs pages). It read no source code and no other directory, and it used no prior knowledge of ocx internals.
- Task T01: install a first tool package (cmake) with ocx, run it, switch to a second version, and uninstall it.
- Pages read, in order: `installation.md`, `getting-started.md`, the first 230 lines of `user-guide.md`, and the `package install`, `uninstall`, `select`, `deselect`, `which` and `--remote` entries of `reference/command-line.md`.
- Environment: ocx 0.6.5 was already installed. `OCX_HOME` pointed at a scratch directory and `OCX_NO_UPDATE_CHECK=true` was set. Network access worked. Linux x86_64, glibc, zsh.
- Steps that blocked me: 1 (the cmake 3.28 install). Steps that slowed me without blocking: 3 (the `--remote` flag position, the missing command blocks in the snapshot, and finding a way to call `cmake` by name).

## Pros and cons

Pros

- `install --select`, `select`, `uninstall`, `uninstall --deselect --purge`, `which --current` and `clean` behaved as the command reference describes. Each exited 0.
- The Getting Started comparison to `apt install` gave me a working mental model of install versus one-shot exec.
- The warning "Uninstall removes the symlink, not necessarily the binary" matched what I saw: the object stayed on disk after a plain uninstall, and `ocx clean` removed it.
- The User Guide section on stable paths told me where the selected version's binary lives (`current/content/bin/...`), which was the only way I found to call it directly.
- `uninstall --purge` and `clean` print each removed entry, so I could see what was deleted.

Cons

- The docs' running example, `kitware/cmake:3.28`, does not exist in the index I reached. Following the docs literally ended in exit 79 on my first install.
- Getting Started says to pass `--remote` on a fresh install. For the failing install that changed nothing, and the error text never mentioned it.
- `--remote` is accepted before the subcommand and rejected after it (exit 64). No page I read says where global flags go.
- The command blocks and terminal casts on Getting Started and the User Guide are not in the snapshot, so the exact commands were visible only through prose and the command reference.
- No page I read says how to call `cmake` by bare name after `install --select`. Getting Started points to shell integration and the global toolchain, which is a different mechanism. I used `package exec` and the long `current/content/bin/cmake` path.
- Package names appear with and without the registry prefix (`kitware/cmake:3.28` in prose, `ocx.sh/kitware/cmake:3.28` in the `ocx.toml` examples). No sentence I read says the default registry is `ocx.sh`; I learned it from `ocx about`.

## Detailed stream of consciousness

I start with Installation because the sidebar order says to. It is about installing ocx itself, which I was told is already done, so I take from it only that `ocx about` verifies an install.

Next is Getting Started. A tip says my local index is empty on a fresh install and I should pass `--remote`. The sections after it are Quick Start (`package exec`), Installing (`package install`, `package which --candidate`), Switching Versions (`install --select`, `select`, `deselect`), and Uninstalling (`uninstall`, `--purge`, `--deselect`, `clean`). Where a command block should be, my snapshot shows only an include line, and the terminal casts are also absent. I piece the commands together from prose and from the command reference, and I read the User Guide's install and "Choose between versions" sections. Every example there uses `kitware/cmake:3.28`, so I try that.

```
$ ocx about
Version:   0.6.5
Commit:    26ad03a1 (clean)
Registry:  ocx.sh
Platforms: linux/amd64
Libc:      libc.glibc
Shell:     Zsh
Home:      /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01
exit=0
```

`Registry: ocx.sh` is how I learn the default registry.

```
$ ocx package install kitware/cmake:3.28
Installing packages: ocx.sh/kitware/cmake:3.28
pulling count=1
error: failed to install package: ocx.sh/kitware/cmake:3.28 — chained index source walk failed: 'ocx.sh/kitware/cmake:3.28' is not in the index at https://index.ocx.sh, which is authoritative for every name in registry 'ocx.sh'; announce it there with `ocx package announce`, or take the namespace off the index with `[registries."ocx.sh"] index = ""`
exit=79
```

This blocks me. I do not know what "chained index source walk" or "authoritative" mean, and the remedies named are about publishing, not installing. I cannot tell whether the package is missing, the version is missing, or the network is down. The docs tip said a fresh install needs `--remote`, so I try it.

```
$ ocx --remote package install kitware/cmake:3.28
Installing packages: ocx.sh/kitware/cmake:3.28
pulling count=1
error: failed to install package: ocx.sh/kitware/cmake:3.28 — chained index source walk failed: 'ocx.sh/kitware/cmake:3.28' is not in the index at https://index.ocx.sh, which is authoritative for every name in registry 'ocx.sh'; announce it there with `ocx package announce`, or take the namespace off the index with `[registries."ocx.sh"] index = ""`
exit=79
```

Same error. The tip did not help here. Next I try to see what versions exist, using the browse commands named at the end of Getting Started. I put `--remote` after the subcommand first, which is how I would write it for most CLIs.

```
$ ocx index list kitware/cmake --remote
error: unexpected argument '--remote' found

  tip: to pass '--remote' as a value, use '-- --remote'

Usage: ocx index list <PACKAGE>...

For more information, try '--help'.
exit=64
```

```
$ ocx index catalog --remote
error: unexpected argument '--remote' found

  tip: to pass '--remote' as a value, use '-- --remote'

Usage: ocx index catalog [OPTIONS] [REGISTRY]...

For more information, try '--help'.
exit=64
```

The command reference lists `--remote` under global flags, so I try it before the subcommand.

```
$ ocx --remote index list kitware/cmake
Package        Tag             
kitware/cmake  3               
kitware/cmake  3.31            
kitware/cmake  3.31.0          
kitware/cmake  3.31.0_20260731 
  (output trimmed here: the real listing continued with every 3.31.x, 4.0.x to 4.4.x tag, each also with a _<date> build suffix, and ended with "latest". There was no 3.28 line.)
exit=0
```

So the network works and the docs' example version is simply not published. The listing is long and unsorted by version (3.31.10 appears before 3.31.1_20260731), and nothing in it says which tags I should pick. I choose `3.31` and `4.0` because they are short rolling tags, which the User Guide's tag table describes as "Rolling minor".

```
$ ocx --remote index catalog
Repository                                   
ocx.sh/actionlint/actionlint                 
ocx.sh/adoptium/temurin                      
ocx.sh/age/age                               
  (trimmed to the first lines)
```

```
$ ocx package install --select kitware/cmake:3.31
Installing packages: ocx.sh/kitware/cmake:3.31
pulling count=1
Downloading layer sha256:5cad2c7093da to /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/temp/9c0c718e10d259ebd0b82f1b902828ce
Package             Version                    Path                                                                                                                
kitware/cmake:3.31  ocx.sh/kitware/cmake:3.31  /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/symlinks/ocx.sh/kitware/cmake/current
exit=0
```

It worked. The output tells me the path is a `current` symlink, but not that I have to call the binary through it. I try the result.

```
$ ocx package which --current kitware/cmake
Package        Kind     Path                                                                                                                
kitware/cmake  package  /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/symlinks/ocx.sh/kitware/cmake/current
exit=0
$ cmake --version
(eval):1: command not found: cmake
exit=127
```

I expected `cmake` to be on my `PATH` after an install that says "select". It is not, and the install output does not say it would not be. Getting Started's tip about `$OCX_HOME/env.sh` is about the global toolchain and the profile that the real installer writes, and I used a scratch `OCX_HOME`, so I cannot tell from here whether that route would have made `cmake` appear. I fall back to the commands the docs name for running a package.

```
$ ocx package exec kitware/cmake:3.31 -- cmake --version
cmake version 3.31.12

CMake suite maintained and supported by Kitware (kitware.com/cmake).
exit=0
$ $OCX_HOME/symlinks/ocx.sh/kitware/cmake/current/content/bin/cmake --version
cmake version 3.31.12

CMake suite maintained and supported by Kitware (kitware.com/cmake).
exit=0
```

Both work. Now the second version, then the switch.

```
$ ocx package install kitware/cmake:4.0
Installing packages: ocx.sh/kitware/cmake:4.0
pulling count=1
Downloading layer sha256:e5173373d10f to /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/temp/dfbe6aa196b25e6a5b0e728adbb1b8fd
Package            Version                   Path                                                                                                                       
kitware/cmake:4.0  ocx.sh/kitware/cmake:4.0  /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/symlinks/ocx.sh/kitware/cmake/candidates/4.0
exit=0
$ ocx package select kitware/cmake:4.0
Package            Version                   Path                                                                                                                
kitware/cmake:4.0  ocx.sh/kitware/cmake:4.0  /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/symlinks/ocx.sh/kitware/cmake/current
exit=0
$ current/content/bin/cmake --version
cmake version 4.0.7
$ ocx package exec kitware/cmake:3.31 -- cmake --version
cmake version 3.31.12
$ ls -l symlinks/ocx.sh/kitware/cmake symlinks/ocx.sh/kitware/cmake/candidates
current -> .../packages/ocx.sh/sha256/f4/bd2e236ba93f946887774f408c5aad
3.31 -> .../packages/ocx.sh/sha256/85/65e8b57e3f4cb1b8b6e9ce586072c4
4.0 -> .../packages/ocx.sh/sha256/f4/bd2e236ba93f946887774f408c5aad
```

(The `...` stands for the scratch `OCX_HOME` prefix, shortened by me. The real output printed the full path.)

The switch works: `current` now points at the 4.0 object, `current/content/bin/cmake` reports 4.0.7, and `package exec` with an explicit tag still gives 3.31.12. That matches the Getting Started description of `current` as a floating pointer. The version I get from a tag (3.31 gives 3.31.12, 4.0 gives 4.0.7) is only visible after running the binary.

Last, uninstall. Getting Started says plain `uninstall` keeps the binary and `--purge` removes it.

```
$ ocx package uninstall kitware/cmake:3.31
Package             Status 
kitware/cmake:3.31  removed
exit=0
$ ocx package uninstall --deselect --purge kitware/cmake:4.0
Removing unreferenced entry: .../blobs/ocx.sh/sha256/93/bfa84881c61489abeee42aefa2aede
Removing unreferenced entry: .../blobs/ocx.sh/sha256/96/6db4aff0c5fdb85e29cc0e7d3f6666
Removing unreferenced entry: .../blobs/ocx.sh/sha256/f4/bd2e236ba93f946887774f408c5aad
Removing unreferenced entry: .../layers/ocx.sh/sha256/e5/173373d10feab7141e6c34e2ee1ce5
Removing unreferenced entry: .../packages/ocx.sh/sha256/f4/bd2e236ba93f946887774f408c5aad
Package            Status 
kitware/cmake:4.0  removed
kitware/cmake:4.0  purged 
exit=0
$ ls -lR symlinks/ocx.sh/kitware/cmake
candidates/
.select.lock  0B
$ ocx clean
Removing unreferenced entry: .../blobs/ocx.sh/sha256/22/c7e8cc5b673d57c5d475bf9107eced
Removing unreferenced entry: .../blobs/ocx.sh/sha256/85/65e8b57e3f4cb1b8b6e9ce586072c4
Removing unreferenced entry: .../layers/ocx.sh/sha256/5c/ad2c7093da3548bd117f18468ebeed
Removing unreferenced entry: .../packages/ocx.sh/sha256/85/65e8b57e3f4cb1b8b6e9ce586072c4
Type    Path
object  .../blobs/ocx.sh/sha256/22/c7e8cc5b673d57c5d475bf9107eced
object  .../blobs/ocx.sh/sha256/85/65e8b57e3f4cb1b8b6e9ce586072c4
object  .../layers/ocx.sh/sha256/5c/ad2c7093da3548bd117f18468ebeed
object  .../packages/ocx.sh/sha256/85/65e8b57e3f4cb1b8b6e9ce586072c4
exit=0
$ du -sh $OCX_HOME/packages
20K	/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t01/packages
```

After the second uninstall the candidate and `current` links are gone and the 4.0 object is purged. The 3.31 object stayed until `ocx clean` removed it, as the warning in Getting Started says. The output of `uninstall` for 3.31 says "removed" with no hint that the data is still on disk. I only learned that from the docs, which I had read before.

Two things I did not do and cannot judge: I did not try `ocx package pull`, and I did not set up shell integration with `ocx self setup`, so I do not know whether a bare `cmake` would resolve on a machine where the installer had run.
