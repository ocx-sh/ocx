# Handover — container test legs lose the executable bit during extraction

**Status:** OPEN BUG, unreported. Reproduced in production CI, 4/4 container legs.
**Date:** 2026-08-02. **Affects:** the statically-linked `ocx` the generated mirror
workflow downloads for container legs (renderer-pinned constant, not a spec field).
**Origin:** `ocx-contrib/mirror-powershell` — the last package of the 40-repo fleet
migration; it is the only one this blocks, and it blocks it completely.
**Severity:** a correct artifact with a correct spec cannot be published. The
failure names a permission error on the *consumer* side, so it reads as a bad
artifact or a bad image — it is neither.

## Symptom

Every Linux container leg fails; every native leg passes.

```
WARN  Could not resolve 'pwsh' via PATH, falling back to OS lookup.
error: failed to spawn 'pwsh': Permission denied (os error 13)
  --> powershell/tests/smoke.star:28:13
```

The `WARN` is the tell, not the `error`. `ocx` skips PATH entries that are not
executable, so it never found the package's own `pwsh`, fell through to an OS
lookup, and EACCES'd on whatever that resolved to. The spawn failure is a
downstream consequence of the file not being executable.

## The split that localises it

One run, `mirror-powershell` workflow, same bundle for every leg:

| Leg | Extraction | Result |
|---|---|---|
| `darwin/amd64`, `darwin/arm64` | native | **pass** |
| `windows/amd64`, `windows/arm64` | native | **pass** |
| `linux/amd64+libc.glibc` × 2 images | in container | **fail** |
| `linux/arm64+libc.glibc` × 2 images | in container | **fail** |

Native passes, container fails, everything else held constant. That is the whole
diagnosis: the container leg's own extraction is the only variable.

## The exec bit is present everywhere it can be inspected

Checked at each stage — it is `-rwxr-xr-x` throughout:

```
$ tar tvzf powershell-7.6.4-linux-x64.tar.gz | grep ' pwsh$'
-rwxr-xr-x root/root  78256 2026-07-16 20:32 pwsh          # upstream asset

$ tar tvJf .../linux_amd64_libc.glibc/bundle.tar.xz | grep ' pwsh$'
-rwxr-xr-x 0/0        78256 2006-07-24 03:21 pwsh          # prepared bundle

$ docker run --rm -v /tmp/pwshtest:/pkg mcr.microsoft.com/dotnet/runtime-deps:9.0 \
    sh -c 'ls -l /pkg/pwsh; /pkg/pwsh -NoProfile -Command "1+1"'
-rwxr-xr-x 1 1000 1000 78256 ... /pkg/pwsh
2                                                          # runs fine
```

That last one matters: the binary executes **inside the same image the CI leg
uses**, when the tree is extracted on the host and mounted in. Mounting bypasses
`ocx`'s in-container extraction — which is exactly the step that cannot be
exercised locally, and exactly the step that fails.

Local `ocx package test --platform "linux/amd64+libc.glibc" … --script …` also
passes, because that runs natively.

## What is NOT the cause

- **Not the artifact.** Exec bit present in the upstream tarball and in the
  bundle; the binary runs in the target image.
- **Not the image.** Both `mcr.microsoft.com/dotnet/runtime-deps:9.0` and
  `:9.0-noble` fail identically, and the binary runs in `:9.0` when mounted.
- **Not a missing runtime library.** That is a *different*, real PowerShell
  requirement — on `ubuntu:24.04` `pwsh` starts and then aborts in
  `System.Globalization.CultureInfo..cctor()` (missing libicu). It is a distinct
  failure mode with a distinct message, and the MS images satisfy it. Do not
  conflate the two: chasing the ICU error is what sent this investigation
  sideways once already.
- **Not the spec.** `metadata.json` PATH is `${installPath}`, the binary sits at
  the content root, `ocx-mirror package validate` exits 0, and every other
  package in the 40-repo fleet with the same shape publishes fine.

## Why only this package hits it

