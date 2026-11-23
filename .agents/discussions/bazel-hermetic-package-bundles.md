# Discussion: Package bundles (export/import) and RBE-compatible Bazel integration

State: handed-off → architect · Updated: 2026-11-23
Ratified: 2026-11-23 → architect (earlier: 2026-11-18 → architect, reopened 2026-11-22)
Confidence: ratified by the owner at the restate on 2026-11-23 (both tracks → `/hex-architect`, Track 1 first as a rework of `.claude/artifacts/adr_package_export_import.md`; Track 2 unchanged since its ratification on 2026-11-18). Track 1 runs as a goal loop: `.agents/goals/adr-package-export-import.md`. Research vintage 2026-11-17, 2026-11-18 and 2026-11-22, eleven files under `.agents/research/`. Lines marked "working position" were not ruled on one by one. Three Track 2 spikes and five Track 1 checks are carried under Open questions.

## Intent

Reopened on 2026-11-17. The first discussion under this slug was lost (gitignored, never committed); its output survives as `.claude/artifacts/adr_package_export_import.md` (Proposed, revised after review round 1) and `.claude/artifacts/system_design_package_export_import.md`.

Two targets, in the owner's words: "import/export and fully integrated RBE compatible with very good cache capabilities in bazel".

1. **Package export and import.** `ocx package export` / `ocx package import`. The ADR's format decision was reversed on 2026-11-22; the current design is the block "Track 1 as it stands on 2026-11-22" under Decisions, which also adds blob directories and an opt-in cache directory.
2. **Bazel integration.** rules_ocx works under remote build execution on any REAPI backend, with strong cache reuse.

**Why now.** ocx-sh/ocx#23 was closed with "requires dedicated import/export command", and nothing from the ADR is built. rules_ocx 0.5.1 states remote execution as a non-goal (`rules_ocx:ocx/MODULE.bazel:135-137`).

**Out of scope.**

- The `fetch_plan` report and any design in which Bazel's downloader fetches package blobs.
- Layer extraction in Starlark.
- Features of a single REAPI vendor, the Remote Asset API included.
- Remote execution for ocx's own build.
- A Mach-O signer inside ocx, and any signing off macOS.
- Flattening symlinks in a fetched tree.
- Signature verification inside Bazel, and the rules_ocx `policy` tag class.
- Bazel older than 9, the rules_ocx `bins` and shared-`OCX_HOME` modes, and lazy shims under Bazel.
- Any change to the `dist.json` format.
- Vendor property names inside rules_ocx.
- Running `ocx` inside actions that use a materialised tool.

## Requirements

Provisional, from the owner's intake answers on 2026-11-17.

- rules_ocx must work on any REAPI backend. No design may depend on one vendor's feature, such as a remote asset service.
- A build started on one platform must be able to use ocx tools on executors of another platform. The tool platform comes from Bazel's execution platform, never from the host.
- Actions that use a materialised tool must not need `ocx`. `ocx` is a fetch-time tool only: launchers are static and never call back into `ocx`.
- Executors may have no `sh` or `bash`. Nothing rules_ocx generates for an executor may be a shell script.
- A package may target a platform on which `ocx` cannot run and for which it has no launcher. Such a package must still be usable as a tool or as data.
- The `ocx` bootstrap must serve both materialisers.
- Under remote execution, apart from analysis, a build does no local work and sends no traffic from the client to the cluster (owner, 2026-11-18).
- Under Bazel, signatures are verified once, when a digest enters `ocx.lock`. After that the digest pin carries the trust; Bazel checks digests only.

## Decisions

- **Hand-off is split in two.** Export/import goes to `/hex-plan`; the owner has ruled on all four of the ADR's changed decisions (1 and 2 ratified, 3 settled, 4 reversed by the root `ocx export`). RBE Bazel gets its own ADR through `/hex-architect`. Before the split, this discussion must fix what Bazel needs from the bundle layout, because that layout freezes when export/import ships.
- **No extra research lanes for now.** The entry wave's two lanes are the evidence base.
- **Cache bar: zero registry bytes.** With a warm remote cache, a fresh runner fetches no package bytes from the registry and extracts nothing.
- **Two materialisers over one core.** One ocx command materialises digest D for platform P into a directory, from a registry or from a bundle. Two thin rules_ocx wrappers call it. This replaces the ADR's materialiser C.
  - **Build action, keyed by the package digest.** The default for tools that actions use. It meets the cache bar today on any REAPI backend.
  - **Repository rule.** For tools that another repository rule or module extension runs, for rules that need a label per file, and for `bazel fetch` and `bazel vendor`. It is written reproducible (no absolute links, no links across repositories), so Bazel's remote repo contents cache can share it once that leaves experimental.
- **The "every byte comes through Bazel's downloader" rule is relaxed.** ocx may fetch inside an action, verified by digest. Offline and air-gapped builds feed the same action a bundle directory from `ocx package export`. Bazel's download policy flags no longer govern package fetches.
- **`ocx` becomes a registered Bazel toolchain**, one binary per execution platform, resolved by execution platform. It is used at fetch time only.
- **Three platforms are kept apart** (working position, 2026-11-18): the fetch platform, where `ocx` runs; the tool platform, which the package bytes are for; and the host. Extraction is platform-neutral, so `ocx` on any fetch platform can materialise a tree for any tool platform. Narrowed later on 2026-11-18: the fetch platform equals the tool platform whenever `ocx` exists for it; they differ only for a platform `ocx` is not built for.
- **The launcher is data, not a file** (working position, 2026-11-18; replaces "rules_ocx renders static launchers").
  - The core command writes a content tree and no launcher or entry-point script.
  - rules_ocx exposes environment entries and entry-point paths as a provider. A consuming rule passes them to `ctx.actions.run(executable = …, env = …)`, so Bazel sets the environment on the executor. This needs no shell, no `ocx` and no launcher binary, on any platform.
  - Environment paths in the provider are relative to the execution root. The launcher's environment-wrapper mode, decided later, gives absolute paths.
