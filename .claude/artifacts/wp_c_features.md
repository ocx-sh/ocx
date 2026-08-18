# WP-C — sigstore crate features + deny re-confirm

ADR step 5 of `adr_real_sigstore_stack_and_delegation.md`; WP-C of
`plan_real_sigstore_stack.md`. Written incrementally as each item was confirmed.

## Headline

**Only one of the five named features actually changes the graph.** sigstore
0.14.0 defines `bundle = ["sign", "verify"]`, and both `sign` and `verify` list
`"fulcio"` and `"rekor"` (plus `cert`; `fulcio` pulls `oauth`). So `sign`,
`verify`, `fulcio` and `rekor` were **already enabled transitively** by the
existing `bundle` pin. The genuinely new feature is **`sigstore-trust-root`**,
whose only new crate is **`tough`** — and it carries one real cost, an old
`typed-path` that breaks type inference workspace-wide (see below).

Verbatim, from `~/.cargo/registry/src/index.crates.io-*/sigstore-0.14.0/Cargo.toml`:

```toml
bundle = ["sign", "verify"]
sign   = ["cert", "dep:ecdsa", "dep:hex", "dep:p256", "dep:serde_json_canonicalizer",
          "dep:signature", "dep:sigstore_protobuf_specs", "fulcio", "rekor"]
verify = ["cert", "dep:hex", "dep:serde_json_canonicalizer",
          "dep:sigstore_protobuf_specs", "fulcio", "rekor"]
fulcio = ["dep:reqwest", "dep:serde_repr", "dep:serde_with", "dep:webbrowser", "oauth"]
rekor  = ["dep:hex", "dep:hex-literal", "dep:reqwest", "dep:serde_json_canonicalizer",
          "reqwest?/query"]
sigstore-trust-root = ["dep:async-trait", "dep:futures", "dep:futures-util", "dep:hex",
                       "dep:reqwest", "dep:sigstore_protobuf_specs", "dep:tough",
                       "reqwest?/stream", "tokio/sync"]
rustls-tls = ["oci-client?/rustls-tls", "reqwest?/rustls"]
native-tls = ["oci-client?/native-tls", "reqwest?/native-tls"]   # never enabled — SEC-14
```

The pre-change `cargo tree -e features -i rustls` already showed the implication
chain in the tree itself, before any edit:

```
    │       ├── reqwest feature "blocking"
    │       │   └── sigstore feature "oauth"
    │       │       └── sigstore feature "fulcio"
    │       │           ├── sigstore feature "sign"
    │       │           │   └── sigstore feature "bundle"
    │       │           │       └── ocx_lib v0.5.8 (crates/ocx_lib)
    │       │           └── sigstore feature "verify"
    │       │               └── sigstore feature "bundle" (*)
```

The four are still named explicitly in the manifest: `version = "0.14.0"` is a
caret requirement, so a 0.14.x that narrowed `bundle` would silently drop a
capability this milestone depends on. One line, no graph change, closes that class.


## What changed

`crates/ocx_lib/Cargo.toml` only (plus the lock). Features were added at the
**member** declaration, not the workspace one — they unify additively with
`[workspace.dependencies]`, `serde_json` two dozen lines above uses the same
shape, and the workspace root manifest is outside WP-C's declared file surface.

```toml
sigstore = { workspace = true, features = [
  "sign",
  "verify",
  "fulcio",
  "rekor",
  "sigstore-trust-root",
] }
```

`bundle` and `rustls-tls` are untouched, still on the workspace declaration.
`native-tls` remains off; `default-features = false` there is unchanged.

`Cargo.lock`: **8 packages added, 0 removed, 0 version-bumped.**

```
      Adding async-recursion v1.1.1
      Adding globset v0.4.19 (available: v0.4.20)
      Adding pin-project v1.1.13
      Adding pin-project-internal v1.1.13
      Adding snafu v0.8.9
      Adding snafu-derive v0.8.9
      Adding tough v0.22.0
      Adding typed-path v0.9.3
```

All eight are `tough` and its subtree. No new direct dependency was added.

## Provider resolution — exactly one, unchanged

