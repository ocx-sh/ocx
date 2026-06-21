# Patches Feature — Manual Exploration

This document walks through the three use-cases implemented in `test/manual/`
for the OCX patch overlay feature. All commands assume the current directory is
the repo root (`/path/to/ocx-sion`).

## Prerequisites

1. Local registry running on `localhost:5000`:
   ```sh
   docker run -d -p 5000:5000 --name registry registry:2
   ```
2. Packages published to the registry and OCX home populated:
   ```sh
   test/manual/scripts/setup-patches.sh
   ```
3. Build the debug binary:
   ```sh
   cargo build -p ocx
   ```

The helper below reduces repetition. Source it or prefix every `ocx` call
manually:

```sh
export OCX=./target/debug/ocx
export OCX_HOME=test/manual/.ocx-home
export OCX_INSECURE_REGISTRIES=localhost:5000
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

The global descriptor at `patches/descriptors/base-tool-combined.json` has a
catch-all rule that fires for every base package:

```json
{
  "match": "*",
  "packages": ["localhost:5000/patches/corp-ca-bundle:1.0.0"],
  "required": true
}
```

`required: true` means the env resolution fails closed if `corp-ca-bundle` is
not installed.

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
Key                  Source
PATH                 (own)
TOOL_HOME            (own)
SSL_CERT_FILE        patch
NODE_EXTRA_CA_CERTS  patch
REQUESTS_CA_BUNDLE   patch
LICENSE_SERVER       patch
LM_LICENSE_FILE      patch
```

`--show-patches` adds a `Source` column. Entries tagged `patch` came from
companion overlay projections; untagged entries are the base package's own env.

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
Key                  Source
PATH                 (own)
JAVA_HOME            (own)
JAVA_TOOL_OPTIONS    patch
JVM_TRUSTSTORE_PATH  patch
```

`JAVA_TOOL_OPTIONS` includes `-Djavax.net.ssl.trustStore=<path>/corp-trust.jks`.
The path resolves through the package's content-addressed store, not a
candidate symlink — companion env is always rooted in the CAS.

---

## Use-Case 3 — LICENSE SERVER (required=false, fail-open)

**Goal**: inject `LICENSE_SERVER` when the license-server companion is present,
but skip gracefully when it is absent. Never block the env resolution.

### What the descriptor says

Second rule in `patches/descriptors/base-tool-combined.json`:

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
LICENSE_SERVER    constant  flex://license.corp.internal:27000  patch
LM_LICENSE_FILE   constant  27000@license.corp.internal         patch
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
      base-tool-combined.json  # Global catch-all (corp-ca) + base-tool scoped (license)
      java-specific.json       # base-java scoped (java-truststore)
  .ocx-home/                 # OCX state directory (populated by setup-patches.sh)
  PATCHES.md                 # This file
```
