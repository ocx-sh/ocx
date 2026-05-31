# Research: Linux libc Host Detection Methods

**Date:** 2026-05-28
**Author:** worker-researcher (via /swarm-execute detection redesign)
**Purpose:** Challenge/confirm the loader-probe approach in `HostCapabilities::detect()`; fix two confirmed bugs (non-deterministic first-wins; single-valued model); pick the most robust mechanism. Feeds the redesign of `adr_platform_libc_os_features.md` detection.

---

## Headline

The current approach — allowlist of `ld-*.so` paths probed in parallel, **first to complete wins** — has two confirmed bugs:

1. **Non-deterministic on dual-libc hosts.** First-to-complete is a timing race: a host with both `/lib64/ld-linux-x86-64.so.2` and `/lib/ld-musl-x86_64.so.1` returns glibc OR musl depending on scheduling.
2. **Models only one libc.** Detection must return the **set** of present libc families so `os_features` can be `["libc.glibc", "libc.musl"]` on a dual-libc host. A single result cannot express this.

**Fix:** probe **both** glibc and musl canonical paths concurrently, collect **all** positive results into a sorted `BTreeSet<LibcFlavor>` (deterministic by construction). The disk-enumeration *model* is correct; only the aggregation (and the missing musl probes) were wrong.

The cargo-binstall reference probes glibc paths only and treats musl as a fallback — correct for "pick one download URL," wrong for OCX's "enumerate the host capability set." Adopt its probe mechanics + quirk handling, change the aggregation.

---

## Critical distinction: self-linkage vs. host capability

OCX installs **foreign** binaries; the question is *"what libc families can this host execute?"* — NOT *"what is ocx itself linked against?"* These are orthogonal. A static-musl ocx on a pure glibc Ubuntu host is still on a host that runs glibc binaries fine.

This invalidates two mechanisms outright for host detection:

- **`/proc/self/maps`** — loaders mapped into ocx's own address space.
- **`dl_iterate_phdr`** — DSOs loaded by ocx itself.

Both report ocx's compile-time linkage, not host capability. (They ARE the right tool for the *publisher-side* ELF/PT_INTERP lint — "what is THIS artifact linked to" — but not for host detection.)

Python/uv/pip get away with self-linkage (`ctypes.CDLL(None).gnu_get_libc_version()`, PT_INTERP of `sys.executable`) only because CPython is **always dynamically linked to the system libc** — so self == host there. That assumption breaks for a static tool. OCX must use disk enumeration.

---

## Mechanism comparison

| Mechanism | Detects | Multi-libc | NixOS | Subprocess | Verdict |
|---|---|---|---|---|---|
| `getconf GNU_LIBC_VERSION` | glibc version | glibc only | if nixpkgs on PATH | yes | clean negative on musl (`unknown variable`); glibc-only. Good supplementary check for future `os.version`, not family detection. |
| `ldd --version` | libc of system `ldd` | one only | ldd often absent | yes | glibc `ldd (GNU libc) 2.x` exit 0; Alpine musl exits 1 with `musl` in the error *path* (accidental); **Ubuntu 20.04 glibc 2.31 exits 127** → needs `{loader} /bin/true` fallback. Fragile alone. |
| `ldconfig -p` | glibc loader cache | glibc only | absent | yes | **absent on Alpine/musl** (musl uses `/etc/ld-musl-$ARCH.path`, no ld.so.cache); musl loaders not registered. Can find canonical glibc path, but no musl. |
| `/proc/self/maps` | ocx's OWN loaders | never (wrong Q) | yes | no | **wrong question** — see above. |
| `dl_iterate_phdr` | ocx's OWN DSOs | never (wrong Q) | yes | no | **wrong question** — see above. |
| **Disk enumeration of loaders** (chosen) | loaders present on disk → host can run matching binaries | **yes** (probe both families) | partial (nix-ld shim → works; else empty) | yes (`loader --version`) | **correct model.** Fix aggregation to set-union. |
| ELF PT_INTERP of an installed binary | what THAT binary links to | n/a | yes | no | right for publisher-side lint, not host detection. |

---

## Canonical loader paths

**Glibc (x86_64):** `/lib/ld-linux-x86-64.so.2`, `/lib64/ld-linux-x86-64.so.2`, `/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2` (Debian multiarch — the real file), `/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2`, `/usr/lib64/ld-linux-x86-64.so.2` (Fedora usrmerge). Other arches: `ld-linux-aarch64.so.1`, `ld-linux-armhf.so.3`, `ld-linux-riscv64-lp64d.so.1`.

**Musl:** `$(syslibdir)/ld-musl-$(ARCH).so.1` → `/lib/ld-musl-x86_64.so.1`, `/lib/ld-musl-aarch64.so.1`, `/lib/ld-musl-armhf.so.1`, `/lib/ld-musl-riscv64.so.1`, `/lib/ld-musl-s390x.so.1`, `/lib/ld-musl-powerpc64le.so.1`, etc. (musl arch name).

Deduplicate by canonical path/inode so a multiarch symlink + real file don't double-probe.

**cargo-binstall's probe list contains NO musl paths** — confirmed from source (`detect-targets` v0.1.85). It probes glibc `ld-linux-*`/`libc.so.6` variants only; musl is a pure fallback. OCX must add the `ld-musl-*` probes to model musl presence.

## Distro/edge behaviors