Most fleet packages are single static binaries. PowerShell's archive extracts
flat as a self-contained .NET tree — one small `pwsh` launcher beside dozens of
`*.dll`/`*.so`. Whatever the container-side extraction does with modes, this is
the layout that exposes it. It is not PowerShell-specific in principle: any
package whose interface binary is not re-marked executable downstream would fail
the same way.

## MEASURED: container legs run `ocx v0.4.3`, hardcoded

The generated workflow downloads a statically-linked `ocx` for container legs,
and the tag is a **renderer constant**, not a spec field:

```
$ grep -o 'releases/download/v[0-9.]*/ocx-' .github/workflows/mirror-powershell.yml
releases/download/v0.4.3/ocx-
```

Identical in every repo checked (`mirror-yq`, `mirror-amazon`, `mirror-github`),
so this is fleet-wide, not powershell-specific.

**That is two minor versions behind what the fleet actually pins** — every
`ocx.toml` is on `ocx.sh/ocx/cli:0.5` (resolving to 0.5.2). So container legs
verify artifacts with a binary that predates, among others:

- `4fba1e8f fix(oci): spurious digest mismatch on layer pulls` (0.5.1)
- `7b3c1755 fix(oci): bound stalled registry connections with a 120s read timeout` (0.5.1)
- `a1278962 fix(announce): sync the fork before committing, replay transient forge faults` (0.5.2)

**This is a defect in its own right, independent of the exec-bit symptom:** the
one leg type that exists specifically to *prove* an artifact runs on a real host
is the only one not exercising the shipped toolchain. It also means the
layer-drain digest bug (documented in
`handover_layer_digest_undrained_stream.md`, fixed in 0.5.1) is still live on
every container leg in the fleet.

Note: mode-preservation code and its test **do** exist at v0.4.3
(`crates/ocx_lib/src/archive/tar.rs`, `PermissionsExt` test present at that tag),
so the exec-bit loss is not simply "the feature was missing". It is more likely a
0.4.x path the existing test does not cover — e.g. extraction under a different
umask inside `docker run`, or this archive's shape (one small launcher beside
dozens of `.dll`/`.so`, all at the content root).

### Why it went stale: Renovate has never run on `ocx-mirror`

The pin carries a Renovate anchor and the config is **correct** —
`renovate.json`'s second `customManagers` entry matches
`^src/command/package/pipeline/generate/ci\.rs$` and its regex matches the line
verbatim, capturing `currentValue`/`datasource`/`depName`. But nothing executes
it:

- All **34** PRs in `ocx-sh/ocx-mirror`'s history are authored by
  `michael-herwig`; **zero** by Renovate.
- No self-hosted Renovate workflow exists in `.github/workflows/`.

So the doc comment's promise — "it moves when the renderer moves" — was never
backed by a live mechanism. The pin has sat at `v0.4.3` since the repo was
scaffolded, through the 0.5.0, 0.5.1 and 0.5.2 releases. **Bumping the constant
without enabling Renovate (or adding a release-time check) just restarts the
same drift.**

**First action: advance the renderer's pinned tag to the current release and
re-run.** If the symptom disappears, the bug is already fixed and the pin was the
whole problem. If it persists, the reproduction below isolates it.

A regression test should extract a bundle containing a file with mode `0755`
**inside a container** and assert the extracted file is executable — the native
equivalent already passes and would not have caught this.

## Blast radius

**The exec-bit symptom:** one package today (`powershell/powershell`,
unpublished). Latent for any future mirror whose binary relies on the archive's
own exec bit. It surfaces as a consumer-side `Permission denied`, which invites
blaming the artifact or the image; both are innocent here.

**The stale pin, which is the wider issue:** every container leg in the entire
40-repo fleet — the legs that exist to turn an `os.features` claim into evidence
— runs `ocx v0.4.3` while the packages themselves are built and pushed with
0.5.2. So the fleet's strongest verification step is the one place the shipped
toolchain is never exercised, and known-fixed bugs (notably the layer-digest
drain) remain live there. Fix the pin regardless of what happens to the exec-bit
symptom.
