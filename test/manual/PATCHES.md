# Patches Feature — Manual Exploration

This document walks through the patch use-cases set up by
`test/manual/scripts/setup-patches.sh`, from both the **consumer** perspective
(use-cases 1–3: installing and running patched packages) and the **maintainer**
perspective (authoring, testing, publishing, freezing, and syncing
descriptors). All commands assume the current directory is the repo root
(`/path/to/ocx-sion`).

## Prerequisites

Run these from the repo root, in order:

1. Start a local registry on `localhost:5000`:
   ```sh
   docker run -d -p 5000:5000 --name registry registry:2
   ```
   (or, from the repo: `cd test && docker compose up -d`)
2. Build the binary the rig will use:
   ```sh
   cargo build --release -p ocx
   ```
3. Point the shell at the local registry and a disposable `OCX_HOME`:
   ```sh
   source test/manual/scripts/env.sh
   ```
   This exports `OCX_DEFAULT_REGISTRY=localhost:5000`,
   `OCX_INSECURE_REGISTRIES=localhost:5000`, and a gitignored
   `OCX_HOME=test/manual/.ocx-home/` so manual experiments never touch your
   daily `~/.ocx`.
4. Publish the packages + descriptors and write the `[patches]` config:
   ```sh
   test/manual/scripts/setup-patches.sh
   ```

For the ad-hoc `ocx` calls below, point `$OCX` at the binary you built
(`setup-patches.sh` auto-detects release or debug on its own):

```sh
export OCX=./target/release/ocx
```

## Teardown

```sh
test/manual/scripts/teardown-patches.sh
```

This removes all packages, descriptors, and the local `.ocx-home` snapshot
so a fresh `setup-patches.sh` run starts from scratch.

---

## Use-Case 1 — CORP CA BUNDLE (global, required)

**Goal**: every installed package automatically gets `SSL_CERT_FILE`,
`NODE_EXTRA_CA_CERTS`, and `REQUESTS_CA_BUNDLE` pointing at a corporate CA
certificate, without any per-package configuration.

### What the descriptor says

The global descriptor at `patches/descriptors/global.json` is published once
with `ocx patch publish --global` and has a catch-all rule that fires for every
base package:

```json
{
  "match": "*",
  "packages": ["localhost:5000/patches/corp-ca-bundle:1.0.0"],
  "required": true
}
```

It lives at the reserved `global` repository in the patch registry, so it
applies to every base without per-package configuration. `required: true` means
the env resolution fails closed if `corp-ca-bundle` is not installed.

### Phase 3 — companion discovery at install time

```sh
$OCX package install localhost:5000/patches/base-tool:1.0.0
```

Expected output includes a line like:

```
Installed 2 companion(s) for localhost:5000/patches/base-tool:1.0.0: corp-ca-bundle, license-server
```

Both companions are installed alongside the base package.

### Phase 4 — env overlay at resolve time

```sh
$OCX package env --candidate --show-patches localhost:5000/patches/base-tool:1.0.0
```

Expected output (abbreviated):

```
Key                  Type      Value                                Source
PATH                 path      <path>/base-tool/bin
TOOL_HOME            constant  <path>/base-tool
SSL_CERT_FILE        constant  <path>/certs/corp-ca.pem              localhost:5000/patches/corp-ca-bundle:1.0.0 (rule: *)
NODE_EXTRA_CA_CERTS  constant  <path>/certs/corp-ca.pem              localhost:5000/patches/corp-ca-bundle:1.0.0 (rule: *)
REQUESTS_CA_BUNDLE   constant  <path>/certs/corp-ca.pem              localhost:5000/patches/corp-ca-bundle:1.0.0 (rule: *)
LICENSE_SERVER       constant  flex://license.corp.internal:27000    localhost:5000/patches/license-server:1.0.0 (rule: localhost:5000/patches/base-tool*)
LM_LICENSE_FILE      constant  27000@license.corp.internal           localhost:5000/patches/license-server:1.0.0 (rule: localhost:5000/patches/base-tool*)
```