- **Alpine (pure musl):** only `/lib/ld-musl-x86_64.so.1`. No glibc, no ldconfig.
- **Alpine + gcompat:** musl loader **plus** a gcompat stub at the glibc loader path. Per ADR predictability rule, gcompat host reports **`libc.musl` only** — do NOT count the gcompat stub as glibc capability (identity, not equivalence). The gcompat stub responds distinctly; detect and exclude it.
- **Ubuntu/Debian + `musl-tools`:** glibc primary **plus** a musl loader → genuine dual-libc. Reports `{glibc, musl}`.
- **Ubuntu 20.04 (glibc 2.31):** `{loader} --version` exits 127; fall back to `{loader} /bin/true` to confirm the loader is live.
- **NixOS (no nix-ld):** loaders live under `/nix/store/...`, nothing at FHS paths → empty set. Degrade to `None` (no `os_features`), debug-log when `/nix` exists. User overrides via `--platform`. **nix-ld** installs an FHS shim at `/lib/ld-linux-x86-64.so.2` → probe works normally.

## Dual-libc is real and relevant

A single host can run both families (gcompat Alpine; Ubuntu+musl-tools; CI runners that test multiple targets). OCX targets CI/CD automation — exactly where multi-libc tooling appears. The `os_features` subset model already handles this correctly **once detection populates the full set**: a host advertising `["libc.glibc","libc.musl"]` matches both `libc.glibc`- and `libc.musl`-tagged index entries. This is a strength of the chosen model.

---

## Rust crates surveyed

| Crate | Approach | Multi-libc | Verdict |
|---|---|---|---|
| `detect-targets` (cargo-binstall, v0.1.85) | disk probe + subprocess, glibc-only | no | best **mechanics** reference; wrong aggregation. Copy algorithm, not crate. |
| `glibc_musl_version` v0.1.0 | unclear (possibly self-linkage) | claims both | too early / maintenance-uncertain / wrong-question risk. Avoid. |
| `target-lexicon`, `platforms` | compile-time triple metadata | n/a | not runtime detection. |

No crate provides "runtime host libc capability **set** via disk probe." Implement it (copy binstall mechanics, add musl probes, set-union aggregation).

---

## `--platform` feature-list syntax

Surveyed: Zig uses `+<flag>` ABI feature suffixes (`...-musl+ldbl64`); Docker `--platform` = `os/arch/variant` only; manylinux joins tags with `.`; RubyGems `arch-os-libc`. For OCX's CLI override string the cleanest multi-value form is **repeated `+`**: `linux/amd64+libc.glibc+libc.musl`.

- Beats `+{a,b}` (shell-quoting) and `+a,b` (comma ambiguity with other args).
- No collision with OCI `os/arch/variant` (`/`-delimited).
- OCI manifest side is structured JSON (`os.features: [...]`) — no string encoding needed there; this is only the CLI convenience layer.

**Decision: keep the already-shipped repeated-`+` syntax.**

---

## Recommendation (for `HostCapabilities::detect()`)

1. Probe glibc canonical paths (multiarch-aware) **and** the musl path, concurrently (`tokio`, `join_all` — no early abort).
2. Per probe, apply cargo-binstall exit-code interpretation: exit 0 + `GNU libc`/`GLIBC` in stdout → glibc; musl loader responds → musl; exit 127 → `{loader} /bin/true` fallback; **gcompat stub → exclude** (musl-only rule).
3. **Aggregate: set-union into `BTreeSet<LibcFlavor>`** (deterministic; fixes the race + the single-value model).
4. `os_features()` → sorted `Vec<String>` of `libc.*` tags for the whole set, or `None` if empty.
5. NixOS / empty set: if `/nix` exists, debug-log; return `None`. Degrade to `Any`-only matching; `--platform` override available.
6. Any probe failure (absent/permission/timeout) is skipped silently; all-fail → `None`, never an error. Keep the per-probe timeout (DoS bound).
7. Do **not** use `/proc/self/maps` or `dl_iterate_phdr` for host detection. `getconf GNU_LIBC_VERSION` is fine later for glibc **version** (`os.version`), not family.

This fixes both bugs, models dual-libc, handles the distro quirks, and degrades cleanly.

## Product context

No `product-context.md` shift. Multi-libc reality reinforces the `os_features` subset model already chosen.

---

## Sources

- cargo-binstall `detect-targets/src/detect/linux.rs` — https://github.com/cargo-bins/cargo-binstall/blob/main/crates/detect-targets/src/detect/linux.rs (source reviewed)
- `detect-targets` v0.1.85 — https://lib.rs/crates/detect-targets
- PEP 656 (musllinux) — https://peps.python.org/pep-0656/
- PEP 513 (manylinux glibc detection) — https://peps.python.org/pep-0513/
- pip `_manylinux.py` (`os.confstr("CS_GNU_LIBC_VERSION")`) — https://fossies.org/linux/pip/src/pip/_vendor/packaging/_manylinux.py
- musl supported platforms / loader naming — https://wiki.musl-libc.org/supported-platforms.html , https://wiki.musl-libc.org/faq
- Debian Multiarch library paths — https://wiki.debian.org/Multiarch/LibraryPathOverview
- NixOS nix-ld — https://wiki.nixos.org/wiki/Nix-ld ; https://fzakaria.com/2025/02/26/nix-pragmatism-nix-ld-and-envfs
- Alpine running glibc programs (gcompat) — https://wiki.alpinelinux.org/wiki/Running_glibc_programs
- musl-first ordering bug (real-world) — https://github.com/anthropics/claude-agent-sdk-typescript/issues/296
- `ldd --version` Alpine exit code — https://github.com/appsignal/appsignal-nodejs/issues/369
- Platform-strings cross-ecosystem survey (Zig `+`) — https://nesbitt.io/2026/02/17/platform-strings.html
- `glibc_musl_version` crate — https://crates.io/crates/glibc_musl_version/0.1.0
- lovell/detect-libc (Node reference) — https://github.com/lovell/detect-libc
