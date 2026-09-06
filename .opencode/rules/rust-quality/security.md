# Security

The threat model is malicious registry content. These tools download, verify,
extract and execute third-party artifacts from a registry we do not control, so
every rule below is ordered by that and not by generic hardening advice. Read it
before touching extraction, subprocess spawn, credential storage, an HTTP client,
`deny.toml`, a terminal write, or anything holding an `unsafe` block.

Contents: [Unsafe Policy](#unsafe-policy) ·
[Archive Extraction and Filesystem](#archive-extraction-and-filesystem) ·
[Network, TLS, and Content Trust](#network-tls-and-content-trust) ·
[Subprocess Execution](#subprocess-execution) · [Supply Chain and Build](#supply-chain-and-build) ·
[Output and Claims](#output-and-claims) · [Terminal Rendering](#terminal-rendering-of-untrusted-text) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

- **The mechanism** — forbid unsafe by lint instead of by care, contain
  extraction with the OS resolver, bound every stream, verify before use — is
  general Rust practice for any tool that consumes untrusted archives.
- **The pinned decisions** — `unsafe_code = "forbid"` in every crate with named
  FFI exemptions only; `rustls` and no other TLS backend; `cargo deny check` as
  the single authoritative advisory gate (`cargo audit` survives only as
  `cargo audit bin` against a shipped artifact); `overflow-checks = true` in
  release; digest verification and no signature verification. Not re-litigated
  per crate.

Authenticity and containment are orthogonal and both are mandatory. A correctly
signed archive still carries `../../.ssh/authorized_keys`: a verified digest
never licenses a relaxed extractor, and a hardened extractor never licenses
skipping the digest.

## Unsafe Policy

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-01 | Set `unsafe_code = "forbid"` in `[lints.rust]` of every crate; exempt a crate only with a `Cargo.toml` comment naming the specific FFI/platform API that requires it — `forbid`, unlike `deny`, cannot be re-enabled by a downstream `#[allow]`. | `rg --files-without-match 'unsafe_code' --glob '**/Cargo.toml' --glob '!external/**' .` — every manifest listed is a violation unless its comment names the FFI exemption | MUST |
| SEC-02 | Precede every `unsafe {}` block with a `// SAFETY:` comment naming the invariant that makes it sound; give every `unsafe fn` a `# Safety` doc section. A comment restating the operation ("SAFETY: dereferences the pointer") is a failed review — Clippy checks presence, a human checks content. | `cargo clippy -- -D clippy::undocumented_unsafe_blocks -D clippy::missing_safety_doc` | MUST (new code) / backfill existing |
| SEC-03 | Never call `mem::uninitialized`, `mem::zeroed`, or `mem::transmute` to reinterpret bytes. Use `from_ne_bytes`/`from_le_bytes`, an explicit constructor, or `TryFrom` for discriminants — an out-of-range enum discriminant is instant UB, not a wrong value. | `rg -n --type rust --glob '!external/**' -e 'mem::uninitialized' -e 'mem::zeroed' -e 'transmute' .` — no hits | MUST |
| SEC-04 | Any `extern "C"` function we export (not ones we call) wraps its body in `std::panic::catch_unwind` and returns an error code — unwinding a panic across the `"C"` ABI is UB. | `rg -n --type rust --glob '!external/**' 'extern "C" fn' .` — each hit has a `catch_unwind` | MUST |
| SEC-05 | Call `env::set_var`/`remove_var` only in test code, under a documented single-owning-test convention. Never in a production path — mutating the process env races every thread reading it (CVE-2020-26235 class), and edition 2024 makes both `unsafe fn`. | `rg -n --type rust --glob '!external/**' -e 'env::set_var' -e 'env::remove_var' .` — every hit under `#[cfg(test)]` | MUST |
| SEC-06 | Run `cargo +nightly miri test` in CI for the pure-logic modules of any crate containing `unsafe`. Not repo-wide — Miri cannot execute syscalls or FFI, which is most of our test surface. | CI job exists and exits 0 for the named crates | SHOULD |

## Archive Extraction and Filesystem

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-07 | Pin explicit version floors `tar >= 0.4.45` and `zip >= 2.3.0`, not "latest compatible" — both crates shipped symlink-driven escapes in their *documented-safe* entry points. | `cargo tree -i tar -i zip` | MUST |
| SEC-08 | The extraction loop matches on entry type and rejects — not skips — symlinks, hardlinks, device nodes, and any entry with `mode & 0o6000` set, unless a documented requirement says otherwise. Rejection is visible in logs; skipping is silent. | Extraction loop has an explicit `EntryType` match arm with an error branch | MUST |
| SEC-09 | Enforce max entry count, max per-entry decompressed bytes, and max cumulative decompressed bytes **while streaming**, counting bytes actually written — never the header's declared size, which is attacker-controlled (zip-bomb class, ~1032:1 single-layer DEFLATE). | Counting-reader wrapper present, not bare `read_to_end`/`unpack`; a bomb fixture test fails fast | MUST |
| SEC-10 | Registry-supplied archive entries are written through a directory-handle-relative resolver (`cap-std::fs::Dir` / `openat2` with `RESOLVE_BENEATH`). `canonicalize()` + `starts_with` is acceptable only for locally-authored trees, and only with an inline comment naming the residual TOCTOU window (CWE-367) and the condition that would require closing it. | `rg -nU --type rust --glob '!external/**' 'canonicalize\([\s\S]{0,300}?starts_with' .` — each hit has the residual-risk comment or a `Dir` handle; discard hits outside the extraction/containment module the change touches | MUST |
| SEC-11 | Create every download/extraction temp file via `tempfile::NamedTempFile`/`Builder` and land it with `.persist()`; never `format!("{}.tmp", …)` in a shared directory — a predictable name there is a symlink-race target. | `rg -n --type rust --glob '!external/**' '\.tmp"' .` | MUST |
| SEC-12 | Set `0600` on credential/lock files and `0700` on their directories explicitly at creation (`OpenOptionsExt::mode`), never via ambient umask — umask is uncontrolled host state and is legitimately `0022` on many machines. | `rg -n --type rust --glob '!external/**' -e '0o600' -e '0o700' .` co-located with credential writes | MUST |

## Network, TLS, and Content Trust

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-14 | Use `rustls`; ban `openssl`, `openssl-sys` and `native-tls` in `deny.toml`. Merge compiled-in roots as *extra* roots, never as a replacement, so `SSL_CERT_FILE`/`SSL_CERT_DIR` keep working — embedded-roots-only breaks every corporate MITM proxy. | `cargo tree -i openssl-sys -i native-tls` empty; `deny.toml` `[bans]` lists them | MUST |
| SEC-15 | Never construct an HTTP client with certificate verification disabled, including behind a test-only feature flag — `danger_accept_invalid_certs` and always-accept `ServerCertVerifier` impls survive into production behind flags nobody audits. | `rg -n --type rust --glob '!external/**' -e 'danger_accept_invalid' -e 'accept_invalid_hostnames' -e 'impl.*ServerCertVerifier' .` — no hits | MUST |
| SEC-16 | Every `reqwest::ClientBuilder` sets both `.timeout()` and `.connect_timeout()`; every subprocess or registry wait is wrapped in `tokio::time::timeout` — reqwest sets **no** request timeout by default and a stalled upstream hangs the tool forever. | Every `ClientBuilder::new()` call site has `.timeout(` in its chain | MUST |
| SEC-17 | Bound response bodies while streaming (`bytes_stream()` + running counter). Never `.bytes()`/`.text()` on registry-sourced content — `.bytes()` buffers the whole body before any size check can run, so OOM precedes rejection. | `rg -nU --type rust --glob '!external/**' -e '\.bytes\(\)\s*\.await' -e '\.text\(\)\s*\.await' .` — no hit sits on a registry path. `-U` and `\s*` are load-bearing: rustfmt breaks the chain across lines, and the line-anchored form misses `grimoire/src/catalog/index_source.rs:173`, a real unbounded read of a registry index | MUST |
| SEC-18 | Any host taken from a wire document or remote-controlled config is validated at **connect** time via a custom `reqwest::dns::Resolve` that re-checks every resolved address against loopback/private/link-local/metadata ranges, and re-validated on every redirect hop — hostname string matching loses to DNS rebinding. | Client is built with a `.dns_resolver()` hook, not a pre-flight URL check | MUST |
| SEC-19 | Verify `Content-Length` against the descriptor's declared size *before* hashing, hash incrementally while streaming, and never re-open the verified artifact by path outside a tool-exclusive `0700` directory — carry the handle, or the quarantine dir, forward to extraction/exec. Verify-then-reopen-by-path reintroduces the TOCTOU the digest eliminated. | Hashing adapter wraps the same stream written to the cache target; no `File::open(path)` between verify and use | MUST |
| SEC-20 | Check the scope actually granted by an OCI bearer token; do not infer authorization from a 200 — the server may silently intersect requested scope with actual permissions without erroring. | Token-acquisition code inspects returned scope or fails closed on the specific operation | SHOULD |

## Subprocess Execution

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-21 | Spawn any downloaded or verified binary by absolute, canonicalized path — never a bare name resolved through `PATH`. A writable directory early in `PATH` substitutes the binary we just verified. | `rg -n --type rust --glob '!external/**' 'Command::new\(' .` — flag bare-string targets on download paths | MUST |
| SEC-22 | Insert a literal `--` before the first untrusted positional argument, or validate that the value does not begin with `-`. `Command` never invokes a shell, so shell injection does not apply — but the *invoked program's* argv parser reads `-u./payload` as a flag (argument-injection CVE class). | `rg -n --type rust --glob '!external/**' 'Command::new' .` — untrusted `.arg()` preceded by `.arg("--")` | MUST |
| SEC-23 | Every `tokio::process::Command` spawn of an extracted or untrusted binary sets `.kill_on_drop(true)` and wraps its wait in `tokio::time::timeout` with an explicit `child.kill()` on elapse — dropping a Tokio `Child` leaves the process running, and an elapsed timeout terminates nothing by itself. | Each `tokio::process::Command::new` has a paired `.kill_on_drop(true)` | MUST |
| SEC-24 | After `env_clear()`, always set `PATH` explicitly. Never pass a secret via `.arg()` or an inherited env var — use stdin, an fd, or a `0600` file. `execvp` falls back to a compiled-in `/bin:/usr/bin` rather than failing closed, and argv is world-readable in `/proc/<pid>/cmdline`. | `rg -n --type rust --glob '!external/**' 'env_clear\(\)' .` — each with an adjacent `.env("PATH"` | MUST |

## Supply Chain and Build

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-25 | `cargo deny check` is the single authoritative advisory gate, with `[sources] unknown-registry = "deny"` and `unknown-git = "deny"`, `unsound = "all"`, and `unmaintained = "workspace"`. Every `[advisories].ignore` entry carries an inline machine-checkable removal condition. Gating `unmaintained` as hard as `unsound` produces the noise that gets the gate disabled. | `cargo deny check`; `grep -A3 '\[sources\]' deny.toml` | MUST |
| SEC-26 | Pass `--locked` to every `cargo build`, `cargo test` and `cargo install` in CI, release workflows, scripts and docs — `cargo install` **ignores** the published `Cargo.lock` by default. | `rg -n -e 'cargo build' -e 'cargo test' -e 'cargo install' .github/ taskfiles/` — every hit carries `--locked` | MUST |
| SEC-27 | Pin every git dependency to `rev = "<full 40-char SHA>"`, never a branch or bare tag. Review any `[patch]` diff with the weight of a new dependency — git deps carry no checksum in `Cargo.lock`, a branch moves with zero `Cargo.toml` diff, and `[patch]` reroutes a trusted crate name graph-wide. | `rg -n --glob '**/Cargo.toml' --glob '!external/**' 'git = ' .` — every hit carries `rev = ` on the same line | MUST |
| SEC-28 | Read the source of `build.rs` and proc-macro crates before merging a dependency that ships one; a green `cargo deny` is not a substitute. Both execute arbitrary code on the dev/CI machine at build time with full fs/network/env access — including CI secrets — and nothing in mainline cargo sandboxes it. | PR adding a dep with `build.rs` or `proc-macro = true` shows evidence the file was opened | MUST |
| SEC-29 | Build releases with `cargo auditable build --release` and emit GitHub Artifact Attestations; document `gh attestation verify` in the release notes. | `cargo audit bin <artifact>` returns dependency data; `gh attestation verify` passes | SHOULD |
| SEC-30 | Set `[profile.release] overflow-checks = true` in the workspace root, and use `checked_*`/`saturating_*` explicitly wherever an attacker-declared length or offset is combined with another value. The release default is `false`, so a size check that should reject a wrapped value silently accepts it in the exact binary users run. The flag is the net; checked arithmetic is the control. | `grep -A3 '\[profile.release\]' Cargo.toml`; `cargo clippy -- -W clippy::arithmetic_side_effects` on size-handling modules | MUST |

```toml
# deny.toml — [sources] is the section generated configs drop
[advisories]
unsound = "all"
unmaintained = "workspace"

[bans]
deny = [{ name = "openssl" }, { name = "openssl-sys" }, { name = "native-tls" }]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

## Output and Claims

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-31 | Sanitize **all** registry-sourced text at the render boundary — the rendered error chain at the top-level stderr exit, every log line, and every string entering the TUI — through the single sanitizer SEC-34 defines, and make the raw deserialized type unable to reach a write call without it (a `SafeDisplay`-style newtype). The render boundary is the only point every path is structurally forced through; ingest-time filtering is routed around by the next data path added, and `tracing-subscriber` passes `\n`, `\r`, NUL and the whole `Cf` bidi set straight to the terminal (CWE-150). The defect mode is a *missing* call, which no behavioural assertion catches. | Boundary file calls the sanitizer, pinned by a test reading the boundary file's own source; a second `#[test]` asserts zero `println!`/`eprintln!`/`write!(…stdout`/`Span::raw`/`Line::from` sites fed a raw registry field | MUST |
| SEC-32 | Never document, claim, or write an audit checklist entry for a security control that does not exist in shipped code. When a control is removed or was never built, say so explicitly in the doc. Signature verification is the live example: we do not do it, and every doc must say so rather than list it as an attack-surface control. | Review: every control named in security docs resolves to a module/function | MUST |
| SEC-33 | Wrap credentials in `secrecy::SecretString` and never claim more than it provides: it stops `Debug`/`Display` leaks, use-after-drop, and accidental copy-out. It does **not** provide `mlock`, swap protection, or defence against a memory dump or attached debugger. | `rg -n --type rust --glob '!external/**' 'expose_secret\(\)' .` — no exposed value flows into `Command::arg`, `format!`, or a log macro | SHOULD |

## Terminal Rendering of Untrusted Text

| ID | Rule | Verification | Severity |
|---|---|---|---|
| SEC-34 | One sanitizer function owns every write: strip with a real VT state machine (`strip-ansi-escapes`, `vte`-backed, or `console::strip_ansi_codes`) — never a hand-rolled `ESC\[[0-9;]*m` regex — then drop the remaining C0/C1 controls, the bidi overrides and isolates (U+202A–U+202E, U+2066–U+2069) and the zero-width codepoints (U+200B–U+200D, U+FEFF), or keep a printable-plus-`\n`/`\t` allowlist. An SGR-only regex passes OSC, DCS and the tmux `\x1bPtmux;…` passthrough wrapper straight through. | Table-driven corpus, one case per class — CSI colour, cursor move, OSC 8, OSC 52, tmux DCS passthrough, U+202E, U+2066, ZWJ, BOM, CJK, multi-codepoint emoji — each asserting the stripped output and the resulting display width | MUST |
| SEC-35 | Strip before truncating; never truncate raw text and strip after. A cut through a live CSI/OSC/DCS sequence leaves a dangling escape that eats the next field as its parameters, and stripping afterwards removes only the fragment's head. | The strip call appears textually before any width/grapheme truncation on the same value; a unit test cuts through `\x1b[38;5;196m` and asserts no `\x1b` and no `[`+digits fragment survives | MUST |
| SEC-36 | Truncate on grapheme cluster boundaries by measured display width (`unicode-segmentation::graphemes(true)` + `unicode-width`, or `console::truncate_str`) — never `.len()`, `.chars().take(n)`, or byte slicing: none of them is column width, byte slicing panics off a char boundary, and char slicing orphans combining marks and ZWJ sequences. | Restrict to added lines on a diff: `git diff -U0 -G'\.chars\(\)\.take\(' -- '*.rs'`; `git diff -U0 -G'&[a-z_]*\[\.\.' -- '*.rs'` — neither may introduce a truncation of a registry-sourced value | MUST |
| SEC-37 | Strip bidi overrides and isolates from runtime strings as part of SEC-34, and never cite rustc's `text_direction_codepoint_in_{comment,literal}` as coverage — both are deny-by-default but run at lex time over source literals, and a codepoint deserialized from a registry response never passes the lexer. No clippy lint fills the runtime gap. | Sanitizer unit test asserts `\u{202E}` is removed or the input rejected | MUST |
| SEC-38 | Sanitize before any string reaches a ratatui `Cell`, `Span`, `Line`, list item, table cell or widget title, and before any `eprintln!` issued inside the TUI event loop. `Buffer::set_stringn` drops control-bearing graphemes on that one path only; `Cell::set_symbol()` is unfiltered and the crossterm backend prints `cell.symbol()` verbatim, so "it goes through ratatui" is not an argument. | `rg -n --type rust --glob '!external/**' 'set_symbol\(' .` — every hit sits downstream of the sanitizer; a test renders each custom widget from a `\x1b[31m`-bearing string and asserts no cell symbol contains `\x1b` | MUST |
| SEC-39 | Never interpolate registry-sourced text into an OSC 8 hyperlink URI or an OSC 52 clipboard payload — the OSC 8 spec defers all mitigation to the terminal emulator, and OSC 52 sets the system clipboard from anything that can write to the tty. | `rg -n --type rust --glob '!external/**' -e '\\x1b\]8;' -e '\\x1b\]52;' .` — each site interpolates only tool-generated values | MUST |
| SEC-40 | When forwarding a child process's output, decide colour by TTY detection (`anstream`) and sanitization by that child's own trust model — the presence of ANSI bytes is not evidence the stream is safe, and `anstream::StripStream` strips only when the destination is *not* colour-capable, which is backwards for a security boundary. | Every pass-through site names the upstream source feeding that child in a comment; untrusted ones route through the SEC-31 sanitizer | SHOULD |

## What Agents Get Wrong Here

1. Writes `archive.unpack(dest)?` / `archive.extract(dest)?` and considers
   extraction handled. It is what every tutorial shows, and it was insufficient
   *in-version* across four separate CVEs. Entry-type filtering, size caps and a
   scoped directory never appear unless demanded (SEC-07/08/09/10).
2. Produces `canonicalize()` then `starts_with()` as the containment check — the
   top answer to "prevent path traversal in Rust" everywhere in training data.
   The TOCTOU caveat is never surfaced (SEC-10).
3. Prints a registry field raw — `println!("{}", pkg.name)`, `Line::from(desc)` —
   or cites ratatui's buffer as proof escapes cannot land (SEC-31/34/38).
4. Reasons "I used `Command::arg()`, not a shell string, therefore
   injection-safe" and stops. True for shell injection; false for argument
   injection and false for `PATH` resolution of a binary we just downloaded
   (SEC-21/22).
5. Writes `cargo install foo` without `--locked`, assuming it respects the
   published lockfile the way `cargo build` does (SEC-26).
6. Generates a `deny.toml` with `[sources]` absent — the least
   security-shaped of the four checks, and the one line that blocks a registry
   swap (SEC-25).
7. Writes a `// SAFETY:` comment that restates the operation instead of naming
   the invariant. It passes `clippy::undocumented_unsafe_blocks`, which only
   checks presence. Heuristic: under ~8 words, or the phrase "this is safe",
   fails review (SEC-02).
8. Adds `SecretString` and marks secret handling done, then over-claims the
   property in a doc comment or leaves the argv/env/log vectors it does not
   cover untouched (SEC-33).
9. Adds `#[serde(deny_unknown_fields)]` when asked to harden a parser against
   untrusted input. It bounds nothing — not recursion depth, not collection
   size, not payload bytes — and runs after recursive descent already had the
   stack (SEC-17).
10. "Fixes" overflow with a truncating `as u64` / `as usize` cast, converting a
    debug-build panic into a release-build silent wrong value — exactly the state
    the vulnerability needs (SEC-30).
11. Reaches for `native-tls`/`openssl` for a new HTTP client; training data
    predates the 2025 rustls consolidation (SEC-14).
12. Reproduces `mem::uninitialized()`, `static mut` globals, or an `unsafe fn`
    body with no inner `unsafe {}` block — all now deprecated or deny-by-default
    under edition 2024 (SEC-03).
13. Claims a CI job passed because it generated the workflow YAML. Miri,
    cargo-deny and attestation steps get written and never run (SEC-06/25/29).

## Sources

- [OCI image-spec: descriptor.md](https://github.com/opencontainers/image-spec/blob/main/descriptor.md) — normative size-before-hash and stream-don't-buffer digest verification
- [RUSTSEC-2026-0067 — tar](https://rustsec.org/advisories/RUSTSEC-2026-0067.html) — the symlink escape that sets the 0.4.45 floor
- [GHSA-94vh-gphv-8pm8 — zip CVE-2025-29787](https://github.com/advisories/GHSA-94vh-gphv-8pm8) — the escape past `enclosed_name()`, sets the 2.3.0 floor
- [cap-std](https://github.com/bytecodealliance/cap-std/blob/main/README.md) — `openat2`/`RESOLVE_BENEATH` capability semantics and platform fallbacks
- [`std::process::Command`](https://doc.rust-lang.org/std/process/struct.Command.html) — no-shell semantics, the Windows batch-file warning, `env_clear`'s `PATH` fallback
- [Cargo Book: cargo-install](https://doc.rust-lang.org/cargo/commands/cargo-install.html) — ignores the published lockfile without `--locked`
- [Cargo Book: profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#overflow-checks) — `overflow-checks` is `false` in release
- [EmbarkStudios `deny.toml`](https://github.com/EmbarkStudios/cargo-deny/blob/main/deny.toml) — the production shape to imitate, `[sources]` included
