---
outline: deep
---
# Authoring

You publish to OCX in three layers: build a deterministic archive, attach metadata declaring how OCX should expose its environment, and push the result to any [OCI registry][oci-distribution]. Every step in that chain has its own page in this section. This overview frames the publisher journey end-to-end and tells you which page to open for each decision you face.

## TL;DR — Publish a Binary {#tldr}

You have a compiled binary, a Python script, or any other executable, and you want consumers to fetch and run it with [`ocx install`][cmd-install]. The shortest path is three steps: lay the binary out under a `bin/` directory, drop a sibling `metadata.json` that adds `bin/` to `PATH`, then bundle and push.

Lay out the working directory so the bundle contents live under `build/` and the metadata sidecar sits next to it. The sidecar is not bundled into the archive — [`ocx package push`][cmd-package-push] uploads it as the [OCI manifest config blob][oci-manifest-config], and OCX restores it as a sibling of `content/` in the assembled package directory at install time:

<Tree :collapsible="false">
  <Node name="mytool/">
    <Description>workdir (cwd for the commands below)</Description>
    <Node name="build/">
      <Description>bundle contents — becomes <code>content/</code> after install</Description>
      <Node name="bin/">
        <Node name="mytool">
          <Description>your binary, Python script, shell wrapper, …</Description>
        </Node>
      </Node>
    </Node>
    <Node name="metadata.json">
      <Description>OCI config sidecar — not part of the archive</Description>
    </Node>
  </Node>
</Tree>

The sidecar declares one [public env entry][authoring-env-visibility] that prepends the package's `bin/` to `PATH`:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    {
      "key": "PATH",
      "type": "path",
      "value": "${installPath}/bin",
      "visibility": "public"
    }
  ]
}
```

Bundle `build/` into a deterministic archive, then push it to any registry you can authenticate against. The push attaches `metadata.json` as the manifest config blob — there is no copy inside the archive:

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json \
  ghcr.io/me/mytool:1.0.0 mytool-1.0.0.tar.xz
```

<Terminal src="/casts/package-push.cast" title="Bundle and push mytool:1.0.0" collapsed />

Consumers now reach it with [`ocx install ghcr.io/me/mytool:1.0.0`][cmd-install] and the binary lands on their `PATH`. Everything below this section refines that flow: deterministic bundling, dependency edges, env visibility, named launchers, rolling tags, multi-platform manifests, and patterns for wrapping third-party tools you do not own.

::: tip Scripted runtimes
If your "binary" is a Python script, a JAR, or anything that needs an interpreter, the script's shebang or wrapper still works — but the cleaner path is to declare the runtime as a [dependency][authoring-dependencies] and ship a [named entry point][authoring-entry-points] that resolves the interpreter from that dep. Skip the ambient `python3` lookup; pin the version your tool expects.
:::

## The Publisher Journey {#journey}

Most packages start as an upstream binary release on [GitHub][gh-releases] or a vendor's download page. The work between "I have a binary" and "consumers can `ocx install` it" splits into seven decisions:

- **[Bundle anatomy][authoring-bundle-anatomy]** — what goes in the archive, whether to repack the upstream layout, which compression to pick. Reach for this when you have a directory and need to turn it into a `.tar.xz` ready for [`ocx package push`][cmd-package-push].
- **[Declaring dependencies][authoring-dependencies]** — when to depend on another OCX package instead of bundling its bytes, how to pin by digest, what visibility to choose for the dependency edge. Reach for this when your tool needs to find another tool on disk at runtime.
- **[Env surface][authoring-env-surface]** — which env vars to declare, how to mark each one (`private` / `public` / `interface`), when last-wins matters, how to migrate packages published before the entry-points release. Reach for this when designing the contract between you and consumers — and when retrofitting older `metadata.json` files.
- **[Entry points][authoring-entry-points]** — when to ship named launchers instead of exposing `bin/` on PATH, how to pick non-colliding names, how `target` templates thread dependency paths. Reach for this when your tool depends on a shared runtime (Python, Node, JVM) and needs its dep graph encapsulated so two installed tools cannot fight over a single ambient interpreter.
- **[Building and pushing][authoring-building-pushing]** — first push, `--cascade` for rolling tags, BYO archives, cross-package layer reuse by digest. Reach for this when ready to publish.
- **[Testing locally][authoring-testing]** — verify a package works before pushing: run the same install pipeline in a temp directory, exec a command in the composed env, inspect the layout with `--keep`. No registry round-trip, no new digest.
- **[Multi-platform packages][authoring-multi-platform]** — pushing per platform under one tag, how OCX assembles the [OCI Image Index][oci-image-index]. Reach for this when supporting more than one OS/arch.
- **[Migration patterns][authoring-migration]** — `ocx_mirror` specs, repackaging Homebrew and GitHub Releases, attaching description metadata. Reach for this when wrapping an upstream tool you do not own.

