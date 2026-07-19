# Research: Mirror & Registry-Redirection Config Schemes — Cross-Ecosystem Survey

**Date:** 2026-07-18
**Domain:** oci, configuration, package-manager, infrastructure
**Provenance:** workflow `mirror-config-survey`, 3 parallel Sonnet-5 web-research agents (containers / lang-pkg / compound-scheme axes), 190,948 tokens, 44 tool calls, 2026-07-18.

## Direct Answer

Surveyed how 11 tools express "redirect this pull to somewhere else": containerd, podman/CRI-O, Docker (OCI/container registries); Cargo, Go modules, pip/uv, npm (language package indexes); pip-VCS + npm/Yarn git deps, Nix flake refs, Nix substituters, Helm/ORAS/Flux (URL-scheme conventions). Findings feed the OCX mirror-config design — see "Synthesis for OCX" below.

---

## Container / OCI Registries

### containerd (`hosts.toml` / `config_path`)

- **Mirror keying:** by the *original* registry host+port, as a directory: `<config_path>/<host>[:<port>]/hosts.toml`. Inside, each candidate mirror is its own `[host."<url>"]` table.
- **Config example:**
  ```toml
  server = "https://registry-1.docker.io"
  [host."http://192.168.31.250:5000"]
    capabilities = ["pull", "resolve"]
    skip_verify = true
  ```
- **Protocol/scheme markers:** none — `https://`/`http://` is embedded directly in the TOML table *key* for each mirror. No `sparse+`/`oci://`-style prefix exists in this format.
- **HTTP/insecure handling:** `http://` key = plaintext; `skip_verify = true` = TLS kept, cert trust skipped; `ca = "path"` = custom CA bundle — three orthogonal per-host knobs.
- **Fallback semantics:** ordered try-list of `[host.*]` entries; if all fail/miss, falls back to top-level `server` (true upstream). `push` only attempted against hosts whose `capabilities` include `push`.
- **Identity vs policy:** split by directory — WHICH host a file applies to is identity (path); WHERE/HOW to reach it (mirrors, TLS, capabilities) is policy (file contents). `server` is the fallback identity of last resort.
- **Sources:** https://github.com/containerd/containerd/blob/main/docs/hosts.md, https://containerd.io/docs/main/hosts/, https://github.com/containerd/containerd/blob/main/docs/cri/registry.md

### podman / CRI-O (`containers-registries.conf` v2)

- **Mirror keying:** by `prefix` (a repo-namespace pattern matched against the image ref) on a `[[registry]]` table; mirrors are child `[[registry.mirror]]` arrays under that prefix.
- **Config example:**
  ```toml
  unqualified-search-registries = ["example.com"]

  [[registry]]
  prefix = "example.com/foo"
  insecure = false
  blocked = false
  location = "internal-registry-for-example.com/bar"

  [[registry.mirror]]
  location = "example-mirror-0.local/mirror-for-foo"

  [[registry.mirror]]
  location = "example-mirror-1.local/mirrors/foo"
  insecure = true
  ```
- **Protocol/scheme markers:** none — `location`/`prefix` are bare `host[:port]/path` strings.
- **HTTP/insecure handling:** `insecure = true` (bool) on the registry is inherited by its mirrors but each mirror can independently override it — allows plain HTTP or untrusted TLS.
- **Fallback semantics:** ordered try-then-fall-through — first mirror that can be contacted AND has the image wins; only if none do, falls through to `location` (or the unmodified reference) tried last. `pull-from-mirror` further restricts which pull forms (digest/tag/all) may use a mirror.
- **Identity vs policy:** explicit — `prefix` = identity (what name the reference matches), `location` = where that identity's canonical bytes live, `registry.mirror` = policy layered on top. `insecure`/`blocked` attach to physical locations, not identities.
- **Sources:** https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md

### Docker Engine (`daemon.json`)

- **Mirror keying:** none — `registry-mirrors` is a flat, unkeyed list applied globally and **only to Docker Hub**. `insecure-registries` is a separate flat list keyed by `host[:port]`. No per-arbitrary-registry mirror table exists.
- **Config example:**
  ```json
  {
    "registry-mirrors": ["https://mirror.example.com"],
    "insecure-registries": ["my-registry.local:5000"]
  }
  ```