- **A native launcher serves generic executable labels** (owner, 2026-11-18). A label carries no environment, and Bazel cannot hand out one file from inside a directory output. So for `genrule` tools and third-party `attr.label(executable = True)`, rules_ocx hands out a small prebuilt static binary, copied under the entry point's name. It sets the environment with absolute paths and execs the real binary. It is in the first release, because rules_ocx 0.5.1 already exposes executable targets. It may be built for more platforms than `ocx`. A platform with no launcher build gets the provider only. `crates/ocx_shim` is the precedent.
- **`ocx` bootstrap** (working position, 2026-11-18): one sha256-pinned binary per platform, downloaded by Bazel's downloader from `rules_ocx:dist/dist.json`. The repository rule uses the host binary. The build action uses the fetch platform's binary through the toolchain. The Linux binary is static musl, so it needs no libc or shell on the executor.
- **Environment interface** (owner, 2026-11-18): ocx already reports environment values as interpolated placeholders. The Bazel path needs them unresolved, relative to a physical storage of all packages, dependencies included. Working shape: each path value is a package key plus a path inside that package, and rules_ocx maps each key to its tree. One tree per package digest, so a shared dependency is fetched once. This keeps the environment-segment part of the dropped `fetch_plan` report; only its blob-download part is gone.
- **One seed file, then `ocx` downloads the rest** (owner, 2026-11-18). Bazel has no built-in download action and a bare executor has no executable, so the first `ocx` binary cannot be fetched by an action there.
  - Default: Bazel's downloader fetches one pinned `ocx` file per execution platform. The cluster stores it once; a client uploads it only when the cluster lacks it.
  - From then on `ocx` runs as an action on the executor and downloads every package there. The launcher arrives with the seed.
  - Opt-in: a toolchain setting uses an `ocx` installed in the executor image. That removes the seed, and Bazel then does not check the `ocx` version.
- **The user chooses the materialiser by label** (owner, 2026-11-18). Both variants exist for every tool. Bazel fetches a repository only when something references it, so the unused variant costs nothing. Working sketch:
  - `ocx.toolchain(version, dist_manifest)` replaces `ocx.download` and pins the `ocx` release. The launcher comes embedded in that `ocx`.
  - `ocx.project(name, ocx_toml, ocx_lock, config, bundle)` keeps its shape. `config` is an ocx config file for the executor, such as mirrors. `bundle` is the offline source.
  - `@tools//:cmake` is the executable label: the native launcher with the fetch action behind it.
  - `@tools//cmake:package` carries the provider: tree, environment and entry points.
  - `@tools.cmake.files//:<path>` is the repository-rule variant with one label per file.
  - The platform is chosen by `select()` on the execution platform.
- **The fetch is an ordinary action** (owner, 2026-11-18). It runs wherever Bazel executes actions: on a remote executor under remote execution, on the client otherwise. It is not pinned to the client, because routing bytes from the registry through the client to the cache is slow. Executors therefore need network access to the registry or a mirror. This also removes the earlier question of forcing a host-compatible `ocx`: the fetch uses the execution platform's `ocx` from the toolchain.
- **Metadata is cached in Bazel's lock file, keyed by digest** (owner, 2026-11-18). The digest already fixes environment, entry points and dependencies, so nothing new is locked. Bazel only needs the bytes of the config blob before the build starts. The module extension stores them as facts in `MODULE.bazel.lock`, one entry per package digest, and a warm build reads no registry. Patches are the exception: they are not covered by the package digest.
- **An air-gapped workspace hands the bundle to rules_ocx as a label**: a directory in the repository or any external repository, including an archive Bazel fetches. A `bazel run` target produces the bundle from `ocx.lock` by calling `ocx package export` for every locked digest and platform.
- **Private-registry credentials come from the executor** (owner, 2026-11-18). rules_ocx never transports secrets. The executor has its own identity, or an ocx config file given as a label points the fetch at an internal mirror that needs no login.
- **Only pinned patches apply under Bazel** (owner, 2026-11-18). The patch set is frozen and is an input to the build. Companions become extra packages in the closure. Unpinned site patches are ignored.
- **One pinned release manifest drives the bootstrap** (owner, 2026-11-18). The module extension reads the `dist_manifest` label and generates one pinned target per platform. The repository rule and the seed both draw from that fixed list. The manifest format stays unchanged.
- **Lazy shims are not supported under Bazel** (owner, 2026-11-18). The rules_ocx `bins` mode is dropped. Bazel runs the fetch action only when something needs the tool.
- **A root command `ocx export` bundles the whole locked project** (owner, 2026-11-18): every tool and platform in `ocx.lock`. `ocx package export` stays digest-only. This reverses the ADR's "no root `ocx export`" and settles its changed decision 4.
- **The shared-`OCX_HOME` mode of rules_ocx is dropped; the home stays a cache** (owner, 2026-11-18). The repository rule writes no link into `~/.ocx`. It may read `~/.ocx` as a download cache, verified by digest, and then writes its own self-contained tree.
- **The native launcher is the existing shim, made portable** (owner, 2026-11-18). `crates/ocx_shim` gains a POSIX half and a mode that sets the environment and execs the target directly, with no `ocx`. It ships with ocx for the same platform, because tools run on the platform the seed `ocx` runs on. An earlier position, separate launcher rows in `dist.json`, is withdrawn: rules_ocx `select_release` takes the first row that matches version and target, so a second kind of row would be picked by mistake. The owner's aim is one executable for every launcher use case on Windows, Linux and macOS, replacing the POSIX shell launchers too.
- **The fetch action yields one content tree per package digest and nothing else** (working position from research, 2026-11-18). `content/` holds no machine-specific data and no absolute link; `refs/`, `install.json` and `entrypoints/` do. So the core command writes a content tree, not an `OCX_HOME`.
- **rules_ocx writes the launcher's sidecar itself** (working position, 2026-11-18). At analysis time it knows each tree's runfiles path and the composed environment from the facts. It copies the launcher from the toolchain, writes the sidecar with `ctx.actions.write`, and puts the trees in runfiles. `ocx` is not involved.
- **ocx reports the composed environment with symbolic roots** (working position, 2026-11-18). ocx owns composition: visibility, dependency order, patches. It reports each path value as a package digest plus a path inside it. rules_ocx only joins. Reading the raw templates (`${installPath}`, `${deps.NAME.installPath}`) in Starlark would duplicate that logic.
- **Patches enter through `patches.snapshot.json`** (working position from research, 2026-11-18). `ocx patch freeze` writes it beside `ocx.lock`, with companions and descriptors pinned by digest. It becomes a label on the extension. Companions are ordinary packages, so they are extra trees, and a patch adds environment entries only. The facts are keyed by package digest plus the snapshot's sha256.
- **In bundle mode the fetch action takes exactly the blobs its package needs** (working position, 2026-11-18): the manifest, config and layer files of the bundle, as labels. A bundle change then re-runs only the packages it touches. This puts one requirement on the export/import work: `LayoutTransport` must serve a blob by digest without consulting `index.json`.
- **Launcher unification is part of the Bazel ADR** (owner, 2026-11-18). One ADR covers the portable shim, the direct-exec mode and the replacement of ocx's own POSIX shell launchers by the same executable. Known cost: the Bazel integration waits on a change that touches every ocx install.
- **Network for the fetch action is set on the extension** (owner, 2026-11-18). `ocx.project(...)` takes a dictionary of execution properties that rules_ocx applies only to its fetch actions. rules_ocx knows no vendor property names.
- **Offline remote execution uses ocx's existing mirror and managed configuration** (owner, 2026-11-18), not a new rules_ocx mechanism. Executors pull from an internal mirror. The fetch action gets its configuration by one of two existing routes: an explicit config file given as a label (`--config` / `OCX_CONFIG`), or the executor's own system tier (`/etc/ocx/config.toml`) and managed snapshot. Mirrors and credentials only change transport, and every byte is checked by digest, so the executor's own configuration need not be a build input. The bundle fills that mirror across the air gap, and the bundle label stays for local builds.
- **Signatures are verified when locking, not in Bazel** (owner, 2026-11-18). `ocx lock` and `ocx add` verify against the trust policy on the machine that writes `ocx.lock`, and the lock is reviewed like code. The fetch action checks digests only: no trust policy and no Sigstore traffic on executors. A cache hit is then as trustworthy as a fetch.
- **The unified launcher is embedded in `ocx` on every platform** (owner, 2026-11-18), as on Windows today, so installing ocx stays a single file. Bazel gets the launcher file from a small action in which `ocx` writes it out.
- **The new rules_ocx supports Bazel 9.x only** (owner, 2026-11-18).
- **Keeping relative symlinks is a stated backend requirement** (owner, 2026-11-18). rules_ocx ships a conformance test that a user runs against their cluster. Trees are never flattened, so a package is byte-identical to what ocx installs elsewhere.
- **The core command is a public verb under `ocx package`** (owner, 2026-11-18): it writes one package's content tree into a directory by digest, with no home state, and has a JSON report. Any backend may use it. Its name is the architect's call.
- **A floating tag is allowed only with a local index** (owner, 2026-11-18). The tag resolves from that index alone, as under `--frozen`, with no registry fallthrough. The tag is then a fixed input: the same index always gives the same digest. A tag with no index is an error. A digest from `ocx.lock` or an explicit digest needs no index.
- **The work runs on two parallel tracks** (owner, 2026-11-18).
  - Track 1 starts now: plan and build export/import.
  - Track 2: spikes, then the Bazel ADR, then the ocx side (direct-exec launcher mode, core verb, environment report), an ocx release, then rules_ocx against a registry. After that, bundle mode in rules_ocx, which needs export/import's layout reader. Last, ocx's own shell launchers switch to the unified executable.
