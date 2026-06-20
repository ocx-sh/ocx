# Phase 6 Implementation Map — Patches Maintainer Commands, Tests, Docs, Manual Env

Grounding map for milestone #111 Phase 6 (issue #117). Verified against live code 2026-06-21.
Authoritative spec: `adr_infrastructure_patches.md` §"Patch maintainer workflow"; plan Phase 6.

Phase 6 adds two verbs to the existing `PatchGroup` (`Freeze`, `Sync` today) plus acceptance tests,
website docs with gate-tested CASTs, and a manual exploration environment.

## 1. `ocx patch publish` (push a `__ocx.patch` descriptor + companions)

Template to copy: `crates/ocx_cli/src/command/package_push.rs` (`PackagePush`, `execute` 86–143).

Reuse:
- `Publisher::{new, push, push_cascade}` (`crates/ocx_lib/src/publisher.rs:48/74/90`) for companion packages; `PushOutcome { manifest_digest, cascade_tags }`.
- `ManifestBuilder::{new, artifact_type, config_bytes, layers, build}` (`oci/manifest_builder.rs:73/77/102/117/136`).
- Patch media-type constants (Phase 2): `PATCH_MANIFEST_ARTIFACT_TYPE = "application/vnd.sh.ocx.patch.v1"`, `PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE = "application/vnd.sh.ocx.patch.descriptor.v1+json"` (`patch/descriptor.rs:80/87`); `MEDIA_TYPE_OCI_EMPTY_CONFIG` (`media_type.rs`).
- `Identifier::clone_with_tag(InternalTag::PATCH_TAG)` (`package/tag.rs:35`, `"__ocx.patch"`, auto-hidden) + `canonical_reference`; `Publisher::ensure_auth`.
- `PatchDescriptor::from_json_bytes` (`patch/descriptor.rs:234`) to validate authored JSON before push; `LayerRef: FromStr` (`publisher/layer_ref.rs:101`) for CLI layer args.

GAPS (new code):
- **`Client::push_patch_descriptor(identifier, descriptor_bytes)`** — clone the `push_description` body (`oci/client.rs:948–1024`): `clone_with_tag(PATCH_TAG)` → `ensure_auth(Push)` → `push_blob(empty config)` → `push_blob(layer payload)` → `ManifestBuilder…artifact_type(PATCH_MANIFEST_ARTIFACT_TYPE)…build()` → `push_manifest_raw`. `push_blob`/`push_manifest_raw` are private `Transport` methods (`oci/client/transport.rs:164/176`), only reachable from inside a `Client` method.
- `PatchDescriptor::to_json_bytes` helper (only `from_json_bytes` exists; `#[derive(Serialize)]` present so `serde_json::to_vec` works — add named helper for symmetry).
- `PatchPublishArgs` clap struct: `--descriptor-file`, root-global vs template sub-path target, `--companion <pkg> <archive>`, `--cascade`.

## 2. `ocx patch test` (validate descriptor + companion composes expected env)

Template: `crates/ocx_cli/src/command/package_test.rs` (`PackageTest`, `execute` 98–403):
`manager.pull_local` (materialize, no registry) → `install_info_from_package_root` → `manager.resolve_env(&[info], self_view)` → `env::Env::{clean,new}` + `apply_entries` + `apply_ocx_config` → `script::run_script` OR `child_process::spawn_and_wait`.

Reuse (~90%): the materialize→compose chain above; `script::run_script` (`script.rs:198`) + `expect.*` (`script/expect_module.rs`: `ok/eq/ne/true/false/contains/matches/fail`) + `ScriptOutcome`; `PatchDescriptor::{from_json_bytes, collect_companions}` to materialize companions from a local file.

GAP (the one genuinely new mechanism): **local-descriptor override seam.** `build_site_patch_set` (`resolve.rs:389`) reads only the persisted tag store + BlobStore (no in-memory override); `compose`/`resolve_env` take no descriptor arg; `OCX_PATCHES` carries config (registry/path/required) not descriptor bytes. `ocx patch test` must thread an `Option<local descriptor>` (e.g. `Option<HashMap<PinnedIdentifier, PatchDescriptor>>`) through `resolve_env → build_site_patch_set`, or scope a temporary patch state for the test's lifetime. ADR "local-descriptor override seam".
Also new: `PatchTestArgs` (`<base-id>`, `--descriptor-file`, `--script | -- cmd`).

NOTE: Phase 5D adds a `no_patches: &BTreeSet<String>` param to `resolve_env_with_patch_boundary` + `build_site_patch_set` — the descriptor-override param threads alongside it. Re-confirm signatures post-5D-merge before coding.

## 3. Acceptance tests — `test/tests/test_patches.py`