- **Protocol/scheme markers:** none — plain `https://`/`http://` URLs.
- **HTTP/insecure handling:** an `http://` entry in `registry-mirrors` talks plaintext to that mirror; `insecure-registries` is the general-purpose escape hatch for *any* host (not just mirrors) — the only way to mark a non-Hub registry insecure, since no per-mirror `insecure` field exists.
- **Fallback semantics:** `registry-mirrors` consulted in order as a Hub-only pull-through cache; not a general replace-any-registry mechanism. No digest/tag pull granularity.
- **Identity vs policy:** none — no named identity distinct from mirror policy; Docker Hub is implicit identity, `registry-mirrors`/`insecure-registries` are pure policy lists.
- **Sources:** https://docs.docker.com/reference/cli/dockerd/, https://docs.docker.com/docker-hub/image-library/mirror/

---

## Language Package Managers

### Cargo (crates.io, incl. sparse registries)

- **Mirror keying:** arbitrary named `[source.NAME]` / `[registries.NAME]` tables. `[source.crates-io].replace-with` points the well-known `crates-io` alias at `NAME`.
- **Config example:**
  ```toml
  [source.crates-io]
  replace-with = "my-mirror"

  [source.my-mirror]
  registry = "sparse+https://my-intranet:8080/index/"

  [registries.my-mirror]
  index = "sparse+https://my-intranet:8080/index/"
  ```
- **Protocol/scheme markers:** `sparse+` prefix on the index URL selects wire dialect — bare `https://…/.git` = legacy full git-clone-the-index; `sparse+https://` = sparse per-crate HTTP JSON fetch (default since 1.68). Both dialects share one string field, so the scheme doubles as the protocol tag.
- **HTTP/insecure handling:** no dedicated flag — `sparse+http://` works; plaintext is accepted purely by scheme choice, no cert-skip knob exists.
- **Fallback semantics:** pure replace, no fallback chain — `replace-with` swaps the source wholesale for the whole build; mirror down = build fails, Cargo never retries crates.io.
- **Identity vs policy:** clean split — `[registries.NAME]`/`[source.NAME]` declare what a source *is* (identity); `replace-with` is the policy switch. `Cargo.lock` only ever stores the identity name, never `sparse+`.
- **Sources:** https://doc.rust-lang.org/cargo/reference/source-replacement.html, https://doc.rust-lang.org/cargo/reference/registries.html, https://doc.rust-lang.org/cargo/reference/registry-index.html

### Go modules (`GOPROXY`)

- **Mirror keying:** none — flat, ordered proxy-URL list tried for every module path. Per-module routing instead comes from `GOPRIVATE`/`GONOPROXY`/`GONOSUMDB` globs matched against the import path.
- **Config example:** `GOPROXY=https://corp.example.com,https://proxy.golang.org,direct`
- **Protocol/scheme markers:** none — every entry is a plain `https://` URL to one fixed GOPROXY HTTP API; `direct`/`off` are bare keywords, not URLs.
- **HTTP/insecure handling:** `GOINSECURE=<glob>` skips TLS-cert AND checksum-db verification per module path (a trust downgrade); `GOSUMDB=off`/`GONOSUMDB=<glob>` disable just the checksum lookup; `GOPRIVATE=<glob>` sets both implicitly.
- **Fallback semantics:** ordered chain, narrow trigger — only HTTP 404/410 advances to the next entry; any other failure is terminal. `direct` as final entry = clone straight from the VCS host; `off` = hard fail.
- **Identity vs policy:** weak separation — `GOPROXY` is simultaneously the mirror-identity list AND the fallback-order policy. `GOPRIVATE` is the real policy-only knob (exemptions); it never names a mirror.
- **Sources:** https://go.dev/ref/mod#goproxy-protocol, https://go.dev/ref/mod#private-module-privacy, https://go.dev/ref/mod#goinsecure

### pip / uv (PyPI)

- **Mirror keying:** pip = role-only (one unnamed `index-url` + a flat unnamed `extra-index-url` list). uv upgrades to named `[[tool.uv.index]]` entries (`name` + `url` + `default`/`explicit` flags).
- **Config example:**
  ```ini
  # pip.conf
  [global]
  index-url = https://pypi.corp.example.com/simple
  extra-index-url = https://pypi.org/simple
  trusted-host = pypi.corp.example.com

  # uv pyproject.toml
  [[tool.uv.index]]
  name = "corp"
  url = "https://pypi.corp.example.com/simple"
  default = true
  ```
