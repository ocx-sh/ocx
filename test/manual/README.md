# `test/manual/` — hands-on testing rig for `ocx`

A curated personal scratch space for exercising the new `ocx package test`
command and the multi-layer / entrypoints / dependencies surface end-to-end
against the local `registry:2` from `test/docker-compose.yml`. Everything
here is committed and reproducible — `scripts/bootstrap.sh` rebuilds the
registry state from scratch.

This dir does **not** ship to end users; it complements the auto-tested
shell scenarios under `test/scenarios/` (which double as acceptance-test
input via `tests/test_scenarios_smoke.py`).

## Contents

```
test/manual/
├── README.md                 ← this file
├── packages/                 ← source trees, one per package shape
├── scripts/
│   ├── env.sh                ← source to point at localhost:5000
│   ├── bootstrap.sh          ← idempotent build + push of every package
│   └── teardown.sh           ← rm -rf $OCX_HOME (with confirm)
└── adversarial/
    └── README.md             ← test-implementer vs loophole-searcher process
```

## Prerequisites

1. Build a release binary the manual scripts can use (from the repo root):
   ```sh
   cargo build --release -p ocx
   cp target/release/ocx test/bin/ocx
   ```
2. Start the local test registry once:
   ```sh
   cd test && docker compose up -d        # starts registry:2 on 5000
   ```

## Configure the shell

```sh
source test/manual/scripts/env.sh
```

Sets `OCX_DEFAULT_REGISTRY=localhost:5000`, `OCX_INSECURE_REGISTRIES=localhost:5000`,
and a disposable `OCX_HOME=test/manual/.ocx-home/` (gitignored). Manual
experiments cannot accidentally clobber your daily `~/.ocx` or leak into
commits.

## Bootstrap the registry

```sh
test/manual/scripts/bootstrap.sh
```

The script publishes the packages below into the namespace
`dojo/<name>:1.0.0`, walking the dep graph in order. Every package ships a
committed `metadata.in.json` (the source of truth); bootstrap renders it
into a gitignored `metadata.json` — substituting `@@KEY@@` tokens with
upstream `<fq>@<digest>` for templated packages, or copying verbatim for
plain ones.

| Package | Layers | Entrypoints | Deps | Exercise |
|---|---|---|---|---|
| `dojo/single-layer-hello` | 1 | `hello` | — | smoke: `package test`, `package push`, `package exec` |
| `dojo/multi-layer-app` | 3 | `myapp` | — | multi-layer assembly, layer-reuse via digest refs |
| `dojo/multi-entry-toolkit` | 1 | `tool-a` … `tool-d` | — | entrypoint dedup; collision detection |
| `dojo/deps-leaf-a` | 1 | `leaf-a` | — | leaf for chains |
| `dojo/deps-leaf-b` | 1 | `leaf-b` | — | second leaf |
| `dojo/deps-mid` | 1 | `mid` | leaf-a (interface) | transitive surface gating |
| `dojo/deps-app` | 1 | `app` | mid (interface) + leaf-b (private) | surface visibility (`--self`) |
| `dojo/cross-layer-entrypoint` | 1 | `wrap-leaf-a` | leaf-a (interface) | `${deps.NAME.installPath}` template |
| `dojo/baked-args-demo` | 1 | `hello-script` | — | baked `args` with `${installPath}` interpolation; ships `content/scripts/hello.sh` (committed), no dep required |

---

## Scenario catalogue

Every block below assumes you sourced `env.sh` and ran `bootstrap.sh`.
Substitute `$NS` with `dojo` (the default namespace).

### `ocx package test` — single-layer happy path

```sh
cd test/manual/packages/single-layer-hello
ocx package create build -m metadata.json -o /tmp/hello-1.0.0.tar.xz
ocx package test -p linux/amd64 -m metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/single-layer-hello:1.0.0 \
    /tmp/hello-1.0.0.tar.xz -- hello
```
Expect a single line ending with `(HELLO_HOME=/.../packages/.../tmp_test/.../hello-1.0.0)`.

### `ocx package test --keep` — inspect the tempdir

```sh
ocx package test --keep -p linux/amd64 \
    -m test/manual/packages/single-layer-hello/metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/single-layer-hello:1.0.0 \
    /tmp/hello-1.0.0.tar.xz -- hello
```
The kept path is printed to stderr. `ls $OCX_HOME/temp/test/` to find it.

### `ocx package test --output <dir>`

```sh
mkdir -p /tmp/out && rmdir /tmp/out         # must be absent or empty
ocx package test --output /tmp/out \
    -p linux/amd64 \
    -m test/manual/packages/single-layer-hello/metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/single-layer-hello:1.0.0 \
    /tmp/hello-1.0.0.tar.xz -- hello
```
Inputs must live on the same filesystem as `$OCX_HOME/layers/` — hardlink
assembly has no fallback copy.

