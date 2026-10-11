# Friction log: T03 ocx tools in CI

## Context

Written by a subagent that was limited to the Markdown docs snapshot (`docs-snapshot/`) and the installed `ocx` binary (reports `0.6.5`). Persona: a CI maintainer who has never used ocx before and owns one GitHub Actions workflow and one GitLab pipeline. Goal T03: find in the docs how to get ocx-provided tools onto `PATH` inside a CI job (GitHub Actions and GitLab CI), find how to run unattended, then try locally whatever can run locally.

Pages opened: `in-depth/ci.md`, `installation.md`, `docker.md`, `getting-started.md` (sections on package env and project toolchain, plus the CI tip), `user-guide.md` (sections "Carry PATH to the next CI step" and "Use OCX in CI", plus a few greps), `reference/environment.md` (CI variables, `OCX_NO_CONSENT`, `OCX_NO_UPDATE_CHECK`, `OCX_FROZEN`, `OCX_QUIET`), `reference/command-line.md` (`ocx env` root section, and greps for `--no-tty`), `faq.md` (grep only). Not opened: the rest of `in-depth/`, `authoring/`, `contributing/`.

Local setup: `OCX_HOME` pointed at `home-t03`, `OCX_NO_UPDATE_CHECK=true`, `CI=true`, and the GitHub-style variables pointed at files inside the work directory. Both `work/t03/` and `home-t03/` already held content from earlier runs, so I worked in a new subdirectory `work/t03/fresh/`. I did not run the `curl ... | sh` installer: it writes to `$HOME`, outside the allowed directory.

Steps that blocked me: 2. A third step (the `node:20` example) failed but I worked around it.

## Pros and cons

Pros
- `in-depth/ci.md` answers the main question on one page, with a GitHub section, a GitLab section and a "which flag to use" box up front (`--shell` for the current step, `--ci` for later steps).
- The GitHub path works as written when run locally: with `GITHUB_PATH`/`GITHUB_ENV` pointing at files, `ocx env --ci=github` wrote the package directory to the path file, and `shellcheck` ran from it.
- The documented failure modes are real: outside a runner `--ci=github` exits 78 and bare `--ci` exits 64, as the command reference says.
- `OCX_NO_CONSENT` has a long, clear explanation of why a CI job should set it.
- The `CI` variable is documented as switching off update checks, so the "unattended" part is partly automatic.

Cons
- There is no page or section about running unattended. The relevant facts are spread over `reference/environment.md` (`CI`, `OCX_NO_UPDATE_CHECK`, `OCX_NO_CONSENT`, `OCX_FROZEN`, `OCX_OFFLINE`, `OCX_QUIET`), `reference/command-line.md` (`--no-tty` on the auth commands) and `user-guide.md` (headless keychain). Nothing says which ones a pipeline should set.
- The GitLab section covers only the experimental step runner (`run:` keyword). For ordinary `script:` jobs the docs say the format cannot be consumed and "requires a separate solution", and give none.
- `user-guide.md` ("Use OCX in CI") says a dedicated CI export command is "a deferred extension point", while `in-depth/ci.md` documents `ocx env --ci` as existing. Two pages disagree.
- `getting-started.md` still points at `ocx package env --shell=sh` and `ocx --format json package env` for CI, not at `--ci`.
- The OCI-tier examples in `in-depth/ci.md` use `node:20` and `python:3.12`; `ocx package env --ci=github node:20` fails with exit 79 here.
- The `user-guide.md` CI section includes `<<< @/_scripts/user-guide/ci-project.sh` and `ci-oci.sh`; the `_scripts` directory in the snapshot did not contain them (only a `.gitignore` listed), so the recommended project flow was not visible to me.
- The GitHub snippet in `user-guide.md` ("Carry PATH") uses `jq` without saying it must be installed, and its output path differs from the one `ocx env --ci=github` writes.
- `${{ export_file }}` only exists inside the GitLab step runner, so none of the GitLab examples could be tried locally.
- `setup-ocx` is shown as `ocx-sh/setup-ocx@<sha> # v1.2.2` with a literal `<sha>` placeholder; the docs do not say where to get the SHA.

## Detailed stream of consciousness

Start: "CI" is in the sidebar-ish file list as `in-depth/ci.md`. Good. I opened it first. The opening paragraph explains exactly my problem (env does not cross steps) and names `--ci`. Nice.