Pre-change and post-change both resolve **`aws-lc-rs` only**; `ring` is absent
in both.

```
$ cargo tree -e normal -i ring            # before AND after
warning: nothing to print.

To find dependencies that require specific target platforms, try to use option
`--target all` first, and then narrow your search scope accordingly.
```

```
$ cargo tree -e normal -i aws-lc-rs       # after
aws-lc-rs v1.17.3
├── jsonwebtoken v10.4.0
│   └── oci-client v0.17.0 (external/rust-oci-client)
│       └── ocx_lib v0.5.8 (crates/ocx_lib)
├── rustls v0.23.43
│   ├── hyper-rustls v0.27.9
│   ├── reqwest v0.13.4
│   ├── rustls-platform-verifier v0.7.0
│   └── tokio-rustls v0.26.4
├── rustls-webpki v0.103.13
```

### The one thing worth a second look, and why it is not a finding

`tough` depends on `rustls` with **default features on**, which the post-change
feature tree shows as a new edge into `rustls feature "default"`:

```
├── rustls feature "aws-lc-rs"
│   ├── reqwest feature "__rustls-aws-lc-rs" (*)
│   └── rustls feature "aws_lc_rs"
│       ├── hyper-rustls feature "aws-lc-rs" (*)
│       ├── rustls feature "default"
│       │   └── tough v0.22.0
│       │       └── sigstore v0.14.0 (*)
│       ├── rustls feature "prefer-post-quantum"
│       │   └── rustls feature "default" (*)
```

`rustls` 0.23.43 defines `default = ["aws_lc_rs", "logging",
"prefer-post-quantum", "std", "tls12"]`, so on the face of it this reads like a
new edge turning on the provider *and* changing handshake preference
workspace-wide via feature unification. It is not. `reqwest` 0.13.4 declares
rustls **without** `default-features = false`:

```toml
[target.'cfg(not(all(target_arch = "wasm32", …)))'.dependencies.rustls]
version = "0.23.4"
features = ["std", "tls12"]
optional = true
```

No `default-features` key means Cargo's default (`true`), so `rustls/default` —
`aws_lc_rs` and `prefer-post-quantum` included — was **already enabled by
reqwest before this change**. `tough`'s edge is redundant with an existing one.
Nothing about the TLS backend or the handshake preference order moves.

## The one real cost — `sigstore-trust-root` breaks the build until call sites move

This is the "stop and report rather than paper over it" case, and it is a hard
build break, not a warning.

### What happens

`sigstore-trust-root` → `dep:tough` → `typed-path 0.9.3`. The workspace already
carries `typed-path 0.12.3` via `zip 8.6.0`. The two are semver-incompatible
0.x minors, so Cargo links both:

```
$ cargo tree -e normal -i typed-path@0.12.3
typed-path v0.12.3
└── zip v8.6.0
    └── ocx_lib v0.5.8 (crates/ocx_lib)

$ cargo tree -e normal -i typed-path@0.9.3
typed-path v0.9.3
└── tough v0.22.0
    └── sigstore v0.14.0
        └── ocx_lib v0.5.8 (crates/ocx_lib)
```

`cargo check --workspace --all-targets` then fails with **19 × E0283**, exit 101:

```
      6 crates/ocx_lib/src/cli/data_interface.rs
     13 crates/ocx_lib/src/package/metadata/template.rs
```

```
error[E0283]: type annotations needed
   --> crates/ocx_lib/src/package/metadata/template.rs:932:23
    |
932 |             !resolved.contains(dep_path.as_ref()),
    |                       ^^^^^^^^          ------ type must be known at this point
    |
    = note: multiple `impl`s satisfying `std::borrow::Cow<'_, str>: AsRef<_>` found
            in the following crates: `alloc`, `typed_path`:
            - impl<T> AsRef<T> for std::borrow::Cow<'_, T> where T: ToOwned, T: ?Sized;
            - impl<T> AsRef<typed_path::common::utf8::path::Utf8Path<T>> for std::borrow::Cow<'_, str>
              where for<'enc> T: typed_path::common::utf8::Utf8Encoding<'enc>;
