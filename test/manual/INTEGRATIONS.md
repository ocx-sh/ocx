# `integrations` — Manual Exploration

Hands-on rig for the vendor-namespaced `integrations` block: what a
publisher declares, what OCX composes, and — the point of this document —
what it actually **prints**.

`integrations` is a map of namespace → opaque JSON. OCX validates the
container (key grammar, size caps, its own `${...}` tokens) and never the
contents, and it **never merges**: two packages declaring one namespace
produce two rows, and the consuming application adjudicates — where
`devcontainer.json`'s closest analogue, `customizations`, merges.

Every output block below was captured from a real run against the local
`registry:2`. Paths and digests will differ on your machine; the shapes will
not.

## Prerequisites

From the repo root, in order:

1. Start the local registry:
   ```sh
   cd test && docker compose up -d
   ```
   If port 5000 is already taken — another repo's `registry:2`, a leftover
   container — this service exits with code 2 and `docker compose ps` shows it
   `Exited`. The rig does not care *whose* registry answers on 5000, only that
   one does; `docker ps` tells you which container holds the port.
2. Build the binary the rig will use:
   ```sh
   cargo build --release -p ocx
   ```
3. Point the shell at the local registry and a disposable `OCX_HOME`:
   ```sh
   source test/manual/scripts/env.sh
   ```
4. `jq` on `PATH` (the driver script uses it to project the JSON envelope).

## One-command setup

```sh
test/manual/scripts/setup-integrations.sh
```

Idempotent. It publishes the four `dojo/custom-*` packages, delegates the
whole patch tier to `setup-patches.sh`, publishes a base-specific descriptor
binding the `corp-ca-bundle` companion to `custom-tool`, installs the two
roots, syncs descriptors, and locks the two toolchain projects.

## The guided tour

```sh
test/manual/scripts/show-integrations.sh
```

Nine banner-separated sections, each a real command and its real output. The
sections that state a negative — "no namespace reaches the shell channel",
"`--self` is `[]`", "the companion's row is gone" — **assert** it and exit
non-zero when it does not hold; a demo that only prints cannot tell a passing
contract from a broken one.

`--no-managed` skips section 9, the only section that writes to
`$OCX_HOME/config.toml` (it restores the file on exit either way).

## Teardown

```sh
test/manual/scripts/teardown-integrations.sh          # prompts
test/manual/scripts/teardown-integrations.sh --force  # no prompt
```

Removes the disposable `OCX_HOME`, the `custom-*` build artifacts, the staged
managed-config payload and the two project locks. The registry keeps running.

---

## What the rig publishes

| Package | Namespace(s) | Role |
|---|---|---|
| `dojo/custom-leaf:1.0.0` | `sh.ocx.leaf` | **interface** dep of `custom-tool` — proves closure attribution reaches a consumer |
| `dojo/custom-private:1.0.0` | `com.example.private` | **private** dep of `custom-tool` — proves it does not cross |
| `dojo/custom-tool:1.0.0` | `com.microsoft.vscode`, `com.jetbrains` | root; one payload uses `${self.installPath}`, the other `${deps.leaf.installPath}` |
| `dojo/custom-other:1.0.0` | `com.microsoft.vscode` | second root, **same namespace** as `custom-tool` — proves the no-merge rule |
| `patches/corp-ca-bundle:1.0.0` | `com.microsoft.vscode` | patch **companion** — contributes like a package, attributed to itself |

Plus two toolchain-tier projects under `projects/`:
`integrations/` and `integrations-no-patches/` (identical bindings, patch
tier opted out).

`custom-leaf` squats `sh.ocx.leaf` on purpose. `sh.ocx` is reserved by
documentation only — nothing in the validator rejects it, and the row below
is the proof. If that ever becomes an enforced rule, this package is the
first thing that stops publishing.

---

## Section 1 — `package env --format json`: the array with attribution

```sh
ocx --format json package env dojo/custom-tool:1.0.0 | jq '.integrations'
```

One row per `(package, namespace)` pair, carrying the **interpolated**
payload:

```json
[
  {
    "namespace": "sh.ocx.leaf",
    "package": "localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa75b…",
    "payload": {
      "declaredBy": "custom-leaf",
      "edge": "interface",
      "leafRoot": "/…/.ocx-home/packages/localhost_5000/sha256/93/445c2aa75b…/content"
    }
  },
  {
    "namespace": "com.jetbrains",
    "package": "localhost:5000/dojo/custom-tool:1.0.0@sha256:3b8ad3279d…",
    "payload": {
      "declaredBy": "custom-tool",
      "sdkPath": "/…/.ocx-home/packages/localhost_5000/sha256/93/445c2aa75b…/content",
      "plugins": [
        "sh.ocx.custom-tool"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-tool:1.0.0@sha256:3b8ad3279d…",
    "payload": {
      "declaredBy": "custom-tool",
      "settings": {
        "customTool.executable": "/…/sha256/3b/8ad3279d…/content/bin/custom-tool"
      },
      "extensions": [
        "ocx.custom-tool"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/patches/corp-ca-bundle:1.0.0@sha256:c46c22a69d…",
    "payload": {
      "declaredBy": "corp-ca-bundle",
      "settings": {
        "http.systemCertificates": true,
        "http.proxyStrictSSL": true,
        "customTool.caBundle": "/…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem"
      }
    }
  }
]
```

What each row proves:

- **`sh.ocx.leaf`** — declared by the *interface* dep, so it crosses to the
  consumer. `com.example.private`, declared by the *private* dep, is absent.
- **`com.jetbrains`** — `sdkPath` resolved from `${deps.leaf.installPath}` to
  **custom-leaf's** content dir, not custom-tool's. A digest-derived path no
  publisher could hand-write; this is the point of the feature.
- **`com.microsoft.vscode` (custom-tool)** — `${self.installPath}` resolved to
  the declaring package's own dir. Note `extensions` survived as an array and
  the booleans in the companion row survived as booleans: only **string
  leaves** are interpolated, so the payload's structure is invariant.
- **Two rows for one namespace** — the root's and the companion's, side by
  side, unmerged.

## Section 2 — plain output: the availability hint

```sh
ocx package env dojo/custom-tool:1.0.0
```

```
Key                  Type      Value
PATH                 path      /…/sha256/93/445c2aa75b…/content/bin
CUSTOM_LEAF_HOME     constant  /…/sha256/93/445c2aa75b…/content
PATH                 path      /…/sha256/93/445c2aa75b…/entrypoints
PATH                 path      /…/sha256/3b/8ad3279d…/content/bin
CUSTOM_TOOL_HOME     constant  /…/sha256/3b/8ad3279d…/content
PATH                 path      /…/sha256/3b/8ad3279d…/entrypoints
SSL_CERT_FILE        constant  /…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem
NODE_EXTRA_CA_CERTS  constant  /…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem
REQUESTS_CA_BUNDLE   constant  /…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem
2 binaries available (custom-leaf, custom-tool); 2 entrypoints available (custom-leaf, custom-tool); 3 integration namespaces (sh.ocx.leaf, com.jetbrains, com.microsoft.vscode); use --format json for the full list
```

The entries table is byte-stable — integrations add no column and no second
table. Availability is one hint line, and it counts and names **namespaces**,
so it dedupes: the four JSON rows above are three distinct namespaces here.
A hint reading `2 integration namespaces (com.microsoft.vscode,
com.microsoft.vscode)` would be false on its own terms.

## Section 3 — `--self` carries zero integrations

```sh
ocx --format json package env --self dojo/custom-tool:1.0.0 | jq '.integrations'
```

```
[]
```

Interface surface only, **at every depth**. This is a surface-level rule, not
a visibility one: no visibility value produces it, and `--self` composes zero
integrations even for the root's own declared namespaces.

> If you are looking for the private dep's namespace, `--self` is not where it
> shows up — nothing does, on the composed surface. Where it *is* visible as a
> declaration is `inspect --closure`; see section 7.

## Section 4 — the shell and CI channels carry no namespace

```sh
ocx package env --shell=bash  dojo/custom-tool:1.0.0
ocx package env --ci=gitlab   dojo/custom-tool:1.0.0
```

```
OK — no namespace key reaches --shell=bash
OK — no namespace key reaches --ci=gitlab
```