- **`ocx lock` and `ocx add` gate the lock write on verification** (owner, 2026-11-18). They verify every leaf they are about to pin, for all platforms, before `ocx.lock` is written. A covered package that fails is not written. `--no-verify` stays the explicit opt-out. This is new ocx work: today the lock is written from manifests alone and verification runs later, on pull (`crates/ocx_cli/src/command/lock.rs:63,76,108-109`).
- **The fetch action runs on the package's own platform** (working position, 2026-11-18; the owner's statement "the pull and execution platform is probably always the same"). Whenever `ocx` exists for the package's platform, the fetch runs on an executor of that platform, which may be the client. macOS packages are then signed by Apple's `codesign` as today (`crates/ocx_store/src/codesign.rs:14-27`: per file, only Mach-O files whose signature does not verify, no-op off macOS), and Windows links are made natively. ocx gains no signer of its own. Only a package for a platform `ocx` cannot run on is fetched elsewhere, and such a platform is never macOS.
- **Launcher code size is a hard constraint** (owner, 2026-11-18). A size budget per launcher and in total is enforced by a CI gate, like the existing release limits, and the launcher is written for size. Measured today: the committed Windows shims are 340,992 bytes (x86_64) and 244,736 bytes (aarch64) raw, about 148 and 107 KiB at `xz -9`. The shim ADR's figure of about 130 KiB is stale.
- **Each `ocx` embeds the minimal launcher set** (owner, 2026-11-18): its own platform's launcher plus those for platforms `ocx` is not built for. It does not embed the launchers of the other `ocx` platforms, because the fetch runs on the package's own platform and never uses them. This replaces the owner's earlier answer "every `ocx` embeds every launcher target", which the owner withdrew as "may not even be the right decision".
- **The launcher has an environment-wrapper mode** (owner, 2026-11-18): it runs any command under the package's environment with absolute paths. A rule author sets the wrapper as the executable and passes the real command as arguments. The raw environment stays in the provider, named as relative to the execution root.
- **The repository-rule variant is one lazily fetched repository per package and platform** (owner, 2026-11-18), selected by the build's platform, so it is not host-only. Sharing between machines uses Bazel's own repository caches, with Bazel 9's remote repo contents cache as an opt-in while it is experimental. rules_ocx adds no mechanism of its own.
- **libc is a platform constraint** (owner, 2026-11-18). rules_ocx offers a libc constraint with the values glibc and musl, and glibc is the default. A platform that carries musl gets the musl leaf of `ocx.lock` (`linux/amd64+libc.musl`). Executors in one build may differ. ocx does not probe the executor. Whether an existing community constraint can be reused is unverified.
- **The rules_ocx test suite runs against Buildbarn plus one more self-hosted backend** (owner, 2026-11-18). Buildbarn's docker-compose setup is the strict reference: network off, no shell in the runner. A spike picks the second one, NativeLink or Buildfarm, by its symlink and network behaviour. The rig runs nightly in rules_ocx CI on Linux, not per pull request.
- **`ocx export` carries the frozen patch set when one exists** (owner, 2026-11-18). If `patches.snapshot.json` sits beside `ocx.lock`, the bundle also holds its companion packages and patch descriptors. Without a snapshot nothing patch-related is exported.
- **This repository moves to the new rules_ocx as the last step** (owner, 2026-11-18), for its local builds. It is the first real consumer. Remote execution for this repository stays out of scope, as `.claude/artifacts/adr_bazel_build_adoption.md` says.
- **The ADR's changed decision 3 is settled by the shape:** ocx extracts, never Starlark.
- **The `fetch_plan` report is dropped** from the export/import ADR, with its phase (`inspect --fetch-plan`, `Client::blob_url`, `bundle_path`, `bundle_files`). If the Bazel design needs a metadata report, its own ADR defines it.
- **The ADR's changed decision 1 is ratified:** export carries referrers by default, import stores them, and `--no-referrers` opts out. This serves an air-gapped install under a trust policy and has no effect on the Bazel graph.
- **The ADR's changed decision 2 is ratified:** import writes tag pins only for IDs named on the command line, only tags the home lacks, never a routing pointer.