GitHub half is clear. Two things I noticed: the page says OCX-composed dirs land leftmost via `$GITHUB_PATH`; and the turnkey path is a third-party action with `<sha>` left as a placeholder. As a maintainer I would pin by SHA, and I do not know which SHA. The page has a manual alternative with `curl ... | sh` and an `echo ... >> "$GITHUB_PATH"` for the ocx binary itself.

GitLab half: first line says the feature is the experimental step runner and "not compatible with traditional `script:` jobs". My pipeline is a traditional `script:` pipeline. The page then says dotenv cannot be produced and "requires a separate solution". That is where I got stuck (blocked step 1): there is no instruction for my pipeline. I searched `user-guide.md` and `getting-started.md` for GitLab and found nothing. The only workable thing I could find is `eval "$(ocx env --shell=sh)"` inside one script block, which is documented for single steps only; I tried it below to see what it prints, but I could not run it inside GitLab.

Unattended: I searched for "unattended", "non-interactive", "headless". Hits: one line about headless keychain for login, one about the fleet refresh tick. No page. I assembled my own list from `reference/environment.md`: `CI` suppresses update checks, `OCX_NO_UPDATE_CHECK`, `OCX_NO_CONSENT` ("how automation declines to answer a question it was never asked"), `--no-tty` for auth. I cannot tell from the docs whether a job with none of these set would ever stop to ask a question.

Installation page: `--no-modify-path` mentioned for CI, plus a pinned URL `setup.ocx.sh/sh/0.5.0`. The installed binary here is 0.6.5; docker.md shows `0.9.2`. I could not tell which release the docs describe.

Local trials follow. Output cut at 300 columns where marked.

First, the work directory was not new:

```
$ ocx version
0.6.5
exit=0
$ ocx init
error: /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/ocx.toml: ocx.toml already exists at '/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/ocx.toml': /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/ocx.toml: ocx.toml already exists at '/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/ocx.toml': ocx.toml already exists at '/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/ocx.toml'
exit=64
```

The error repeats the same path-and-message four times. I moved to `t03/fresh/`.

```
$ ocx init
created /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/ocx.toml
exit=0
$ ocx add ocx.sh/shellcheck/shellcheck:0.11
pulling count=1
Binding     Group    Digest                                                                 
shellcheck  default  sha256:3981b442fff3f6bf9ae93e4c293e53322ffc9135e0b654620bc800464bdaf379
exit=0
```

Now the GitHub flow. First with no runner variables, then bare `--ci`:

```
$ ocx env --ci=github
error: CI environment variable '$GITHUB_PATH' is not set; is this running inside a CI system
exit=78
$ ocx env --ci
error: could not autodetect CI provider; pass --ci=github or --ci=gitlab; the value must be attached with `=`
exit=64
```

Matches the docs (78 and 64). Then with the variables pointed at files in the work dir (`GITHUB_ACTIONS=true GITHUB_PATH=$PWD/gh_path GITHUB_ENV=$PWD/gh_env`, files emptied first):

```
$ ocx env --ci=github
exit=0
$ cat gh_path; echo "--"; cat gh_env
/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/links/default/shellcheck/content
--
$ ocx env --ci
exit=0
/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/links/default/shellcheck/content
```

The command is silent on success; the only evidence is the file. The path is under the project directory (`.ocx/toolchain/links/...`), not under the package store as the page's wording ("resolved package paths") suggested. I checked it is usable:

```
$ PATH="$(cat gh_path):$PATH" shellcheck --version
ShellCheck - shell script analysis tool
version: 0.11.0
license: GNU General Public License, version 3
website: https://www.shellcheck.net
exit=0
```