Harness: `test/docker-compose.yml` (`registry` registry:2 `5000`, `mirror-registry` `5001`); `cd test && uv run pytest`. Fixtures (`test/conftest.py`, `test/tests/conftest.py`): `registry` (session, auto compose-up, gated by `OCX_TESTS_NO_REGISTRY`), `ocx_home` (function tmp), `ocx` (`OcxRunner`), `unique_repo` (UUID, xdist-safe), `published_package`.
Helpers (`test/src/helpers.py`, `runner.py`): `make_package(ocx, repo, tag, tmp_path, *, new, cascade, bins=[]→env-only, env=[…], dependencies=…) -> PackageInfo`; `OcxRunner.{json,plain,run}`; `PackageInfo {repo, tag, short, fq, content_dir, marker}`.
Conventions (`subsystem-tests.md`): per-test tmp `ocx_home`; shared session `registry`; UUID repos; `assert_symlink_exists` (Windows junctions); v2 default env visibility = `private` (pass `visibility:"public"` for interface); env-only companion = `bins=[]`.
Three real-world scenarios (all via `make_package`, env-only):
- (a) corp-CA global: `env=[{"key":"SSL_CERT_FILE","type":"constant","value":"${installPath}/ca-bundle.crt","visibility":"public"}]`, at patch-registry root `__ocx.patch`.
- (b) JDK-truststore package-specific: companion at template sub-path; base dep `visibility:"private"` (private-surface tiering).
- (c) license-server `required=false`: `env=[{"key":"LICENSE_SERVER","required":False,…}]`, fail-open.
Run: `uv run pytest tests/test_patches.py -v --no-build`.

## 4. Docs + tested CAST convention

Docs root `website/src/docs/`. Cross-link: `reference/env-composition.md`, `reference/configuration.md` (has `[mirrors."<host>"]` + a RESERVED placeholder `### [patches] section {#future-patches}` at ~257 — Phase 6 fleshes it out), `reference/environment.md`, `reference/command-line.md`, `user-guide.md`. Sidebar: `website/.vitepress/config.mts` (~45–118) — register new pages.
DOC GAPS: `OCX_PATCHES` is **undocumented** in `environment.md` (only `OCX_PATCH_SNAPSHOT` at line 188 exists) — `quality-rust.md` checklist requires forwarded resolution vars documented; `configuration.md [patches]` still a placeholder.
CAST gate (one-tree convergence — see `project_doc_cast_two_tree_drift`): authored scripts in `test/doc_scripts/{slug}__{scenario}.sh` with headers `# doc: <nested/slug>`, `# cast: true`, `# region cast`/`# endregion cast`, `# state: setup:<name>`; output to `website/src/public/casts/<nested/slug>.cast` (nested-slug paths). Recorder/gate: `test/recordings/` (`cast_layer.py`, `cast_recorder.py`). Taskfile `website/recordings.taskfile.yml`: `recordings:build`, `recordings:parallel`, `recordings:gifs`. Embed: `<Terminal src="/casts/<slug>.cast" title="…" collapsed />` (`website/.vitepress/theme/components/Terminal.vue`). Rules: `subsystem-website.md`, `docs-style.md`.
Add a CAST: write `test/doc_scripts/patches__<scenario>.sh` (`# doc: user-guide/patches`, `# cast: true`, wrap demo in `# region cast`) → `task recordings:build` → embed `<Terminal>` → convergence gate keeps snippet↔cast aligned. Maintainer-loop cast (publish→sync→freeze) + consumer-loop cast (run / env --show-patches).

## 5. Manual exploration environment

Already exists: `test/manual/` with `packages/` (8 sample trees), `scripts/` (`env.sh`, `bootstrap.sh`, `teardown.sh`, `_lib.sh`), `ux/`, `adversarial/`. `test/manual/scripts/env.sh` sets `OCX_DEFAULT_REGISTRY=localhost:5000`, `OCX_INSECURE_REGISTRIES=localhost:5000`, disposable `OCX_HOME=test/manual/.ocx-home`. Registry: `cd test && docker compose up -d`.
Env-only companion metadata shape: `test/manual/packages/single-layer-hello/metadata.in.json`; env entry schema `package/metadata/env/var.rs` (`visibility: public|private|interface`).
Phase 6 deliverable: `test/manual/scripts/setup-patches.sh` + companion package dirs standing up the 3 use-cases (corp-CA global system-required / JDK truststore package-specific / license-server required=false) + a README walking maintainer (`ocx patch publish/sync/freeze`) and consumer (`ocx run`, `ocx env --show-patches`, zip→offline→unzip parity via disposable `OCX_HOME`) personas end-to-end. Uses `ocx patch publish` (once built) for descriptors.

## 6. Command registration + report pattern

`command.rs`: `Patch(patch::PatchGroup)` `#[command(subcommand)]` (96–98); dispatch `Command::Patch(g) => g.execute(context).await` (134).
`command/patch.rs`: `pub enum PatchGroup { Freeze, Sync }` — add `Publish(PatchPublishArgs)`, `Test(PatchTestArgs)` variants + arms + `impl …Args::execute(&self, context) -> anyhow::Result<ExitCode>`. clap doc-comment = `--help` about (`quality-cli-help.md`: user-contract only, ASCII, no internal refs).
Report: create `api/data/patch_publish.rs` + `patch_test.rs` (`#[derive(Serialize)] …Report` + `impl Printable`), register `pub mod …` in `api/data.rs`; report via `context.api().report(&…Report::new(...))`. `ocx patch test` exec branch may emit no report (like `run.rs`), reporting only on `--script`.

## Open design decisions for Phase 6 planning
- Descriptor-override seam shape (param vs scoped state) — coordinate with the 5D `no_patches` param already on `resolve_env_with_patch_boundary`/`build_site_patch_set`.
- `ocx patch publish` UX: single descriptor + companions in one invocation, or descriptor-only with companions pushed separately via `ocx package push`? ADR maintainer-workflow section is authority.
- Whether `companions_installed` count (TODO in `api/data/patch_sync.rs`) is closed here or left as polish.