- **Protocol/scheme markers:** none — one PEP 503/691 "simple" wire format over bare `http(s)://`.
- **HTTP/insecure handling:** `trusted-host <host>` (pip) / `--allow-insecure-host` (uv) — a per-host allowlist skipping TLS-cert verification.
- **Fallback semantics:** pip's `extra-index-url` is **additive merge** — queries ALL indexes and pools matching versions (the classic dependency-confusion foot-gun). uv defaults to safer first-index-wins; pip-style pooling is opt-in via `--index-strategy unsafe-best-match`.
- **Identity vs policy:** pip barely separated (the URL *is* the policy); uv adds identity via `name`, with `default`/`explicit` as independent policy flags.
- **Sources:** https://docs.astral.sh/uv/concepts/indexes/, https://docs.astral.sh/uv/concepts/authentication/certificates/, https://pip.pypa.io/en/stable/topics/configuration/

### npm

- **Mirror keying:** by `@scope` segment of the package name, plus one unscoped default (`registry=`).
- **Config example:**
  ```ini
  registry=https://registry.npmjs.org/
  @myorg:registry=https://npm.corp.example.com/
  //npm.corp.example.com/:_authToken=${NPM_TOKEN}
  ```
- **Protocol/scheme markers:** none — plain `http(s)://` URL, single npm REST wire protocol.
- **HTTP/insecure handling:** no per-host trusted-host allowlist; `strict-ssl=false` disables cert verification globally (blunt, unscoped); `cafile`/`ca` for a private CA. Auth tokens are bound via URL-prefix keys, incidentally scoping credentials to a registry.
- **Fallback semantics:** pure replace per scope — a scoped package resolves against exactly one registry; if it's missing there, npm does not try another.
- **Identity vs policy:** not separated — `@scope:registry=<url>` is simultaneously identity and complete policy in one line.
- **Sources:** https://docs.npmjs.com/cli/v11/using-npm/registry/, https://docs.npmjs.com/cli/v11/configuring-npm/npmrc

---

## URL-Scheme Conventions

### VCS Git-URL family (pip-VCS requirements; npm/Yarn git dependencies)

- **Mirror keying:** none — no mirror table; the dependency string itself IS the location (one URL = one identity).
- **Config example:** `MyProject @ git+https://git.example.com/MyProject.git@v1.0` (pip) · `"fancy": "git+ssh://git@github.com:strzibny/fancy.git#desired-branch"` (npm/Yarn)
- **Protocol/scheme markers:** compound `<vcs>+<transport>` prefix — `git+https`, `git+ssh`, `git+file`, `git+git` (pip also: `hg+`, `svn+`, `bzr+` variants); npm/Yarn add bare `git://` and a `github:user/repo` shorthand that elides the scheme entirely.
- **HTTP/insecure handling:** `git+http://` / bare `git://` accepted with zero warning in both ecosystems — insecurity is opt-in purely via literal scheme choice, no separate flag.
- **Fallback semantics:** none — single hard-coded location per dependency; unrelated to each ecosystem's separate index-mirroring mechanism (pip `index-url`, npm `registry=`).
- **Identity vs policy:** no separation — the scheme token simultaneously picks VCS tool AND transport; no independent security flag.
- **Sources:** https://pip.pypa.io/en/stable/topics/vcs-support/, https://docs.npmjs.com/cli/v10/configuring-npm/package-json#git-urls-as-dependencies

### Nix — flake references

- **Mirror keying:** none at ref level (flake *registries* resolving short names like `nixpkgs` are a separate indirection layer).
- **Config example:** `git+https://example.org/my/repo?shallow=1`
- **Protocol/scheme markers:** `git+https`/`ssh`/`file`; `tarball+https`/`http`/`file` (droppable when the URL extension implies an archive format); scheme-free shorthands `github:owner/repo`, `sourcehut:~user/repo` imply their own fixed transport.
- **HTTP/insecure handling:** `tarball+http://` / `git+http://` accepted like pip/npm, no separate flag; extra params ride as query strings (`?shallow=1`), not CLI flags.
- **Fallback semantics:** none at ref level — single fixed source per input; `nix.conf` substituters (next) is the actual fallback mechanism.
- **Identity vs policy:** scheme fuses fetcher-type and transport, same pattern as pip/npm — no independent security flag.
- **Sources:** https://github.com/NixOS/nix/blob/master/src/nix/flake.md

### Nix — `nix.conf` substituters

- **Mirror keying:** global ordered/prioritized flat list, not keyed to a named upstream identity — closer to a priority-ordered pool than mirrors-of-a-source.
- **Config example:** `substituters = https://cache.nixos.org?priority=40 s3://my-cache?priority=10`
- **Protocol/scheme markers:** full scheme per entry — `https://`, `s3://`, `file://`, `ssh://` — heterogeneous transports coexist because each is fetched by a different store backend, selected purely from the scheme.
- **HTTP/insecure handling:** plain `http://` works with no extra flag (same scheme-signals-plaintext pattern as containerd/Docker); `?priority=N` tunes ordering only.
- **Fallback semantics:** queried in priority order (lower first); a 404 falls through to the next; final fallback is building from source.
- **Identity vs policy:** none — the substituter URL (scheme + priority param) IS both identity and routing policy in one string; the least separated of everything surveyed.
- **Sources:** https://nix.dev/manual/nix/2.24/command-ref/conf-file