### Reopened 2026-11-22: the bundle becomes an OCX data directory

The Track 1 hand-off to `/hex-plan` is withdrawn until this block is ratified. The Track 2 hand-off to `/hex-architect` stands, but its bundle mode follows this block.

- **The bundle is an OCX data directory, not an OCI image layout** (owner, 2026-11-22). It reuses the home's own layouts: `index/<source>/…` in the local-index wire grammar and `blobs/<registry>/<alg>/…/data` in the `BlobStore` layout. Its `blobs/` also holds the compressed layer archives, which the home never keeps. This reverses the ADR's bundle-format decision (option A, `oci-layout` + `index.json` + flat `blobs/<alg>/<hex>`), which never scored this option. Its `ref.name` grammar, the `sh.ocx.bundle.version` annotation and `LayoutTransport` fall away. It supersedes the "`LayoutTransport` must serve a blob by digest without consulting `index.json`" line above: a blob's path follows from registry and digest.
- **Reason one: registries must not share a namespace** (owner, 2026-11-22). The owner's security policy forbids overlap between registries. One flat blob pool lets a package of registry B be satisfied by a blob only registry A shipped. `blobs/<registry>/` is already split.
- **Reason two: one lookup chain for index and blobs** (owner, 2026-11-22). The index already chains `LocalIndex`, then one remote per namespace, and `--index` moves the local part. Metadata blobs are already read from the home `blobs/` first, then the source. The owner wants the same for package data, with file, HTTP and OCI sources possible later for blobs as for the index. First step: extra blob directories consulted after the home and before the network, every read checked by digest.
- **The home keeps no compressed layers** (owner, 2026-11-22). Holding them by default would store every package twice. A directory holds them only when the user asks for it explicitly. Measured on the development machine: `~/.ocx/blobs` 120 files, largest 1,760 bytes; `~/.ocx/layers` 4.2 GB.
- **The same directory is the cache unit** (owner, 2026-11-22). `index/` plus `blobs/` with layer archives depend on no host path and no volume. `layers/` and `packages/` are hard-linked and carry machine-specific state, so they are the wrong thing to cache. One scheme serves offline export/import and CI caching.
- **Working position: two modes of one mechanism.** A read-only directory is the export case. A read-write directory is the cache case: a pull that reaches the network also writes the layer archive there. Import then reduces to a pull with the directory attached, and export to filling a directory for a named set without installing.
- **The cache directory fills during a normal pull, and only when configured** (owner, 2026-11-22). It is opt-in, aimed at CI. A default install keeps no archives, because that doubles disk use for an ordinary user.
- **Export/import and the cache share one configuration surface** (owner, 2026-11-22). Open: its shape. The owner's sketch is a data-directory setting that defaults to the home, an ordered path-separated list queried as a chain, and a notation for read versus write and for the default write target. Two research lanes were dispatched on 2026-11-22: prior art in other tools, and the existing config and blob seams in this repository.
- **Configuration shape** (owner, 2026-11-22, "shape lgtm"): the home stays fixed as the first place read and the only install target; an ordered list of extra data directories is read after it and before the network; at most one cache directory is the write target, opt-in, and is read too. No per-entry read/write notation. Environment form: a `PathList` like `OCX_CONSENT_PATHS`. Names are open.
- **Work the shape implies** (from recon, 2026-11-22): `BlobStore` needs a streaming read and write, because it handles whole blobs in memory today; an archive is written to a temporary file and renamed only after its digest checks; nothing prunes a cache directory.
- **The index must keep its single-authority guarantee** (owner, 2026-11-22). One index answers every tag, and a tag points at the same digest until someone moves it on purpose. The owner flagged that a "data directory" holding both `index/` and `blobs/` blurs this, and that either the name is wrong or the index part needs a second look.
- **Working position: only blobs chain; the index never does** (2026-11-22, not ruled on). A blob is the same bytes wherever it is found, so any number of directories may be searched. A pin is not: two indexes can disagree, and search order would then decide what a tag means. So the new list and the cache directory are blob settings and point at a `blobs/` directory. An export directory still holds `index/` and `blobs/`, attached through two settings: the existing `--index` / `OCX_INDEX` swap for pins, the new list for blobs. A cache directory never holds an index, so a stale cache cannot pin a tag. The name "data directory" is dropped.
- **Three command-line flags** (owner, 2026-11-22): one shorthand flag that attaches a whole bundle, one flag for blob directories and one for the cache directory, beside the config keys and environment variables. Names are open.
- **Working position: how the pieces work together** (2026-11-22, not ruled on).
  - Read order for any blob, in every command: the home, then the blob directories in listed order, then the cache directory, then the network. Blobs are content-addressed, so the order changes cost and never the result.
  - A normal pull writes metadata blobs to the home as today and extracts layers into the home. With a cache directory set, every blob it fetched from the network is also written there, metadata included, so a fresh machine with a restored cache needs no network. A hit in a blob directory is not copied into the cache.
  - A cache is best effort: a machine whose home is already warm fetches nothing and so adds nothing. Export is the complete form.
  - Export fills its output directory with every blob of the named set, through the same read order, and writes the pin snapshot into its `index/`. It extracts nothing. Layer archives cannot come from the home, which holds them only unpacked, so without a cache or blob directory export downloads them again.
  - Import is a pull with the bundle attached: metadata to the home `blobs/`, layers extracted into the home, archives copied nowhere. The bundle can then be removed.
  - The shorthand flag swaps the index to the bundle's `index/`, freezes resolution to it and adds its `blobs/` to the read list. It never writes into the bundle.
- [NEEDS CLARIFICATION: does a bundle carry a list of what it holds?]
  Recommended: not in the first release. Without it there is no "import everything" verb and a half-copied bundle is found only when a blob is missing at use.
