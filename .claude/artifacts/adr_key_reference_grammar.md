# ADR: Key Reference Grammar — one spelling for a file

## Metadata

**Status:** Accepted
**Date:** 2026-08-30
**Deciders:** mherwig
**Related Issues:** [ocx-sh/ocx#369](https://github.com/ocx-sh/ocx/pull/369) (cosign parity),
[ocx-sh/ocx#379](https://github.com/ocx-sh/ocx/issues/379) (shared local-file reference type — follow-up)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** cli | config | oci | wire-format
**Supersedes:** N/A — refines the `KeyRef` grammar frozen in [`design_spec_cosign_parity.md`](./design_spec_cosign_parity.md)

## Context

`[[trust.policy]] signers[].key` — and the `--key` flag it shares a parser with
(`crates/ocx_lib/src/oci/sign/key_ref.rs`, `KeyRef::parse`) — took three forms, evaluated
in order:

1. anything containing `://` splits on the **first** `://` into scheme + rest;
2. otherwise a `file:` prefix (single colon, case-sensitive) means a file;
3. otherwise the whole value is a bare file path.

Form 2 was the form the documented examples used, the form `key_ref_parse_table` pinned as
an `Ok` row, and the form `trust.rs` called "the frozen spelling".

### The decision reversed once. Both halves are on the record.

The owner first ruled that **`file:` stays** — supporting it was fine as an exception,
provided one struct encapsulated the grammar and was reused everywhere so it was genuinely
consistent. An earlier revision of this file was written on that premise and designed that
struct.

E-1 below then reversed it. Measured against cosign v3.1.1, `file:cosign.key` is a string
cosign *resolves* — as a file **literally named** `file:cosign.key` — while OCX stripped
the prefix and opened `cosign.key`. One string, two meanings, silently. On that evidence
the owner's ruling changed: *"the new file syntax is confirmed, so we found actually a bug
… and we have this extra file scheme that we support in ocx that is consistent."* Form 2
goes; bare and `file://` stay, `file://` understood as an OCX extra that cosign refuses
outright.

### What the superseded revision found that survives

Its census stands and is not re-derived here: **one `config.toml` parses a local-file
reference with three different hand-rolled grammars**, two of them in the same `[trust]`
table.

| Key | Spellings | Relative policy |
|---|---|---|
| `[registries.<ns>] index` | `file://<abs>` only | refused — must be absolute |
| `[[trust.policy]] signers[].key` | bare, `file://` (after this ADR) | anchored to the declaring `config.toml`'s dir |
| `[trust.sigstore] trusted_root` | **bare only** — a plain serde `Option<PathBuf>`, no scheme at all | anchored to the declaring `config.toml`'s dir |

`trusted_root` was in nobody's brief and is the sharpest instance: it sits three lines from
`signers[].key` in the same table and accepts a strictly smaller vocabulary, for no stated
reason.

**That unification is descoped, not dropped** — it is
[ocx-sh/ocx#379](https://github.com/ocx-sh/ocx/issues/379), landing as its own PR. This ADR
removes a spelling; #379 shares the parser for the two that remain. Sequential, not rival.
Its designed type is restated there on **two** spellings (`Spelling { Bare, FileUrl }`);
anywhere an earlier draft said three, that is stale.

## Evidence

### E-1 — cosign accepts **no** `file:` and **no** `file://`

`ghcr.io/sigstore/cosign/cosign:v3.1.1`, run locally against a generated P-256 pair, on
both key-ref code paths — `public-key --key` (`signature.SignerVerifierFromKeyRef`) and
`verify-blob --key` (`signature.PublicKeyFromKeyRef`):

| `--key` value | cosign result |
|---|---|
| `cosign.key` / `/w/cosign.key` | **accepted**, exit 0 |
| `file:cosign.key` | `open file:cosign.key: no such file or directory` |
| `file://cosign.key`, `file:///w/cosign.key`, `file://./cosign.key` | `loading URL: unrecognized scheme: file://` |

So:

- **Form 3 is cosign's only file spelling.** Confirmed.
- **Form 1 is not cosign.** `file://` is refused outright — `blob.LoadFileOrURL` sees
  `://` and hands off to a URL loader that knows only http/https. OCX accepting it is a
  superset, not parity.
- **Form 2 is worse than "not cosign": it actively disagrees with cosign.** One value, two
  files. This is the finding that reversed the decision.

Two further cosign facts from the same run, load-bearing for the Deferred section:

| `--key` value | cosign result |
|---|---|
| `awskms:us-east-1/abc`, `hashivault:transit/k` | `open <value>: no such file or directory` |
| `k8s://ns/name` | dispatched to the k8s backend |
| `pkcs11:token=x` | dispatched to the pkcs11 backend |

`pkcs11:` is the **only** single-colon prefix cosign honours (RFC 7512 URIs carry no `//`).

### E-2 — the single-colon defect, measured

Built binary, clean `OCX_HOME`, `ocx package verify --key <ref> ghcr.io/acme/thing:1`:

| `--key` value | exit | message |
|---|---|---|
| `etc/nope.pub`, `file:etc/nope.pub`, `file://etc/nope.pub` | 74 | `… cannot be read: etc/nope.pub` — all three converge, the prefix silently stripped |
| `awskms://alias/release` | **85** | `unsupported key backend 'awskms'` |
| `awskms:us-east-1/abc`, `k8s:ns/secret` | **74** | `… cannot be read: awskms:us-east-1/abc` |
| `vault://x` | 64 | `unknown key reference scheme 'vault'` |
| `file:` | 64 | `key reference is empty` |

Row 1 is the defect: `file:etc/nope.pub` and `etc/nope.pub` name the same file to OCX and
different files to cosign.

### E-3 — the `index` side has no single-colon `file:` concept at all

`scheme_of` (`ocx_index.rs:520`) splits on `://` only, so `file:/x` and `file:x` never
reach `resolve_file_base`; they fall into the http/https arm, where
`config::mirror::parse_url` reads `file:` as the **host**. Measured, one
`[registries."probe.example"] index` value per run:

| `index` value | exit | what happens |
|---|---|---|
| `file:///srv/ocx-index/corp` | 69 | proper file base; dir absent |
| `file://host/srv/corp`, `file:///` | **78** | `invalid index url` — authority and bare root refused |
| `file:/srv/ocx-index/corp` | 69 | **silently becomes `https://file:/srv/…`** → DNS lookup of `file` |

There is therefore no `file:` form on the `index` side for form 2 to be consistent with.
Form 2 was consistent with nothing in the codebase.

### E-4 — form 2 was not round-trippable, and the code said so

`TrustPolicy::anchor_relative_keys` rewrote a relative signer key to be absolute against
the declaring `config.toml`'s directory — and **dropped the `file:` prefix** doing it, a
wart its own comment documented. The documented example spelling did not survive one pass
of the config loader.

### E-5 — no published artifact can carry form 2

A managed-config payload **refuses every path-form key signer**
(`validate_managed_config_payload` rule 5, `names_a_path`), acceptance-pinned at exit 78. A
key reference by path exists only in a *local*, operator-owned `config.toml` / `ocx.toml`.
Never in an OCI manifest, never in `ocx.lock`, never in package metadata.

The read-path backward-compat carve-out in `CLAUDE.md` ("already-published packages must
keep resolving") therefore does not reach this grammar. The break is confined to text on an
operator's own disk, and it fails loudly at parse time with a message naming the fix.

## Decision

**Remove form 2. `file:<path>` becomes an error whose message carries the fix.**

The shorthand test settles it on its own: `file:etc/x.pub` is seven characters *longer*
than `etc/x.pub`, so it is not a shorthand and not the common case. The evidence removes
every remaining defence — no cosign basis (E-1), no `index` basis (E-3), not
round-trippable (E-4), no published artifact to protect (E-5).

**Form 1 (`file://<rest>`) stays.** It is the only escape for a path containing `://`
(rule 1 splits on the first one, so `./weird://name` is otherwise unreachable), and it is
the spelling a reader of `[registries.<ns>] index` in the same file already knows. A
superset of cosign, not a disagreement with it.

**Form 1 is deliberately *not* tightened to the `index` rules.** `index` names a directory
root joined against for many fetches, where a CWD-relative root is a real hazard; `key`
names one file, and a relative one resolves against the declaring `config.toml`'s
directory — deterministic, CWD-independent, and the documented intent of
`etc/acme-release.pub`. The two `file://` grammars differ on purpose; the docs say so
rather than implying one rule.

### Resulting grammar, in evaluation order

Two accepting rules, and one refusal.

1. The value contains `://` — the text before the **first** `://` is the scheme token,
   `rest` is the remainder verbatim.
2. Otherwise the whole value is a bare file path. cosign's only file spelling, and now
   OCX's only one too.
3. Except `file:` with a single colon — `FileColonPrefix` (64), whose message names the
   bare path as the fix. The one token claimed by a refusal; every other single-colon value
   (`awskms:alias/x`, `C:\keys\cosign.pub`) is rule 2, exactly as it is to cosign.
4. A scheme token outside `Scheme::SPELLINGS` — `UnknownScheme` (64).
5. A recognised but unimplemented scheme — `UnsupportedBackend` (85).
6. An empty `rest` — `Empty` (64), so `file:` alone is unchanged.

A Windows drive path is untouched: rule 3 keys on the literal token `file`, so
`C:\keys\cosign.pub` falls to rule 2. E-01 is preserved.

A file *genuinely* named `file:x` stays addressable, as `file://file:x` — rule 1 hands back
`rest` verbatim. Rule 3 is a grammar rule, not a hole in the addressable filesystem.
`key_ref_parse_table` pins that composition.

## Remediation

### Parser — `crates/ocx_lib/src/oci/sign/key_ref.rs`

The collapsed rule-2/3 arm (`None => (Scheme::File, value.strip_prefix("file:").unwrap_or(value))`)
becomes:

```rust
// No `://`. The one value that is not a bare path is a `file:` prefix —
// a near-miss for rule 1 that cosign reads as a literal filename, so it
// names its fix rather than silently meaning a different file here than
// it does there.
None => match value.strip_prefix("file:") {
    Some("") => return Err(KeyRefError::Empty),
    Some(path) => {
        return Err(KeyRefError::FileColonPrefix { path: path.to_owned() });
    }
    None => (Scheme::File, value),
},
```

No helper function: the rule keys on one literal token and `strip_prefix` is the whole of
it.

New error variant. The message **is** the migration story — there is no deprecation window,
so it must carry the fix, not just report the fault:

```rust
/// `file:<path>` — the removed single-colon prefix form. Exit 64.
#[error("key reference `file:{path}` is not a supported spelling; write the path on its own as `{path}`")]
FileColonPrefix {
    /// Everything after `file:` — the path the author meant.
    path: String,
},
```

### Classification — two sites, both compile-forced

`impl From<KeyRefError> for SignErrorKind` (`oci/sign/error.rs:490`) and
`impl From<KeyRefError> for VerifyErrorKind` (`oci/verify/error.rs:821`) are exhaustive
with no wildcard, so both failed to compile until `FileColonPrefix` was classified. It
joins `UnknownScheme | Empty` → `KeyReferenceInvalid` → **exit 64**.

No new `error.detail` slug — envelope slugs are per `SignErrorKind` / `VerifyErrorKind`
variant, not per `KeyRefError` variant. `key_reference_invalid` already covers it. **No
wire-format change.**

Trust side needed no edit: `trust.rs` maps any `KeyRefError` into
`TrustPolicyError::KeyReferenceInvalid`, whose `#[source]` surfaces the new message.

### Deletion this enabled

`TrustPolicy::anchor_relative_keys`: the `file:`-prefix caveat (E-4) is obsolete — a
`file:`-prefixed reference no longer parses, so the `let Ok(parsed) = … else { continue }`
guard leaves it for `compile()` to name, which is what the function's doc already said it
does.

### Tests

| Site | Change |
|---|---|
| `key_ref.rs::key_ref_parse_table` | `file:etc/acme-release.pub` moves out of the `Ok` rows into a rejection asserting `FileColonPrefix` **and** that the message contains the bare path. |
| `key_ref.rs::key_ref_parse_table` | New `Ok` row `awskms:us-east-1/abc` → itself, pinning the *dropped* rider: token-gating single-colon schemes reds it. New `Ok` row `file://file:etc/acme-release.pub` → `file:etc/acme-release.pub`, pinning that the refused spelling still names an addressable file. |
| `trust.rs::a_key_signers_reference_is_the_same_grammar_the_key_flag_parses` | Value swapped to the bare path; gains an assertion that a policy `key` takes the same refusal `--key` does — one grammar, one parser, on the refusal too. |
| `trust.rs::anchor_relative_keys_treats_a_bare_path_like_a_file_reference` | Renamed to `…leaves_the_removed_file_colon_spelling_alone`: the removed spelling is left verbatim for `compile()` to name. The bare-path anchor claim moves to its sibling, which no longer duplicates it. |
| `managed_config/publish.rs::validate_rejects_a_key_signer_declared_by_path` | The `file:` row becomes a refusal assertion, not a deletion: structurally `InvalidTrustPolicy{KeyReferenceInvalid{FileColonPrefix}}` and **not** `ManagedConfigKeyByPath`, whose `key_pem` remedy is wrong advice for a misspelling. |
| `test/tests/test_verify.py` | **New.** `--key file:etc/acme-release.pub` → exit 64, `key_reference_invalid`, message names the bare path and never says "no such file". The CLI-boundary half no unit test can make. |
| `test/tests/test_trust_policy_signers.py` | Tuple entry dropped — that test asserts `"key_pem" in stderr`, which the grammar error correctly does not say. The refusal is pinned by the new acceptance test instead. |
| `test/tests/test_exit_codes.py` | Three `--key file:{path}` rows drop the prefix; they pin 74 for a missing key file, where the prefix was always incidental. |

### Docs — the estimate was low by 3x

An earlier draft budgeted "eight line edits across four files". The real census is **~24
lines across three website files, plus six Rust doc comments** the draft did not list.
`command-line.md` alone used `` `--key file:…` `` as prose shorthand for "the file form"
in **17** places, every one of which became false.

| File | Change |
|---|---|
| `website/src/docs/reference/configuration.md` | 7 lines: the `key` field table row, both `key = "file:…"` examples, and the relative / unreadable / publishing prose |
| `website/src/docs/reference/command-line.md` | 17 lines: the `--key` prose on `sign` and `verify`, the 78 and 85 exit rows, the `case $?` snippet, and every `error.detail` table row spelling the file form as `--key file:…` |
| `website/src/docs/in-depth/signing.md` | 1 line |
| `crates/ocx_cli/src/options/key.rs`, `command/package_sign_common.rs`, `oci/sign/key_backend.rs`, `oci/verify/error.rs` | 8 doc comments naming the removed spelling |

Per the standing no-migration-prose rule — **no migration note in the docs.** The docs
describe the grammar as it is; the error message is the whole migration story, and the
changelog line is the commit subject.

### Changelog

The commit subject is the changelog entry. Breaking, so `!`:

```
feat(trust)!: take a key reference as a bare path or file://, never file:
```

## Consequences

**Positive.** One canonical spelling for a file, and it is cosign's. A value pasted from a
cosign `--key` now means the same thing in both tools or is refused — never silently
something else. `anchor_relative_keys` loses a wart it documented but could not fix.

**Negative.** Every `config.toml` using the documented `file:` spelling breaks. Scoped to
local operator files (E-5), loud at parse time, and the message names the exact
replacement. This repo was the largest known user of the spelling, in fixtures.

**Accepted divergence.** OCX accepts `file://`, which cosign refuses (E-1). A superset of
cosign in the direction of naming a problem rather than mis-naming it.

## Deferred — not part of this decision

**Routing single-colon recognised schemes to exit 85 — proposed twice, rejected.** Both
earlier revisions had `awskms:alias/x` answer `UnsupportedBackend` (85) rather than being
read as a filename (74), via a `single_colon_scheme` token gate.

The reason it is out is not that cosign also answers 74 — that is true (E-1) and
incidental. **The reason is that this change exists to collapse the grammar to two rules,
and a token gate reintroduces exactly the single-colon scheme parsing being deleted, in the
same change, for a backend we do not implement.** It also pre-empts the `pkcs11:` question
below, which is the real decision about which schemes may legitimately take one colon.
`key_ref_parse_table` carries `awskms:us-east-1/abc` as an `Ok` row, so reintroducing the
rider reds a test rather than passing silently.

**`pkcs11:` is a cosign spelling OCX does not recognise** (E-1), so `pkcs11:token=x` is a
bare path and reports a missing file. Adding it is one `Scheme` variant, one `SPELLINGS`
entry, one `parse` arm and `is_implemented() == false` — but with the token gate gone it
would also need a rule of its own, since it carries no `://`. Left out because it widens
the recognised vocabulary rather than fixing the grammar, and because it and the `awskms:`
rider are one decision, not two: *which schemes may legitimately take a single colon.*

**The shared local-file reference type** — `[registries.<ns>] index`, `signers[].key`,
`[trust.sigstore] trusted_root`, `--key`, `--sigstore-trusted-root` behind one grammar:
[ocx-sh/ocx#379](https://github.com/ocx-sh/ocx/issues/379). Two spellings now, not three.
Watch the trap recorded there: `index` must **never** accept a bare path, because a
schemeless `index = "index.corp.example"` already means `https://index.corp.example`.

**Two Tier-3 defects surfaced during the census** and are separate issues: a relative
`OCX_CEILING_PATH` can never equal the always-absolute `current` in
`walk_for_project_file`, so the ceiling silently never fires; and the two `OCX_HOME`
fallbacks use different home-dir APIs (`loader::home_dir` uses `dirs::home_dir`,
`file_structure::default_ocx_root` uses `std::env::home_dir`).

**The `index` side's silent `file:/x` reinterpretation (E-3)** — `index = "file:/srv/x"`
should be an `InvalidIndexUrl` (78) naming `file:///srv/x`, not a DNS failure for a host
called `file`. Same shape as the defect fixed here, different parser, no shared code.