### `oci://` pseudo-scheme family (Helm, ORAS, Flux)

- **Mirror keying:** none in any of the three — references are used directly, no mirror/redirect table. Flux keys the *CR* by k8s object name/namespace but still hard-points one registry per resource.
- **Config example:** `helm pull oci://registry.example.com/charts/mychart --plain-http` (Helm) · `oras pull --plain-http localhost:5000/hello:v1` (ORAS — usually drops `oci://` entirely on the CLI) ·
  ```yaml
  spec:
    url: oci://localhost:5000/my-app/manifests
    insecure: true
  ```
  (Flux `OCIRepository`)
- **Protocol/scheme markers:** `oci://` is a fixed pseudo-scheme meaning "this is OCI-shaped content, not a classic index" — carries **zero** transport information in all three tools.
- **HTTP/insecure handling:** pure out-of-band flags/fields, never in the URL — Helm `--plain-http` + `--insecure-skip-tls-verify`; ORAS `--plain-http` + `--insecure`; Flux `insecure: true` boolean (+ `certSecretRef` for a private CA — three separate axes: plaintext / self-signed / normal TLS).
- **Fallback semantics:** none in any — one ref, one destination.
- **Identity vs policy:** cleanest split of everything surveyed — the ref/`url` is pure identity+kind marker; flags/fields are pure transport policy, fully orthogonal to it.
- **Sources:** https://helm.sh/docs/topics/registries/, https://oras.land/docs/compatible_oci_registries/, https://oras.land/docs/commands/oras_pull/, https://fluxcd.io/flux/components/source/ocirepositories/

---

## Synthesis for OCX

1. **Compound schemes are a two-dialect tax, not a default.** `sparse+`/`git+`-style fused schemes only appear where **2+ wire dialects must share one string field** (Cargo's legacy-git vs sparse-HTTP index; pip/npm/Nix picking which VCS binary to shell out to). OCX has one wire dialect per endpoint kind — so mirror/index fields should be **plain URLs**, with the **field name itself** as the kind marker (Cargo's own `[registries.NAME] index = "..."` proves this: no scheme prefix needed once the table already says what kind of thing it is).

2. **Transport security lives in the scheme of the mirror TARGET**, not a wrapper. containerd (`[host."http://..."]`), Docker (`registry-mirrors: ["http://..."]`), and Nix substituters all signal plaintext-vs-TLS via the literal scheme on the *mirror URL itself* — no separate `protocol:` field. Pair this with a **separate escape hatch for unmirrored/arbitrary hosts** (Docker's `insecure-registries`, podman's per-registry `insecure`) — this is the shape for `OCX_INSECURE_REGISTRIES`.

3. **Identity and network-policy are different ownership domains and must be different tables.** podman's `prefix`/`location` (identity) vs `registry.mirror` (policy) and Cargo's `[registries.NAME]` (identity) vs `replace-with` (policy) both keep the *what a source is* declaration override-proof from the *where to actually reach it* declaration — the latter is what a corp/CI layer injects, the former is what a project commits. This maps directly onto "identity tables are project-owned, mirror/policy tables are corp-owned."

4. **`oci://` and friends are pure kind markers, never transport.** Helm/ORAS/Flux prove a bare pseudo-scheme can carry zero protocol information and still be useful — it says "this is OCI," full stop; HTTP-vs-HTTPS and cert trust are always separate flags/fields. If OCX ever needs to disambiguate an OCX reference from another kind of URL, do it the same way: a marker with no transport semantics riding on it.

5. **Replace vs. fallback is a deliberate, opposite choice per tool — and OCX should pick replace.** Cargo commits hard to pure replace (`replace-with` swaps the whole source, no retry against crates.io — correct for air-gapped/vendored builds where reachability of the "real" origin is not guaranteed and silent fallback would be a supply-chain surprise). containerd/podman/Docker/Go instead do ordered try-then-fall-through-to-upstream (correct for a transparent pull-through cache that must keep working when the cache is cold or down). OCX's mirror config is for **air-gapped/pinned-infrastructure use**, so **replace is the correct default**; an opt-in ordered-fallback mode is a defensible future knob, not a day-one requirement.