- **The shorthand flag freezes resolution and conflicts with `--index`** (owner, 2026-11-22).
- **The word "bundle" is not used for this feature** (owner, 2026-11-22). It is taken: `"type":"bundle"` is the package metadata type (`Metadata::Bundle`), a wire format. The flag name, the directory's noun and this feature's title are open. "export" has one existing meaning too, the environment export of `ocx direnv export` and the shell export surface. Free words checked on 2026-11-22: `depot`, `offline-dir`, `kit`.
- **A blob directory and the cache hold every blob kind** (confirmed with the owner's question, 2026-11-22): image indexes, manifests, configs, referrers and layer archives. A network install writes metadata to the home and to the cache, the layer archive to the cache only, and the extracted layer to the home.
- **Owner leaning: no shorthand flag** (2026-11-22, "maybe it would be more plausible to just not have that flag"). A use that leaves the home index untouched makes results hard to predict. An export is then nothing more than a blob directory and a local index, attached with the two primitive settings. This withdraws the shorthand flag and its freeze rule above unless the owner brings it back.
- **Owner's model of import: a sync against an index that is stored locally** (2026-11-22), for everything in the export or for a named subset.
- **Finding: that mechanism exists** (2026-11-22). `.claude/artifacts/adr_servable_index_snapshot.md` (accepted 2026-08-09) makes a local index directory servable as a `file://` source (`crates/ocx_index/src/file_transport.rs`, `[registries."<ns>"] index = "file:///abs/path"`). `ocx index sync <REGISTRY>` and `ocx index update <pkg>` already copy pins from a source into the local index with defined merge rules: a refresh adopts every tag the source lists, keeps any tag only the local copy holds, and never deletes a durable pin. Neither downloads package content.
- **Working position: import is two existing steps in one** (2026-11-22, not ruled on). Pins: an index sync (everything) or an index update (named subset) with the export's `index/` as the source. Content: a pull with the export's `blobs/` as a blob directory, for the host's platform only by default. Other platforms' archives stay in the export.
- **Working position: a multi-platform export** (2026-11-22). The dispatch object is content-addressed and always lists every platform the publisher has. Blobs are present only for the exported platforms. A listed platform with no blobs is simply absent and needs no marker. Running export again into the same directory adds platforms.
- **The shorthand flag is dropped** (owner, 2026-11-22).
- **Import consumes the whole export** (owner, 2026-11-22). With no names given, every tag in the export is imported and the home index updated. After an import the tools must run with the export removed. Open: the ADR gives imported content pull semantics, so `ocx clean` removes it; whether import should install instead, which roots it against garbage collection.
- **Import does not install** (owner, 2026-11-22). Installing happens only when the user installs. Imported content keeps pull semantics, as the ADR already says: present, not rooted. Protection comes from what the user does next: `ocx install`, or a project whose `ocx.lock` pins the digests (`subsystem-file-structure.md`: lock pins of a registered project are garbage-collection roots). Owner's use case: an export that carries one project's whole setup, several groups, the same tool in several versions for regression tests. Finding: several versions would not collide as install candidates (`symlinks/<registry>/<repo>/candidates/<tag>`), only `current` is single; the decision stands on the first reason. Accepted cost: `ocx clean` after an import, with the export gone and no install or project, removes the content.
- **Finding: which commands root a project's pins.** A project becomes a garbage-collection root when it is registered in the `projects/` ledger. Two sites register: every `ocx.lock` write (`ocx lock`, `ocx add`, `ocx remove`, `ocx update`, `ocx upgrade`; `crates/ocx_project/src/lock.rs:707`, `mutation.rs:179`) and the toolchain render of a project `ocx pull` (`crates/ocx_cli/src/command/pull.rs:217`, `render_toolchain.rs:462-466`, skipped on a dry run). `ocx exec` does not register, on purpose: `crates/ocx_cli/src/app/project_context.rs:6` says the loaders never register, so that `exec` and `pull` loading stays cheap. So creating a package does not root it; on a receiving machine with a committed `ocx.lock`, one project `ocx pull` does.
- **Finding: `ocx exec` installs missing project tools and leaves them unrooted** (owner calls it a bug, 2026-11-22; read from code, not reproduced). `crates/ocx_cli/src/command/toolchain_exec.rs:179-193` composes with `Materialization::Install` and records `composed.pulled`, and nothing on that path registers the project. On a fresh clone with a committed `ocx.lock`, `ocx exec` alone pulls the tools and `ocx clean` then collects them. The stated reason in `project_context.rs:6` ("a flock, JSON read and rename" per call) describes the old JSON ledger; the ledger is now one symlink per project (`adr_project_gc_symlink_ledger.md`). Mechanism: materialization is one method on every path (`pull`, `crates/ocx_package_manager/src/tasks/find_or_install.rs:48`, `install.rs:22`), and it writes only a package's outgoing edges, never a root. A root is a second step with an owner: `create_install_symlinks` after the pull in `install` (`install.rs:24`), or the project ledger entry. `exec` runs the pull and neither second step. Owner's expectation (2026-11-22): materializing for a project must always leave the garbage-collection root in place. Separate from this feature: filed as [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759). It matters here only for import: a fix that registers only when `exec` pulled something would not fire after an import that already assembled the packages, so the fix should register whenever the project is not yet in the ledger.
- **Import is a pull with a given blob directory and index, plus the home index update** (owner, 2026-11-22: "these three may be equivalent"). So import assembles the packages, as a pull does, for the platforms asked for; the open entry below is settled toward extraction, and the compressed archives stay in the export. Import takes the same `--platform` option `ocx package pull` already has, default the host platform. Packages are keyed by digest, so several platforms can sit in one home. Consequence for [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759): a project command must register the project even when every package is already present, which the issue's scope already says ("fetch or use").
- **Platforms on export: no `--platform` means every platform** (owner, 2026-11-22: "no platform means every platform"). The owner's sentence was about export; the import default is still a working position. `--platform` is repeatable and there is no `all` keyword, the rule `ocx package copy` already has (`crates/ocx_cli/src/command/package_copy.rs:32-39`): omitted means every platform the source offers, given means a filter, and a digest identifier needs exactly one. Export follows the copy side (default every platform), import follows the pull side (default the host). Reason for the export default: an export is made for another machine, and a host-only default fails at import on a machine that cannot fetch the missing platform. Against: size. An `all` keyword would sit beside the existing `any` platform, which means platform-agnostic content. This reverses the ADR's "`-p` defaults to the host" for export; "exporting again appends a platform" stays.
- **Settled by the entry above: what import writes into the home.** Owner's model (2026-11-22): import copies the blobs into the home `blobs/`, extracts nothing, and the packages are created by whatever command needs them next. That conflicts with the earlier entry "The home keeps no compressed layers", because the layer archives would then sit in the home `blobs/` beside the later extracted copy. The other reading: import extracts the host platform's layers and assembles the packages, as a pull does, and leaves the archives in the export. Garbage collection is the same under both; neither roots anything.
- **Known cost: `oras` and `skopeo` cannot read such a bundle.** Filling a registry mirror needs an ocx command.
- **Import default platform: the host** (owner, 2026-11-22). Pins for every platform are imported; other platforms' content needs `--platform`.
- **Registry key: the registry of the identifier as written, never a mirror host** (owner, 2026-11-22). For a package an index serves, that is the logical name (`ocx.sh`). For a package named directly by its OCI registry, the identifier's registry is the physical address and that is the key. A mirror is only a re-translation of the physical address and never names a directory. This is what the home does today: bytes are fetched from the physical transport and "storage paths stay keyed on the logical" identifier (`crates/ocx_package_manager/src/tasks/pull.rs:856`).
- **No pruning of a cache directory in the first release** (owner, 2026-11-22). ocx never deletes from it; the CI system expires or replaces its caches, and the docs say so.
- **Follow-up: one cache directory shared by several runners, with eviction** (owner, 2026-11-22), filed as [ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760). Use case: one volume mounted by several CI runners or dev containers. The issue covers the stated concurrency guarantee, leftover temporary files, differing user IDs and network file systems, an eviction policy, and where that policy lives (in the cache directory or in each runner's config; the owner is undecided). **The split** (owner, 2026-11-23, "i agree with that split"): concurrent use of one cache directory is already safe in the first release, which the temp-file-then-rename write gives for a content-addressed store; eviction and the formally stated guarantee wait for the follow-up. Not checked: rename on network file systems, containers with different user IDs.

### Track 1 as it stands on 2026-11-22 (read this first)

The entries above are in the order they were said, and several were overtaken. This is the current state.

- **What an export is.** A directory with two parts in OCX's own layouts: `index/<source>/…` (a local index: tag pins and dispatch objects) and `blobs/<registry>/<alg>/…/data` (image indexes, manifests, configs, referrers and compressed layer archives). No OCI image layout, no manifest of contents, no feature-specific format.
- **Blob lookup, every command.** The home, then the configured blob directories in order, then the cache directory, then the network. Every read is checked by digest. Only blobs chain. The index never chains: one index answers every tag.
- **Settings.** An ordered list of read-only blob directories and at most one opt-in cache directory, each as a config key, an environment variable (`PathList` for the list) and a flag. No shorthand flag. An export is used in place with the existing `--index` plus the blob-directory setting.
- **Cache directory.** Opt-in. A pull that reaches the network also writes what it fetched there, metadata and layer archives. It never holds an index. Nothing prunes it. The home never keeps layer archives.
- **Export.** Fills the output directory for the named set through the same lookup order, writes the pin snapshot, extracts nothing. No `--platform` means every platform; `--platform` is repeatable and filters; a digest identifier needs exactly one. Running it again adds to the directory.
- **Import.** A pull with the export's `blobs/` attached, plus an update of the home index from the export's `index/` under the existing index refresh rules. No names means every tag in the export. Content is assembled for the host platform unless `--platform` says otherwise. Metadata blobs go to the home `blobs/`, layers are extracted, archives stay in the export. Afterwards the tools run with the export removed and the network blocked.
- **Garbage collection.** Import does not install and roots nothing. A project `ocx pull`, a lock write or `ocx install` protects the content; [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759) adds `ocx exec`.
- **Registries.** Each keeps its own `blobs/<registry>/` directory, keyed by the identifier's registry as written.
- **Not re-ruled on 2026-11-22, so unchanged from the ADR:** the command placement (`ocx package export` and `ocx package import` for named packages, a root command for a project's whole lock), referrers carried by default with `--no-referrers`, exit codes. Sketches in the room wrote `ocx export` and `ocx import` for short.
- **Open names:** the two flags, the config keys, the environment variables and the feature's noun. "bundle" is ruled out. Placeholders used in the room: `--blob-dir` / `OCX_BLOB_DIRS`, `--blob-cache` / `OCX_BLOB_CACHE`, `[blobs] dirs` and `cache`.

## Threads

- **Closed: cache bar, downloader rule, ocx toolchain, fetch plan, import pins.** See Decisions.
- **Closed on 2026-11-18: is a build action always the right place?** No. A build action's output does not exist in the loading phase. See "Two materialisers over one core" in Decisions.
- **Closed: bundles carry referrers.** See Decisions.

- **Closed on 2026-11-18: bootstrap, launcher delivery, lazy shims, embedded shims.** See Decisions. Facts read from code:
  - rules_ocx already takes a release manifest label: `dist_manifest`, default `//dist:dist.json` (`rules_ocx:ocx/extensions.bzl:23`), plus the `OCX_INSTALL_DIST_URL` override (`rules_ocx:ocx/private/download.bzl:36-41`). The manifest is `schema: 1` with one row per version and target, each with a sha256.
  - `crates/ocx_shim` is Windows-only and spawns `ocx`, so it cannot serve as the Bazel launcher unchanged. Its blobs are embedded in the ocx binary (`crates/ocx_store/src/shim.rs:18,25`).
  - A lazy shim is a deferred tool: a launcher on `PATH` whose content is fetched by `ocx` on first use (`crates/ocx_store/src/file_structure/shim_store.rs`, producer `prepare_lazy`).

- **Closed on 2026-11-18: launcher unification scope, network setting, offline remote execution.** See Decisions. The earlier answer "the shims `ocx` embeds today stay as they are" is superseded by the unification.

- **Closed on 2026-11-18 by experiment: lock-file facts on a fresh machine.** With `reproducible = True` and facts, Bazel 9.2.0 re-runs the extension on a fresh output root, hands it the committed facts, and the extension makes no registry call. `--lockfile_mode=error` passes. A new digest costs exactly one call. The control run with the lock deleted did call. Scope: `bazel mod deps` only; pruning of stale facts was not checked.
- **Open, owner on 2026-11-18: four platform limits are not acceptable as documented limits** and need a design answer: unsigned macOS data fetched off macOS (answered: fetch on the package's own platform); a launcher on only the platforms ocx is built for (answered: every `ocx` embeds all launcher targets); relative environment paths without the launcher (answered: wrapper mode); a repository-rule variant that is host-only and not shared (answered: per-platform repositories and Bazel's caches).
- **Closed on 2026-11-18: glibc versus musl, and the test backends.** See Decisions. Background: `ocx.lock` records one leaf per libc variant and ocx picks one on the host at use time (`crates/ocx_project/src/resolve.rs:577-617`).
- **Consequence, not asked: a Windows tree holds no links.** ocx refuses layer symlinks on Windows today (`.claude/rules/subsystem-file-structure.md`, assembly walker), so a Windows content tree is plain files and relocatable.
- **Consequence, not asked: the rules_ocx `policy` tag class loses its purpose** once signatures are verified at lock time and the fetch checks digests only.

## Research

- `.agents/research/research_bundle-rules-ocx-current-state.md` — codebase recon, 2026-11-17.
  - No Phase 1 or Phase 2 building block exists in `crates/`. No spike has run.
  - rules_ocx 0.5.1 states remote execution as a non-goal, selects the platform from the host, registers no toolchain and ships one host-selected `ocx`.
  - The ADR and system design contain no sentence on running under a remote executor.
- `.agents/research/research_ocx-lock-verification-libc.md` — codebase recon, 2026-11-18. No lock-time verification; lock pins leaf digests per platform and libc variant; the dependency closure is pinned inside each package, not in the lock.
- `.agents/research/research_bazel-facts-fresh-machine-experiment.md` — local experiment, 2026-11-18. Facts with a reproducible extension on a fresh output root.
- `.agents/research/research_reapi-backends-test-rig.md` — competitive scan, 2026-11-18. Buildbarn, BuildBuddy, Buildfarm, NativeLink, Bazel's reference worker. Mostly unverified on symlinks and shell-less images.
- `.agents/research/research_ocx-extraction-tree-env.md` — codebase recon, 2026-11-18. Extraction, tree shape, environment templates, entry points, closest existing primitives. It has no file counts.
- `.agents/research/research_ocx-patches-dist-shim.md` — codebase recon, 2026-11-18. Patch snapshot, `dist.json` consumers, the shim's sidecars.
- `.agents/research/research_reapi-symlinks-tooldata-network-facts.md` — fact check, 2026-11-18. Symlinks in tree artifacts, tool data in runfiles, executor network properties, extension facts.
- `.agents/research/research_bazel9-mechanics-fact-check.md` — fact check, 2026-11-18.
  - Module extensions can persist facts in `MODULE.bazel.lock`, meant for values a key fixes for ever. Reported as available from Bazel 8.5 by a secondary source.
  - Bazel has no built-in download action.
  - Nothing read says an execution tag changes platform or toolchain selection.
  - Bazel's credential helpers are documented for its own fetches and cache client, not for build actions.
- `.agents/research/research_bazel-rbe-prebuilt-tool-delivery.md` — prior-art web scan, 2026-11-17.
  - Repository rules run on the client. Their output is shared remotely only through the experimental remote repo contents cache.
  - Per-platform repositories plus `toolchain()` with `exec_compatible_with` is the established idiom.
  - Few servers implement the Remote Asset API.
  - bazelbuild/bazel#29660 (Authorization forwarded across redirects) is still open.
- `.agents/research/research_local-sources-cache-config-prior-art.md` (2026-11-22): how thirteen tools express source order, read-only versus read-write and the write target. Closest matches to the owner's idea are Git alternates and XDG (own store plus an ordered read-only list) and Go (the cache layout doubles as a `file://` source). Opt-in write targets: BuildKit `--cache-to`, Gradle `push`. Per-entry modes (ccache) leave order unspecified. Every writable cache surveyed had a growth problem.
- `.agents/research/research_ocx-config-and-blob-seams.md` (2026-11-22): `PathList` is an existing environment value kind; plain config lists replace across tiers; `OCX_INDEX` swaps the local index and never overlays; a `file://` index source exists; `BlobStore` reads and writes whole blobs in memory; the layer stream has one point where compressed bytes pass (`crates/ocx_oci/src/client.rs:921-923`); `ocx package copy` reads through `OciTransport` only; this repository's CI caches no ocx home.

## Related

- `.claude/artifacts/adr_package_export_import.md`, `.claude/artifacts/system_design_package_export_import.md`
- `.claude/artifacts/research_package_bundle_{recon,prior_art,oci_layout,competitive,archaeology,distro,launcher_bytes}.md`, `.claude/artifacts/research_bazel_downloader_oci_blobs.md`
- rules_ocx: https://github.com/ocx-sh/rules_ocx at e311b99 (0.5.1). No local checkout exists.
- ocx-sh/ocx#23 (closed: "requires dedicated import/export command"), ocx-sh/ocx#179 (lazy-mode store race)

## Files and interfaces

**Export/import (this repository).**

- Commands `ocx package export` and `ocx package import`, and the report roots `package_export` and `package_import`: `crates/ocx_cli/src/command/`, `crates/ocx_cli/src/api/data/`, `crates/ocx_schema/contract/`.
- Superseded on 2026-11-22: `LayoutTransport`. The by-digest, read-only source the Bazel core command pulls from is now a blob directory.
- Blob lookup chain and the cache write: `crates/ocx_index/src/chained_index.rs:771-826` (metadata chain today), `crates/ocx_store/` `BlobStore` (`blob_store.rs:54-58` path, `:74` and `:124-131` whole-blob read and write that need a streaming form), `crates/ocx_oci/src/client.rs:916-923` (layer network read and the one point compressed bytes pass), `crates/ocx_package_manager/src/tasks/pull.rs:856`.
- Settings: `crates/ocx_config/src/lib.rs` (new section), `crates/ocx_env/src/vars.rs` (two variables), `crates/ocx_cli/src/app/context.rs:309-350` and `context_options.rs` (two flags beside `--index`).
- Index side of import: `crates/ocx_index/src/file_transport.rs`, the `ocx index sync` and `ocx index update` merge rules, `adopt_pins` in `crates/ocx_index/src/local_index.rs`.
- Export and import tasks: `crates/ocx_package_manager/src/tasks/`.
- Referrer markers and their garbage-collection edge: `crates/ocx_store/`. Local-first referrer read: `crates/ocx_sign/`.
- Docs: `website/src/content/docs/guides/work-offline.mdx`, `website/src/content/docs/explanation/local-index.md`, `website/src/content/docs/reference/command-line/`.
- `.claude/artifacts/adr_package_export_import.md` and `.claude/artifacts/system_design_package_export_import.md` need an amendment: remove the fetch plan, Phase 2, Phase 3 and Decision 6, and record the ratifications above. Since 2026-11-22 they also need a rework: the format decision (option A) is reversed, the settings and the cache directory are new, and the export platform default changed.

**Bazel (new ADR).**

- A new public verb under `ocx package` for the core materialise step: digest and platform in, one content tree out, from a registry or a bundle. Its name and report are open. It is a CLI interface.
- A report of the composed environment with symbolic roots (package digest plus path). Whether it is a new report or a mode of an existing one is open.
- The root command `ocx export`, which also reads `patches.snapshot.json`: `crates/ocx_cli/src/command/`.
- The lock-write verification gate: `crates/ocx_cli/src/command/lock.rs`, `crates/ocx_project/src/resolve.rs`, `crates/ocx_package_manager/src/tasks/auto_verify.rs`, `crates/ocx_package_manager/src/tasks/signer_rounds.rs`.
- The unified launcher: `crates/ocx_shim/` (portable, direct-exec mode, environment-wrapper mode, its sidecar ABI), `crates/ocx_store/src/shim.rs` and `crates/ocx_store/src/shims/` (embedding), `crates/ocx_package_manager/src/launcher/body.rs` (the POSIX shell launchers it replaces), `.github/workflows/build-windows-shims.yml`, and a size limit beside `test/limits/`.
- This repository's `MODULE.bazel` and `tools` repository, when it moves to the new rules_ocx.
- rules_ocx (https://github.com/ocx-sh/rules_ocx): `ocx/private/package.bzl`, `ocx/private/project.bzl`, `ocx/private/download.bzl`, `ocx/private/extensions.bzl`, `ocx/private/repo_utils.bzl`, `ocx/private/platforms.bzl`, `ocx/MODULE.bazel`, `docs/`.
- This repository consumes rules_ocx 0.5.0 (`MODULE.bazel:25`).

## Open questions

- [NEEDS CLARIFICATION: Are the Mach-O bytes that the macOS ad-hoc signing step writes identical across machines?]
  Recommended: spike it. Everything else in `content/` is fixed by the archive: modes are masked, absolute links are refused, and Bazel ignores mtimes.
- [NEEDS CLARIFICATION: Which backends keep a relative symlink inside a tree when the tree is an input?]
  Recommended: the conformance test answers it per backend; run it first on two open-source backends with a package that depends on links, such as node. Bazel 9.2.0 uploads relative links as links; whether inputs keep them is unverified.
- [NEEDS CLARIFICATION: How does the launcher find its trees inside an action?]
  Recommended: look in `<launcher>.runfiles` first and fall back to the runfiles manifest, which Windows uses by default. Bazel sets no runfiles variable for actions. Prove it in a spike on Linux and Windows.
- Answered on 2026-11-22: the registry split follows the identifier's registry as written, never a mirror host; the index never chains. See Decisions.
- [NEEDS CLARIFICATION: how does import point the index refresh at the export's `index/` without a config edit?]
  Recommended: import builds the `file://` source in memory for the registries the export holds; no new user-facing setting.
- [NEEDS CLARIFICATION: can an export of a subset write a valid catalog file under `index/<source>/c/`?]
  Recommended: check the index format first; if a partial catalog is not valid, the export writes none and import enumerates `p/`.
- [NEEDS CLARIFICATION: can `ocx package copy` fill a mirror from a blob directory, and does one lookup seam serve both pull and copy?]
  Recommended: spike before the ADR rework. `copy` reads through the sealed `OciTransport` only (`crates/ocx_oci/src/copy.rs:116,382`).
- [NEEDS CLARIFICATION: how is a `PathList` split on Windows, where a path contains `:`?]
  Recommended: reuse the parser `OCX_CONSENT_PATHS` has; it was not read.
- [NEEDS CLARIFICATION: what are the names of the two flags, the config keys, the environment variables and the feature?]
  Recommended: the architect proposes one set; "bundle" is ruled out.
- [NEEDS CLARIFICATION: does anything assume every file under `blobs/` is small JSON (`ocx clean`, read caps)?]
  Recommended: check before layer archives are written into a home-shaped tree.

## Verification

Each check is shown red once and green once.

**Export/import.**

- Export a package, then import it into an empty home with no registry reachable. An install by tag succeeds for an ID named at import.
- A bundle with one changed byte is refused (exit 65).
- A bundle imported for one package writes no pin for another package.
- A package covered by a trust policy verifies offline from the carried referrers, and fails after an export with `--no-referrers`.
- `oras cp --from-oci-layout` reads the bundle. (Falls away with the 2026-11-22 format reversal.)

**Export/import, added 2026-11-22 (owner: these are acceptance tests).**

- Import into an empty home, delete the export, block the network. `ocx exec` on an imported package runs. This proves that import wrote the manifest, image index and config blobs into the home `blobs/`, not only the extracted layers.
- The same with a project: import, delete the export, `ocx exec` in a project whose `ocx.lock` pins the imported digests, `ocx clean`, then `ocx exec` again offline. It runs. Depends on [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759).
- Import with no install and no project, then `ocx clean`: the packages and their metadata blobs are both removed, and no metadata blob is left behind without its package.
- Import with no names writes a pin for every tag in the export; import with one name writes a pin only for it.
- Import with `--platform` for a platform the export holds assembles that platform's package; for a platform the export does not hold, with the network blocked, it fails and names the missing platform.
- An export that holds registries `a` and `b` with the same repository path keeps their blobs apart, and the import writes each under its own registry.
- Several pulls run at once into one empty cache directory. All succeed, each blob is present once and passes its digest check, and no temporary file is accepted as a blob.
- A pull with a cache directory set and a warm cache makes no registry request for a blob the cache holds, counted in the registry's access log.
- An export with no `--platform` holds the blobs of every platform the package ships; with two `--platform` flags it holds exactly those two.
- The interface gate passes: `cli.json`, report goldens, contract rows, `test/SUITE_FLOOR`.

**Bazel.**

- Two fresh output bases share one generic REAPI cache. The second build makes zero registry requests, counted in the registry's access log, and runs zero fetch actions.
- A build whose execution platform differs from the host resolves the execution platform's tool.
- The execution log of an action that uses a tool lists no `ocx` binary among its inputs.
- A tool runs on an executor image that has no shell, once through the provider and once through the native launcher as a `genrule` tool.
- A package for a platform with no `ocx` build is materialised and used as an action input.
- Fetch-action outputs from two machines have equal digests in the execution log.
- With the network blocked and a bundle as input, the build passes.
- Under remote execution with a warm cluster, a build from a fresh client uploads nothing to the cluster and runs no local action. The only client download is the seed `ocx` file.
- A private package is fetched on an executor with no secret in any action key or in the execution log.
- A repository rule runs a tool that the repository-rule materialiser provided.
- The conformance test passes on Buildbarn and on one more self-hosted backend, and fails on a backend configured to copy symlinks.
- A platform that carries the musl constraint gets the `+libc.musl` leaf, and the default platform gets the glibc leaf.
- A tool that changes its working directory still finds its files when run through the launcher's environment-wrapper mode, and fails without it.
- A floating tag with a local index resolves with no registry request; the same tag with no index is an error.
- This repository builds with the new rules_ocx.

**ocx side of the Bazel track.**

- `ocx lock` refuses to write a pin for a covered package whose signature does not verify, and writes it with a valid one. `--no-verify` writes it and warns.
- The launcher size gate fails when a launcher exceeds its budget.
- Every existing launcher acceptance test passes on Linux, macOS and Windows with the unified launcher, on a machine with no `/bin/sh` for the POSIX cases.
- `ocx export` of a project with a patch snapshot imports into an empty home offline, and the composed environment equals the connected one.