```

**Not test-only.** `cargo check -p ocx_lib --lib` also fails, exit 101, 3 errors
in `crates/ocx_lib/src/cli/data_interface.rs` — production code:

```
error[E0283]: type annotations needed
   --> crates/ocx_lib/src/cli/data_interface.rs:250:47
    |
250 |             header = header.render(col.header.as_ref(), &style);
    |                                               ^^^^^^
```

### Root cause, verified against both crate sources

The offending blanket impl exists in 0.9.3 and was **removed upstream** by 0.12:

```
$ grep -rn "impl.*AsRef<Utf8Path" typed-path-0.9.3/src
typed-path-0.9.3/src/common/utf8/path.rs:1059:impl<T> AsRef<Utf8Path<T>> for str
typed-path-0.9.3/src/common/utf8/path.rs:1069:impl<T> AsRef<Utf8Path<T>> for Cow<'_, str>   <-- this one
typed-path-0.9.3/src/common/utf8/path.rs:1079:impl<T> AsRef<Utf8Path<T>> for String

$ grep -rn "impl.*AsRef<Utf8Path" typed-path-0.12.3/src
typed-path-0.12.3/src/common/utf8/path.rs:1060:impl<T> AsRef<Utf8Path<T>> for str
typed-path-0.12.3/src/common/utf8/path.rs:1070:impl<T> AsRef<Utf8Path<T>> for String
                                                    (no Cow<'_, str> impl)
```

So the break is caused by **0.9.3 entering the graph at all**, not by the
duplication. Removing `typed-path` from `zip` would not help: 0.9.3's impl alone
is enough to make `Cow<'_, str>::as_ref()` ambiguous at every inference site.

### No version escape exists

Every `tough` release in the local index — 0.18.0 through **0.24.0** — pins
`typed-path = "^0.9"`, and `sigstore` 0.14.0 pins `tough = "0.22"`, so 0.22.0 is
the only reachable one:

```
  0.21.0 | typed-path: ^0.9
  0.22.0 | typed-path: ^0.9     <-- the only one satisfying sigstore's ^0.22
  0.23.0 | typed-path: ^0.9
  0.24.0 | typed-path: ^0.9
```

`[patch.crates-io] typed-path` is not a way out either: it replaces the crate
graph-wide, so whichever version wins breaks the other consumer's API.

### Resolution — enabled, with the call sites fixed by `&*`

`sigstore-trust-root` **is** enabled. The ambiguity is resolved at the call
sites, not by dropping the feature: `Cow<'_, str>` derefs to `str` with `&*`,
which names the target type and needs no turbofish.

```rust
-  header = header.render(col.header.as_ref(), &style);
+  header = header.render(&*col.header, &style);
```

Deferring the feature to WP-J was considered and rejected — the ADR names it in
step 5, nothing else in the graph moves, and the call-site edit is smaller and
more readable than a turbofish. The upstream ask on `tough` (bump `typed-path`
0.9 -> 0.12, one line) is still worth filing: it would let these call sites
revert to plain `.as_ref()`.

**Progress at the time of writing.** `cargo check --workspace --all-targets
--locked` is **red**, exit 101, 15 x E0283 remaining:

```
      2 crates/ocx_cli/src/command/self_group/activate.rs
     13 crates/ocx_lib/src/package/metadata/template.rs
```

`crates/ocx_lib/src/cli/data_interface.rs` (6 sites) is already fixed. All three
files are `.rs` and therefore **outside WP-C's declared file surface** — they are
being fixed by the agent that owns them. WP-C's own three files are done.

## Gate results

| Gate | Result |
|---|---|
| 1. `cargo tree -e features -i rustls` | **exactly one provider.** `aws-lc-rs` before and after; `cargo tree -e normal -i ring` prints `warning: nothing to print.` in both |
| 2. `cargo deny check` | **exit 0** — `advisories ok, bans ok, licenses ok, sources ok` |
| 3. `cargo tree -e normal -d` | one new duplicate: `typed-path` 0.9.3 (tough) alongside 0.12.3 (zip). 62 -> 64 duplicate lines; nothing resolved. Reported above, not papered over |
| 4. `cargo check --workspace --all-targets --locked` | **red, 15 x E0283**, all in `.rs` files outside this WP's surface (see above) |
| 5. binary-size delta | **not measured — skipped, not guessed.** A release build of `ocx_cli` was not run: the workspace does not currently compile (gate 4), so there is no artifact to `stat`. Re-measure once the call-site fixes land |

Gate 3 was verified with `comm`, not `diff` — the shell proxy in this
environment mangles `diff` output and appended a spurious "IDENTICAL" line to a
run that had in fact reported changes:

```
$ comm -13 .dup-before.txt .dup-after.txt     # only in AFTER
typed-path v0.12.3
typed-path v0.9.3
$ comm -23 .dup-before.txt .dup-after.txt     # only in BEFORE
(empty)
```

## What I refused to do, and why

1. **Did not edit any `.rs` file.** The 19 E0283 sites are the direct
   consequence of this WP's change, and fixing them was tempting. `.rs` is
   explicitly outside the declared file surface, and a second writer was already
   editing those files — see below.
2. **Did not add `[[bans.deny]]` for `native-tls` / `openssl` / `openssl-sys`**
   to `deny.toml`, although `deny.toml` is in my surface and SEC-14 requires it.
   ADR D2a raises this as a side-finding and explicitly states "Filed rather than
   fixed here — it is outside this ADR's scope." That is a standing decision, so
   it is surfaced rather than taken. It remains a real gap: the only thing
   keeping `native-tls` out today is the `default-features = false` pin, with no
   gate behind it. The stanza, when someone wants it:

   ```toml
   [bans]
   multiple-versions = "warn"
   deny = [
     { name = "openssl" }, { name = "openssl-sys" }, { name = "native-tls" },
   ]
   ```
3. **Did not add any new direct dependency.** All eight new lock entries are
   `tough`'s transitive subtree.
4. **Did not commit.** (Someone else did — see below.)

## Concurrency collision — flagged, not hidden

This worktree had **more than one concurrent writer** while WP-C ran, which is
worth knowing because it invalidated two of my intermediate conclusions.

- The tree was clean at start. Partway through, `git status` showed
  `crates/ocx_lib/src/cli/data_interface.rs`, `test/src/helpers.py`,
  `test/tests/conftest.py`, `test/tests/fixtures/sigstore_stack.py` and later
  `crates/ocx_lib/src/oci/sign/fulcio.rs` modified by another agent.
- WP-C's own changes were **committed by the owner mid-flight** as
  [`0b19abc4`](https://github.com/ocx-sh/ocx/commit/0b19abc4) *"chore(deps):
  enable the sigstore features the delegation needs"*, with the WP-B/WP-E stack
  landing just before it as `a109eb54`.
- Consequence: a `git checkout HEAD -- crates/ocx_lib/Cargo.toml deny.toml
  Cargo.lock` I ran to isolate a lock question restored the *already-committed*
  version rather than the pre-WP-C one, which briefly made a "pristine HEAD
  already fails `--locked`" reading look like pre-existing drift. It was not —
  it was this WP's own change, already landed. Corrected here rather than left
  standing.
- **I cannot prove I did not clobber an uncommitted edit** of those three files
  belonging to the other writer during that checkout. Current `git status` shows
  all three unmodified and HEAD carries the intended content, so nothing appears
  lost, but the window existed and is reported rather than assumed away.

## Final state of WP-C's three files

- `crates/ocx_lib/Cargo.toml` — five features named on the member declaration,
  `bundle` + `rustls-tls` untouched on the workspace one, `native-tls` still off.
- `deny.toml` — RUSTSEC-2023-0071 ignore re-confirmed with a DEP-08 removal
  condition (`cargo tree -e normal -i rsa`) that also records *why* the feature
  set cannot make it go empty.
- `Cargo.lock` — 8 packages added, all `tough`'s subtree; none removed or bumped.
- `crates/ocx_cli/Cargo.toml` — **not touched.** No forwarding was needed:
  `ocx_lib` declares the features directly and Cargo unifies them for dependents.