(I also ran `which shellcheck` without the prefix and it found `/home/mherwig/dev/ocx/.ocx/toolchain/links/default/shellcheck/content/shellcheck`. That comes from my own shell's per-prompt hook for a different checkout, so I discarded it as evidence. It shows that a developer machine with the hook hides whether the CI step really did the work.)

Next, GitLab. I can run the command; I cannot run the step runner (blocked step 2: `${{ export_file }}` has no meaning outside it, so the `run:` examples are untestable locally).

```
$ GITLAB_CI=true ocx env --ci=gitlab --export-file=./gl_export
exit=0
$ cut -c1-250 gl_export
{"name":"PATH","value":"/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/links/default/shellcheck/content:/home/mherwig/dev/ocx/.ocx/toolchain/links/default/uv/content:/home/mherwig/dev/ocx/.ocx/toolchain
$ ocx env --ci=gitlab | cut -c1-250
{"name":"PATH","value":"/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/links/default/shellcheck/content:/home/mherwig/dev/ocx/.ocx/toolchain/links/default/uv/content:/home/mherwig/dev/ocx/.ocx/toolchain
$ ocx env --shell=sh | cut -c1-250
__ocx_p='/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/links/default/shellcheck/content'; export __ocx_p; export PATH="$__ocx_p$(printf %s "${PATH-}" | awk 'BEGIN{ORS="";RS=":";d=ENVIRON["__ocx_p"]} $0
$ ocx env --ci=gitlab --shell=sh
error: the argument '--ci[=<PROVIDER>]' cannot be used with '--shell[=<SHELL>]'

Usage: ocx env --ci[=<PROVIDER>]

For more information, try '--help'.
exit=64
```

(The `exit=` lines for the two piped commands were lost because of my `${PIPESTATUS}` use under zsh; I did not rerun them.) The GitLab line contains my whole ambient `PATH`, as the page says ("flattened ... prepended to the current process value"). In a runner, that means the JSON line freezes the `PATH` of the moment; the docs state it but do not say what happens if the later step runs on a different image.

The `--shell=sh` output exists, and is the only thing a `script:` job could eval; the docs mention it for single steps only.

Now unattended pieces and the `user-guide.md` recipe:

```
$ OCX_NO_CONSENT=1 ocx pull
pulling count=1
Package                                                                                               Kind     Path                                                                                                                                   
ocx.sh/shellcheck/shellcheck@sha256:3981b442fff3f6bf9ae93e4c293e53322ffc9135e0b654620bc800464bdaf379  package  /home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t03/packages/ocx.sh/sha256/39/81b442fff3f6bf9ae93e4c293e5332
exit=0
$ ocx --format json shell state | jq -r .toolchain_bin
/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/t03/fresh/.ocx/toolchain/active/bin
$ ocx --offline exec -- shellcheck --version
ShellCheck - shell script analysis tool
version: 0.11.0
license: GNU General Public License, version 3
website: https://www.shellcheck.net
exit=0
```

`OCX_NO_CONSENT=1 ocx pull` ran without any question, but I never ran `ocx pull` without the variable, so I do not know whether the variable changed anything for me. The `shell state` recipe printed a path inside the project, while the section it sits in is about the global toolchain; the directory held one 329-byte `shellcheck` launcher. Whether this is the directory the section means, I could not tell from the text.

Then the global tier and the OCI tier:

```
$ ocx --global env --ci=github      # GITHUB_ACTIONS/GITHUB_PATH/GITHUB_ENV set to fresh empty files
exit=0
$ wc -c gh_path4 gh_env4
0 gh_path4
0 gh_env4
Σ 0
$ ocx package env --ci=github ocx.sh/shellcheck/shellcheck:0.11
exit=0
$ cat gh_path4
/home/mherwig/dev/ocx/.agents/hex-run/docs-next-level/scratch/p6/work/home-t03/packages/ocx.sh/sha256/39/81b442fff3f6bf9ae93e4c293e5332/content
$ ocx package env --ci=github node:20
error: failed to find package: ocx.sh/node:20 — chained index source walk failed: 'ocx.sh/node:20' is not in the index at https://index.ocx.sh, which is authoritative for every name in registry 'ocx.sh'; announce it there with `ocx package announce`, or take the namespace off the index with `[regist
exit=79
```

`--global` with an empty global toolchain writes nothing and exits 0, consistent with the "lenient" rule in the reference. The documented OCI-tier example `node:20` fails: this is the example I would copy first. The error talks about announcing a package and about registry namespaces, which a CI maintainer new to ocx does not have context for. Elsewhere (`getting-started.md`) the example identifiers are `nodejs/node:24`, so a name like `node:20` appears to need a namespace; the CI page does not say so. I counted this as a failure, not a block, since I had a working identifier (`ocx.sh/shellcheck/shellcheck:0.11`) from the project flow.

What I could not answer from the docs:
- Which settings make a job fully non-interactive and offline-safe for a first run (no single page).
- What a GitLab `script:` job should do. Docs say a separate solution is needed and stop there.
- Where to get the `setup-ocx` commit SHA that the example leaves as `<sha>`.
- Whether `ocx pull` needs `OCX_NO_CONSENT` on a runner (the variable page says "wherever a machine drives OCX against a checkout its operator did not choose"; my checkout was my own).