Both are env-only wire formats. An integrations payload is arbitrary JSON
with no env-var shape, so neither channel carries one; `--format json` is the
only path to the payload.

> The driver greps for the **exact** namespace keys, not a substring like
> `vscode`. A bare `vscode` matches the ambient `PATH` on any machine with VS
> Code installed, and the assertion then passes or fails for the wrong reason.

## Section 5 — two roots, one namespace: two rows, never merged

```sh
ocx --format json package env dojo/custom-tool:1.0.0 dojo/custom-other:1.0.0 \
    | jq '.integrations | map(select(.namespace == "com.microsoft.vscode"))'
```

```json
[
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-tool:1.0.0@sha256:3b8ad3279d…",
    "payload": {
      "declaredBy": "custom-tool",
      "settings": {
        "customTool.executable": "/…/sha256/3b/8ad3279d…/content/bin/custom-tool"
      },
      "extensions": [
        "ocx.custom-tool"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-other:1.0.0@sha256:bdcef7e814…",
    "payload": {
      "declaredBy": "custom-other",
      "settings": {
        "customOther.executable": "/…/sha256/bd/cef7e814…/content/bin/custom-other"
      },
      "extensions": [
        "ocx.custom-other"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/patches/corp-ca-bundle:1.0.0@sha256:c46c22a69d…",
    "payload": {
      "declaredBy": "corp-ca-bundle",
      "settings": {
        "http.systemCertificates": true,
        "http.proxyStrictSSL": true,
        "customTool.caBundle": "/…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem"
      }
    }
  }
]
```

Three declarations of one key, three rows, each carrying its own untouched
`payload` — no merge, no deep-merge of the two `settings` objects, no last-wins.
An array longer than the distinct-namespace count is the structural guarantee.

## Section 6 — the toolchain tier (`ocx.toml`)

```sh
cd test/manual/projects/integrations
ocx --format json env | jq '.integrations'
```

```json
[
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-other@sha256:bdcef7e814…",
    "payload": {
      "declaredBy": "custom-other",
      "settings": {
        "customOther.executable": "/…/sha256/bd/cef7e814…/content/bin/custom-other"
      },
      "extensions": [
        "ocx.custom-other"
      ]
    }
  },
  {
    "namespace": "sh.ocx.leaf",
    "package": "localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa7…",
    "payload": {
      "declaredBy": "custom-leaf",
      "edge": "interface",
      "leafRoot": "/…/sha256/93/445c2aa7…/content"
    }
  },
  {
    "namespace": "com.jetbrains",
    "package": "localhost:5000/dojo/custom-tool@sha256:3b8ad3279d…",
    "payload": {
      "declaredBy": "custom-tool",
      "sdkPath": "/…/sha256/93/445c2aa7…/content",
      "plugins": [
        "sh.ocx.custom-tool"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-tool@sha256:3b8ad3279d…",
    "payload": {
      "declaredBy": "custom-tool",
      "settings": {
        "customTool.executable": "/…/sha256/3b/8ad3279d…/content/bin/custom-tool"
      },
      "extensions": [
        "ocx.custom-tool"
      ]
    }
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/patches/corp-ca-bundle:1.0.0@sha256:c46c22a69d…",
    "payload": {
      "declaredBy": "corp-ca-bundle",
      "settings": {
        "http.systemCertificates": true,
        "http.proxyStrictSSL": true,
        "customTool.caBundle": "/…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem"
      }
    }
  }
]
```

```sh
ocx env      # plain
```

The tour prints the whole entries table; its last line is the hint:

```
3 binaries available (custom-other, custom-leaf, custom-tool); 4 entrypoints available (mytool, custom-other, custom-leaf, ...); 3 integration namespaces (com.microsoft.vscode, sh.ocx.leaf, com.jetbrains); use --format json for the full list
```

Same envelope as the OCI tier, keyed by the project's lock instead of raw
identifiers. Two details worth noticing:

- **Lock-pinned roots print without a tag** (`dojo/custom-tool@sha256:…`) while
  the transitively-reached dep keeps its advisory tag
  (`dojo/custom-leaf:1.0.0@sha256:…`). The lock pins a digest; the dep edge
  carries the publisher's tag.