### `ocx package test --self` vs the default interface surface

```sh
ocx package test -p linux/amd64 \
    -m test/manual/packages/deps-app/metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/deps-app:1.0.0 \
    /tmp/deps-app-1.0.0.tar.xz -- app                  # interface surface
ocx package test --self -p linux/amd64 \
    -m test/manual/packages/deps-app/metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/deps-app:1.0.0 \
    /tmp/deps-app-1.0.0.tar.xz -- app                  # private surface
```
The first run prints `MID_HOME` + `LEAF_A_HOME`. The second adds `LEAF_B_HOME`
because the leaf-b dep is private (`--self` exposes the consumer-private surface).

### `ocx package test --clean` — strip ambient env

```sh
SOMEVAR=present ocx package test --clean -p linux/amd64 \
    -m test/manual/packages/single-layer-hello/metadata.json \
    -i $OCX_DEFAULT_REGISTRY/dojo/single-layer-hello:1.0.0 \
    /tmp/hello-1.0.0.tar.xz -- env
```
Look for the absence of `SOMEVAR=present` in the printed env.

### Multi-layer push with digest reuse

```sh
# Bundle each layer.
m=test/manual/packages/multi-layer-app/metadata.json
ocx package create -m $m -o /tmp/base.tar.gz test/manual/packages/multi-layer-app/layer-base
ocx package create -m $m -o /tmp/libs.tar.gz test/manual/packages/multi-layer-app/layer-libs
ocx package create -m $m -o /tmp/app.tar.gz  test/manual/packages/multi-layer-app/layer-app

# Push v1 with three file layers.
fq=$OCX_DEFAULT_REGISTRY/dojo/multi-layer-app:1.0.0
ocx package push -n -p linux/amd64 -m $m -i $fq /tmp/base.tar.gz /tmp/libs.tar.gz /tmp/app.tar.gz

# Inspect the manifest to grab the base layer's digest.
base_digest=$(curl -fs http://$OCX_DEFAULT_REGISTRY/v2/dojo/multi-layer-app/manifests/1.0.0 \
    -H 'Accept: application/vnd.oci.image.manifest.v1+json' \
    | jq -r '.layers[0].digest')

# Re-push as 1.0.1 referencing the base layer by digest (no re-upload).
ocx package push -p linux/amd64 -m $m -i $OCX_DEFAULT_REGISTRY/dojo/multi-layer-app:1.0.1 \
    "${base_digest}.tar.gz" /tmp/libs.tar.gz /tmp/app.tar.gz
```

### Entrypoint collision

```sh
# Both packages declare the same entrypoint name → install of the second one
# fails with EntrypointCollision (exit 65, DataError).
ocx package install --select dojo/single-layer-hello:1.0.0
# Build a sibling package whose entrypoint is also named `hello` and try to
# install it — expected exit 65. (Use a fresh `metadata.json` declaring
# `{ "entrypoints": [{ "name": "hello", "target": "..." }] }`.)
```

### Dep visibility — interface vs private surface

```sh
ocx package install --select dojo/deps-app:1.0.0

# Interface surface: only mid + leaf-a env vars reach the consumer.
ocx package exec dojo/deps-app:1.0.0 -- env | grep _HOME

# Private surface (`--self` on `package exec` exposes private deps too).
ocx package exec --self dojo/deps-app:1.0.0 -- env | grep _HOME
```

### Cross-layer entrypoint via `${deps.NAME.installPath}`

```sh
ocx package install --select dojo/cross-layer-entrypoint:1.0.0
ocx package exec dojo/cross-layer-entrypoint:1.0.0 -- wrap-leaf-a
```
The launcher target resolves to leaf-a's binary even though
`cross-layer-entrypoint` ships nothing of its own.

### Offline reinstall after `rm -rf $OCX_HOME/packages`

```sh
ocx package install --select dojo/multi-layer-app:1.0.0
rm -rf $OCX_HOME/packages
ocx --offline package exec dojo/multi-layer-app:1.0.0 -- myapp
```
Re-assembles from `$OCX_HOME/blobs/` + `$OCX_HOME/layers/` without any
network round-trip. Fails clearly when the cache is also gone:

```sh
rm -rf $OCX_HOME/packages $OCX_HOME/blobs $OCX_HOME/layers
ocx --offline package exec dojo/multi-layer-app:1.0.0 -- myapp   # exits non-zero
```

### `ocx package deps` — inspect the dep graph