`--show-patches` adds a `Source` column. A companion-overlay entry names the
companion identifier and the descriptor rule that admitted it; the base
package's own entries leave `Source` blank.

---

## Use-Case 2 — JDK TRUSTSTORE (package-specific, required)

**Goal**: when `base-java` is installed, automatically inject
`JAVA_TOOL_OPTIONS` to point the JVM at a corporate truststore. Only
`base-java` gets this companion — other packages are unaffected.

### What the descriptor says

The per-package descriptor at `patches/descriptors/java-specific.json`:

```json
{
  "match": "localhost:5000/patches/base-java*",
  "packages": ["localhost:5000/patches/java-truststore:1.0.0"],
  "required": true
}
```

The bare `*` suffix matches both Phase 3 (tag present) and Phase 4
(tag-stripped) identifiers without needing to know whether `:tag` or `@digest`
appears after the name.

### Phase 3 — companion discovery at install time

```sh
$OCX package install localhost:5000/patches/base-java:1.0.0
```

Expected: `java-truststore` installed as companion.

### Phase 4 — env overlay at resolve time

```sh
$OCX package env --candidate --show-patches localhost:5000/patches/base-java:1.0.0
```

Expected output:

```
Key                  Type      Value                                          Source
PATH                 path      <path>/base-java/bin
JAVA_HOME            constant  <path>/base-java
JAVA_TOOL_OPTIONS    constant  -Djavax.net.ssl.trustStore=<path>/corp-trust.jks  localhost:5000/patches/java-truststore:1.0.0 (rule: localhost:5000/patches/base-java*)
JVM_TRUSTSTORE_PATH  constant  <path>/corp-trust.jks                          localhost:5000/patches/java-truststore:1.0.0 (rule: localhost:5000/patches/base-java*)
```

`JAVA_TOOL_OPTIONS` includes `-Djavax.net.ssl.trustStore=<path>/corp-trust.jks`.
The path resolves through the package's content-addressed store, not a
candidate symlink — companion env is always rooted in the CAS.

---

## Use-Case 3 — LICENSE SERVER (required=false, fail-open)

**Goal**: inject `LICENSE_SERVER` when the license-server companion is present,
but skip gracefully when it is absent. Never block the env resolution.

### What the descriptor says