- **Row order differs from the OCI tier** — the project composes bindings in
  its own order. Order is the admitted-set visit order, not a sort; do not
  build a consumer that depends on it across tiers.

## Section 7 — `inspect --closure`: declarations vs. what crosses

Three commands, three subtrees — each `jq` is a path into the envelope, so
every key below is a key ocx actually emits at that path:

```sh
ocx --format json package inspect --closure dojo/custom-tool:1.0.0 \
    | jq '.packages[].closure.deps'
```

```json
[
  {
    "name": "custom-leaf",
    "identifier": "localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa7…",
    "effective_visibility": "interface",
    "binaries": [
      "custom-leaf"
    ],
    "entrypoints": [
      "custom-leaf"
    ],
    "integrations": [
      "sh.ocx.leaf"
    ],
    "dependencies": []
  },
  {
    "name": "custom-private",
    "identifier": "localhost:5000/dojo/custom-private:1.0.0@sha256:9630835a8f…",
    "effective_visibility": "private",
    "binaries": [
      "custom-private"
    ],
    "entrypoints": [
      "custom-private"
    ],
    "integrations": [
      "com.example.private"
    ],
    "dependencies": []
  }
]
```

```sh
ocx --format json package inspect --closure dojo/custom-tool:1.0.0 \
    | jq '.packages[].closure.surface.interface.integrations'
```

```json
[
  {
    "namespace": "sh.ocx.leaf",
    "package": "localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa7…"
  },
  {
    "namespace": "com.jetbrains",
    "package": "localhost:5000/dojo/custom-tool:1.0.0@sha256:3b8ad3279d…"
  },
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/dojo/custom-tool:1.0.0@sha256:3b8ad3279d…"
  }
]
```

```sh
ocx --format json package inspect --closure dojo/custom-tool:1.0.0 \
    | jq '.packages[].closure.surface.private.integrations'
```

```json
[]
```

This is the one place the private dep's namespace is observable, and the
contrast is the whole point:

- `closure.deps[].integrations` is a plain array of the dep's **own declared
  namespace keys**, unfiltered — `com.example.private` is right there.
- `closure.surface.interface.integrations` is what actually **crosses** —
  `com.example.private` is gone.
- `closure.surface.private.integrations` is `[]`, the same surface rule as
  `--self`.

The closure envelope carries **no `payload`**: a closure node is not installed,
so `${installPath}` has no concrete payload yet. The key is `namespace`,
matching the flat envelope — never `name`, which is reserved for
PATH-resolving claims, so a consumer's `select(.namespace == "…")` can never
match a binary row by accident.

The plain tree renders the same split:

```
└── closure
    ├── deps
    │   ├── custom-leaf · localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa7… · interface
    │   └── custom-private · localhost:5000/dojo/custom-private:1.0.0@sha256:9630835a8f… · private
    └── surface
        ├── interface
        │   ├── binaries
        │   │   ├── custom-leaf · custom-leaf
        │   │   └── custom-tool · custom-tool
        │   ├── entrypoints
        │   │   ├── custom-leaf · custom-leaf
        │   │   └── custom-tool · custom-tool
        │   ├── env
        │   │   ├── PATH · path · custom-leaf
        │   │   ├── CUSTOM_LEAF_HOME · constant · custom-leaf
        │   │   ├── PATH · path · custom-tool
        │   │   └── CUSTOM_TOOL_HOME · constant · custom-tool
        │   └── integrations
        │       ├── sh.ocx.leaf · custom-leaf
        │       ├── com.jetbrains · custom-tool
        │       └── com.microsoft.vscode · custom-tool
        └── private
            ├── binaries
            │   ├── custom-private · custom-private
            │   └── custom-tool · custom-tool
            ├── entrypoints
            │   └── custom-private · custom-private
            └── env
                ├── PATH · path · custom-private
                ├── CUSTOM_PRIVATE_HOME · constant · custom-private
                ├── PATH · path · custom-tool
                └── CUSTOM_TOOL_HOME · constant · custom-tool
```

The `private` branch has **no `integrations` child at all** — an empty
projection renders no branch, while JSON still carries the key as `[]`.

## Section 8 — the patch tier: a companion contributes like a package

```sh
ocx --format json package env dojo/custom-tool:1.0.0 \
    | jq '.integrations | map(select(.package | contains("corp-ca-bundle")))'
```