```sh
ocx package install --select dojo/deps-app:1.0.0

ocx package deps dojo/deps-app:1.0.0                  # tree, interface surface only
ocx package deps --self dojo/deps-app:1.0.0           # tree incl. private (leaf-b)
ocx package deps --flat dojo/deps-app:1.0.0           # resolved evaluation order
ocx package deps --depth 1 dojo/deps-app:1.0.0        # one level (mid only)

# "Why is leaf-a in the closure?" → because mid pulls it in.
ocx package deps --why dojo/deps-leaf-a dojo/deps-app:1.0.0
```
The `--why` output lists every parent edge in the resolved graph. With
`--self` the same query also surfaces `leaf-b` as a private dep of
`deps-app`; without it, `leaf-b` is hidden.

### `ocx package env` — resolved environment per surface

```sh
ocx package env dojo/deps-app:1.0.0           # APP_HOME, MID_HOME, LEAF_A_HOME, PATH
ocx package env --self dojo/deps-app:1.0.0    # adds LEAF_B_HOME (private)
ocx package env --candidate dojo/deps-app:1.0.0   # resolve via candidates/ symlink
ocx package env --current   dojo/deps-app     # resolve via current symlink
```
Mirrors `ocx package exec` env semantics — `--self` flips from interface to
private visibility. `--candidate`/`--current` swap content-store paths for
symlink paths so the output stays stable across re-installs.

### `ocx package which` — resolve the content directory

```sh
ocx package which dojo/single-layer-hello:1.0.0       # /<OCX_HOME>/packages/.../<digest>
ocx package which --candidate dojo/single-layer-hello:1.0.0
ocx package which --current   dojo/single-layer-hello
ocx package which -p linux/amd64 dojo/multi-layer-app:1.0.0
```
Useful inside scripts that need the on-disk path to the assembled package
(e.g. shelling out to a binary, or asserting layout in a test).

### `ocx package select` / `ocx package deselect` — pin the current version

```sh
ocx package install dojo/deps-leaf-a:1.0.0
ocx package select  dojo/deps-leaf-a:1.0.0       # promote to "current"
ocx package which --current dojo/deps-leaf-a     # follows the current symlink
ocx package deselect dojo/deps-leaf-a            # drop the current pointer
```
Selecting and deselecting only flips a symlink; the candidate stays
installed. To demo a/b switching, re-run bootstrap with `OCX_MANUAL_TAG=2.0.0`
first, then alternate between `package select dojo/leaf-a:1.0.0` and `:2.0.0`.

### `ocx package uninstall --purge` and `ocx clean`

```sh
ocx package install dojo/deps-leaf-a:1.0.0
ocx package uninstall --deselect dojo/deps-leaf-a:1.0.0   # drop candidate + current
ocx package uninstall --purge    dojo/deps-leaf-a:1.0.0   # also GC unreferenced blobs

ocx clean --dry-run    # list orphaned blobs/layers/packages
ocx clean              # actually remove them
```
`package uninstall` alone leaves blobs in the object store so a re-install
is cheap. `--purge` is the equivalent of `package uninstall` + `clean` for
that one package. Run `clean` periodically when many packages have been
churned.

### `ocx package env --ci` — emit env to a CI runner

```sh
ocx package install --select dojo/deps-app:1.0.0
ocx package env --ci=github dojo/deps-app:1.0.0           # writes $GITHUB_PATH / $GITHUB_ENV
ocx package env --self --ci=github dojo/deps-app:1.0.0    # private surface
```
The provider is auto-detected with a bare `--ci` when run inside a real CI;
pass `--ci=<provider>` (`github` or `gitlab`) explicitly when smoke-testing
locally. `--ci=github` appends tool dirs and `KEY=value` vars to
`$GITHUB_PATH` / `$GITHUB_ENV` so subsequent steps see resolved package envs
without sourcing anything; `--ci=gitlab` writes JSON-lines to `--export-file`.

## Where this couples to the automated suite

- `test/scenarios/` — the same shell flows, written as automated pytest
  cases via `tests/test_scenarios_smoke.py`. Edit one, port to the other.
- `test/src/scenarios/` — predefined `Scenario` subclasses publish a fresh
  prefix of throwaway packages per test; the manual-testing rig instead
  publishes the deterministic `dojo/...` namespace once and reuses it.
- The bug fix that made `--offline exec` after `rm packages/` work lives in
  `crates/ocx_lib/src/package_manager/tasks/{find_or_install,pull,pull_local}.rs`
  and is regression-tested in `tests/test_offline.py`.

## Adversarial review

When you hand-write a new scenario or extend an existing test, a paired
`Agent` invocation can challenge the test from a loophole-searcher angle.
Process and prompt templates: [`adversarial/README.md`](adversarial/README.md).