## Decision Flowchart {#decisions}

Common questions you will hit while authoring, and the page that answers each:

| Question | Page |
|---|---|
| Should I repack the upstream archive or use `strip_components`? | [Bundle anatomy → strip components][authoring-bundle-anatomy-strip] |
| Which env vars should consumers see vs. only my launchers? | [Env surface → choosing visibility][authoring-env-visibility] |
| When do I need named entry points instead of `PATH += bin/`? | [Entry points → when to declare][authoring-entry-points-when] |
| When do I need a `name` override on a dependency? | [Dependencies → name override][authoring-deps-name] |
| How do I verify a package works before pushing it to the registry? | [Testing locally][authoring-testing] |
| How do I make rolling tags (`1.0`, `1`, `latest`) follow new releases? | [Building & pushing → cascade][authoring-cascade] |
| How do I publish for amd64 + arm64 + darwin under one tag? | [Multi-platform packages][authoring-multi-platform] |
| How do I avoid re-uploading a base layer that's already in the registry? | [Building & pushing → layer reuse][authoring-layer-reuse] |
| How do I add a README or logo to a published package? | [Migration patterns → describe][authoring-describe] |
| How do I retrofit an older `metadata.json` for entry visibility? | [Env surface → migrating][authoring-env-migrating] |

## See Also {#see-also}

- [Metadata reference][reference-metadata] — every field, every constraint
- [Command-line reference — package commands][reference-cli-package] — `create`, `push`, `pull`, `test`, `describe`, `info`
- [Storage in depth][in-depth-storage] — the three-tier CAS that makes layer reuse work
- [Environments in depth][in-depth-environments] — composition order, edge filter, conflicting constants
- [User guide][user-guide] — the consumer side of every decision documented here

<!-- external -->
[oci-distribution]: https://github.com/opencontainers/distribution-spec
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[oci-manifest-config]: https://github.com/opencontainers/image-spec/blob/main/manifest.md#image-manifest
[gh-releases]: https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases

<!-- reference -->
[reference-metadata]: ../reference/metadata.md
[reference-cli-package]: ../reference/command-line.md#package

<!-- commands -->
[cmd-install]: ../reference/command-line.md#install
[cmd-package-push]: ../reference/command-line.md#package-push

<!-- in-depth -->
[in-depth-storage]: ../in-depth/storage.md
[in-depth-environments]: ../in-depth/environments.md

<!-- internal -->
[user-guide]: ../user-guide.md

<!-- authoring -->
[authoring-bundle-anatomy]: ./bundle-anatomy.md
[authoring-bundle-anatomy-strip]: ./bundle-anatomy.md#strip-components
[authoring-dependencies]: ./dependencies.md
[authoring-deps-name]: ./dependencies.md#name-field
[authoring-env-surface]: ./env-surface.md
[authoring-env-visibility]: ./env-surface.md#visibility
[authoring-env-migrating]: ./env-surface.md#migrating
[authoring-entry-points]: ./entry-points.md
[authoring-entry-points-when]: ./entry-points.md#when
[authoring-building-pushing]: ./building-pushing.md
[authoring-cascade]: ./building-pushing.md#cascade
[authoring-layer-reuse]: ./building-pushing.md#layer-reuse
[authoring-testing]: ./testing.md
[authoring-multi-platform]: ./multi-platform.md
[authoring-migration]: ./migration.md
[authoring-describe]: ./migration.md#describe