```json
[
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/patches/corp-ca-bundle:1.0.0@sha256:c46c22a69d…",
    "payload": {
      "declaredBy": "corp-ca-bundle",
      "settings": {
        "http.systemCertificates": true,
        "http.proxyStrictSSL": true,
        "customTool.caBundle": "/…/sha256/c4/6c22a69d…/content/certs/corp-ca.pem"
      }
    }
  }
]
```

No package declares `corp-ca-bundle` as a dependency — a descriptor admits it.
It contributes integrations exactly like a package, attributed to its **own**
identifier, with no carrier-specific exception, and `${self.installPath}`
resolves against the companion's own content dir.

### 8b — the same composition with the patch tier opted out

**There is no `--no-patches` flag.** The opt-out is the project-tier
`[package."<id>"] no-patches = true`, so this is a toolchain-tier A/B between
two sibling project directories:

```sh
cd test/manual/projects/integrations-no-patches
ocx --format json env | jq '.integrations'
```

The same five-row array as section 6 minus its last row — four rows, each
still carrying its own `payload`:

```
com.microsoft.vscode  localhost:5000/dojo/custom-other@sha256:bdcef7e814…
sh.ocx.leaf           localhost:5000/dojo/custom-leaf:1.0.0@sha256:93445c2aa7…
com.jetbrains         localhost:5000/dojo/custom-tool@sha256:3b8ad3279d…
com.microsoft.vscode  localhost:5000/dojo/custom-tool@sha256:3b8ad3279d…
```

The companion's row is gone (and so are its `SSL_CERT_FILE` /
`NODE_EXTRA_CA_CERTS` / `REQUESTS_CA_BUNDLE` entries); the four
package-declared rows are untouched.

**Read `integrations-no-patches/ocx.toml` before you copy this pattern.** It
opts out **four** packages, not the three it binds — `custom-leaf` is in the
list even though nothing binds it directly. Two descriptors admit this
companion: a base-specific one on `dojo/custom-tool`, and `setup-patches.sh`'s
global `match: "*"` one, which attaches it to *every admitted package*
including transitive interface deps. The composition then dedupes the
companion's contribution by `(package, namespace)`, so **one un-opted-out
admitted package is enough to keep the row**. Opting out only the bindings
looks like it should work and does nothing observable.

`custom-private` is deliberately absent from the list: a private dep is not
admitted onto the consumer's surface, so no descriptor ever attaches to it.

## Section 9 — managed config: the patch tier by site policy

The corporate story, end to end: an operator publishes a `config.toml` package
carrying the `[patches]` pointer; a machine adopts it; the companion — and its
integrations — arrive because **site policy** says so, with no `[patches]`
in the machine's own config at all.

```sh
ocx config push -i localhost:5000/corp/ocx-config:1.0.0 site-config.toml
```

```
Identifier                            Digest                             Tags  Canonical Tags  Layers
localhost:5000/corp/ocx-config:1.0.0  sha256:aa4e4c5c1691ab…                    0               mounted=0,uploaded=1,verified=0
```

With the local `config.toml` stripped to a bare comment — no `[patches]`, no
`[managed]` — the companion is absent:

```
→ before adopting: no [patches] anywhere — the companion must be absent
companion rows: 0
```

Adopt the published config:

```sh
ocx config setup --managed-config localhost:5000/corp/ocx-config:1.0.0
```

```
Field           Value
Managed config  adopted (sha256:aa4e4c5c1691ab…)
```

The resulting `$OCX_HOME/config.toml` carries a *pointer*, never the policy:

```toml
# local config: NO [patches] section — the patch tier arrives by site policy.

# >>> ocx managed v1 69c1ddbb >>>
[managed]
source = "localhost:5000/corp/ocx-config:1.0.0"
required = true
refresh = "notify"
interval = "1d"

# <<< ocx managed <<<
```

The payload lands beside it, in
`$OCX_HOME/state/managed-config/{snapshot.json,config.toml}` — `snapshot.json`
is the metadata (source, tag, digest, fetch time) and `config.toml` is the
operator's file verbatim, `[managed]` stripped. And now:

```
→ after adopting: the companion contributes, on site policy alone
[
  {
    "namespace": "com.microsoft.vscode",
    "package": "localhost:5000/patches/corp-ca-bundle:1.0.0@sha256:c46c22a69d…",
    "payload": { "declaredBy": "corp-ca-bundle", "settings": { … } }
  }
]
OK — 0 rows before adoption, 1 after; the managed tier is what supplied the patch registry
```

`ocx config setup --managed-config ""` clears the seed and deletes the
snapshot; the row goes away again. The 0-before / 1-after pair is what makes
this section evidence rather than a screenshot — the green state is reachable
only because the red one was demonstrated on the same machine, seconds apart.

---

## Things the rig taught us (read before debugging a surprise)

### The global patch descriptor is a registry-wide singleton — last writer wins

`ocx patch publish --global` writes to the reserved
`<patch-registry>/global:__ocx.patch`. That path is **one slot per registry**,
not one per publisher. During development of this rig, a concurrent `task test`
run against the same `localhost:5000` published its own global descriptor and
silently replaced ours; `ocx patch why dojo/custom-tool:1.0.0` then answered
with a stranger's companion:

```
Variable          Rule  Companion
MANAGED_PATCH_CA  *     localhost:5000/t_7f9f1b97_test_setup_refresh_syncs_patch_descripto_companion:1.0.0
```

Nothing warns; the composition just changes underneath you. This is why
`setup-integrations.sh` binds its companion with a **base-specific**
descriptor on `dojo/custom-tool` instead of relying on the global slot — a
base-specific descriptor is addressed by the base's own repository and cannot
collide that way.

If a section suddenly shows an unfamiliar companion, `ocx patch why <base>` is
the first command to run, and re-running `setup-integrations.sh` republishes
the rig's descriptors.

### A bare `ocx patch sync` fails on a single-platform rig

Every package here is published for one platform. A bare `patch sync` fans out
over the concrete ship matrix and fails the whole sync on the platforms that
were never pushed:

```
ERROR failed to resolve package: required companion install failed for
'localhost:5000/patches/corp-ca-bundle:1.0.0': package not found
```

Pass `-p linux/amd64` (the setup script does).

### Install-time discovery does not re-fire on an already-installed root

Publishing a new descriptor and re-running `ocx package install` changes
nothing if the root is already installed — the base keeps composing the
descriptor it cached at first install. `ocx patch sync -p <platform>` is what
refreshes it.

### `no-patches` is per admitted package, not per binding

See section 8b. The failure mode is quiet: the opt-out parses, `ocx status`
reports it under `package_settings`, and the companion still shows up.

---

## File map

```
test/manual/
  INTEGRATIONS.md                       # this file
  scripts/
    setup-integrations.sh               # publish + install + lock (idempotent)
    show-integrations.sh                # the nine-section guided tour
    teardown-integrations.sh            # remove OCX_HOME + build artifacts + locks
  packages/
    custom-leaf/metadata.in.json          # ns sh.ocx.leaf         (interface dep)
    custom-private/metadata.in.json       # ns com.example.private (private dep)
    custom-tool/metadata.in.json          # ns com.microsoft.vscode + com.jetbrains (root)
    custom-other/metadata.in.json         # ns com.microsoft.vscode (second root)
    patches/
      corp-ca-bundle/metadata.in.json     # companion; gained an integrations block
      descriptors/
        integrations-companion.json     # binds corp-ca-bundle to dojo/custom-tool
  projects/
    integrations/ocx.toml               # toolchain tier, patch tier active
    integrations-no-patches/ocx.toml    # same bindings, patch tier opted out
  managed-config/                          # staged site-config.toml (gitignored)
```

The `build/`, `out/`, `metadata.json`, `ocx.lock` and `managed-config/`
artifacts are generated and gitignored; `metadata.in.json`, the descriptor and
the two `ocx.toml` files are the committed source of truth.

## Where this couples to the rest of the rig

- [`PATCHES.md`](PATCHES.md) — the patch tier this rig reuses. `corp-ca-bundle`
  is shared: its `integrations` block is additive and changes nothing in the
  patches walkthrough.
- [`README.md`](README.md) — the `dojo/` package catalogue and the base
  scenario tour.