The rule in `patches/descriptors/license-fail-open.json` (published to
base-tool's path):

```json
{
  "match": "localhost:5000/patches/base-tool*",
  "packages": ["localhost:5000/patches/license-server:1.0.0"],
  "required": false
}
```

`required: false` = fail-open. Missing companion → env resolution continues
without `LICENSE_SERVER`.

### Installed (companion present)

```sh
$OCX package env --candidate --show-patches localhost:5000/patches/base-tool:1.0.0
```

Expected:

```
Key              Type      Value                                Source
LICENSE_SERVER   constant  flex://license.corp.internal:27000    localhost:5000/patches/license-server:1.0.0 (rule: localhost:5000/patches/base-tool*)
LM_LICENSE_FILE  constant  27000@license.corp.internal           localhost:5000/patches/license-server:1.0.0 (rule: localhost:5000/patches/base-tool*)
```

### Absent (companion missing)

Remove the license-server tag entry from the local index to simulate an
environment where it was never installed, then run:

```sh
$OCX package env --candidate --show-patches localhost:5000/patches/base-tool:1.0.0
```

Expected: `SSL_CERT_FILE` and the CA bundle entries still appear (`required=true`
companion is present). `LICENSE_SERVER` and `LM_LICENSE_FILE` are absent —
no error, no WARN at default log level. The `--log-level debug` flag would show
a debug-level skip message.

---

## Maintainer perspective — author, test, publish, freeze, sync

`setup-patches.sh` already published everything, so the consumer use-cases
above work out of the box. This section walks the four maintainer commands so
you can feel the authoring loop yourself: edit a descriptor, preview it,
publish it, pin it, and refresh it.

The descriptors live in `packages/patches/descriptors/`:

| File | Rule |
|------|------|
| `global.json` | `match="*"` → `corp-ca-bundle`, `required=true` |
| `java-specific.json` | `match="localhost:5000/patches/base-java*"` → `java-truststore`, `required=true` |
| `license-fail-open.json` | `match="localhost:5000/patches/base-tool:*"` → `license-server`, `required=false` |
| `base-tool-combined.json` | the global CA rule **and** the fail-open license rule |

### 1. Preview a descriptor without publishing (`ocx patch test`)

`patch test` composes a descriptor onto a base in a scratch store — no registry
write, no change to `$OCX_HOME`. With no trailing command it prints the composed
environment:

```sh
$OCX patch test \
    --descriptor test/manual/packages/patches/descriptors/global.json \
    localhost:5000/patches/base-tool:1.0.0
```

Add a trailing command to run it in the composed env (here the base's own
`mytool` stub, which echoes whether `SSL_CERT_FILE` is set):

```sh
$OCX patch test \
    --descriptor test/manual/packages/patches/descriptors/global.json \
    localhost:5000/patches/base-tool:1.0.0 -- mytool
```

Required companions must resolve — installed locally or pullable from the
registry. For a companion you have built but not yet pushed, hand `patch test` a
local archive so you can preview before publishing:

```sh
$OCX patch test \
    --descriptor test/manual/packages/patches/descriptors/global.json \
    --companion-archive test/manual/packages/patches/corp-ca-bundle/out/corp-ca-bundle-1.0.0.tar.xz \
    localhost:5000/patches/base-tool:1.0.0 -- mytool
```

### 2. Publish the companion, then the descriptor (`ocx patch publish`)

A descriptor only references companions by identifier, so publish the companion
package itself first (this is what `setup-patches.sh` did via `ocx package
push`). Then publish the descriptor to a base's package-specific path:

```sh
$OCX patch publish \
    --descriptor test/manual/packages/patches/descriptors/license-fail-open.json \
    localhost:5000/patches/base-tool:1.0.0
```

`--global` publishes a descriptor that applies to every base instead of one
named base. It lands at the reserved `global` repository in the patch registry
(`<patch-registry>/global:__ocx.patch`), so it works on any OCI registry —
including the local `registry:2`:

```sh
$OCX patch publish \
    --descriptor test/manual/packages/patches/descriptors/global.json \
    --global
```

### 3. Pin companion digests for reproducible builds (`ocx patch freeze`)

OCI tags are mutable: the same `:1.0.0` tag may point at a new digest tomorrow.
`patch freeze` resolves every companion + descriptor in the active overlay and
writes `patches.snapshot.json` beside the active `ocx.lock`. On the manual rig
the global toolchain under `$OCX_HOME` is the project in effect:

```sh
$OCX --global patch freeze
```

Point `OCX_PATCH_SNAPSHOT` at the written snapshot to make composition prefer
the pinned digests and skip live tag lookups — even offline:

```sh
export OCX_PATCH_SNAPSHOT="$OCX_HOME/patches.snapshot.json"
$OCX --offline package env --candidate --show-patches \
    localhost:5000/patches/base-tool:1.0.0
```

Unset `OCX_PATCH_SNAPSHOT` to float back to live tags.

### 4. Refresh descriptors + companions (`ocx patch sync`)

After you re-publish a descriptor (point a companion at a new digest, or add a
rule), `patch sync` re-fetches every descriptor for the installed bases plus the
global root and installs any newly-referenced companions:

```sh
$OCX patch sync
```

`patch sync` is the only consumer-side command that contacts the patch registry;
the install dirs and `resolve.json` of the base packages are never rewritten.

---

## Known Product Bugs (found during exploration)

### Bug 1 — Phase 3 / Phase 4 identifier mismatch for match patterns

**Symptom**: a descriptor rule with `"match": "registry/repo@*"` works in
Phase 4 (tag-stripped admitted identifier) but silently misses in Phase 3
(full tagged identifier `registry/repo:tag@digest`).

**Root cause**: `collect_companions` calls `base_identifier.to_string()`, which
includes `:tag` in Phase 3 but omits it in Phase 4 (after `strip_advisory()`).
A pattern `registry/repo@*` cannot match `registry/repo:tag@digest`.

**Workaround**: use a bare `*` suffix: `registry/repo*`. The flat glob treats
`*` as matching any byte, including `:`, `@`, and `/`, so it covers both phases.

### Bug 2 — Empty tar layer causes install failure

**Symptom**: a companion package with no content files (e.g. a pure-env package
with no binaries) fails to install with "package not found" after extraction.

**Root cause**: the OCX install pipeline extracts the tar layer into a temp
directory and renames it into `layers/{registry}/{digest}/`. It then expects
`content/` to exist. An empty tar produces no `content/` subdirectory, so
`find_in_store` returns `None`.

**Workaround**: add a placeholder file (e.g. `.keep`) inside the package's
content directory so the tar layer includes at least one file.

### Bug 3 — Error identifier displays as `/`

**Symptom**: error messages for companion lookup failures show
`"failed to resolve package: / — required companion install failed..."`.
The `/` is not a real identifier.

**Root cause**: `From<PackageErrorKind> for crate::Error` at
`crates/ocx_lib/src/error.rs` wraps non-`Internal` error kinds in a
`PackageError::new(Identifier::new_registry("", ""), ...)`. An empty
registry + empty repository displays as `"/"`.

**Impact**: diagnostic quality only. The actual companion identifier appears
after the dash (`— required companion install failed for '<id>':`).

### Bug 4 — `ocx clean` removes descriptor manifest blobs

**Symptom**: after `ocx clean`, Phase 4 env resolution fails with a descriptor
corrupt error (tag store says blob exists, but blob is gone from CAS).

**Root cause**: descriptor manifest blobs written by Phase 3 are stored in the
blob CAS but have no `refs/blobs/` forward-reference from any package. The GC
BFS pass cannot reach them and removes them as unreachable.

**Impact**: only surfaces after an explicit `ocx clean` between Phase 3 install
and Phase 4 env resolve. Fresh installs are unaffected; blobs are re-written at
next install.

**Mitigation for exploration**: if `package env --show-patches` fails with a
descriptor corrupt error, re-run `ocx package install <base>` to re-populate
the descriptor blobs.

### Bug 5 — Partial layer extraction blocks future extractions

**Symptom**: a layer directory contains only a `digest` file (no `content/`
subdirectory). Subsequent install attempts for the same digest fail with
`ENOTEMPTY` when trying to rename the temp dir into that path.

**Root cause**: if the process is interrupted (kill-9, disk full, etc.) after
the temp dir is renamed into `layers/` but before `content/` is created, the
layer directory is in a half-extracted state. Future extractions see a
non-empty target and cannot rename over it.

**Workaround**: manually remove the partial layer directory. `ocx clean` cannot
remove it because the directory walk hits max depth at the `{digest}/` level
(the `{digest}/` dir itself counts as a layer entry — the partial state is
indistinguishable from a valid in-progress extraction from the GC perspective).

---

## File Map

```
test/manual/
  scripts/
    setup-patches.sh         # Publishes packages, installs base packages
    teardown-patches.sh      # Removes all state
  packages/patches/
    base-java/build/         # Mock JDK content (bin/java stub)
    base-tool/build/         # Mock tool content (bin/tool stub)
    corp-ca-bundle/build/    # CA cert companion (certs/corp-ca.pem)
    java-truststore/build/   # JKS truststore companion (truststore/corp-trust.jks)
    license-server/build/    # No binary files; .keep placeholder required
    descriptors/
      global.json              # match="*" -> corp-ca-bundle (required)
      java-specific.json       # base-java scoped -> java-truststore (required)
      license-fail-open.json   # base-tool scoped -> license-server (required=false)
      base-tool-combined.json  # corp-ca (required) + license-server (fail-open)
  .ocx-home/                 # OCX state directory (populated by setup-patches.sh)
  PATCHES.md                 # This file
```
