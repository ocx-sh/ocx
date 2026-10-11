# Friction log: T02 pin and run a project toolchain

## Context

Written by a subagent restricted to the published docs snapshot (Markdown only; no other source on the machine). Persona: a platform engineer who has never used ocx before and wants every teammate and CI job to get the same pinned tool versions for one repository. Goal T02: pin the tools one project needs, lock them, and run a command with exactly those tools without installing them globally. Binary: ocx 0.6.5, fresh `OCX_HOME`, empty scratch git repo. Start page: `getting-started.md`, then links as a reader would follow them. Where the docs were silent, that is recorded as a silence.

## Pros and cons

Pros
- The Project Toolchain section of `getting-started.md` names the three commands (`ocx init`, `ocx add`, `ocx exec`) and states what each one does in one sentence.
- All three commands worked on the first try, against the live registry, with exit 0.
- The `exec` reference states that `--` is mandatory and that an unresolvable command exits 65. Both matched what I saw.
- The stale-lock behaviour (edit `ocx.toml`, run `ocx exec`, exit 65, "run `ocx lock`") matches the user guide.

Cons
- Every example script the Getting Started section points to (`<<< @/_scripts/getting-started/project-toolchain.sh`) is absent from the snapshot. The section says "Three commands set up the toolchain:" and then shows nothing. The reader gets no example package name or tag from that page.
- The `ocx.toml` example package I used had to be taken from a different page (`user-guide.md`, "Pin a project's packages").
- No page I read says how to confirm the child really used the locked tools rather than something already on `PATH`. I found out by accident (see step 5).
- Exit 65 is documented for `exec` as covering four unrelated situations (stale lock, conflicting separators, tampered bundle, command not resolving). The `--clean` failure below used the same code for a different cause.
- The `ocx.toml` gets a `.ocx/` directory inside the checkout; the pages say it is gitignored, but I found no ignore file in the checkout (step 6).
- Getting Started says `ocx exec` reads `ocx.lock`, but never says what happens when there is no network. I had to try `--offline` myself, and the flag position was not obvious from the `exec` page.
- `ocx --version` is rejected (exit 64). Nothing I read before trying it said how to print the version.

## Detailed stream of consciousness

Step 1. I opened `getting-started.md`. The page is about packages first: Quick Start uses `ocx package exec`, then install, select, uninstall, environment. I wanted the project flow, so I scrolled past a lot. "Project Toolchain" is the second to last section. The page order made me read about candidate symlinks and `current` pointers, which T02 does not need.

Step 2. The Project Toolchain section says "Three commands set up the toolchain:" followed by a code include. In my snapshot the include is a literal `<<< @/_scripts/getting-started/project-toolchain.sh{sh}` line and the `_scripts` directory holds only a `.gitignore` that says `*.sh` is generated. So the section shows no commands at all. I inferred them from the next paragraph: `ocx init`, `ocx add`, `ocx exec`. I did not know which package to add, so I looked in the User Guide, "Pin a project's packages", which uses `ocx.sh/shellcheck/shellcheck:0.11`. The same missing-file problem applies to `project-lifecycle.sh`, `fresh-clone.sh` and `run-basic.sh` in the User Guide.

Step 3. First command I tried was a version check, which I did on instinct before setting up:

```
$ ocx --version
error: unexpected argument '--version' found

Usage: ocx [OPTIONS] [COMMAND]

For more information, try '--help'.
[exit 64]
```

The docs I read never mention it either way. I moved on.

Step 4. Init and add.

```
$ ocx init
created /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t02/ocx.toml
[exit 0]
$ cat ocx.toml
#:schema https://ocx.sh/schemas/project/v1.json
# OCX project toolchain — managed by `ocx add` / `ocx remove`

[tools]
[exit 0]
$ ocx add ocx.sh/shellcheck/shellcheck:0.11
pulling count=1
Downloading layer sha256:794849587f27 to /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t02/temp/e65c6e61c6a61998d63a31ef4f597d59
Binding     Group    Digest
shellcheck  default  sha256:3981b442fff3f6bf9ae93e4c293e53322ffc9135e0b654620bc800464bdaf379
[exit 0]
```

(The header row in the real output has trailing padding; I trimmed trailing spaces only.) Two things I noticed. The `add` page says it "installs the package in one step", and the output does show a download, so that matches. But Getting Started describes `ocx add` as appending a binding, resolving, and writing `ocx.lock`; it does not mention the download there. The digest shown is for one platform only. Nothing in the output says which platform, and I had to read `ocx.lock` to learn that the digest is the `linux/amd64` leaf.

