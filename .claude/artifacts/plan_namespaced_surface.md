# Plan — one namespaced surface across docs, examples, and casts

## Status

- **State:** all layers applied. Drift suite 249 passed; recordings 40 passed; all 25 casts
  reassembled and free of flat identifiers. Nothing pushed.
- **Branch:** `chore/toolchain-hosted-index` (extends the toolchain migration)
- **Gates:** `task verify`, `task test:doc-scripts:drift`, `task recordings:build`, `task website:build`

## Why

The fleet rename made `ocx.sh/<upstream-org>/<tool>` the addressable form; a flat
`ocx.sh/<tool>` has no index root (`name_segments: 2`) and resolves only by falling
through to plain OCI. The repository's own toolchain moved in
[68f8eeda](https://github.com/ocx-sh/ocx/commit/68f8eeda), but every *reader-facing*
surface still shows the flat form: website prose, hand-written reference pages,
executed doc scripts, recorded casts, and Rust doc comments. Mixed forms teach the
wrong convention and the wrong shape for storage paths.

## Mapping table (decided — do not re-derive)

Verified against `ocx-sh/index` `p/**` roots on 2026-08-09.

| Shown as | Becomes |
|---|---|
| `cmake` | `kitware/cmake` |
| `uv` | `astral-sh/uv` |
| `bun` | `oven-sh/bun` |
| `nodejs` | `nodejs/node` |
| `corretto`, `java` | `amazon/corretto` |
| `python` | `astral-sh/python-build-standalone` — the real package publishes exactly one variant prefix, `slim`, so the variants demo uses `slim-3.13.14` against the unprefixed `3.13.14`. The old fixture invented `pgo.lto`, `debug`, and `freethreaded`, teaching a tag grammar no reader could reproduce. |
| `shellcheck` | `shellcheck/shellcheck` |
| `shfmt` | `shfmt/shfmt` |
| `lychee` | `lychee/lychee` |
| `go-task` | `go-task/task` |
| `ninja` | `ninja-build/ninja` |
| `jq` | `jqlang/jq` |
| `ripgrep` | `ripgrep/ripgrep` |
| `helm` | `helm/helm` |
| `gh` | `github/cli` |
| `opentofu` | `opentofu/opentofu` |
| `goreleaser` | `goreleaser/goreleaser` |
| `ocx` | `ocx/cli` (already) |
| narrative own-packages (`webapp`, `mytool`, `server`, `renderer`, `templates`) | `acme/<name>` — the convention docs already use (`acme/cmake`, `acme/mytool`, `acme/tools`) |
| `scenario:*` fixtures (`hello`, `leaf`, `left`, `right`, `app`, `mid`, `toolkit`, `multilayer`) | **unchanged** — every doc script declares a `setup:*` state, so these never reach a reader |
| `corp-ca` | `corp/ca` — `corp/*` already means corporate infra in these docs |

**Never rewrite:** a bare name in *binary* position (`ocx run -- cmake --version`),
digest-keyed store paths (`packages/ocx.sh/sha256/…`), `setup.ocx.sh/*` URLs, and
already-namespaced `ocx/cli` / `ocx/mirror`.

## Layers

Each layer ends at a gate; later layers assume earlier ones landed.

1. **Machinery** — display names must survive a `/`. `helpers.py:304` already does
   `.replace("/", "_")`; four sites do not: `test/src/scenarios/__init__.py:125,133`
   and `test/src/state_providers.py:161,167,312` (`PKG_<KEY>`, `HOME_KEY_<KEY>`, the
   rendered display-env token). `rewrite_command` (`test/recordings/cast_layer.py:39`)
   needs no change — it already skips the first word and everything after ` -- `, which
   is exactly the identifier-vs-binary-name split. Prove with one red→green case that a
   slashed display name yields `PKG_KITWARE_CMAKE`.
2. **Provisioned names** — `test/recordings/setups.py` display maps and
   `DECLARED_PACKAGES` in `test/src/state_providers.py`. DE6 cross-checks the two, so
   they move together or the suite reds.
3. **Doc scripts** — ~30 `.sh` under `website/src/_scripts/` plus any `# expect:`
   goldens. Gate: `task test:doc-scripts:drift`.
4. **Casts** — `task recordings:build` re-renders; CI regenerates on deploy.
5. **Website prose** — 16 files carry flat `ocx.sh/<tool>` identifiers (~85 hits) and
   10 carry bare-name identifiers in command position (~29 hits). Hand-written
   `reference/command-line.md` (4337 lines) is the largest single page and is **not**
   generated. Storage/symlink examples gain a segment in 18 spots.
6. **Convention prose** — `user-guide.md` §184 still asserts the old rule ("Mirrored
   upstream tools sit at the registry root under their common name: `cmake`,
   `shellcheck`, `uv`"). Rewrite, do not rename. Check `in-depth/indices.md` for the
   same claim.
7. **Rust surfaces** — doc comments and help text showing symlink paths:
   `self_group/activate.rs` (2), `setup/shims.rs` (6), `reference_manager.rs` (1),
   `ocx_shim/src/main.rs` (1).

## Out of scope

`website/src/public/data/catalog/**` (generated, gitignored), acceptance fixtures whose
names never reach a reader (`foo`, `bar`, `some-tool`), and `.claude/artifacts/*` — those
are dated records, not documentation.