Step 5. Run the tool. First I wanted to check where the binary came from.

```
$ ocx exec -- shellcheck --version
ShellCheck - shell script analysis tool
version: 0.11.0
license: GNU General Public License, version 3
website: https://www.shellcheck.net
[exit 0]
```

It works. But earlier, `which shellcheck` in my plain shell gave a path under a different checkout:

```
$ which shellcheck
/home/mherwig/dev/ocx/.ocx/toolchain/links/default/shellcheck/content/shellcheck
[exit 0]
```

That is the shell hook activating another ocx project's toolchain, not mine. So "version 0.11.0" alone could not prove which toolchain served the run. The docs do not tell a newcomer how to tell the two apart. I checked inside the child:

```
$ ocx exec -- sh -c 'command -v shellcheck'
/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t02/.ocx/toolchain/links/default/shellcheck/content/shellcheck
[exit 0]
```

Good, that is my project's tree. The echoed command above lost its inner quotes only because my log helper printed `$*`; I typed it quoted. Next I tried `--clean` hoping for an isolated environment:

```
$ ocx exec --clean -- sh -c 'command -v shellcheck'
error: "sh" does not resolve in the composed environment; searched: ["/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t02/.ocx/toolchain/links/default/shellcheck/content"]
[exit 65]
```

The `exec` reference explains this in one sentence ("searches only the composed environment's PATH"), but I only understood it after it failed. The word "clean" in the flag description says only the composed variables remain; it does not say that `sh` itself disappears. The error message does list what was searched, which helped.

I also tried naming the binding, as the reference documents:

```
$ ocx exec shellcheck -- shellcheck --version
ShellCheck - shell script analysis tool
version: 0.11.0
license: GNU General Public License, version 3
website: https://www.shellcheck.net
[exit 0]
```

Step 6. Looking at what was written in the checkout.

```
$ ls -a
.
..
.git
.ocx
ocx.lock
ocx.toml
[exit 0]
$ ls -a .ocx .ocx/toolchain
.ocx:
.
..
toolchain

.ocx/toolchain:
.
..
active
.gitignore
links
shells
[exit 0]
$ cat .gitignore
cat: .gitignore: No such file or directory
[exit 1]
$ git status --short
?? ocx.lock
?? ocx.toml
[exit 0]
```

So `.ocx/toolchain/.gitignore` exists inside the rendered tree and `git status` stays clean. I never ran `ocx pull`, yet `.ocx/toolchain` was already rendered by `add`/`exec`. The Getting Started page says the toolchain is rendered by `ocx pull`; the User Guide says "that `ocx pull` renders directly into the project". My checkout got the tree without any `pull`. That is a docs mismatch with what I saw, though harmless for the goal. The first mention of `.ocx/` in the checkout that I met was in the User Guide, long after the Getting Started section.

Step 7. Offline, and the pinned flag. I tried `--offline` after `exec` by habit:

```
$ ocx exec --pinned --offline -- sh -c 'command -v shellcheck'
error: unexpected argument '--offline' found

  tip: to pass '--offline' as a value, use '-- --offline'

Usage: ocx exec --pinned [NAMES]... -- <ARGV>...

For more information, try '--help'.
[exit 64]
```

Getting Started links `--offline` as a global flag in Next Steps but never says global flags go before the subcommand in that sentence; the `exec` page says it only for `--global`. I retried with the flag first:

```
$ ocx --offline exec -- shellcheck --version
ShellCheck - shell script analysis tool
version: 0.11.0
license: GNU General Public License, version 3
website: https://www.shellcheck.net
[exit 0]
```

Works. The goal says "without installing them globally"; nothing in my `OCX_HOME` has a global toolchain, and the tool ran only for this project.

Step 8. Staleness. I edited the tag by hand and ran `exec` again:

```
$ sed -i 's/:0.11"/:0.10"/' ocx.toml
[exit 0]
$ ocx exec -- shellcheck --version
error: ocx.lock is stale (it does not match ocx.toml); run `ocx lock`: ocx.lock is stale (it does not match ocx.toml); run `ocx lock`
[exit 65]
```

This matches the User Guide tip. The error message repeats the same sentence twice. I did not run `ocx lock` afterward (command budget used), so I did not verify the recovery path. I also did not test a fresh-clone `ocx pull`, groups, or the CI path.

Summary of blocked steps: none stopped me from finishing T02. The missing example scripts removed the only concrete example in Getting Started, and I had to leave that page to find a package to add.
